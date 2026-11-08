use std::thread;
use std::time::{Duration, SystemTime};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::{Element, Size, Subscription, Task, window};
use kortty_jobs::{JobId, RunRequest};

use crate::connection_store::Store as ConnectionStore;
use crate::job_executor;
use crate::profile;
use crate::recording_window::RecordingWindow;
use crate::scheduler_window::{self, RemoteAction, SchedulerView, SchedulerWindow, SshCommand};
use crate::sftp_window::SftpWindow;
use crate::vault_window::Kind;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, Workspace, tool_tabs};

pub(super) const TITLE: &str = "JobScheduler";
const MAX_RUNNING_JOBS: usize = 4;

#[derive(Clone, Copy, Hash)]
struct Clock(Duration);

fn clock_stream(clock: &Clock) -> BoxStream<'static, ()> {
    let (sender, receiver) = async_channel::bounded(1);
    let interval = clock.0;
    let _ = thread::Builder::new()
        .name("kortty-desktop-clock".into())
        .spawn(move || {
            while !sender.is_closed() {
                thread::sleep(interval);
                let _ = sender.try_send(());
            }
        });
    receiver.boxed()
}

pub(super) fn desktop_clock(interval: Duration) -> Subscription<()> {
    Subscription::run_with(Clock(interval), clock_stream)
}

impl Workspace {
    pub(super) fn initialize_scheduler(&mut self, source: window::Id, existing_only: bool) -> bool {
        if self.scheduler.is_some() {
            return true;
        }
        let result = profile::jobs_path()
            .map_err(|error| error.to_string())
            .and_then(|path| {
                if existing_only && !path.exists() {
                    Ok(None)
                } else {
                    SchedulerWindow::open(path).map(Some)
                }
            });
        match result {
            Ok(Some(mut scheduler)) => {
                scheduler.set_execution_allowed(
                    self.clipboard.policy.problem.is_none() && !self.isolation_required(),
                );
                scheduler.set_recording_allowed(self.clipboard.policy.recording_allowed);
                self.scheduler = Some(scheduler);
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.notice(source, format!("Could not open the scheduler ({error})"));
                false
            }
        }
    }

    pub(super) fn show_scheduler(&mut self, source: window::Id) -> Task<Message> {
        self.show_scheduler_with_selection(source, None)
    }

    pub(super) fn show_scheduler_job(&mut self, source: window::Id, job: String) -> Task<Message> {
        self.show_scheduler_with_selection(source, Some(job))
    }

    fn show_scheduler_with_selection(
        &mut self,
        source: window::Id,
        job: Option<String>,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&source) {
            return Task::none();
        }
        let job = job.and_then(|job| JobId::new(job).ok());
        let hosted = self.window_settings.open_tool_windows_as_tabs;
        if hosted && let Some(id) = self.hosted_tool_in(source, tool_tabs::Kind::Scheduler) {
            if let Some((scheduler, view)) = self
                .scheduler
                .as_ref()
                .zip(self.scheduler_views.get_mut(&id))
                && let Some(job) = &job
            {
                scheduler.select_journal_job(view, job);
            }
            return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
        }
        if !self.initialize_scheduler(source, false) {
            return Task::none();
        }
        let connections = profile::connections_path()
            .and_then(ConnectionStore::open)
            .ok();
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.refresh_snippets();
            scheduler.set_connections(connections.as_ref());
        }
        self.refresh_scheduler_archive_passwords();
        let scheduler = self.scheduler.as_ref().expect("initialized scheduler");
        let mut view = scheduler.new_view();
        if let Some(job) = &job {
            scheduler.select_journal_job(&mut view, job);
        }
        if hosted {
            let id = window::Id::unique();
            self.scheduler_views.insert(id, view);
            let placed = self.host_tool(source, tool_tabs::Kind::Scheduler, id);
            if self.hosted_tool_in(source, tool_tabs::Kind::Scheduler) != Some(id) {
                self.scheduler_views.remove(&id);
            }
            return placed;
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::JobScheduler,
            window::Settings {
                size: Size::new(1360.0, 800.0),
                min_size: Some(Size::new(700.0, 440.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.scheduler_views.insert(id, view);
        self.scheduler_owners.insert(id, source);
        opened.map(|_| Message::Noop)
    }

    pub(super) fn is_scheduler_window(&self, id: window::Id) -> bool {
        self.scheduler_owners.contains_key(&id)
    }

    pub(super) fn native_scheduler_close_targets(
        &self,
        owner: Option<window::Id>,
    ) -> Vec<window::Id> {
        let mut targets = self
            .scheduler_owners
            .iter()
            .filter_map(|(&id, &source)| owner.is_none_or(|owner| owner == source).then_some(id))
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets
    }

    pub(super) fn scheduler_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let view = self.scheduler_views.get(&id)?;
        let scheduler = self.scheduler.as_ref()?;
        Some(
            view.view(scheduler, self.window_settings.job_menu_status_enabled)
                .map(move |message| Message::Scheduler(id, message)),
        )
    }

    pub(super) fn dismiss_hosted_scheduler(&mut self, id: window::Id) -> Task<Message> {
        if self.is_scheduler_window(id) {
            return Task::none();
        }
        self.update_scheduler(id, scheduler_window::Message::Dismiss)
    }

    pub(super) fn hosted_scheduler_dirty(&self, id: window::Id) -> bool {
        self.scheduler_views
            .get(&id)
            .is_some_and(SchedulerView::dirty)
    }

    pub(super) fn prompt_hosted_scheduler_close(&mut self, id: window::Id, token: u64) {
        if let Some(view) = self.scheduler_views.get_mut(&id) {
            view.hosted_close = Some(token);
            view.request_close();
        }
    }

    pub(super) fn cancel_hosted_scheduler_close(&mut self, id: window::Id) {
        if let Some(view) = self.scheduler_views.get_mut(&id)
            && view.hosted_close.take().is_some()
        {
            view.cancel_close();
        }
    }

    pub(super) fn remove_hosted_scheduler(&mut self, id: window::Id) {
        if !self.is_scheduler_window(id) {
            self.scheduler_views.remove(&id);
        }
    }

    pub(super) fn scheduler_subscription(&self) -> Subscription<Message> {
        let interval = if self.sftp.values().any(SftpWindow::transfer_running)
            || self.snippet_recovery_needs_tick()
        {
            Some(Duration::from_millis(200))
        } else if self
            .scheduler
            .as_ref()
            .and_then(SchedulerWindow::next_due)
            .is_some()
        {
            Some(Duration::from_secs(1))
        } else {
            None
        };
        interval.map_or_else(Subscription::none, |interval| {
            desktop_clock(interval).map(|()| Message::RuntimeTick)
        })
    }

    pub(super) fn runtime_tick(&mut self) -> Task<Message> {
        if self
            .scheduler
            .as_ref()
            .and_then(SchedulerWindow::next_due)
            .is_some_and(|due| due <= SystemTime::now())
        {
            self.update_scheduler_runtime(scheduler_window::Message::Tick)
        } else {
            #[cfg(target_os = "macos")]
            self.sync_native_jobs_menu();
            Task::none()
        }
    }

    pub(super) fn update_scheduler(
        &mut self,
        id: window::Id,
        message: scheduler_window::Message,
    ) -> Task<Message> {
        let Some(view) = self.scheduler_views.get(&id) else {
            return Task::none();
        };
        if let scheduler_window::Message::MenuStatusVisible(visible) = message {
            if self.window_settings.job_menu_status_enabled != visible
                && !self.exiting
                && !self.scheduler_draining
                && view.hosted_close.is_none()
                && !view.close_pending()
            {
                let mut settings = self.window_settings.clone();
                settings.job_menu_status_enabled = visible;
                match profile::window_settings_path().and_then(|path| settings.save(&path)) {
                    Ok(()) => {
                        self.window_settings = settings;
                        #[cfg(target_os = "macos")]
                        self.sync_native_jobs_menu();
                    }
                    Err(error) => {
                        if let Some(view) = self.scheduler_views.get_mut(&id) {
                            view.report_status(format!(
                                "Could not save Jobs menu settings ({error})"
                            ));
                        }
                    }
                }
            }
            return Task::none();
        }
        if let scheduler_window::Message::HostedClose(token, approve) = message {
            return if view.hosted_close == Some(token) {
                self.hosted_tool_close_answer(id, token, approve)
            } else {
                Task::none()
            };
        }
        if matches!(message, scheduler_window::Message::Dismiss)
            && let Some(token) = view.hosted_close
        {
            return self.hosted_tool_close_answer(id, token, false);
        }
        if self.exiting || self.scheduler_draining || view.hosted_close.is_some() {
            return Task::none();
        }
        if view.close_pending()
            && !matches!(
                message,
                scheduler_window::Message::CloseConfirmed | scheduler_window::Message::Dismiss
            )
        {
            return Task::none();
        }
        match message {
            scheduler_window::Message::OpenArchiveVault => {
                let owner = self
                    .scheduler_owners
                    .get(&id)
                    .copied()
                    .or_else(|| self.hosted_tool_owner(id));
                return owner.map_or_else(Task::none, |owner| self.show_vault(owner));
            }
            scheduler_window::Message::Close => {
                return if self.is_scheduler_window(id) {
                    self.close_window(id)
                } else {
                    self.request_hosted_tool_close(id)
                };
            }
            scheduler_window::Message::OpenRecording(run) => {
                let path = self
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.selected_recording_path(view, run));
                return path.map_or_else(Task::none, |path| {
                    self.show_recording_window(RecordingWindow::journal(path))
                });
            }
            scheduler_window::Message::DiagramCopy => {
                let source = view.diagram_mermaid();
                return source.map_or_else(Task::none, |source| self.write_clipboard(source));
            }
            scheduler_window::Message::DiagramExport(format) => {
                return view
                    .export_diagram(format)
                    .map(move |message| Message::Scheduler(id, message));
            }
            scheduler_window::Message::CloseConfirmed => {
                if self.is_scheduler_window(id)
                    && let (Some(scheduler), Some(view)) =
                        (self.scheduler.as_ref(), self.scheduler_views.get_mut(&id))
                    && scheduler.confirm_close(view)
                {
                    return self.close_scheduler(id);
                }
                return Task::none();
            }
            _ => {}
        }
        let Some((scheduler, view)) = self
            .scheduler
            .as_mut()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        let requests = scheduler.update_view(view, message);
        self.sync_scheduler_views();
        let task = self.enqueue_jobs(requests);
        #[cfg(target_os = "macos")]
        self.sync_native_jobs_menu();
        task
    }

    fn update_scheduler_runtime(&mut self, message: scheduler_window::Message) -> Task<Message> {
        let Some(scheduler) = self.scheduler.as_mut() else {
            return Task::none();
        };
        let requests = scheduler.update_runtime(message);
        self.sync_scheduler_views();
        let task = self.enqueue_jobs(requests);
        #[cfg(target_os = "macos")]
        self.sync_native_jobs_menu();
        task
    }

    fn sync_scheduler_views(&mut self) {
        if let Some(scheduler) = &self.scheduler {
            for view in self.scheduler_views.values_mut() {
                scheduler.sync_view(view);
            }
        }
    }

    pub(super) fn job_finished(&mut self, outcome: job_executor::Completion) -> Task<Message> {
        self.scheduler_workers = self.scheduler_workers.saturating_sub(1);
        let task = self.update_scheduler_runtime(scheduler_window::Message::Finished(outcome));
        if self.scheduler_draining
            && self
                .scheduler
                .as_ref()
                .is_none_or(|scheduler| !scheduler.has_active_runs())
        {
            return Task::batch([task, self.exit_after_workers()]);
        }
        task
    }

    pub(super) fn finish_cancelled_queued_job(&mut self, id: u64) -> Task<Message> {
        let Some(index) = self
            .scheduler_queue
            .iter()
            .position(|run| run.id.value() == id)
        else {
            return Task::none();
        };
        let Some(request) = self.scheduler_queue.remove(index) else {
            return Task::none();
        };
        self.update_scheduler_runtime(scheduler_window::Message::Finished(
            job_executor::Completion {
                id: request.id,
                outcome: kortty_jobs::RunOutcome::Cancelled,
                detail: None,
                recording: None,
            },
        ))
    }

    fn enqueue_jobs(&mut self, requests: Vec<RunRequest<SshCommand>>) -> Task<Message> {
        self.scheduler_queue.extend(requests);
        let mut tasks = Vec::new();
        let capture_directory = self
            .scheduler
            .as_ref()
            .and_then(SchedulerWindow::capture_directory);
        let snippet_path = self
            .scheduler
            .as_ref()
            .map(|scheduler| scheduler.snippet_path().to_owned());
        let file_transfer_allowed = self.clipboard.policy.file_transfer_allowed;
        while self.scheduler_workers < MAX_RUNNING_JOBS {
            let Some(request) = self.scheduler_queue.pop_front() else {
                break;
            };
            let id = request.id;
            let archive_password = self.scheduler_archive_password(&request, file_transfer_allowed);
            let saved_auth = self.scheduler_auth(
                &request,
                file_transfer_allowed,
                capture_directory.is_some(),
                archive_password.is_ok(),
            );
            let done = job_executor::spawn(
                request,
                capture_directory.clone(),
                snippet_path.clone(),
                file_transfer_allowed,
                saved_auth,
                archive_password,
            );
            self.scheduler_workers += 1;
            tasks.push(Task::future(async move {
                let result = done.recv().await.unwrap_or(job_executor::Completion {
                    id,
                    outcome: kortty_jobs::RunOutcome::Failed { exit_code: None },
                    detail: Some("SSH job worker ended without a result".into()),
                    recording: None,
                });
                Message::JobFinished(result)
            }));
        }
        Task::batch(tasks)
    }

    pub(super) fn refresh_scheduler_archive_passwords(&mut self) {
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_archive_passwords(
                self.vault
                    .as_ref()
                    .map_or_else(Vec::new, |vault| vault.choices(Kind::ArchivePassword)),
            );
        }
    }

    fn scheduler_archive_password(
        &self,
        request: &RunRequest<SshCommand>,
        file_transfer_allowed: bool,
    ) -> job_executor::ArchivePassword {
        if request.cancellation.is_requested()
            || !file_transfer_allowed
            || request.action.remote.is_none()
        {
            return Ok(None);
        }
        let RemoteAction::Archive(archive) = request.action.remote_action()? else {
            return Ok(None);
        };
        let Some(id) = archive.password_credential.as_deref() else {
            return Ok(None);
        };
        self.vault
            .as_ref()
            .ok_or("Unlock the vault before running an encrypted ZIP job.")?
            .archive_password(id)
            .map(Some)
    }

    fn scheduler_auth(
        &self,
        request: &RunRequest<SshCommand>,
        file_transfer_allowed: bool,
        recording_allowed: bool,
        archive_password_available: bool,
    ) -> job_executor::JobAuth {
        if request.cancellation.is_requested()
            || !archive_password_available
            || (!recording_allowed && request.action.virtual_terminal.is_some())
            || (!file_transfer_allowed
                && request
                    .action
                    .remote_action()
                    .is_ok_and(|action| action.transfers_files()))
        {
            return if request.action.additional_connections.is_empty() {
                job_executor::JobAuth::Manual
            } else {
                job_executor::JobAuth::Batch(Err("This SSH job is not allowed to start"))
            };
        }
        if !request.action.additional_connections.is_empty() {
            return job_executor::JobAuth::Batch(self.scheduler_batch_auth(&request.action));
        }
        let action = &request.action;
        let Some(reference) = &action.connection else {
            return job_executor::JobAuth::Manual;
        };
        let saved = self
            .saved_connection(reference.id)
            .ok_or("The saved SSH connection is missing or unavailable.")
            .and_then(|current| {
                job_executor::Credentials::resolve(action, &current, |id, kind, endpoint| {
                    self.vault_credential(id, kind, endpoint)
                })
            })
            .map(Box::new);
        job_executor::JobAuth::Saved(saved)
    }

    fn scheduler_batch_auth(&self, action: &SshCommand) -> job_executor::BatchAuth {
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        job_executor::Credentials::resolve_batch(action, &store, |id, kind, endpoint| {
            self.vault_credential(id, kind, endpoint)
        })
    }

    pub(super) fn request_scheduler_close(&mut self, id: window::Id) -> Option<Task<Message>> {
        if !self.is_scheduler_window(id) {
            return None;
        }
        if self
            .scheduler_views
            .get(&id)
            .is_some_and(|view| view.hosted_close.is_some())
        {
            return Some(Task::none());
        }
        if self
            .scheduler_views
            .get_mut(&id)
            .is_none_or(SchedulerView::request_close)
        {
            Some(self.close_scheduler(id))
        } else {
            Some(Task::none())
        }
    }

    pub(super) fn close_native_scheduler_after_preflight(
        &mut self,
        id: window::Id,
    ) -> Task<Message> {
        if self.scheduler_owners.remove(&id).is_none() {
            return Task::none();
        }
        self.scheduler_views.remove(&id);
        self.close_placed_dialog(id)
    }

    fn close_scheduler(&mut self, id: window::Id) -> Task<Message> {
        let closed = self.close_native_scheduler_after_preflight(id);
        if self.should_exit_after_window_close() {
            Task::batch([closed, self.quit()])
        } else {
            closed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-scheduler-view-{}",
                crate::private_files::nonce().unwrap(),
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hosted_scheduler_keeps_the_view_after_transfer_and_leaves_the_runtime_open() {
        let scratch = Scratch::new();
        let mut workspace = Workspace {
            scheduler: Some(SchedulerWindow::open(scratch.0.join("jobs.json")).unwrap()),
            ..Workspace::default()
        };
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        let _ = workspace.show_scheduler(source);
        let id = workspace
            .hosted_tool_in(source, tool_tabs::Kind::Scheduler)
            .unwrap();
        let tab = workspace.windows[&source].tabs[0].id();
        let _ = workspace.show_scheduler(source);
        assert_eq!(workspace.scheduler_views.len(), 1);
        assert!(workspace.scheduler_view(id).is_some());
        assert!(!workspace.is_scheduler_window(id));

        let _ = workspace.update_scheduler(id, scheduler_window::Message::Enabled(false));
        let _ = workspace.transfer(source, Some(destination), tab);
        assert_eq!(workspace.hosted_tool_owner(id), Some(destination));
        assert!(workspace.hosted_scheduler_dirty(id));
        let _ = workspace.request_hosted_tool_close(id);
        let token = workspace.scheduler_views[&id].hosted_close.unwrap();
        let _ =
            workspace.update_scheduler(id, scheduler_window::Message::HostedClose(token, false));
        assert!(workspace.hosted_scheduler_dirty(id));

        let _ = workspace.request_hosted_tool_close(id);
        let token = workspace.scheduler_views[&id].hosted_close.unwrap();
        let _ = workspace.update_scheduler(id, scheduler_window::Message::HostedClose(token, true));
        assert!(workspace.scheduler_views.is_empty());
        assert!(workspace.scheduler.is_some());
        assert!(workspace.hosted_tool_owner(id).is_none());
        assert!(workspace.sessions.is_empty());
    }

    #[test]
    fn two_native_scheduler_windows_share_one_runtime_and_cancel_quit_together() {
        let scratch = Scratch::new();
        let path = scratch.0.join("jobs.json");
        let mut workspace = Workspace {
            scheduler: Some(SchedulerWindow::open(path.clone()).unwrap()),
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        let (other_owner, _) = workspace.open_window(false);
        let _ = workspace.show_scheduler(owner);
        let _ = workspace.show_scheduler(owner);
        let targets = workspace.native_scheduler_close_targets(Some(owner));
        let [first, second] = targets.as_slice() else {
            panic!("each request should create its own native Scheduler window");
        };
        let (first, second) = (*first, *second);
        assert_eq!(workspace.native_scheduler_close_targets(None), targets);
        assert!(
            workspace
                .native_scheduler_close_targets(Some(other_owner))
                .is_empty()
        );
        assert_eq!(workspace.scheduler_views.len(), 2);
        assert!(SchedulerWindow::open(path).is_err());

        let _ = workspace.update_scheduler(first, scheduler_window::Message::Enabled(false));
        assert!(workspace.hosted_scheduler_dirty(first));
        assert!(!workspace.hosted_scheduler_dirty(second));
        let _ =
            workspace.update_scheduler(second, scheduler_window::Message::ScheduleEnabled(true));
        assert!(workspace.hosted_scheduler_dirty(second));

        assert!(workspace.guard_hosted_quit(false).is_some());
        let token = workspace.scheduler_views[&first].hosted_close.unwrap();
        let _ =
            workspace.update_scheduler(second, scheduler_window::Message::HostedClose(token, true));
        assert!(workspace.scheduler_views[&second].hosted_close.is_none());
        let _ =
            workspace.update_scheduler(first, scheduler_window::Message::HostedClose(token, true));
        assert_eq!(workspace.scheduler_views[&second].hosted_close, Some(token));
        let _ = workspace.update_scheduler(second, scheduler_window::Message::Dismiss);
        for id in [first, second] {
            assert!(workspace.is_scheduler_window(id));
            assert!(workspace.hosted_scheduler_dirty(id));
            assert!(!workspace.scheduler_views[&id].close_pending());
        }

        assert!(workspace.request_scheduler_close(first).is_some());
        let _ = workspace.update_scheduler(first, scheduler_window::Message::CloseConfirmed);
        assert_eq!(
            workspace.native_scheduler_close_targets(Some(owner)),
            [second]
        );
        assert!(workspace.hosted_scheduler_dirty(second));
        assert!(workspace.scheduler.is_some());
    }
}
