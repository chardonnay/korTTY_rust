//! Application-owned automation journal retention and closing-summary handoff.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::Task;
use jiff::Timestamp;
use kortty_jobs::{RunId, RunRequest};

use crate::clipboard_policy::ClipboardPolicy;
use crate::job_executor::automation::Plan;
use crate::scheduler_window::SshCommand;
use crate::session_journal::automation::retention::{ResultSet, Sweep};
use crate::session_journal::automation::{Artifact, Caps, CaptureSettings, Kind, Options, Run};
use crate::session_journal::tree::SourceKind;
use crate::session_journal::{LogFormat, Summary};

use super::{Message as Desktop, Workspace};

mod run_links;

const STARTUP_DELAY: Duration = Duration::from_secs(60);
const PERIOD: Duration = Duration::from_secs(60 * 60);
const POLICY_CHECK_PERIOD: Duration = Duration::from_secs(1);
const MAX_COMPLETED: usize = 128;

#[derive(Clone, Debug)]
pub enum Message {
    Swept(u64, Result<Arc<ResultSet>, String>),
    RunJournalsLoaded(
        Arc<run_links::Request>,
        Result<Arc<run_links::RunJournals>, String>,
    ),
}

#[derive(Default)]
pub(super) struct State {
    started: bool,
    scope: Option<Scope>,
    capture_allowed: bool,
    installed_capture_check: Option<(Instant, bool)>,
    stopped: Arc<AtomicBool>,
    next: Option<Instant>,
    recovery_needed: bool,
    active: Option<Active>,
    reading_run: Option<run_links::Lookup>,
    queued_run_journals: BTreeMap<iced::window::Id, run_links::Lookup>,
    scheduler_writers: HashSet<RunId>,
    completed: Vec<Artifact>,
    status: Option<String>,
}

struct Active {
    ticket: u64,
    stopped: Arc<AtomicBool>,
    done: async_channel::Receiver<()>,
    completed: Vec<Artifact>,
    recovering: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Scope {
    root: PathBuf,
    format: LogFormat,
    max_parts: Option<u16>,
    caps: Caps,
    user_delete: bool,
    summarize: bool,
}

impl Workspace {
    pub(in crate::workspace) fn automation_journal_capture_allowed(&self) -> bool {
        !self.exiting
            && self.clipboard.policy.automation_journal_allowed()
            && self
                .journal_automation
                .installed_capture_check
                .is_none_or(|(_, allowed)| allowed)
    }

    pub(super) fn automation_journal_policy_needs_tick(&self) -> bool {
        !self.exiting
            && self.clipboard.policy.automation_journal_allowed()
            && (!self.scheduler_views.is_empty()
                || self.swarm_windows_are_open()
                || Arc::strong_count(&self.journal_automation.stopped) > 1)
    }

    pub(in crate::workspace) fn refresh_automation_journal_capture_policy(&mut self) {
        if !self.exiting && self.clipboard.policy.automation_journal_allowed() {
            self.journal_automation.installed_capture_check = Some((
                Instant::now() + POLICY_CHECK_PERIOD,
                ClipboardPolicy::load().automation_journal_allowed(),
            ));
        }
        self.apply_automation_journal_policy();
    }

    fn automation_journal_scope(&self) -> Option<Scope> {
        if self.exiting || !self.journal_allowed() {
            return None;
        }
        let policy = &self.clipboard.policy.terminal_logging;
        Some(Scope {
            root: self.journal_directory().ok()?,
            format: policy
                .journal_log_format()
                .unwrap_or(self.window_settings.session_journal.log_format),
            max_parts: policy.journal_max_log_parts(),
            caps: policy.automation_journal_caps(),
            user_delete: policy.journal_delete_allowed(),
            summarize: self.journal_summaries_allowed(),
        })
    }

    pub(super) fn initialize_automation_journals(&mut self) {
        self.journal_automation.started = true;
        self.revalidate_automation_journals();
    }

    pub(super) fn revalidate_automation_journals(&mut self) {
        if self.automation_journal_policy_needs_tick()
            && self
                .journal_automation
                .installed_capture_check
                .is_none_or(|(next, _)| Instant::now() >= next)
        {
            self.refresh_automation_journal_capture_policy();
        } else {
            self.apply_automation_journal_policy();
        }
    }

    fn apply_automation_journal_policy(&mut self) {
        let capture_allowed = self.automation_journal_capture_allowed();
        for view in self.scheduler_views.values_mut() {
            view.set_automation_journal_allowed(capture_allowed);
        }
        if !self.journal_automation.started {
            return;
        }
        let current = self.automation_journal_scope();
        let state = &mut self.journal_automation;
        if capture_allowed != state.capture_allowed
            || current.as_ref().map(|scope| &scope.root)
                != state.scope.as_ref().map(|scope| &scope.root)
        {
            state.stopped.store(true, Ordering::Release);
            state.stopped = Arc::new(AtomicBool::new(false));
            state.capture_allowed = capture_allowed;
        }
        if current != state.scope {
            if let Some(active) = &state.active {
                active.stopped.store(true, Ordering::Release);
            }
            state.completed.retain(|artifact| {
                current.as_ref().is_some_and(|scope| {
                    artifact.source.directory().parent() == Some(scope.root.as_path())
                })
            });
            state.next = current.as_ref().map(|_| Instant::now() + STARTUP_DELAY);
            state.recovery_needed = true;
            state.scope = current;
        }
    }

    pub(super) fn automation_journals_need_tick(&self) -> bool {
        self.journal_automation.next.is_some() && self.journal_automation.active.is_none()
    }

    pub(in crate::workspace) fn automation_retention_busy(&self) -> bool {
        self.journal_automation.active.is_some()
    }

    pub(in crate::workspace) fn automation_journal_protects(&self, summary: &Summary) -> bool {
        summary.source_kind != SourceKind::Interactive && self.automation_retention_busy()
    }

    pub(in crate::workspace) fn automation_journal_status(&self) -> Option<&str> {
        self.journal_automation.status.as_deref()
    }

    pub(super) fn stop_automation_journals(&mut self) -> Option<async_channel::Receiver<()>> {
        let state = &mut self.journal_automation;
        state.reading_run = None;
        state.queued_run_journals.clear();
        state.stopped.store(true, Ordering::Release);
        state.active.take().map(|active| {
            active.stopped.store(true, Ordering::Release);
            active.done
        })
    }

    pub(in crate::workspace) fn automation_capture(
        &mut self,
        kind: Kind,
        source_id: String,
        source_name: String,
        action: &'static str,
        started: Timestamp,
        options: Options,
    ) -> io::Result<(CaptureSettings, Run)> {
        self.initialize_automation_journals();
        if !self.automation_journal_capture_allowed() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Automation journal capture is disabled by policy",
            ));
        }
        let scope = self.journal_automation.scope.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Automation journals are unavailable",
            )
        })?;
        let run = Run::new(
            kind,
            source_id,
            source_name,
            action,
            started,
            options,
            scope.caps,
        )?;
        Ok((
            CaptureSettings {
                root: scope.root.clone(),
                format: scope.format,
                max_parts: scope.max_parts,
                markers: self.journal_marker_rules(),
                stopped: Arc::clone(&self.journal_automation.stopped),
            },
            run,
        ))
    }

    pub(super) fn scheduler_automation_capture(
        &mut self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Option<Plan>, String> {
        let action = request.action.as_ref();
        if request.cancellation.is_requested() {
            return Ok(None);
        }
        let Some(options) = action
            .automation_journal
            .as_ref()
            .filter(|options| options.enabled)
            .cloned()
        else {
            return Ok(None);
        };
        self.refresh_automation_journal_capture_policy();
        if !self.journal_allowed() {
            return Err(
                "Automation journal could not start (Session Journals are disabled by policy)."
                    .into(),
            );
        }
        if !self.automation_journal_capture_allowed() {
            return Ok(None);
        }
        if !action.supports_automation_journal() {
            return Err("Automation journal is unavailable for this scheduler action.".into());
        }
        let name = self
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.automation_job_name(&request.job))
            .ok_or("Automation journal could not identify this job.")?
            .to_owned();
        let kind = if let Some(remote) = &action.remote {
            remote.automation_action()
        } else if let Some(agent) = &action.agent {
            match agent.kind {
                crate::scheduler_window::agent::Kind::Agent => "AI_AGENT",
                crate::scheduler_window::agent::Kind::Swarm => "AI_SWARM",
            }
        } else if action.snippet.is_some() {
            "SNIPPET_SCRIPT"
        } else if action.script.is_some() {
            "SCRIPT"
        } else {
            "COMMAND"
        };
        let started = Timestamp::try_from(request.started_at)
            .map_err(|_| "Automation journal could not read the job start time.")?;
        let (capture, run) = self
            .automation_capture(
                Kind::Job,
                request.job.as_str().into(),
                name,
                kind,
                started,
                options,
            )
            .map_err(|error| format!("Automation journal could not start ({error})."))?;
        let plan = Plan::new(capture, run, self.clipboard.policy.terminal_logging.clone());
        self.journal_automation.scheduler_writers.insert(request.id);
        Ok(Some(plan))
    }

    pub(super) fn automation_scheduler_finished(&mut self, id: RunId) {
        self.journal_automation.scheduler_writers.remove(&id);
    }

    pub(in crate::workspace) fn automation_journals_completed(
        &mut self,
        artifacts: Vec<Artifact>,
    ) -> Task<Desktop> {
        if artifacts.is_empty() {
            return Task::none();
        }
        self.initialize_automation_journals();
        if self.journal_automation.scope.is_none() {
            return Task::none();
        }
        let state = &mut self.journal_automation;
        state.restore_completed(artifacts);
        state.next = Some(Instant::now());
        // The runtime tick observes the restored owner group and any sibling jobs.
        Task::none()
    }

    pub(super) fn tick_automation_journals(&mut self) -> Task<Desktop> {
        let state = &self.journal_automation;
        if state.active.is_some()
            || state.next.is_none_or(|next| next > Instant::now())
            || !state.scheduler_writers.is_empty()
            || self.swarm_journals_are_writing()
        {
            return Task::none();
        }
        let Some(scope) = state.scope.clone() else {
            return Task::none();
        };
        let protected = self.pending_journal_summary_sources();
        let cancelled_closing = self.cancelled_automation_summaries();
        let ticket = self.allocate();
        let state = &mut self.journal_automation;
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stopped);
        let completed = state.completed.clone();
        let recover = state.recovery_needed;
        let (finished, done) = async_channel::bounded(1);
        let (send, receive) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-journal-retention".into())
            .spawn(move || {
                let _finished = finished;
                let result = Sweep {
                    root: &scope.root,
                    caps: scope.caps,
                    user_delete: scope.user_delete,
                    summarize: scope.summarize,
                    cancelled_closing: &cancelled_closing,
                    protected: &protected,
                    completed: &completed,
                    recover,
                    now: Timestamp::now(),
                    cancelled: &worker_stop,
                }
                .run()
                .or_else(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        Ok(ResultSet::default())
                    } else {
                        Err(error)
                    }
                });
                let _ = send.try_send(result.map(Arc::new).map_err(|error| error.to_string()));
            });
        state.next = Some(Instant::now() + PERIOD);
        if worker.is_err() {
            state.status = Some("Could not start automation journal maintenance.".into());
            return Task::none();
        }
        state.recovery_needed = false;
        state.active = Some(Active {
            ticket,
            stopped,
            done,
            completed: std::mem::take(&mut state.completed),
            recovering: recover,
        });
        Task::future(async move {
            Desktop::AutomationJournal(Message::Swept(
                ticket,
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Automation journal maintenance stopped.".into())),
            ))
        })
    }

    pub(super) fn update_automation_journals(&mut self, message: Message) -> Task<Desktop> {
        let (ticket, result) = match message {
            Message::Swept(ticket, result) => (ticket, result),
            Message::RunJournalsLoaded(request, result) => {
                return self.finish_scheduler_session_journals(request, result);
            }
        };
        let state = &mut self.journal_automation;
        let Some(active) = state.active.take_if(|active| active.ticket == ticket) else {
            return Task::none();
        };
        if active.stopped.load(Ordering::Acquire) {
            state.recovery_needed |= active.recovering;
            state.restore_completed(active.completed);
            return self.tick_automation_journals();
        }
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                state.recovery_needed |= active.recovering;
                state.restore_completed(active.completed);
                state.status = Some(format!("Could not maintain automation journals ({error})."));
                return Task::none();
            }
        };
        state.status = (report.warnings != 0).then(|| {
            format!(
                "Automation journal maintenance skipped {} changed or unavailable journals.",
                report.warnings
            )
        });
        let refresh = if report.removed != 0 {
            state
                .scope
                .as_ref()
                .map(|scope| scope.root.clone())
                .map_or_else(Task::none, |root| {
                    self.refresh_journal_action_managers(&root)
                })
        } else {
            Task::none()
        };
        let scheduler_details = if report.removed == 0 {
            Task::none()
        } else {
            let views: Vec<_> = self.scheduler_views.keys().copied().collect();
            Task::batch(
                views
                    .into_iter()
                    .map(|id| self.preview_scheduler_session_journals(id)),
            )
        };
        Task::batch([
            refresh,
            scheduler_details,
            self.enqueue_automation_closing_summaries(&report.closing),
            self.tick_automation_journals(),
        ])
    }
}

impl State {
    fn restore_completed(&mut self, artifacts: Vec<Artifact>) {
        let Some(scope) = &self.scope else {
            return;
        };
        for artifact in artifacts
            .into_iter()
            .filter(|artifact| artifact.source.directory().parent() == Some(scope.root.as_path()))
        {
            if self.completed.len() < MAX_COMPLETED {
                self.completed.push(artifact);
            } else {
                // The writer's durable metadata still permits recovery after the buffer fills.
                self.recovery_needed = true;
            }
        }
    }
}

#[cfg(test)]
pub(super) mod route_tests;
