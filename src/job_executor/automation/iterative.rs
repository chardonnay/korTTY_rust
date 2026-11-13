//! A target's journal spans approved rounds without owning any SSH channel.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use kortty_ssh::TrustStore;
use zeroize::Zeroizing;

use crate::connection_store::Store;
use crate::job_executor::connection::reference_allowed;
use crate::scheduler_window::agent::Target;
use crate::terminal_agent::Status;

use super::{ClipboardPolicy, Outcome, Plan, Recorder, RedactionSecrets, RunOutcome};

#[derive(Clone)]
pub(crate) struct IterativeCapture {
    plan: Plan,
    state: Arc<Mutex<State>>,
}

enum State {
    Waiting(Option<Box<PreparedTarget>>),
    Recording(Box<Active>),
    Unavailable,
    Finished,
}

struct PreparedTarget {
    target: IterativeTarget,
    secrets: RedactionSecrets,
}

impl PreparedTarget {
    fn matches(&self, target: &IterativeTarget, secrets: &RedactionSecrets) -> bool {
        self.target.same_binding(target) && self.secrets.matches(secrets)
    }
}

struct Active {
    recorder: Recorder,
    target: IterativeTarget,
    secrets: RedactionSecrets,
    note_outcome: bool,
    last_exit: Option<i32>,
    truncated: bool,
    revoked: bool,
}

#[derive(Clone)]
pub(in crate::job_executor) struct IterativeTarget {
    pub(in crate::job_executor) approved: Target,
    pub(in crate::job_executor) connection_store: PathBuf,
    pub(in crate::job_executor) trust: TrustStore,
    pub(in crate::job_executor) vault_revision: Option<Weak<()>>,
}

impl IterativeTarget {
    fn current(&self, policy: &ClipboardPolicy) -> bool {
        let reference = &self.approved.reference;
        !self
            .vault_revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
            && reference_allowed(reference, policy.server_access())
            && Store::open(self.connection_store.clone())
                .ok()
                .and_then(|store| {
                    Target::capture(reference, store.get(reference.id)?, &self.trust).ok()
                })
                .as_ref()
                == Some(&self.approved)
    }

    fn same_binding(&self, other: &Self) -> bool {
        self.approved == other.approved
            && self.connection_store == other.connection_store
            && match (&self.vault_revision, &other.vault_revision) {
                (Some(current), Some(other)) => current.ptr_eq(other),
                (None, None) => true,
                _ => false,
            }
    }
}

impl IterativeCapture {
    pub(crate) fn new(plan: Plan) -> Self {
        Self {
            plan,
            state: Arc::new(Mutex::new(State::Waiting(None))),
        }
    }

    /// Binds an admitted Agent target without starting a capture writer.
    /// A model-only outcome has no acknowledged command or remote transcript.
    pub(crate) fn prepare_status_target(
        &self,
        approved: Target,
        connection_store: PathBuf,
        trust: TrustStore,
        vault_revision: Option<Weak<()>>,
        secrets: RedactionSecrets,
    ) {
        let target = IterativeTarget {
            approved,
            connection_store,
            trust,
            vault_revision,
        };
        let mut state = self.lock();
        if let State::Waiting(prepared) = &mut *state {
            match prepared {
                Some(current) if !current.matches(&target, &secrets) => *state = State::Unavailable,
                Some(_) => {}
                None => *prepared = Some(Box::new(PreparedTarget { target, secrets })),
            }
        }
    }

    /// Starts capture only after SSH confirms an exact reviewed exec request.
    pub(in crate::job_executor) fn command(
        &self,
        target: &IterativeTarget,
        secrets: &RedactionSecrets,
        policy: &ClipboardPolicy,
        command: &str,
    ) {
        let mut state = self.lock();
        if let State::Waiting(prepared) = &*state {
            if !self.plan.capture_enabled(policy) {
                *state = State::Finished;
                return;
            }
            let active = match prepared {
                Some(prepared) if !prepared.matches(target, secrets) => None,
                _ => self.start(target, secrets, policy, prepared.is_some()),
            };
            *state = active.map_or(State::Unavailable, |active| {
                State::Recording(Box::new(active))
            });
        }
        if let State::Recording(active) = &mut *state
            && active.may_write(&self.plan, target, secrets, policy)
        {
            active.last_exit = None;
            active.recorder.command(command);
        }
    }

    pub(in crate::job_executor) fn output(
        &self,
        target: &IterativeTarget,
        secrets: &RedactionSecrets,
        policy: &ClipboardPolicy,
        outcome: Option<&Outcome>,
    ) {
        let mut state = self.lock();
        let State::Recording(active) = &mut *state else {
            return;
        };
        if !active.may_write(&self.plan, target, secrets, policy) {
            return;
        }
        let Some(outcome) = outcome else {
            active.truncated = true;
            return;
        };
        active.last_exit = match outcome.status {
            Status::Exited(code) => i32::try_from(code).ok(),
            _ => {
                active.truncated = true;
                None
            }
        };
        // Mask each complete retained stream before splitting it into journal records.
        active
            .recorder
            .output(&outcome.stdout.text(), outcome.stdout.truncated());
        let stderr = outcome.stderr.text();
        if !stderr.trim().is_empty() {
            let stderr = Zeroizing::new(format!("[stderr] {}", stderr.as_str()));
            active.recorder.output(&stderr, outcome.stderr.truncated());
        }
        if outcome.stdout.truncated() || outcome.stderr.truncated() {
            active.truncated = true;
            active
                .recorder
                .output("Command output exceeded its capture limit.", false);
        }
    }

    pub(crate) async fn finish(self, outcome: RunOutcome) -> bool {
        let previous = std::mem::replace(&mut *self.lock(), State::Finished);
        let pending = match previous {
            State::Waiting(Some(prepared)) => {
                let policy = ClipboardPolicy::load();
                if !self.plan.capture_enabled(&policy) {
                    return true;
                }
                let Some(active) = self.start(&prepared.target, &prepared.secrets, &policy, true)
                else {
                    return false;
                };
                active.recorder.system_note(outcome_note(outcome));
                active.recorder.begin_finish_status(outcome.into(), false)
            }
            State::Recording(active) => {
                let policy = ClipboardPolicy::load();
                let current = !active.revoked
                    && self.plan.can_capture(&policy)
                    && active.target.current(&policy);
                if current && active.note_outcome {
                    active.recorder.system_note(outcome_note(outcome));
                }
                active.recorder.begin_finish(
                    outcome.into(),
                    active.last_exit,
                    active.truncated || !current,
                )
            }
            State::Unavailable => return false,
            State::Waiting(None) | State::Finished => return true,
        };
        let artifact = pending.complete().await;
        let complete = artifact.complete;
        self.plan.completed(artifact);
        complete
    }

    pub(crate) fn stop(&self) {
        let mut state = self.lock();
        match &mut *state {
            State::Waiting(_) => *state = State::Unavailable,
            State::Recording(active) => active.revoke(),
            State::Unavailable | State::Finished => {}
        }
    }

    fn start(
        &self,
        target: &IterativeTarget,
        secrets: &RedactionSecrets,
        policy: &ClipboardPolicy,
        note_outcome: bool,
    ) -> Option<Active> {
        if !self.plan.can_capture(policy) || !target.current(policy) {
            return None;
        }
        let destination = Plan::destination(&target.approved.reference).ok()?;
        let recorder = self
            .plan
            .start_masked(destination, secrets.clone())
            .ok()?
            .ordered_commands();
        Some(Active {
            recorder,
            target: target.clone(),
            secrets: secrets.clone(),
            note_outcome,
            last_exit: None,
            truncated: false,
            revoked: false,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn outcome_note(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Success => "Scheduled Agent target succeeded.",
        RunOutcome::Failed { .. } => "Scheduled Agent target failed.",
        RunOutcome::Blocked => "Scheduled Agent target was blocked.",
        RunOutcome::Cancelled => "Scheduled Agent target was cancelled.",
    }
}

impl Active {
    fn revoke(&mut self) {
        self.revoked = true;
        self.truncated = true;
    }

    fn may_write(
        &mut self,
        plan: &Plan,
        target: &IterativeTarget,
        secrets: &RedactionSecrets,
        policy: &ClipboardPolicy,
    ) -> bool {
        if self.revoked {
            return false;
        }
        if !self.target.same_binding(target)
            || !self.secrets.matches(secrets)
            || !plan.can_capture(policy)
            || !self.target.current(policy)
        {
            self.revoke();
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::executor::block_on;
    use jiff::Timestamp;
    use kortty_ssh::{Endpoint, HostKey};

    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::scheduler_window::ConnectionRef;
    use crate::session_journal::{
        CaptureKind,
        automation::{Caps, CaptureSettings, Kind, Options, Run},
        tree::RunStatus,
    };
    use crate::snippets::tests::Profile;
    use crate::ssh_dialog::LoginTarget;
    use crate::terminal_logging::Secrets;

    use super::*;

    #[test]
    fn admitted_agent_journal_records_a_typed_status_without_an_acknowledged_command() {
        let fixture = Profile::new();
        let path = fixture.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let id = store
            .save(Connection {
                id: 0,
                name: "Reviewed fixture".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: LoginTarget {
                        host: "fixture.example".into(),
                        port: 22,
                        username: "fixture".into(),
                    },
                    auth: Auth::VaultPassword("password-reference".into()),
                    keyboard_interactive: false,
                },
                jump: None,
                emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
                tunnels: Vec::new(),
                incognito: false,
                isolation: Default::default(),
                startup: Default::default(),
                logging: Default::default(),
                terminal_effect: None,
                command_timestamps: None,
                tab_color: None,
                paste_warning: None,
                window_geometry: None,
                terminal: Default::default(),
            })
            .unwrap();
        let connection = store.get(id).unwrap();
        let reference = ConnectionRef::from(connection);
        let key = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap();
        let trust =
            TrustStore::from_pins(vec![(Endpoint::new("fixture.example", 22).unwrap(), key)])
                .unwrap();
        let approved = Target::capture(&reference, connection, &trust).unwrap();
        let policy = ClipboardPolicy::load();
        let root = fixture.0.join("journals");
        let new_plan = || {
            let run = Run::new(
                Kind::Job,
                "fixture-agent".into(),
                "Fixture Agent".into(),
                "AI_AGENT",
                Timestamp::now(),
                Options {
                    enabled: true,
                    capture_input: true,
                    ..Options::default()
                },
                Caps::default(),
            )
            .unwrap();
            Plan::new(
                CaptureSettings {
                    root: root.clone(),
                    format: Default::default(),
                    max_parts: None,
                    markers: Default::default(),
                    stopped: Default::default(),
                },
                run,
                policy.terminal_logging.clone(),
            )
        };
        let plan = new_plan();
        let capture = IterativeCapture::new(plan.clone());
        let vault = Arc::new(());
        let secrets = Secrets::default().seal().unwrap();
        capture.prepare_status_target(
            approved.clone(),
            path.clone(),
            trust.clone(),
            Some(Arc::downgrade(&vault)),
            secrets.clone(),
        );
        assert!(!root.exists());
        assert!(block_on(capture.finish(RunOutcome::Blocked)));
        let artifacts = plan.take_completed();
        let [artifact] = artifacts.as_slice() else {
            panic!("expected one status-only journal");
        };
        let journal = artifact.source.read(None).unwrap().unwrap();
        assert_eq!(journal.summary.run_status, Some(RunStatus::Blocked));
        assert_eq!(
            journal.summary.automation_action.as_deref(),
            Some("AI_AGENT")
        );
        assert_eq!(journal.timeline.len(), 1);
        assert_eq!(journal.timeline[0].kind, "SYSTEM");
        let page = artifact.source.capture_page(None, None, None).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].kind, CaptureKind::Note);
        assert_eq!(page.records[0].text, outcome_note(RunOutcome::Blocked));

        let plan = new_plan();
        let capture = IterativeCapture::new(plan.clone());
        capture.prepare_status_target(approved, path, trust, Some(Arc::downgrade(&vault)), secrets);
        drop(vault);
        assert!(!block_on(capture.finish(RunOutcome::Blocked)));
        assert!(plan.take_completed().is_empty());
    }
}
