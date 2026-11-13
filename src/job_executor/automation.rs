//! Collects per-target session journals for scheduled SSH work.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kortty_jobs::RunOutcome;
use kortty_terminal::Terminal;
use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;
use crate::scheduler_window::{ConnectionRef, SshCommand};
use crate::session::journal_output_observer;
use crate::session_journal::automation::{Artifact, CaptureSettings, Destination, Recorder, Run};
use crate::terminal_agent::Outcome;
use crate::terminal_logging::{Policy, RedactionSecrets, Redactor, Secrets};

use super::{Completion, Credentials};

mod file;
mod iterative;
pub(crate) use iterative::IterativeCapture;
pub(super) use iterative::IterativeTarget;

#[derive(Clone)]
pub(crate) struct Plan(Arc<Scope>);

struct Scope {
    capture: CaptureSettings,
    run: Run,
    policy: Policy,
    artifacts: Mutex<Vec<Artifact>>,
}

impl Plan {
    pub(crate) fn new(capture: CaptureSettings, run: Run, policy: Policy) -> Self {
        Self(Arc::new(Scope {
            capture,
            run,
            policy,
            artifacts: Mutex::new(Vec::new()),
        }))
    }

    pub(super) fn start(
        &self,
        action: &SshCommand,
        credentials: Option<&Credentials>,
        archive_password: Option<&str>,
        policy: &ClipboardPolicy,
    ) -> io::Result<Option<Recorder>> {
        if !self.capture_enabled(policy) {
            return Ok(None);
        }
        if !self.can_capture(policy) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let target =
            match &action.connection {
                Some(reference) => Self::destination(reference)?,
                None => Destination {
                    key: crate::session_journal::automation::identity_digest(&serde_json::to_vec(
                        &(&action.host, action.port, &action.username),
                    )?)?,
                    label: "Manual SSH target".into(),
                    host: action.host.clone(),
                    port: Some(action.port),
                    username: action.username.clone(),
                },
            };
        let recorder = self.start_destination(target, credentials, archive_password)?;
        let recorder = if let Some(remote) = &action.remote {
            let recorder = recorder.ordered_commands();
            if let Ok(valid) = action.remote_action() {
                file::record_action(&recorder, valid);
            } else {
                recorder.command(remote.automation_action());
            }
            recorder
        } else {
            recorder
        };
        Ok(Some(recorder))
    }

    pub(super) fn finish_command(&self, recorder: Recorder, outcome: RunOutcome) -> Artifact {
        let exit = match outcome {
            RunOutcome::Success => Some(0),
            RunOutcome::Failed { exit_code } => exit_code,
            RunOutcome::Cancelled | RunOutcome::Blocked => None,
        };
        if self.can_capture(&ClipboardPolicy::load()) {
            recorder.finish(outcome.into(), exit)
        } else {
            recorder.finish_with_truncation(outcome.into(), exit, true)
        }
    }

    pub(super) fn finish_file(
        &self,
        recorder: Recorder,
        completion: &Completion,
        detail: Option<&str>,
    ) -> Artifact {
        let current = self.can_capture(&ClipboardPolicy::load());
        if current {
            record_result(&recorder, completion.outcome, detail);
        }
        recorder.finish_status(completion.outcome.into(), !current)
    }

    pub(super) fn start_target(
        &self,
        reference: &ConnectionRef,
        credentials: Option<&Credentials>,
    ) -> io::Result<Recorder> {
        self.start_destination(Self::destination(reference)?, credentials, None)
    }

    fn destination(reference: &ConnectionRef) -> io::Result<Destination> {
        Ok(Destination {
            key: crate::session_journal::automation::identity_digest(&serde_json::to_vec(
                reference,
            )?)?,
            label: reference.target.label(),
            host: reference.target.host.clone(),
            port: Some(reference.target.port),
            username: reference.target.username.clone(),
        })
    }

    fn start_destination(
        &self,
        target: Destination,
        credentials: Option<&Credentials>,
        archive_password: Option<&str>,
    ) -> io::Result<Recorder> {
        let secrets = Secrets::default();
        if let Some(credentials) = credentials {
            credentials.add_journal_secrets(&secrets);
        }
        if let Some(password) = archive_password {
            secrets.add(password);
        }
        self.start_masked(target, secrets.seal()?)
    }

    fn start_masked(&self, target: Destination, secrets: RedactionSecrets) -> io::Result<Recorder> {
        let redact = Arc::new(Redactor::for_journal(secrets, self.0.policy.clone()));
        Recorder::start(&self.0.capture, self.0.run.clone(), target, redact)
    }

    pub(super) fn capture_enabled(&self, policy: &ClipboardPolicy) -> bool {
        !self.0.capture.stopped.load(Ordering::Acquire) && policy.automation_journal_allowed()
    }

    pub(super) fn can_capture(&self, policy: &ClipboardPolicy) -> bool {
        self.capture_enabled(policy)
            && policy
                .terminal_logging
                .equivalent_redactions(&self.0.policy)
    }

    pub(super) fn completed(&self, artifact: Artifact) {
        self.0
            .artifacts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(artifact);
    }

    pub(crate) fn take_completed(&self) -> Vec<Artifact> {
        std::mem::take(
            &mut *self
                .0
                .artifacts
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

/// An optional target journal stays open through that target's model decision.
pub(super) struct TargetJournal {
    plan: Plan,
    recorder: Option<Recorder>,
    truncated: AtomicBool,
    acknowledged: AtomicBool,
}

impl TargetJournal {
    pub(super) fn start(
        plan: &Plan,
        reference: &ConnectionRef,
        credentials: Option<&Credentials>,
        policy: &ClipboardPolicy,
    ) -> io::Result<Self> {
        Self::start_inner(plan, reference, credentials, policy, false)
    }

    pub(super) fn start_agent(
        plan: &Plan,
        reference: &ConnectionRef,
        credentials: Option<&Credentials>,
        policy: &ClipboardPolicy,
    ) -> io::Result<Self> {
        Self::start_inner(plan, reference, credentials, policy, true)
    }

    fn start_inner(
        plan: &Plan,
        reference: &ConnectionRef,
        credentials: Option<&Credentials>,
        policy: &ClipboardPolicy,
        ordered: bool,
    ) -> io::Result<Self> {
        if !plan.can_capture(policy) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let recorder = plan.start_target(reference, credentials)?;
        Ok(Self {
            plan: plan.clone(),
            recorder: Some(if ordered {
                recorder.ordered_commands()
            } else {
                recorder
            }),
            truncated: AtomicBool::new(false),
            acknowledged: AtomicBool::new(false),
        })
    }

    pub(super) fn capture<'a>(
        &'a self,
        load_policy: &'a (dyn Fn() -> ClipboardPolicy + Sync),
        route_current: &'a (dyn Fn() -> bool + Sync),
    ) -> ProbeCapture<'a> {
        ProbeCapture {
            journal: self,
            load_policy,
            route_current,
        }
    }

    pub(super) fn finish(
        mut self,
        outcome: RunOutcome,
        exit_code: Option<i32>,
        policy: &ClipboardPolicy,
        route_current: bool,
    ) -> bool {
        self.recheck_capture(policy, route_current);
        self.close(outcome, exit_code, false)
    }

    /// Seal the writer on its worker and let the caller publish in target order.
    pub(super) fn finish_deferred(
        mut self,
        outcome: RunOutcome,
        exit_code: Option<i32>,
        policy: &ClipboardPolicy,
        route_current: bool,
    ) -> Option<Artifact> {
        self.recheck_capture(policy, route_current);
        self.take_artifact(outcome, exit_code, false)
    }

    pub(super) fn finish_agent(
        mut self,
        outcome: RunOutcome,
        exit_code: Option<i32>,
        detail: Option<&str>,
        policy: &ClipboardPolicy,
        route_current: bool,
    ) -> bool {
        self.recheck_capture(policy, route_current);
        if !self.truncated.load(Ordering::Acquire)
            && let Some(recorder) = &self.recorder
        {
            record_result(recorder, outcome, detail);
        }
        let status_only = !self.acknowledged.load(Ordering::Acquire);
        self.close(outcome, exit_code, status_only)
    }

    fn recheck_capture(&self, policy: &ClipboardPolicy, route_current: bool) {
        if !self.plan.can_capture(policy) || !route_current {
            self.truncated.store(true, Ordering::Release);
        }
    }

    fn close(&mut self, outcome: RunOutcome, exit_code: Option<i32>, status_only: bool) -> bool {
        let Some(artifact) = self.take_artifact(outcome, exit_code, status_only) else {
            return true;
        };
        let complete = artifact.complete;
        self.plan.completed(artifact);
        complete
    }

    fn take_artifact(
        &mut self,
        outcome: RunOutcome,
        exit_code: Option<i32>,
        status_only: bool,
    ) -> Option<Artifact> {
        let recorder = self.recorder.take()?;
        let truncated = self.truncated.load(Ordering::Acquire);
        Some(if status_only {
            recorder.finish_status(outcome.into(), truncated)
        } else {
            recorder.finish_with_truncation(outcome.into(), exit_code, truncated)
        })
    }
}

impl Drop for TargetJournal {
    fn drop(&mut self) {
        self.close(RunOutcome::Failed { exit_code: None }, None, false);
    }
}

#[derive(Clone, Copy)]
pub(super) struct ProbeCapture<'a> {
    journal: &'a TargetJournal,
    load_policy: &'a (dyn Fn() -> ClipboardPolicy + Sync),
    route_current: &'a (dyn Fn() -> bool + Sync),
}

impl ProbeCapture<'_> {
    fn current(&self) -> Option<&Recorder> {
        if self.journal.truncated.load(Ordering::Acquire) {
            return None;
        }
        if !self.journal.plan.can_capture(&(self.load_policy)()) || !(self.route_current)() {
            self.journal.truncated.store(true, Ordering::Release);
            return None;
        }
        self.journal.recorder.as_ref()
    }

    pub(super) fn command(&self, command: &str) {
        self.journal.acknowledged.store(true, Ordering::Release);
        if let Some(recorder) = self.current() {
            recorder.command(command);
        }
    }

    pub(super) fn note(&self, text: &str) {
        if let Some(recorder) = self.current() {
            recorder.system_note(text);
        }
    }

    pub(super) fn output(&self, outcome: &Outcome) {
        if let Some(recorder) = self.current() {
            drain(outcome.stdout.text().as_bytes(), Some(recorder), false);
            drain(outcome.stderr.text().as_bytes(), Some(recorder), true);
            if outcome.stdout.truncated() || outcome.stderr.truncated() {
                recorder.output("Diagnostic output exceeded its capture limit.", false);
                self.journal.truncated.store(true, Ordering::Release);
            }
        }
    }
}

fn record_result(recorder: &Recorder, outcome: RunOutcome, detail: Option<&str>) {
    let status = match outcome {
        RunOutcome::Success => "Succeeded",
        RunOutcome::Failed { .. } => "Failed",
        RunOutcome::Blocked => "Blocked",
        RunOutcome::Cancelled => "Cancelled",
    };
    if let Some(detail) = detail {
        recorder.system_note(&Zeroizing::new(format!("{status} · {detail}")));
    } else {
        recorder.system_note(status);
    }
}

pub(super) fn drain(mut reader: impl Read, recorder: Option<&Recorder>, stderr: bool) -> bool {
    let Some(recorder) = recorder else {
        return io::copy(&mut reader, &mut io::sink()).is_ok();
    };
    let mut terminal = Terminal::new(120, 24);
    attach(&mut terminal, recorder, stderr);
    let mut bytes = [0; 8192];
    let completed = loop {
        match reader.read(&mut bytes) {
            Ok(0) => break true,
            Ok(count) => {
                terminal.advance(&bytes[..count]);
                let _ = terminal.take_responses();
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };
    terminal.set_journal_observer(None);
    completed
}

pub(super) fn attach(terminal: &mut Terminal, recorder: &Recorder, stderr: bool) {
    let (sink, redactor, stopped) = recorder.observer();
    terminal.set_journal_observer(Some(journal_output_observer(
        sink,
        redactor,
        if stderr { "[stderr] " } else { "" },
        false,
        Some(stopped),
    )));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use jiff::Timestamp;

    use crate::session_journal::automation::{Caps, Kind, Options};
    use crate::snippets::tests::Profile;

    use super::*;

    #[test]
    fn revoked_automation_capture_skips_worker_setup() {
        let fixture = Profile::new();
        let root = fixture.0.join("journals");
        let run = Run::new(
            Kind::Job,
            "fixture".into(),
            "Fixture".into(),
            "COMMAND",
            Timestamp::now(),
            Options {
                enabled: true,
                ..Options::default()
            },
            Caps::default(),
        )
        .unwrap();
        let settings = CaptureSettings {
            root: root.clone(),
            format: Default::default(),
            max_parts: None,
            markers: Default::default(),
            stopped: Arc::new(AtomicBool::new(true)),
        };
        let plan = Plan::new(settings, run, Policy::default());
        let (_, request) = super::super::tests::scheduled_request();
        assert!(
            plan.start(&request.action, None, None, &ClipboardPolicy::default())
                .unwrap()
                .is_none()
        );
        assert!(!root.exists());
    }

    #[test]
    fn automation_policy_disables_worker_capture_for_commands_and_files() {
        let fixture = Profile::new();
        let root = fixture.0.join("journals");
        let run = Run::new(
            Kind::Job,
            "fixture".into(),
            "Fixture".into(),
            "COMMAND",
            Timestamp::now(),
            Options {
                enabled: true,
                ..Options::default()
            },
            Caps::default(),
        )
        .unwrap();
        let settings = CaptureSettings {
            root: root.clone(),
            format: Default::default(),
            max_parts: None,
            markers: Default::default(),
            stopped: Arc::default(),
        };
        let plan = Plan::new(settings, run, Policy::default());
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.session-journal]\nautomation-allowed = false",
        )
        .unwrap();
        let (_, mut request) = super::super::tests::scheduled_request();
        assert!(
            plan.start(&request.action, None, None, &policy)
                .unwrap()
                .is_none()
        );
        let action = Arc::make_mut(&mut request.action);
        action.command.clear();
        action.remote = Some(
            serde_json::from_value(serde_json::json!({"kind": "mkdir", "path": "/owned"})).unwrap(),
        );
        assert!(plan.start(action, None, None, &policy).unwrap().is_none());
        assert!(!root.exists());
    }

    #[test]
    fn revoked_file_automation_capture_preserves_outcome_without_writing_result() {
        use crate::session_journal::tree::RunStatus;

        let fixture = Profile::new();
        let stopped = Arc::new(AtomicBool::new(false));
        let settings = CaptureSettings {
            root: fixture.0.join("journals"),
            format: Default::default(),
            max_parts: None,
            markers: Default::default(),
            stopped: Arc::clone(&stopped),
        };
        let run = Run::new(
            Kind::Job,
            "file-capture".into(),
            "File capture fixture".into(),
            "SFTP_MKDIR",
            Timestamp::now(),
            Options {
                enabled: true,
                capture_input: true,
                ..Options::default()
            },
            Caps::default(),
        )
        .unwrap();
        let plan = Plan::new(settings, run, Policy::default());
        let (_, mut request) = super::super::tests::scheduled_request();
        let action = Arc::make_mut(&mut request.action);
        action.command.clear();
        action.remote = Some(
            serde_json::from_value(serde_json::json!({"kind": "mkdir", "path": "/owned"})).unwrap(),
        );
        let journal = plan
            .start(&request.action, None, None, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();

        stopped.store(true, Ordering::Release);
        let completed = Completion::from_result(request.id, Ok(0));
        let artifact = plan.finish_file(journal, &completed, Some("Revoked result marker"));
        assert!(!artifact.complete);
        assert_eq!(completed.outcome, RunOutcome::Success);
        let document = artifact.source.read(None).unwrap().unwrap();
        assert_eq!(document.summary.run_status, Some(RunStatus::Success));
        let capture = artifact.source.capture_page(None, None, None).unwrap();
        assert!(
            capture
                .records
                .iter()
                .all(|record| !record.text.contains("Revoked result marker"))
        );
    }

    #[test]
    fn target_journals_record_started_commands_only_with_input_capture() {
        use crate::session_journal::CaptureKind;
        use crate::ssh_dialog::LoginTarget;

        for capture_input in [false, true] {
            let fixture = Profile::new();
            let run = Run::new(
                Kind::Job,
                "swarm".into(),
                "Fixture".into(),
                "AI_SWARM",
                Timestamp::now(),
                Options {
                    enabled: true,
                    capture_input,
                    ..Options::default()
                },
                Caps::default(),
            )
            .unwrap();
            let settings = CaptureSettings {
                root: fixture.0.join("journals"),
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Arc::new(AtomicBool::new(false)),
            };
            let plan = Plan::new(settings, run, Policy::default());
            let reference = ConnectionRef {
                id: 9,
                target: LoginTarget {
                    host: "fixture.example".into(),
                    port: 22,
                    username: "fixture".into(),
                },
                jump: None,
            };
            let policy = ClipboardPolicy::default();
            let started = TargetJournal::start(&plan, &reference, None, &policy).unwrap();
            started
                .capture(&ClipboardPolicy::default, &|| true)
                .command("fixed diagnostic");
            assert!(started.finish(RunOutcome::Success, Some(0), &policy, true));
            let blocked = TargetJournal::start(
                &plan,
                &ConnectionRef {
                    id: 10,
                    ..reference
                },
                None,
                &policy,
            )
            .unwrap();
            assert!(blocked.finish(RunOutcome::Blocked, None, &policy, true));

            let artifacts = plan.take_completed();
            assert_eq!(artifacts.len(), 2);
            assert_eq!(artifacts[0].outcome, RunOutcome::Success.into());
            assert_eq!(artifacts[1].outcome, RunOutcome::Blocked.into());
            for (index, artifact) in artifacts.into_iter().enumerate() {
                let page = artifact.source.capture_page(None, None, None).unwrap();
                let input: Vec<_> = page
                    .records
                    .iter()
                    .filter(|row| row.kind == CaptureKind::Input)
                    .map(|row| row.text.as_str())
                    .collect();
                if capture_input && index == 0 {
                    assert_eq!(input, ["fixed diagnostic"]);
                } else {
                    assert!(input.is_empty());
                }
            }
        }
    }
}
