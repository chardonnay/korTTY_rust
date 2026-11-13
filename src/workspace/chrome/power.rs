use std::sync::Arc;

use iced::{Task, window};
use kortty_windowing::{SystemSleepRequest, supports_system_sleep_prevention};

use crate::profile;
use crate::session::{SessionId, Status};

use super::{Message, Workspace, WorkspaceMessage, wrap};

#[derive(Default)]
pub(super) struct State {
    request: Option<Arc<SystemSleepRequest>>,
    pending: Option<Pending>,
    failed_for_current_work: bool,
    connected_terminal: Option<SessionId>,
    #[cfg(target_os = "macos")]
    terminal_activity: Option<kortty_macos::TerminalActivity>,
    #[cfg(target_os = "macos")]
    scheduler_activity: Option<kortty_macos::AppNapActivity>,
}

struct Pending {
    ticket: u64,
    reason: Reason,
}

enum Reason {
    Restore,
    Enable(Option<window::Id>),
}

#[derive(Clone, Debug)]
pub enum Event {
    Acquired(u64, Result<Arc<SystemSleepRequest>, String>),
}

pub(super) const fn label() -> &'static str {
    if supports_system_sleep_prevention() {
        "Prevent System Sleep"
    } else {
        "Prevent System Sleep (not yet supported on Linux)"
    }
}

impl Workspace {
    #[cfg(target_os = "macos")]
    pub(in crate::workspace) fn start_terminal_activity(&mut self) {
        match kortty_macos::TerminalActivity::start() {
            Ok(activity) => self.chrome.power.terminal_activity = Some(activity),
            Err(error) => {
                eprintln!("kortty: could not track terminal activity for App Nap ({error})")
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub(in crate::workspace) fn stop_terminal_activity(&mut self) {
        self.chrome.power.terminal_activity = None;
    }

    #[cfg(target_os = "macos")]
    pub(in crate::workspace) fn reconcile_scheduler_activity(&mut self) {
        let active = !self.exiting
            && self
                .scheduler
                .as_ref()
                .is_some_and(crate::scheduler_window::SchedulerWindow::has_active_runs);
        if active {
            self.chrome
                .power
                .scheduler_activity
                .get_or_insert_with(kortty_macos::AppNapActivity::begin);
        } else {
            self.chrome.power.scheduler_activity = None;
        }
    }

    pub(super) fn sleep_prevention_available(&self) -> bool {
        supports_system_sleep_prevention()
            && !self
                .chrome
                .power
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.reason, Reason::Enable(_)))
    }

    pub(super) fn sleep_prevention_checked(&self) -> bool {
        supports_system_sleep_prevention() && self.window_settings.prevent_system_sleep
    }

    pub(super) fn toggle_sleep_prevention(
        &mut self,
        owner: Option<window::Id>,
    ) -> Task<WorkspaceMessage> {
        if self.exiting || self.scheduler_draining || !self.sleep_prevention_available() {
            return Task::none();
        }
        if self.window_settings.prevent_system_sleep {
            if self.persist_sleep_prevention(false, owner) {
                self.chrome.power.failed_for_current_work = false;
                return release(self.take_sleep_request());
            }
        } else if self.sleep_work_active() {
            if let Some(pending) = &mut self.chrome.power.pending {
                pending.reason = Reason::Enable(owner);
                return Task::none();
            }
            return self.acquire_sleep_request(Reason::Enable(owner));
        } else {
            self.persist_sleep_prevention(true, owner);
        }
        Task::none()
    }

    pub(in crate::workspace) fn reconcile_sleep_prevention(&mut self) -> Task<WorkspaceMessage> {
        // Do not touch terminal frames on the default, disabled path.
        if self.exiting || !supports_system_sleep_prevention() {
            return Task::none();
        }
        if !self.window_settings.prevent_system_sleep {
            return release(self.take_sleep_request());
        }
        if self.sleep_work_active() {
            if self.chrome.power.request.is_none()
                && self.chrome.power.pending.is_none()
                && !self.chrome.power.failed_for_current_work
            {
                return self.acquire_sleep_request(Reason::Restore);
            }
        } else {
            self.chrome.power.failed_for_current_work = false;
            return release(self.take_sleep_request());
        }
        Task::none()
    }

    pub(in crate::workspace) fn take_sleep_request(&mut self) -> Option<Arc<SystemSleepRequest>> {
        self.chrome.power.request.take()
    }

    pub(super) fn update_sleep_prevention(
        &mut self,
        Event::Acquired(ticket, result): Event,
    ) -> Task<WorkspaceMessage> {
        let Some(Pending { reason, .. }) = self
            .chrome
            .power
            .pending
            .take_if(|pending| pending.ticket == ticket)
        else {
            return release(result.ok());
        };
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                match reason {
                    Reason::Restore => {
                        self.chrome.power.failed_for_current_work = true;
                        self.sleep_prevention_problem(None, error);
                    }
                    Reason::Enable(owner) => self.sleep_prevention_problem(owner, error),
                }
                return Task::none();
            }
        };
        let enabled = match reason {
            Reason::Restore => self.window_settings.prevent_system_sleep,
            Reason::Enable(owner) => self.persist_sleep_prevention(true, owner),
        };
        if enabled && self.sleep_work_active() {
            self.chrome.power.request = Some(request);
            Task::none()
        } else {
            release(Some(request))
        }
    }

    fn acquire_sleep_request(&mut self, reason: Reason) -> Task<WorkspaceMessage> {
        let ticket = self.allocate();
        self.chrome.power.pending = Some(Pending { ticket, reason });
        Task::perform(
            async {
                SystemSleepRequest::acquire()
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            move |result| wrap(Message::Power(Event::Acquired(ticket, result))),
        )
    }

    fn persist_sleep_prevention(&mut self, enabled: bool, owner: Option<window::Id>) -> bool {
        let previous = self.window_settings.prevent_system_sleep;
        self.window_settings.prevent_system_sleep = enabled;
        match profile::window_settings_path().and_then(|path| self.window_settings.save(&path)) {
            Ok(()) => true,
            Err(error) => {
                self.window_settings.prevent_system_sleep = previous;
                self.sleep_prevention_problem(owner, error.to_string());
                false
            }
        }
    }

    fn sleep_prevention_problem(&mut self, owner: Option<window::Id>, error: String) {
        let message = format!("Could not change system sleep prevention ({error}).");
        if let Some(owner) = owner {
            self.notice(owner, message);
        } else {
            eprintln!("{message}");
        }
    }

    fn sleep_work_active(&mut self) -> bool {
        let cached = self.chrome.power.connected_terminal;
        if cached.is_some_and(|id| {
            self.sessions
                .get(&id)
                .is_some_and(|pane| pane.frame.status == Status::Running)
        }) {
            return true;
        }
        let connected_terminal = self
            .sessions
            .iter()
            .find_map(|(&id, pane)| (pane.frame.status == Status::Running).then_some(id));
        self.chrome.power.connected_terminal = connected_terminal;
        connected_terminal.is_some()
            || self
                .scheduler
                .as_ref()
                .is_some_and(crate::scheduler_window::SchedulerWindow::requires_awake)
            || self
                .ascii_art
                .values()
                .any(super::super::ascii_art::Dialog::ai_active)
            || self.journal_screenshot_analyses.ai_active()
            || self.journal_note_translations.ai_active()
            || self.journal_exports.ai_active()
    }
}

fn release(request: Option<Arc<SystemSleepRequest>>) -> Task<WorkspaceMessage> {
    let Some(request) = request else {
        return Task::none();
    };
    Task::future(async move {
        drop(request);
        WorkspaceMessage::Noop
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use kortty_terminal::TerminalProfile;

    use crate::session::{GridSize, Session};
    use crate::workspace::tab_titles::WindowSettings;

    use super::super::{Action, Category, Entry};
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn app_nap_keeps_scheduled_work_awake_only_while_a_run_is_active() {
        use crate::job_executor::Completion;
        use crate::scheduler_window::{Message as SchedulerMessage, SchedulerWindow, SshCommand};
        use kortty_jobs::{Job, JobGraph, JobId, JobSchedule, RunOutcome, SchedulerStore};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("jobs.json");
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "host": "server.example", "port": 22, "username": "operator",
            "key_file": profile.0.join("fixture-key"), "command": "true",
        }))
        .unwrap();
        let job = Job::new(JobId::new("fixture").unwrap(), "Fixture", action)
            .unwrap()
            .with_schedule(JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]));
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();
        let mut scheduler = SchedulerWindow::open(path).unwrap();
        scheduler.set_execution_allowed(true);
        let due = scheduler.next_due().unwrap();
        let mut workspace = Workspace {
            scheduler: Some(scheduler),
            ..Workspace::default()
        };
        assert!(workspace.sleep_work_active());
        workspace.reconcile_scheduler_activity();
        assert!(workspace.chrome.power.scheduler_activity.is_none());

        let scheduler = workspace.scheduler.as_mut().unwrap();
        let mut started = scheduler.update_runtime_at(SchedulerMessage::Tick, due);
        let run = started.pop().unwrap();
        assert!(started.is_empty());
        workspace.reconcile_scheduler_activity();
        assert!(workspace.chrome.power.scheduler_activity.is_some());
        let scheduler = workspace.scheduler.as_mut().unwrap();
        scheduler.update_runtime_at(
            SchedulerMessage::Finished(Completion {
                id: run.id,
                outcome: RunOutcome::Cancelled,
                detail: None,
                recording: None,
                swarm_report: None,
            }),
            due,
        );
        workspace.reconcile_scheduler_activity();
        assert!(workspace.chrome.power.scheduler_activity.is_none());
    }

    #[test]
    fn sleep_prevention_menu_uses_opt_in_profile_state_and_platform_support() {
        assert!(matches!(
            Category::Configuration.entries().last(),
            Some(Entry::Action(Action::PreventSystemSleep))
        ));
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let supported = supports_system_sleep_prevention();
        let action = Action::PreventSystemSleep;
        assert_eq!(workspace.chrome_enabled(owner, action), supported);
        assert!(!workspace.chrome_checked(owner, &workspace.windows[&owner], action));
        assert!(
            !serde_json::from_str::<WindowSettings>("{}")
                .unwrap()
                .prevent_system_sleep
        );

        workspace.window_settings.prevent_system_sleep = true;
        assert_eq!(
            workspace.chrome_checked(owner, &workspace.windows[&owner], action),
            supported
        );
        let encoded = serde_json::to_vec(&workspace.window_settings).unwrap();
        assert!(
            serde_json::from_slice::<WindowSettings>(&encoded)
                .unwrap()
                .prevent_system_sleep
        );
        assert_eq!(workspace.reconcile_sleep_prevention().units(), 0);
        assert!(workspace.chrome.power.pending.is_none());
    }

    #[test]
    fn sleep_prevention_only_requests_during_work_and_retries_at_the_next_work_edge() {
        let mut workspace = Workspace::default();
        workspace.window_settings.prevent_system_sleep = true;
        let grid = GridSize::default();
        let session = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        let id = workspace.store_session(session, grid, 14);
        assert!(!workspace.sleep_work_active());

        let disconnected = Arc::clone(&workspace.sessions[&id].frame);
        let local = Session::start(grid, TerminalProfile::Xterm).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let running = loop {
            let frame = local.frame();
            if frame.status == Status::Running {
                break frame;
            }
            assert!(Instant::now() < deadline, "test shell did not start");
            std::thread::sleep(Duration::from_millis(5));
        };
        let stopped = local.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stopped.is_closed() {
            assert!(Instant::now() < deadline, "test shell did not stop");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Test double for frame delivery, using snapshots from real sessions.
        workspace.sessions.get_mut(&id).unwrap().frame = Arc::clone(&running);
        assert!(workspace.sleep_work_active());
        let requested = workspace.reconcile_sleep_prevention();
        if !supports_system_sleep_prevention() {
            assert_eq!(requested.units(), 0);
            return;
        }
        assert!(requested.units() > 0);
        let ticket = workspace.chrome.power.pending.as_ref().unwrap().ticket;
        assert_eq!(workspace.reconcile_sleep_prevention().units(), 0);
        // Test double for an OS acquisition error, without acquiring a power request.
        let _ = workspace
            .update_sleep_prevention(Event::Acquired(ticket, Err("test power error".into())));
        assert_eq!(workspace.reconcile_sleep_prevention().units(), 0);

        workspace.sessions.get_mut(&id).unwrap().frame = disconnected;
        assert!(!workspace.sleep_work_active());
        assert_eq!(workspace.reconcile_sleep_prevention().units(), 0);
        workspace.sessions.get_mut(&id).unwrap().frame = running;
        assert!(workspace.reconcile_sleep_prevention().units() > 0);
        assert_ne!(
            workspace.chrome.power.pending.as_ref().unwrap().ticket,
            ticket
        );
    }
}
