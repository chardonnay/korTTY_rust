use std::thread;
use std::time::{Duration, Instant, SystemTime};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::{Element, Size, Subscription, Task, window};
use kortty_jobs::{JobId, RunRequest, WebhookOutcome};

use crate::ai_manager::Configuration;
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Connection;
use crate::connection_store::Store as ConnectionStore;
use crate::job_executor;
use crate::profile;
use crate::recording_window::RecordingWindow;
use crate::scheduler_window::{
    self, DirectoryField, RemoteAction, SchedulerView, SchedulerWindow, SshCommand, SudoMode,
    SwarmHandoff, agent, webhooks,
};
use crate::session_journal::automation::Artifact;
use crate::sftp_window::SftpWindow;
use crate::vault_window::Kind;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, Workspace, tool_tabs};

pub(super) mod firing;
mod proposal_knowledge;
mod proposal_skills;
pub(super) mod swarm_chat;
pub(super) mod swarm_comparison;

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
                    SchedulerWindow::open_with_journal_retention(
                        path,
                        self.window_settings.job_scheduler_journal_retention_days,
                    )
                    .map(Some)
                }
            });
        match result {
            Ok(Some(mut scheduler)) => {
                scheduler.set_execution_allowed(
                    self.clipboard.policy.problem.is_none() && !self.isolation_required(),
                );
                scheduler.set_recording_allowed(self.clipboard.policy.recording_allowed);
                scheduler.set_swarm_chats_allowed(self.swarm_chat_access_allowed());
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

    pub(super) fn show_scheduler_swarm_handoff(
        &mut self,
        source: window::Id,
        handoff: SwarmHandoff,
    ) -> Result<Task<Message>, String> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&source) {
            return Err("The Swarm owner or JobScheduler is unavailable.".into());
        }
        let installed = ClipboardPolicy::load();
        let config = self.scheduler_agent_policy(agent::Kind::Swarm, &installed)?;
        let connections = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        if !self.initialize_scheduler(source, false) {
            return Err("Could not open JobScheduler.".into());
        }
        let scheduler = self.scheduler.as_mut().expect("initialized scheduler");
        scheduler.refresh_snippets();
        scheduler.set_connections(Some(&connections));
        scheduler.set_agent_profiles(Some(&config));
        self.refresh_scheduler_credentials();
        self.place_scheduler_swarm_handoff(source, handoff)
    }

    fn place_scheduler_swarm_handoff(
        &mut self,
        source: window::Id,
        handoff: SwarmHandoff,
    ) -> Result<Task<Message>, String> {
        let hosted = self.window_settings.open_tool_windows_as_tabs;
        let existing = hosted
            .then(|| self.hosted_tool_in(source, tool_tabs::Kind::Scheduler))
            .flatten();
        let capture = self.automation_journal_capture_allowed();
        let scheduler = self
            .scheduler
            .as_ref()
            .ok_or("JobScheduler is unavailable.")?;
        if let Some(id) = existing
            && let Some(view) = self.scheduler_views.get_mut(&id)
            && view.can_stage_swarm_handoff()
        {
            scheduler.stage_swarm_handoff(view, handoff)?;
            view.set_automation_journal_allowed(capture);
            let focused = self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            return Ok(Task::batch([
                focused,
                self.refresh_scheduler_journal_stats(),
            ]));
        }
        let mut view = scheduler.new_view();
        scheduler.stage_swarm_handoff(&mut view, handoff)?;
        view.set_automation_journal_allowed(capture);
        Ok(self.place_scheduler_view(source, view, hosted && existing.is_none()))
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
            let focused = self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            return Task::batch([focused, self.refresh_scheduler_journal_stats()]);
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
        self.refresh_scheduler_credentials();
        self.refresh_scheduler_agent_profiles();
        let scheduler = self.scheduler.as_ref().expect("initialized scheduler");
        let mut view = scheduler.new_view();
        view.set_automation_journal_allowed(self.automation_journal_capture_allowed());
        if let Some(job) = &job {
            scheduler.select_journal_job(&mut view, job);
        }
        self.place_scheduler_view(source, view, hosted)
    }

    fn place_scheduler_view(
        &mut self,
        source: window::Id,
        view: SchedulerView,
        hosted: bool,
    ) -> Task<Message> {
        if hosted {
            let id = window::Id::unique();
            self.scheduler_views.insert(id, view);
            let placed = self.host_tool(source, tool_tabs::Kind::Scheduler, id);
            if self.hosted_tool_in(source, tool_tabs::Kind::Scheduler) != Some(id) {
                self.scheduler_views.remove(&id);
            }
            return Task::batch([placed, self.refresh_scheduler_journal_stats()]);
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
        Task::batch([
            opened.map(|_| Message::Noop),
            self.refresh_scheduler_journal_stats(),
        ])
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
        let journal_root = self.journal_directory().ok();
        Some(
            view.view(
                scheduler,
                self.window_settings.job_menu_status_enabled,
                self.scheduler_privileged_action_allowed(view),
                journal_root.as_deref(),
            )
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
            || self.terminal_agents.needs_tick()
        {
            Some(Duration::from_millis(200))
        } else if self.automation_journal_policy_needs_tick()
            || self.scheduler_firing_retry_at.is_some()
            || self.swarm_chat_seed_needs_tick()
            || self.scheduler_swarm_comparisons.needs_tick()
            || self.scheduler.as_ref().is_some_and(|scheduler| {
                scheduler.next_due().is_some()
                    || scheduler.webhook_results_pending()
                    || scheduler.firing_reviews.next_deadline().is_some()
            })
        {
            Some(Duration::from_secs(1))
        } else if self.automation_journals_need_tick()
            || (!self.scheduler_views.is_empty() && self.journal_allowed())
        {
            Some(Duration::from_secs(30))
        } else {
            None
        };
        interval.map_or_else(Subscription::none, |interval| {
            desktop_clock(interval).map(|()| Message::RuntimeTick)
        })
    }

    pub(super) fn runtime_tick(&mut self) -> Task<Message> {
        let expiry = self.expire_firing();
        let retry = if self
            .scheduler_firing_retry_at
            .take_if(|deadline| *deadline <= Instant::now())
            .is_some()
        {
            self.enqueue_jobs(Vec::new())
        } else {
            Task::none()
        };
        let scheduler = if self
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
        };
        Task::batch([
            expiry,
            retry,
            scheduler,
            self.refresh_scheduler_journal_stats(),
            self.tick_automation_journals(),
            self.tick_swarm_chat_seed(),
            self.tick_swarm_comparisons(),
        ])
    }

    fn refresh_scheduler_journal_stats(&mut self) -> Task<Message> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let Some(id) = self.scheduler_views.keys().next().copied() else {
            return Task::none();
        };
        let root = self.journal_directory().ok();
        let Some(request) = self
            .scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.journal_stats.begin(root, Instant::now()))
        else {
            return Task::none();
        };
        Task::future(async move {
            let (sender, receiver) = async_channel::bounded(1);
            let worker_request = request.clone();
            let worker = thread::Builder::new()
                .name("kortty-scheduler-journal-stats".into())
                .spawn(move || {
                    let _ = sender.try_send(worker_request.read());
                });
            let result = if worker.is_ok() {
                receiver.recv().await.unwrap_or(Err(()))
            } else {
                Err(())
            };
            Message::Scheduler(
                id,
                scheduler_window::Message::JournalStatsReady(request, result),
            )
        })
    }

    pub(super) fn update_scheduler(
        &mut self,
        id: window::Id,
        message: scheduler_window::Message,
    ) -> Task<Message> {
        let message = match message {
            scheduler_window::Message::JournalStatsReady(request, result) => {
                let root =
                    (!self.exiting && !self.scheduler_draining && !self.scheduler_views.is_empty())
                        .then(|| self.journal_directory().ok())
                        .flatten();
                if let Some(scheduler) = self.scheduler.as_mut() {
                    scheduler
                        .journal_stats
                        .finish(request, root.as_deref(), result);
                }
                return Task::none();
            }
            scheduler_window::Message::AutomationEstimateReady(ticket, estimate) => {
                self.refresh_automation_journal_capture_policy();
                if !self.automation_journal_capture_allowed() {
                    return Task::none();
                }
                let estimate = if self.journal_summaries_allowed() {
                    estimate
                } else {
                    scheduler_window::AutomationEstimate::unavailable()
                };
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.finish_automation_estimate(ticket, estimate);
                }
                return Task::none();
            }
            scheduler_window::Message::AgentProposalProgress(ticket, progress) => {
                return self.answer_scheduler_proposal_progress(id, &ticket, progress);
            }
            scheduler_window::Message::ProposalSkillsFinished(ticket, result) => {
                return self.finish_scheduler_proposal_skills(id, &ticket, result);
            }
            message => message,
        };
        if let scheduler_window::Message::AgentProposed(ticket, _) = &message {
            match self.scheduler_proposal_current(id, Some(ticket)) {
                Ok(true) => {}
                Ok(false) => return Task::none(),
                Err(error) => return self.discard_changed_scheduler_proposal(id, error),
            }
        }
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
        if view.browsing_directory()
            && !matches!(
                message,
                scheduler_window::Message::DirectoryBrowser(_)
                    | scheduler_window::Message::Close
                    | scheduler_window::Message::AgentProposed(_, _)
                    | scheduler_window::Message::ProposalQuotaFinished(..)
                    | scheduler_window::Message::ProposalSkillQuotaFinished(..)
                    | scheduler_window::Message::SwarmComparisonQuotaFinished(..)
            )
        {
            return Task::none();
        }
        if matches!(message, scheduler_window::Message::SaveJournalRetention) {
            if let Some(days) = self
                .scheduler
                .as_ref()
                .and_then(|scheduler| view.journal_retention_change(scheduler))
            {
                self.save_scheduler_journal_retention(id, days);
            }
            return Task::none();
        }
        if let scheduler_window::Message::ApproveFiringRequest(ticket) = &message
            && view.firing_review_ticket() != Some(ticket)
        {
            return Task::none();
        }
        if let scheduler_window::Message::ApproveProposalCommands(ticket) = &message
            && view.proposal_command_ticket() != Some(ticket)
        {
            return Task::none();
        }
        if matches!(
            message,
            scheduler_window::Message::ApproveAgent
                | scheduler_window::Message::ApproveProposalCommands(_)
                | scheduler_window::Message::ApproveFiringRequest(_)
        ) {
            let current = self
                .scheduler_proposal_current(id, None)
                .and_then(|proposal| {
                    self.scheduler_advisory_current(id)?;
                    self.scheduler_swarm_comparison_current(id)?;
                    if !proposal && let Some(kind) = view.reviewed_agent_kind() {
                        self.scheduler_agent_policy(kind, &ClipboardPolicy::load())?;
                    }
                    Ok(())
                });
            if let Err(error) = current {
                return self.discard_changed_scheduler_proposal(id, error);
            }
        }
        match message {
            scheduler_window::Message::JournalRetentionJava(action) => {
                let Some((scheduler, view)) = self
                    .scheduler
                    .as_ref()
                    .zip(self.scheduler_views.get_mut(&id))
                else {
                    return Task::none();
                };
                return view
                    .update_journal_retention_java(scheduler, action)
                    .map(move |message| Message::Scheduler(id, message));
            }
            scheduler_window::Message::ReviewAgent => return self.review_scheduler_agent(id),
            scheduler_window::Message::ReviewSwarmComparison(ticket) => {
                return self.review_scheduler_swarm_comparison(id, &ticket, false);
            }
            scheduler_window::Message::ApproveSwarmComparison(ticket) => {
                return self.review_scheduler_swarm_comparison(id, &ticket, true);
            }
            scheduler_window::Message::CancelSwarmComparison(..)
            | scheduler_window::Message::SwarmComparisonQuotaFinished(..) => {
                let Some((scheduler, view)) = self
                    .scheduler
                    .as_ref()
                    .zip(self.scheduler_views.get_mut(&id))
                else {
                    return Task::none();
                };
                return scheduler
                    .update_swarm_comparison_review(view, &message)
                    .map_or_else(Task::none, |task| {
                        task.map(move |message| Message::Scheduler(id, message))
                    });
            }
            scheduler_window::Message::ApproveFiringRequest(ref ticket) => {
                let Some((scheduler, view)) = self
                    .scheduler
                    .as_ref()
                    .zip(self.scheduler_views.get_mut(&id))
                else {
                    return Task::none();
                };
                match scheduler.refresh_firing_approval_quota(view, ticket) {
                    Ok(Some(task)) => {
                        return task.map(move |message| Message::Scheduler(id, message));
                    }
                    Ok(None) => {}
                    Err(error) => return self.discard_changed_scheduler_proposal(id, error),
                }
            }
            scheduler_window::Message::RequestAgentProposal(ticket) => {
                return self.request_scheduler_proposal(id, &ticket);
            }
            scheduler_window::Message::ProposalKnowledge(ticket, message) => {
                return self.update_scheduler_proposal_knowledge(id, ticket, message);
            }
            scheduler_window::Message::ClassifyProposalSkills(ticket) => {
                return self.request_scheduler_proposal_skills(id, &ticket);
            }
            scheduler_window::Message::ReviewProposalSkills(..)
            | scheduler_window::Message::CancelProposalSkills(..)
            | scheduler_window::Message::ProposalQuotaFinished(..)
            | scheduler_window::Message::ProposalSkillQuotaFinished(..) => {
                let Some((scheduler, view)) = self
                    .scheduler
                    .as_ref()
                    .zip(self.scheduler_views.get_mut(&id))
                else {
                    return Task::none();
                };
                return scheduler
                    .update_proposal_skill_review(view, &message)
                    .map_or_else(Task::none, |task| {
                        task.map(move |message| Message::Scheduler(id, message))
                    });
            }
            scheduler_window::Message::ApproveRunProposal(ticket) => {
                return self.decide_firing(id, ticket, true);
            }
            scheduler_window::Message::RejectRunProposal(ticket) => {
                return self.decide_firing(id, ticket, false);
            }
            scheduler_window::Message::Page(scheduler_window::Page::Action) => {
                self.refresh_scheduler_agent_profiles();
            }
            scheduler_window::Message::ReviewRsyncProgram => {
                if let Err(error) = self.current_scheduler_draft_groups(id) {
                    return self.report_scheduler_group_error(id, error);
                }
                return self
                    .scheduler_views
                    .get_mut(&id)
                    .map_or_else(Task::none, |view| {
                        view.begin_rsync_review()
                            .map(move |message| Message::Scheduler(id, message))
                    });
            }
            scheduler_window::Message::ReviewSync => return self.review_scheduler_sync(id),
            scheduler_window::Message::EstimateAutomationJournal => {
                return self.estimate_automation_journal(id);
            }
            scheduler_window::Message::AutomationJournal(_)
                if !self.automation_journal_capture_allowed() =>
            {
                return Task::none();
            }
            scheduler_window::Message::ReviewPrivileged => {
                return self.review_scheduler_privileged(id);
            }
            scheduler_window::Message::Sudo(mode)
                if mode != SudoMode::Disabled
                    && (view.editing_regular_transfer()
                        || view.editing_privileged_tree()
                        || view.editing_privileged_file_action())
                    && !self.scheduler_privileged_action_allowed(view) =>
            {
                return Task::none();
            }
            scheduler_window::Message::ApprovePrivileged
                if !self.scheduler_privileged_action_allowed(view) =>
            {
                let task = self.update_scheduler(id, scheduler_window::Message::DiscardPrivileged);
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(
                        "Application policy forbids scheduled privileged file actions.".into(),
                    );
                }
                return task;
            }
            scheduler_window::Message::ApproveSync
            | scheduler_window::Message::SyncPreviewed(_, _)
                if !self.scheduler_sync_allowed()
                    || view.editing_privileged_tree() && !self.scheduler_privileged_allowed() =>
            {
                return self.update_scheduler(id, scheduler_window::Message::DiscardSync);
            }
            scheduler_window::Message::BrowseDirectory(field) => {
                return self.browse_scheduler_directory(id, field);
            }
            scheduler_window::Message::DirectoryBrowser(message) => {
                let Some(view) = self.scheduler_views.get_mut(&id) else {
                    return Task::none();
                };
                return view
                    .update_directory_browser(message)
                    .map(move |message| Message::Scheduler(id, message));
            }
            scheduler_window::Message::OpenCredentialVault => {
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
            scheduler_window::Message::OpenSessionJournals(run) => {
                return self.open_scheduler_session_journals(id, run);
            }
            scheduler_window::Message::OpenSessionJournal(run, journal) => {
                return self.open_scheduler_session_journal(id, run, journal);
            }
            scheduler_window::Message::OpenSwarmChat(run) => {
                return self.open_scheduler_swarm_chat(id, run);
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
        let swarm_chats_allowed = self.swarm_chat_access_allowed();
        let Some((scheduler, view)) = self
            .scheduler
            .as_mut()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        let cancel = matches!(message, scheduler_window::Message::CancelRun(_));
        let preview_journals = matches!(
            message,
            scheduler_window::Message::Page(scheduler_window::Page::Journal)
                | scheduler_window::Message::SelectRecord(_)
                | scheduler_window::Message::MarkRecord(_, true)
        );
        scheduler.set_swarm_chats_allowed(swarm_chats_allowed);
        let requests = scheduler.update_view(view, message);
        self.sync_scheduler_views();
        let task = self.enqueue_jobs(requests);
        let task = if preview_journals {
            Task::batch([task, self.preview_scheduler_session_journals(id)])
        } else {
            task
        };
        #[cfg(target_os = "macos")]
        self.sync_native_jobs_menu();
        if cancel {
            Task::batch([task, self.expire_firing(), self.expire_swarm_comparisons()])
        } else {
            task
        }
    }

    fn estimate_automation_journal(&mut self, id: window::Id) -> Task<Message> {
        self.refresh_automation_journal_capture_policy();
        if !self.automation_journal_capture_allowed() {
            return Task::none();
        }
        let profile = self
            .journal_summaries_allowed()
            .then(|| self.configured_journal_ai(false))
            .flatten();
        let root = self.journal_directory().ok();
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        let Some(request) = scheduler.begin_automation_estimate(view) else {
            return Task::none();
        };
        let ticket = request.ticket;
        let Some((profile, root)) = profile.zip(root) else {
            view.finish_automation_estimate(
                ticket,
                scheduler_window::AutomationEstimate::unavailable(),
            );
            return Task::none();
        };
        Task::future(async move {
            let (sender, receiver) = async_channel::bounded(1);
            let worker = thread::Builder::new()
                .name("kortty-job-journal-estimate".into())
                .spawn(move || {
                    let estimate = if ClipboardPolicy::load().automation_journal_allowed() {
                        let (model, budget) = profile.usage_estimate_profile();
                        request.calculate(&root, model, budget)
                    } else {
                        scheduler_window::AutomationEstimate::unavailable()
                    };
                    let _ = sender.try_send(estimate);
                });
            let estimate = if worker.is_ok() {
                receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| scheduler_window::AutomationEstimate::worker_stopped())
            } else {
                scheduler_window::AutomationEstimate::worker_stopped()
            };
            Message::Scheduler(
                id,
                scheduler_window::Message::AutomationEstimateReady(ticket, estimate),
            )
        })
    }

    fn browse_scheduler_directory(
        &mut self,
        id: window::Id,
        field: DirectoryField,
    ) -> Task<Message> {
        if self.clipboard.policy.problem.is_some()
            || self.isolation_required()
            || !self
                .scheduler
                .as_ref()
                .is_some_and(SchedulerWindow::can_browse_directory)
        {
            return Task::none();
        }
        if let Err(error) = self.current_scheduler_draft_groups(id) {
            return self.report_scheduler_group_error(id, error);
        }
        let Some((reference, path)) = self
            .scheduler_views
            .get(&id)
            .and_then(|view| view.directory_browser_target(field))
        else {
            return Task::none();
        };
        let path = path.to_owned();
        let target = reference.target.label();
        let credentials = self
            .saved_connection(reference.id)
            .ok_or("The saved SSH connection is missing or unavailable.")
            .and_then(|profile| {
                job_executor::Credentials::resolve_directory_browser(
                    reference,
                    &profile,
                    self.clipboard.policy.server_access(),
                    |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                )
            });
        let Some(view) = self.scheduler_views.get_mut(&id) else {
            return Task::none();
        };
        match credentials {
            Ok(credentials) => view
                .open_directory_browser(field, target, path, credentials)
                .map(move |message| Message::Scheduler(id, message)),
            Err(error) => {
                view.report_status(error.into());
                Task::none()
            }
        }
    }

    fn current_scheduler_draft_groups(&self, id: window::Id) -> Result<(), String> {
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Err("The scheduler view is unavailable.".into());
        };
        scheduler.require_current_draft_groups(view)
    }

    fn report_scheduler_group_error(&mut self, id: window::Id, error: String) -> Task<Message> {
        if let Some(view) = self.scheduler_views.get_mut(&id) {
            view.report_status(error);
        }
        Task::none()
    }

    fn scheduler_sync_allowed(&self) -> bool {
        self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.file_transfer_allowed
            && !self.isolation_required()
    }

    fn scheduler_privileged_allowed(&self) -> bool {
        self.scheduler_sync_allowed() && self.clipboard.policy.scheduler_sudo_allowed
    }

    fn scheduler_privileged_action_allowed(&self, view: &scheduler_window::SchedulerView) -> bool {
        if view.editing_privileged_file_action() {
            self.clipboard.policy.problem.is_none()
                && self.clipboard.policy.scheduler_sudo_allowed
                && !self.isolation_required()
        } else {
            self.scheduler_privileged_allowed()
        }
    }

    fn review_scheduler_privileged(&mut self, id: window::Id) -> Task<Message> {
        let allowed = self
            .scheduler_views
            .get(&id)
            .is_some_and(|view| self.scheduler_privileged_action_allowed(view));
        let access = self.clipboard.policy.server_access();
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        if !allowed {
            view.report_status(
                "Application policy forbids scheduled privileged file actions.".into(),
            );
            return Task::none();
        }
        let review = scheduler.review_privileged(view, |reference| {
            if access
                .first_blocked(reference.target.servers(reference.jump.as_ref()))
                .is_some()
            {
                return Err("Managed policy blocks this SSH route.".into());
            }
            let current = profile::connections_path()
                .and_then(ConnectionStore::open)
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            if current
                .get(reference.id)
                .map(scheduler_window::ConnectionRef::from)
                .as_ref()
                != Some(reference)
            {
                return Err("The saved SSH route changed. Select the connection again.".into());
            }
            let trust = crate::ssh_connection::trust_store()
                .map_err(|_| "The SSH host-key store is unavailable.")?;
            let pins = scheduler_window::sync::pins(reference, &trust).map_err(
                |_| "Verify the SSH host keys for the complete saved route before review.",
            )?;
            Ok(scheduler_window::sync::fingerprints(&pins))
        });
        if let Err(error) = review {
            view.report_status(error);
        }
        Task::none()
    }

    fn refresh_scheduler_agent_profiles(&mut self) {
        let config = self.ai_configuration();
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_agent_profiles(config.as_deref());
        }
    }

    fn scheduler_agent_policy(
        &self,
        kind: agent::Kind,
        current: &ClipboardPolicy,
    ) -> Result<std::sync::Arc<crate::ai_manager::Configuration>, &'static str> {
        job_executor::agent::require_policy(&self.clipboard.policy, kind)?;
        job_executor::agent::require_policy(current, kind)?;
        self.ai_configuration()
            .ok_or("AI is unavailable under the current settings.")
    }

    fn review_scheduler_agent(&mut self, id: window::Id) -> Task<Message> {
        let prepared = (|| -> Result<_, String> {
            let (scheduler, view) = self
                .scheduler
                .as_ref()
                .zip(self.scheduler_views.get(&id))
                .ok_or("The scheduler view is unavailable.")?;
            let review = scheduler.begin_agent_review(view)?;
            let action = review.job.action();
            let agent = action
                .agent
                .as_ref()
                .ok_or("Choose scheduled AI diagnostics.")?;
            let installed = ClipboardPolicy::load();
            let config = self.scheduler_agent_policy(agent.kind, &installed)?;
            let profile = agent::ProfileChoice::for_reference(&config, &agent.profile)
                .ok_or("The selected AI profile is unavailable.")?;
            if profile.reviewed_cli.is_some()
                && (!crate::local_ai::cli::allowed(&self.clipboard.policy, false)
                    || !crate::local_ai::cli::allowed(&installed, false))
            {
                return Err("Managed policy blocks the configured CLI.".into());
            }
            let store = profile::connections_path()
                .and_then(ConnectionStore::open)
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            action.require_current_target_groups(&store)?;
            let trust = crate::ssh_connection::trust_store()
                .map_err(|_| "The SSH host-key store is unavailable.")?;
            let targets = action
                .targets()
                .map(|reference| {
                    self.scheduler_agent_server(reference)?;
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved SSH connection is unavailable.")?;
                    agent::Target::capture(reference, connection, &trust)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut skill_setup = None;
            let mut advisory = None;
            let prompts = if agent.probe.reviews_advice(&profile) {
                let revision = self.firing_profile_revision(&profile.id)?;
                self.check_swarm_review_policy(&targets, &store, &trust, &installed)?;
                let redactor = self.scheduler_review_redactor(action, &store, &trust)?;
                let (transport, context_limit) = profile.transport(&config)?;
                if !profile.policy_model_matches(transport.policy_authority_fingerprint()) {
                    return Err("The approved AI profile changed or is unavailable.".into());
                }
                transport.require_current_policy_authority()?;
                let directory = action
                    .working_directory
                    .as_deref()
                    .ok_or("Choose a working directory.")?;
                let prompts = targets
                    .iter()
                    .map(|target| {
                        agent::proposal::Prompt::new_advisory(
                            transport.clone(),
                            context_limit,
                            &agent.goal,
                            directory,
                            target.reference.id,
                            redactor.clone(),
                            super::terminal_ai::mask::prompt,
                        )
                        .map(|prompt| prompt.bind_profile(revision))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let question = prompts
                    .first()
                    .and_then(agent::proposal::Prompt::masked_question)
                    .ok_or("The reviewed model question is unavailable.")?;
                let (prompts, setup) = proposal_skills::prepare_standing(
                    std::sync::Arc::clone(&config),
                    &store,
                    &targets,
                    &redactor,
                    prompts,
                )?;
                advisory = Some((question, revision));
                skill_setup = Some((setup, false));
                Some(prompts)
            } else if agent.probe.uses_model() {
                let (transport, context_limit) = profile.transport(&config)?;
                if !profile.policy_model_matches(transport.policy_authority_fingerprint()) {
                    return Err("The approved AI profile changed or is unavailable.".into());
                }
                transport.require_current_policy_authority()?;
                let directory = action
                    .working_directory
                    .as_deref()
                    .ok_or("Choose a working directory.")?;
                let initial_probe = (agent.probe == agent::Probe::SwarmIterative)
                    .then(|| {
                        agent::initial_swarm_probe(directory).ok_or(
                            "The reviewed Swarm environment probe exceeds the SSH command limit.",
                        )
                    })
                    .transpose()?;
                let redactor = if agent.probe.is_firing() || profile.requires_masked_review() {
                    self.scheduler_review_redactor(action, &store, &trust)?
                } else {
                    let secrets = crate::terminal_logging::Secrets::default()
                        .seal()
                        .map_err(|_| "Could not prepare the proposal secret mask.")?;
                    crate::terminal_logging::Redactor::for_journal(
                        secrets,
                        self.clipboard.policy.terminal_logging.clone(),
                    )
                };
                let build = |targets| {
                    agent::proposal::Prompt::new(
                        transport.clone(),
                        context_limit,
                        &agent.goal,
                        directory,
                        targets,
                        redactor.clone(),
                        super::terminal_ai::mask::prompt,
                    )
                    .map(|prompt| {
                        if let Some(command) = initial_probe.as_deref() {
                            prompt.for_swarm().with_swarm_probe(command)
                        } else {
                            prompt
                        }
                    })
                };
                Some(if agent.probe.is_firing() {
                    let revision = self.firing_profile_revision(&profile.id)?;
                    let prompts = targets
                        .iter()
                        .map(|target| {
                            build(vec![target.reference.id])
                                .map(|prompt| prompt.bind_profile(revision))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let (prompts, setup) = proposal_skills::prepare_standing(
                        std::sync::Arc::clone(&config),
                        &store,
                        &targets,
                        &redactor,
                        prompts,
                    )?;
                    skill_setup = Some((setup, false));
                    prompts
                } else {
                    let prompt = build(action.targets().map(|reference| reference.id).collect())?;
                    let (prompt, setup, omitted) = proposal_skills::prepare(
                        std::sync::Arc::clone(&config),
                        &store,
                        &targets,
                        &agent.goal,
                        &redactor,
                        prompt,
                    )?;
                    skill_setup = Some((setup, omitted));
                    vec![prompt]
                })
            } else {
                None
            };
            Ok((review, profile, targets, prompts, advisory, skill_setup))
        })();
        if let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        {
            let result = prepared.and_then(|(review, profile, targets, prompts, advisory, skill_setup)| {
                if let Some(prompts) = prompts {
                    let (setup, omitted) = skill_setup.ok_or("The proposal skill review is unavailable.")?;
                    if let Some((question, revision)) = advisory {
                        scheduler.finish_guided_advisory_review(
                            view, review, profile, targets, prompts, question, revision, setup.pins.clone(),
                        )?;
                    } else if review
                        .job
                        .action()
                        .agent
                        .as_ref()
                        .is_some_and(|action| action.probe.is_firing())
                    {
                        scheduler.finish_agent_firing_review(view, review, profile, targets, prompts)?;
                    } else {
                        let [prompt] = <[_; 1]>::try_from(prompts)
                            .map_err(|_| "The reviewed proposal request is unavailable.")?;
                        scheduler.finish_agent_proposal_review(view, review, profile, targets, prompt)?;
                    }
                    let task = scheduler.attach_proposal_skills(view, setup).inspect_err(|_| {
                        view.discard_proposal_review();
                    })?;
                    scheduler.attach_proposal_knowledge(view);
                    if omitted {
                        view.report_status("The local skill guidance could not be included. Review the available proposal request.".into());
                    }
                    Ok(task)
                } else {
                    scheduler.finish_agent_review(view, review, profile, targets).map(|()| Task::none())
                }
            });
            match result {
                Ok(task) => return task.map(move |message| Message::Scheduler(id, message)),
                Err(error) => view.report_status(error),
            }
        }
        Task::none()
    }

    fn scheduler_review_redactor(
        &self,
        action: &SshCommand,
        store: &ConnectionStore,
        trust: &kortty_ssh::TrustStore,
    ) -> Result<crate::terminal_logging::Redactor, &'static str> {
        let directory = action
            .working_directory
            .as_deref()
            .ok_or("Choose a working directory.")?;
        let executions = action
            .targets()
            .map(|reference| {
                let connection = store
                    .get(reference.id)
                    .ok_or("The saved SSH connection is unavailable.")?;
                self.scheduler_agent_execution(
                    reference,
                    connection,
                    directory,
                    trust,
                    |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                )
            })
            .collect::<Vec<_>>();
        if let Some(error) = executions.iter().find_map(|target| target.as_ref().err()) {
            return Err(*error);
        }
        let secrets = job_executor::agent::seal_targets(&executions)?;
        Ok(crate::terminal_logging::Redactor::for_journal(
            secrets,
            self.clipboard.policy.terminal_logging.clone(),
        ))
    }

    fn check_swarm_review_policy(
        &self,
        targets: &[agent::Target],
        store: &ConnectionStore,
        trust: &kortty_ssh::TrustStore,
        policy: &ClipboardPolicy,
    ) -> Result<(), &'static str> {
        if !self
            .clipboard
            .policy
            .terminal_logging
            .equivalent_redactions(&policy.terminal_logging)
        {
            return Err("The managed redaction rules changed. Reload the policy before reviewing.");
        }
        for target in targets {
            self.scheduler_reviewed_agent_target(target, store, trust, policy)?;
        }
        Ok(())
    }

    fn scheduler_advisory_current(&self, id: window::Id) -> Result<(), String> {
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Ok(());
        };
        let Some(approval) = view.advisory_approval() else {
            return Ok(());
        };
        let grant = approval
            .advisory
            .as_ref()
            .expect("diagnostic advisory review");
        let policy = ClipboardPolicy::load();
        if self.firing_profile_revision(&approval.profile.id)? != grant.profile_revision() {
            return Err("The reviewed AI profile settings changed. Review again.".into());
        }
        let review = scheduler.begin_agent_review(view)?;
        let action = review.job.action();
        let agent = action
            .agent
            .as_ref()
            .ok_or("The reviewed Swarm action changed.")?;
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        action.require_current_target_groups(&store)?;
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        self.check_swarm_review_policy(&approval.targets, &store, &trust, &policy)?;
        let redactor = self.scheduler_review_redactor(action, &store, &trust)?;
        let config = self.scheduler_agent_policy(agent.kind, &policy)?;
        let transport = approval.profile.transport(&config)?.0;
        let redactor = agent::provider_mask::ProviderMask::capture(&transport).apply(&redactor)?;
        let question = super::terminal_ai::mask::prompt(&redactor, &agent.goal)?;
        if !grant.matches_question(&question) {
            return Err("The masked read-only Swarm question changed. Review again.".into());
        }
        if grant.is_guided() {
            let authority = scheduler
                .proposal_authority(view)?
                .filter(|authority| {
                    authority.advisory && authority.prompts.len() == approval.targets.len()
                })
                .ok_or("The reviewed read-only Swarm requests are unavailable.")?;
            let config =
                Configuration::load().map_err(|_| "The saved AI settings are unavailable.")?;
            for (index, prompt) in authority.prompts.iter().enumerate() {
                job_executor::agent::advisory::require_current_guidance(
                    &config, &store, grant, index, prompt,
                )?;
            }
        }
        Ok(())
    }

    fn scheduler_proposal_current(
        &self,
        id: window::Id,
        pending: Option<&str>,
    ) -> Result<bool, String> {
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Ok(false);
        };
        let authority = match pending {
            Some(ticket) => scheduler.pending_proposal_authority(view, ticket),
            None => scheduler.proposal_authority(view),
        }?;
        let Some(authority) = authority else {
            return Ok(false);
        };
        if self.exiting
            || self.scheduler_draining
            || view.hosted_close.is_some()
            || view.close_pending()
        {
            return Err("The scheduler review is closing.".into());
        }
        view.proposal_knowledge_vault_revision()?;
        let (policy, saved_config) = self.scheduler_model_policy(
            authority.kind,
            authority.profile,
            authority.transport,
            authority.context_limit,
            authority.prompt.redaction_policy(),
        )?;
        if let Some(revision) = authority.prompt.profile_revision()
            && self.firing_profile_revision(&authority.profile.id)? != revision
        {
            return Err("The reviewed model profile settings changed. Review again.".into());
        }
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections changed or are unavailable.")?;
        authority.action.require_current_target_groups(&store)?;
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        for target in authority.targets {
            self.scheduler_reviewed_agent_target(target, &store, &trust, &policy)?;
        }
        for (target, prompt) in authority.targets.iter().zip(authority.prompts) {
            proposal_skills::require_firing_guidance(
                &saved_config,
                &store,
                authority.targets,
                target,
                prompt,
            )?;
        }
        let secrets = if authority.prompt.profile_revision().is_some()
            || authority.profile.requires_masked_review()
            || authority.prompt.knowledge().is_some()
        {
            self.firing_secrets(authority.targets, &store)?.0
        } else {
            crate::terminal_logging::Secrets::default()
                .seal()
                .map_err(|_| "Could not verify the proposal secret mask.")?
        };
        let redactor =
            crate::terminal_logging::Redactor::for_journal(secrets, policy.terminal_logging);
        if let Some(setup) = authority.skills {
            proposal_skills::require_current(
                setup,
                self.ai_configuration().as_ref(),
                &store,
                authority.targets,
                &redactor,
                authority.context_limit,
            )?;
        }
        if !view.proposal_mask_current(&redactor, super::terminal_ai::mask::prompt) {
            return Err("The proposal no longer matches the current redaction rules.".into());
        }
        Ok(true)
    }

    fn answer_scheduler_proposal_progress(
        &mut self,
        id: window::Id,
        ticket: &str,
        progress: agent::proposal::Progress,
    ) -> Task<Message> {
        let agent::proposal::Progress::FormatFallback(permit) = progress;
        let current = self.scheduler_proposal_current(id, Some(ticket));
        if matches!(current, Ok(true))
            && permit.try_send(()).is_ok()
            && let Some(view) = self.scheduler_views.get_mut(&id)
        {
            view.report_status("Retrying without the provider's JSON response format…".into());
        }
        permit.close();
        if let Err(error) = current {
            return self.discard_changed_scheduler_proposal(id, error);
        }
        Task::none()
    }

    fn request_scheduler_proposal(&mut self, id: window::Id, preview: &str) -> Task<Message> {
        if !self
            .scheduler_views
            .get(&id)
            .is_some_and(|view| view.matches_proposal_preview(preview))
        {
            return Task::none();
        }
        let guard = match self.scheduler_proposal_guard(id) {
            Ok(Some(guard)) => guard,
            Ok(None) => return Task::none(),
            Err(error) => return self.discard_changed_scheduler_proposal(id, error),
        };
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        match scheduler.request_agent_proposal_guarded(view, guard) {
            Ok(task) => task.map(move |message| Message::Scheduler(id, message)),
            Err(error) => {
                view.report_status(error);
                Task::none()
            }
        }
    }

    fn scheduler_proposal_guard(
        &self,
        id: window::Id,
    ) -> Result<Option<impl Fn() -> Result<(), String> + Send + 'static + use<>>, String> {
        if !self.scheduler_proposal_current(id, None)? {
            return Ok(None);
        }
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Ok(None);
        };
        let Some(authority) = scheduler.proposal_authority(view)? else {
            return Ok(None);
        };
        let vault_revision = if authority.prompt.knowledge().is_some() {
            view.proposal_knowledge_vault_revision()?
        } else if authority.standing || authority.profile.requires_masked_review() {
            let store = profile::connections_path()
                .and_then(ConnectionStore::open)
                .map_err(|_| "The saved SSH connections changed or are unavailable.")?;
            self.firing_secrets(authority.targets, &store)?.1
        } else {
            None
        };
        proposal_skills::dispatch_guard(&authority, vault_revision).map(Some)
    }

    fn request_scheduler_proposal_skills(
        &mut self,
        id: window::Id,
        preview: &str,
    ) -> Task<Message> {
        if !self
            .scheduler_views
            .get(&id)
            .is_some_and(|view| view.matches_proposal_skill_preview(preview))
        {
            return Task::none();
        }
        let guard = match self.scheduler_proposal_guard(id) {
            Ok(Some(guard)) => guard,
            Ok(None) => return Task::none(),
            Err(error) => return self.discard_changed_scheduler_proposal(id, error),
        };
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        match scheduler.request_proposal_skills(view, preview, guard) {
            Ok(task) => task.map(move |message| Message::Scheduler(id, message)),
            Err(error) => {
                view.report_status(error);
                Task::none()
            }
        }
    }

    fn finish_scheduler_proposal_skills(
        &mut self,
        id: window::Id,
        ticket: &str,
        result: Result<Vec<u64>, String>,
    ) -> Task<Message> {
        let updated = (|| -> Result<_, String> {
            let Some((scheduler, view)) =
                self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
            else {
                return Ok(None);
            };
            if !view.has_pending_proposal_skills(ticket)
                || !self.scheduler_proposal_current(id, None)?
            {
                return Ok(None);
            }
            let Some(authority) = scheduler.proposal_authority(view)? else {
                return Ok(None);
            };
            Ok(Some(result.ok().and_then(|ids| {
                proposal_skills::classified_prompts(&authority, &ids)
            })))
        })();
        let prompt = match updated {
            Ok(Some(prompt)) => prompt,
            Ok(None) => return Task::none(),
            Err(error) => return self.discard_changed_scheduler_proposal(id, error),
        };
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        match scheduler.finish_proposal_skills(view, ticket, prompt) {
            Ok(task) => task.map(move |message| Message::Scheduler(id, message)),
            Err(error) => {
                view.discard_proposal_review();
                view.report_status(error);
                Task::none()
            }
        }
    }

    fn discard_changed_scheduler_proposal(
        &mut self,
        id: window::Id,
        error: String,
    ) -> Task<Message> {
        if let Some(view) = self.scheduler_views.get_mut(&id) {
            view.discard_proposal_review();
            view.report_status(error);
        }
        Task::none()
    }

    fn scheduler_agent_server(
        &self,
        reference: &scheduler_window::ConnectionRef,
    ) -> Result<(), &'static str> {
        let servers = reference.target.servers(reference.jump.as_ref());
        if self
            .clipboard
            .policy
            .server_access()
            .first_blocked(servers)
            .is_some()
        {
            return Err("Managed policy blocks this SSH route.");
        }
        Ok(())
    }

    fn scheduler_model_policy(
        &self,
        kind: agent::Kind,
        profile: &agent::ProfileChoice,
        transport: &crate::local_ai::Profile,
        context_limit: usize,
        log_policy: &crate::terminal_logging::Policy,
    ) -> Result<(ClipboardPolicy, Configuration), &'static str> {
        let policy = ClipboardPolicy::load();
        let config = self.scheduler_agent_policy(kind, &policy)?;
        if !log_policy.same_redactions(&self.clipboard.policy.terminal_logging)
            || !log_policy.equivalent_redactions(&policy.terminal_logging)
        {
            return Err(
                "Managed policy no longer authorizes the reviewed AI request. Review it again.",
            );
        }
        let profile_current = |current: &Configuration| {
            current.enabled()
                && agent::ProfileChoice::for_reference(current, &profile.id).as_ref()
                    == Some(profile)
                && profile
                    .transport(current)
                    .is_ok_and(|(selected, limit)| &selected == transport && limit == context_limit)
        };
        if !profile_current(&config) {
            return Err("The approved AI profile or context limit changed. Review it again.");
        }
        let saved = Configuration::load()
            .ok()
            .filter(profile_current)
            .ok_or("The approved AI profile or context limit changed. Review it again.")?;
        Ok((policy, saved))
    }

    fn scheduler_reviewed_agent_target(
        &self,
        target: &agent::Target,
        store: &ConnectionStore,
        trust: &kortty_ssh::TrustStore,
        policy: &ClipboardPolicy,
    ) -> Result<(), &'static str> {
        let reference = &target.reference;
        self.scheduler_agent_server(reference)?;
        if policy
            .server_access()
            .first_blocked(reference.target.servers(reference.jump.as_ref()))
            .is_some()
        {
            return Err("Managed policy blocks this SSH route.");
        }
        let current = store
            .get(reference.id)
            .and_then(|connection| agent::Target::capture(reference, connection, trust).ok());
        if current.as_ref() != Some(target) {
            return Err("The reviewed SSH route or host keys changed. Review it again.");
        }
        Ok(())
    }

    fn scheduler_agent_context(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Result<job_executor::agent::Context, &'static str> {
        self.scheduler_agent_context_with(request, |id, kind, endpoint| {
            self.vault_credential(id, kind, endpoint)
        })
    }

    fn scheduler_agent_context_with(
        &self,
        request: &RunRequest<SshCommand>,
        mut secret: impl FnMut(
            &str,
            Kind,
            &crate::ssh_dialog::LoginTarget,
        ) -> Result<kortty_vault::Secret, &'static str>,
    ) -> Result<job_executor::agent::Context, &'static str> {
        if request.cancellation.is_requested()
            || !matches!(
                request.trigger,
                kortty_jobs::RunTrigger::Manual | kortty_jobs::RunTrigger::Scheduled
            )
        {
            return Err("Scheduled diagnostics require Run Now or a calendar start.");
        }
        let action = &request.action;
        let agent = action
            .agent
            .as_ref()
            .ok_or("The scheduled diagnostics action is unavailable.")?;
        let installed = ClipboardPolicy::load();
        let config = self.scheduler_agent_policy(agent.kind, &installed)?;
        let scheduler = self
            .scheduler
            .as_ref()
            .filter(|scheduler| scheduler.agent_run_approved(request))
            .ok_or("Review the exact scheduled AI action before running.")?;
        let proposal = scheduler.agent_proposal_run(request)?;
        let approval = agent
            .approval
            .as_ref()
            .ok_or("Review the exact scheduled diagnostics before running.")?;
        if approval.profile.reviewed_cli.is_some()
            && (!crate::local_ai::cli::allowed(&self.clipboard.policy, false)
                || !crate::local_ai::cli::allowed(&installed, false)
                || !Configuration::load()
                    .ok()
                    .is_some_and(|saved| approval.profile.transport(&saved).is_ok()))
        {
            return Err("The reviewed CLI profile changed or is blocked. Review the job again.");
        }
        if let Some(grant) = &approval.advisory
            && self.firing_profile_revision(&approval.profile.id)? != grant.profile_revision()
        {
            return Err("The reviewed AI profile settings changed. Review again.");
        }
        if agent::ProfileChoice::for_reference(&config, &approval.profile.id).as_ref()
            != Some(&approval.profile)
        {
            return Err("The approved AI profile changed or is unavailable.");
        }
        let (transport, context_limit) = approval
            .profile
            .transport(&config)
            .map_err(|_| "The approved AI profile is unavailable.")?;
        if !approval
            .profile
            .policy_model_matches(transport.policy_authority_fingerprint())
            || transport.require_current_policy_authority().is_err()
        {
            return Err("The approved AI profile changed or is unavailable.");
        }
        if action.targets().count() != approval.targets.len() {
            return Err("The approved diagnostics targets changed.");
        }
        let directory = action
            .working_directory
            .as_ref()
            .ok_or("The approved SSH directory is unavailable.")?;
        let connection_store = profile::connections_path()
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let store = ConnectionStore::open(connection_store.clone())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        action.require_current_target_groups(&store)?;
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        let advisory = approval.advisory.is_some();
        if let Some(knowledge) = &approval.knowledge {
            proposal_knowledge::require_current_vault_mask(self.vault.as_ref(), knowledge)?;
        }
        let knowledge_vault_revision = approval
            .knowledge
            .as_ref()
            .map(|_| self.vault_change_revision());
        let journal = agent.probe.supports_target_journal(agent.kind)
            && action
                .automation_journal
                .as_ref()
                .is_some_and(|options| options.enabled);
        let mut target_vault_revisions = Vec::new();
        let targets = action
            .targets()
            .zip(&approval.targets)
            .map(|(reference, approved)| {
                let mut vault_revision = None;
                let target = (|| {
                    self.scheduler_agent_server(reference)?;
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved SSH connection is unavailable.")?;
                    if agent::Target::capture(reference, connection, &trust)? != *approved {
                        return Err("The reviewed SSH route or host keys changed.");
                    }
                    self.scheduler_agent_execution(
                        reference,
                        connection,
                        directory,
                        &trust,
                        |id, kind, endpoint| {
                            if advisory || journal {
                                vault_revision = self
                                    .vault
                                    .as_ref()
                                    .and_then(|vault| vault.credential_revision());
                                if advisory && vault_revision.is_none() {
                                    return Err(crate::vault_window::VAULT_LOCKED);
                                }
                            }
                            secret(id, kind, endpoint)
                        },
                    )
                })();
                if advisory || journal {
                    if let Some(revision) = &knowledge_vault_revision {
                        vault_revision = Some(revision.clone());
                    }
                    target_vault_revisions.push(vault_revision);
                }
                target
            })
            .collect();
        Ok(job_executor::agent::Context {
            transport,
            context_limit,
            targets,
            target_vault_revisions,
            connection_store,
            trust,
            log_policy: self.clipboard.policy.terminal_logging.clone(),
            mask: super::terminal_ai::mask::prompt,
            proposal,
            journal: None,
        })
    }

    fn scheduler_agent_execution(
        &self,
        reference: &scheduler_window::ConnectionRef,
        connection: &Connection,
        directory: &str,
        trust: &kortty_ssh::TrustStore,
        secret: impl FnMut(
            &str,
            Kind,
            &crate::ssh_dialog::LoginTarget,
        ) -> Result<kortty_vault::Secret, &'static str>,
    ) -> Result<crate::terminal_agent::swarm::headless::Execution, &'static str> {
        use crate::terminal_agent::swarm::headless::{Execution, Review};

        let review = Review::capture(reference.clone(), directory.to_owned(), trust)
            .map_err(|_| "The reviewed SSH destination is unavailable.")?;
        let credentials = job_executor::Credentials::resolve_directory_browser(
            reference,
            connection,
            self.clipboard.policy.server_access(),
            secret,
        )?;
        Ok(Execution {
            review: std::sync::Arc::new(review),
            current: reference.clone(),
            credentials,
            startup: connection.startup,
        })
    }

    pub(super) fn answer_scheduler_agent_resend(&self, resend: job_executor::agent::Resend) {
        let approval = self.scheduler_agent_resend_authority(&resend.scope);
        resend.settle(approval);
    }

    fn scheduler_agent_resend_authority(
        &self,
        scope: &job_executor::agent::ResendScope,
    ) -> Result<(), &'static str> {
        let request = &scope.request;
        if self.exiting
            || self.scheduler_draining
            || self
                .scheduler
                .as_ref()
                .is_none_or(|scheduler| !scheduler.active_agent_run_approved(request))
        {
            return Err("The scheduled diagnostics run changed or ended.");
        }
        let action = &request.action;
        let agent = action
            .agent
            .as_ref()
            .filter(|agent| !agent.probe.uses_model())
            .ok_or("The scheduled diagnostics action changed.")?;
        let approved = agent
            .approval
            .as_ref()
            .ok_or("The scheduled diagnostics approval changed.")?;
        if let Some(grant) = &approved.advisory {
            if scope.evidence_targets.len() != 1 {
                return Err("Read-only Swarm advice requires exactly one reviewed target.");
            }
            if scope
                .vault_revision
                .as_ref()
                .is_some_and(|revision| revision.strong_count() == 0)
            {
                return Err("The SSH credential vault changed or was locked.");
            }
            if self.firing_profile_revision(&approved.profile.id)? != grant.profile_revision() {
                return Err("The reviewed AI profile settings changed. Review again.");
            }
        }
        let (policy, config) = self.scheduler_model_policy(
            agent.kind,
            &approved.profile,
            &scope.transport,
            scope.context_limit,
            &scope.log_policy,
        )?;
        let store = profile::connections_path()
            .ok()
            .filter(|path| path == &scope.connection_store)
            .and_then(|path| ConnectionStore::open(path).ok())
            .ok_or("The saved SSH connections changed or are unavailable.")?;
        action.require_current_target_groups(&store)?;
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        let mut matched = 0;
        for (index, (reference, target)) in action.targets().zip(&approved.targets).enumerate() {
            if !scope.evidence_targets.contains(&reference.id) {
                continue;
            }
            if &target.reference != reference {
                return Err("The reviewed diagnostic evidence targets changed.");
            }
            self.scheduler_reviewed_agent_target(target, &store, &trust, &policy)?;
            if let Some(grant) = approved.advisory.as_ref().filter(|grant| grant.is_guided()) {
                let prompt = scope
                    .advisory_prompt
                    .as_deref()
                    .ok_or("The reviewed advisory request is unavailable.")?;
                job_executor::agent::advisory::require_current_guidance(
                    &config, &store, grant, index, prompt,
                )?;
            }
            matched += 1;
        }
        if matched == 0 || matched != scope.evidence_targets.len() {
            return Err("The reviewed diagnostic evidence targets changed.");
        }
        Ok(())
    }

    fn review_scheduler_sync(&mut self, id: window::Id) -> Task<Message> {
        let allowed = self.scheduler_sync_allowed();
        let privileged_allowed = self.scheduler_privileged_allowed();
        let Some((scheduler, view)) = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get_mut(&id))
        else {
            return Task::none();
        };
        if !allowed || view.editing_privileged_tree() && !privileged_allowed {
            view.report_status("Application policy forbids scheduled SFTP review.".into());
            return Task::none();
        }
        let request = match scheduler.begin_sync_review(view) {
            Ok(request) => request,
            Err(error) => {
                view.report_status(error);
                return Task::none();
            }
        };
        let ticket = request.ticket.clone();
        let reference = &request.connection;
        let credentials = (|| {
            let store = profile::connections_path()
                .and_then(ConnectionStore::open)
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            let profile = store
                .get(reference.id)
                .ok_or("The saved SSH connection is missing or unavailable.")?;
            let credentials = job_executor::Credentials::resolve_directory_browser(
                reference,
                profile,
                self.clipboard.policy.server_access(),
                |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
            )?;
            if request.sudo != SudoMode::Vault {
                return Ok(credentials);
            }
            let job_executor::JobAuth::Saved(saved) = job_executor::JobAuth::Saved(Ok(Box::new(
                credentials,
            )))
            .with_sudo_passwords(|reference| {
                self.scheduler_sudo_password(reference, store.group_of(reference.id))
            }) else {
                unreachable!();
            };
            saved.map(|value| *value)
        })();
        let result = match credentials {
            Ok(credentials) => {
                let pending = job_executor::preview_sftp_sync(request, credentials);
                return Task::future(async move {
                    let result = pending
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The SFTP review worker stopped.".into()));
                    Message::Scheduler(
                        id,
                        scheduler_window::Message::SyncPreviewed(
                            ticket,
                            result.map(std::sync::Arc::new),
                        ),
                    )
                });
            }
            Err(error) => Err(error.into()),
        };
        self.update_scheduler(id, scheduler_window::Message::SyncPreviewed(ticket, result))
    }

    fn update_scheduler_runtime(&mut self, message: scheduler_window::Message) -> Task<Message> {
        let swarm_chats_allowed = self.swarm_chat_access_allowed();
        let Some(scheduler) = self.scheduler.as_mut() else {
            return Task::none();
        };
        scheduler.set_swarm_chats_allowed(swarm_chats_allowed);
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

    fn save_scheduler_journal_retention(&mut self, id: window::Id, days: u16) {
        let mut settings = self.window_settings.clone();
        settings.job_scheduler_journal_retention_days = days;
        let status = match profile::window_settings_path().and_then(|path| settings.save(&path)) {
            Ok(()) => {
                self.window_settings = settings;
                let removed = self
                    .scheduler
                    .as_mut()
                    .and_then(|scheduler| scheduler.set_journal_retention(days, SystemTime::now()));
                self.sync_scheduler_views();
                match removed {
                    Some(1) => "Run journal retention saved. Removed one expired run.".into(),
                    Some(count) if count > 1 => {
                        format!("Run journal retention saved. Removed {count} expired runs.")
                    }
                    _ => "Run journal retention saved.".into(),
                }
            }
            Err(error) => format!("Could not save run journal retention ({error})"),
        };
        if let Some(view) = self.scheduler_views.get_mut(&id) {
            view.report_status(status);
        }
    }

    pub(super) fn job_finished(
        &mut self,
        outcome: job_executor::Completion,
        journals: Vec<Artifact>,
    ) -> Task<Message> {
        self.scheduler_workers = self.scheduler_workers.saturating_sub(1);
        self.automation_scheduler_finished(outcome.id);
        self.complete_scheduler_run(outcome, journals)
    }

    fn complete_scheduler_run(
        &mut self,
        outcome: job_executor::Completion,
        journals: Vec<Artifact>,
    ) -> Task<Message> {
        let session_journals = journals
            .iter()
            .map(|artifact| artifact.source.id().to_owned())
            .collect();
        let maintenance = self.automation_journals_completed(journals);
        let completion = if let Some(request) =
            self.scheduler_swarm_comparisons.take_admitted(outcome.id)
            && outcome.swarm_report.is_some()
        {
            self.compare_scheduled_swarm(request, outcome, session_journals)
        } else {
            self.commit_scheduler_run(outcome, session_journals)
        };
        Task::batch([maintenance, completion])
    }

    fn commit_scheduler_run(
        &mut self,
        outcome: job_executor::Completion,
        session_journals: Vec<String>,
    ) -> Task<Message> {
        let message = if session_journals.is_empty() {
            scheduler_window::Message::Finished(outcome)
        } else {
            scheduler_window::Message::FinishedWithSessionJournals(outcome, session_journals)
        };
        let task = self.update_scheduler_runtime(message);
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
            return Task::batch([self.expire_firing(), self.expire_swarm_comparisons()]);
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
                swarm_report: None,
            },
        ))
    }

    pub(super) fn enqueue_jobs(&mut self, requests: Vec<RunRequest<SshCommand>>) -> Task<Message> {
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
        let policy = job_executor::ExecutionPolicy {
            file_transfer_allowed: self.clipboard.policy.file_transfer_allowed,
            sudo_allowed: self.clipboard.policy.scheduler_sudo_allowed,
            server_access: self.clipboard.policy.server_access().clone(),
            automation: None,
            sync: None,
            privileged: None,
            group_scope: None,
        };
        let per_firing = |request: &RunRequest<SshCommand>| {
            request
                .action
                .agent
                .as_ref()
                .is_some_and(|action| action.probe.is_firing())
        };
        loop {
            let ssh_room = self.scheduler_workers < MAX_RUNNING_JOBS;
            if ssh_room && let Some(task) = self.start_firing_execution() {
                tasks.push(task);
                continue;
            }
            if let Some(task) = self.start_ready_firing(ssh_room) {
                tasks.push(task);
                continue;
            }
            let next = self.scheduler_queue.iter().position(|request| {
                (ssh_room || per_firing(request))
                    && (request.cancellation.is_requested()
                        || !request.action.agent.as_ref().is_some_and(|action| {
                            self.ai_configuration()
                                .and_then(|config| {
                                    config.resolve_profile_reference(&action.profile)
                                })
                                .is_some_and(|id| self.policy_auto_model_pending(id))
                        }))
            });
            let Some(request) = next.and_then(|index| self.scheduler_queue.remove(index)) else {
                break;
            };
            if !request.cancellation.is_requested()
                && let Err(error) = request.action.require_installed_target_groups()
            {
                self.scheduler_workers += 1;
                tasks.push(Task::done(Message::JobFinished(
                    job_executor::Completion {
                        id: request.id,
                        outcome: kortty_jobs::RunOutcome::Blocked,
                        detail: Some(error.into()),
                        recording: None,
                        swarm_report: None,
                    },
                    Vec::new(),
                )));
                continue;
            }
            self.scheduler_swarm_comparisons.remember(&request);
            if per_firing(&request) {
                tasks.push(self.queue_firing_generation(request));
                continue;
            }
            let id = request.id;
            if self
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.reviewed_cascade_root_blocked(&request))
            {
                self.scheduler_workers += 1;
                tasks.push(Task::done(Message::JobFinished(
                    job_executor::Completion {
                        id,
                        outcome: kortty_jobs::RunOutcome::Blocked,
                        detail: Some("The cascade start conditions changed before dispatch. Review the cascade again.".into()),
                        recording: None,
                        swarm_report: None,
                    },
                    Vec::new(),
                )));
                continue;
            }
            let (done, automation, journal_warning, resends) = if request.action.agent.is_some() {
                let mut context = self.scheduler_agent_context(&request);
                let (automation, journal_warning) = if let Ok(context) = &mut context {
                    match self.scheduler_automation_capture(&request) {
                        Ok(automation) => {
                            context.journal = automation.clone();
                            (automation, None)
                        }
                        Err(error) => (None, Some(error)),
                    }
                } else {
                    (None, None)
                };
                let (authority, resends) = async_channel::bounded(1);
                (
                    job_executor::agent::spawn(request, context, authority),
                    automation,
                    journal_warning,
                    Some(resends),
                )
            } else {
                let archive_password = if policy.any_route_allowed(&request.action) {
                    self.scheduler_archive_password(&request, policy.file_transfer_allowed)
                } else {
                    Ok(None)
                };
                let elevation_allowed =
                    request.action.sudo == SudoMode::Disabled || policy.sudo_allowed;
                let sync = self
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.sync_run_context(&request));
                let reviewed_folder = matches!(
                    request.action.remote.as_ref(),
                    Some(
                        RemoteAction::Sync(_)
                            | RemoteAction::PrivilegedTreeUpload(_)
                            | RemoteAction::PrivilegedTreeDownload(_)
                    )
                );
                let saved_auth = if reviewed_folder && sync.is_none() {
                    job_executor::JobAuth::Manual
                } else {
                    self.scheduler_auth(
                        &request,
                        policy.file_transfer_allowed,
                        capture_directory.is_some(),
                        archive_password.is_ok() && elevation_allowed,
                    )
                };
                let (automation, journal_warning) =
                    match self.scheduler_automation_capture(&request) {
                        Ok(automation) => (automation, None),
                        Err(error) => (None, Some(error)),
                    };
                let mut job_policy = policy.clone();
                job_policy.automation = automation.clone();
                job_policy.sync = sync;
                job_policy.privileged = self
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.privileged_run_context(&request));
                let done = job_executor::spawn(
                    request,
                    capture_directory.clone(),
                    snippet_path.clone(),
                    job_policy,
                    saved_auth,
                    archive_password,
                );
                (done, automation, journal_warning, None)
            };
            self.scheduler_workers += 1;
            let completion = Task::future(async move {
                let mut result = done.recv().await.unwrap_or(job_executor::Completion {
                    id,
                    outcome: kortty_jobs::RunOutcome::Failed { exit_code: None },
                    detail: Some("SSH job worker ended without a result".into()),
                    recording: None,
                    swarm_report: None,
                });
                if let Some(warning) = journal_warning {
                    result.append_detail(&warning);
                }
                let journals = automation.map_or_else(Vec::new, |plan| plan.take_completed());
                Message::JobFinished(result, journals)
            });
            tasks.push(if let Some(resends) = resends {
                Task::run(resends, Message::JobAgentResend).chain(completion)
            } else {
                completion
            });
        }
        Task::batch(tasks)
    }

    pub(super) fn refresh_scheduler_credentials(&mut self) {
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_archive_passwords(
                self.vault
                    .as_ref()
                    .map_or_else(Vec::new, |vault| vault.choices(Kind::ArchivePassword)),
            );
            scheduler.set_webhook_choices(self.vault.as_ref().map_or_else(
                Vec::new,
                crate::vault_window::VaultWindow::job_webhook_choices,
            ));
        }
    }

    pub(super) fn dispatch_job_webhooks(&mut self) -> Task<Message> {
        let Some(scheduler) = &mut self.scheduler else {
            return Task::none();
        };
        let shutting_down = self.exiting || self.scheduler_draining;
        let allowed = !shutting_down && self.clipboard.policy.job_webhooks_allowed;
        let vault = self.vault.as_ref();
        scheduler.revoke_webhooks(allowed, |id| {
            vault.and_then(|vault| vault.job_webhook_settings(id))
        });
        if allowed && vault.is_some_and(crate::vault_window::VaultWindow::busy) {
            scheduler.save_webhook_results(Instant::now());
            return Task::none();
        }
        let mut tasks = Vec::new();
        while let Some(delivery) = scheduler.next_webhook() {
            let ticket = delivery.ticket.clone();
            let destination = (|| {
                if shutting_down {
                    return Err(WebhookOutcome::Revoked);
                }
                if !self.clipboard.policy.job_webhooks_allowed {
                    return Err(WebhookOutcome::PolicyDenied);
                }
                let vault = vault.ok_or(WebhookOutcome::MissingCredential)?;
                let settings = vault
                    .job_webhook_settings(&ticket.credential)
                    .ok_or(WebhookOutcome::MissingCredential)?;
                if !settings.enabled {
                    return Err(WebhookOutcome::TargetDisabled);
                }
                let secret = vault
                    .job_webhook_url(&ticket.credential)
                    .ok_or(WebhookOutcome::MissingCredential)?;
                let url =
                    webhooks::destination(&secret).ok_or(WebhookOutcome::InvalidDestination)?;
                if !self.clipboard.policy.job_webhook_hosts.allows(&url) {
                    return Err(WebhookOutcome::PolicyDenied);
                }
                Ok((kortty_vault::Secret::new(url.into()), settings))
            })();
            match destination {
                Ok((destination, settings)) => {
                    let summary = if settings.include_summary {
                        vault.zip(scheduler.webhook_summary(ticket.run)).and_then(
                            |(vault, detail)| {
                                mask_webhook_summary(
                                    vault,
                                    &self.clipboard.policy.terminal_logging,
                                    detail,
                                )
                            },
                        )
                    } else {
                        None
                    };
                    let body = delivery.body(settings, summary.as_deref());
                    if let Some(gate) = scheduler.start_webhook(delivery, settings) {
                        let result = webhooks::spawn(destination, body, gate);
                        tasks.push(Task::future(async move {
                            let outcome =
                                result.recv().await.unwrap_or(WebhookOutcome::Unavailable);
                            Message::JobWebhookFinished(ticket, outcome)
                        }));
                    } else {
                        scheduler.record_webhook_result(ticket, WebhookOutcome::Revoked);
                    }
                }
                Err(outcome) => scheduler.record_webhook_result(ticket, outcome),
            }
        }
        scheduler.save_webhook_results(Instant::now());
        Task::batch(tasks)
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
        let action = &request.action;
        let Some(reference) = &action.connection else {
            return job_executor::JobAuth::Manual;
        };
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")
            .and_then(|store| {
                action.require_current_target_groups(&store)?;
                Ok(store)
            });
        let server_access = self.clipboard.policy.server_access();
        let secret = |id: &str, kind, endpoint: &crate::ssh_dialog::LoginTarget| {
            self.vault_credential(id, kind, endpoint)
        };
        let auth = if action.additional_connections.is_empty() {
            let saved = store
                .as_ref()
                .map_err(|error| *error)
                .and_then(|store| {
                    let current = store
                        .get(reference.id)
                        .ok_or("The saved SSH connection is missing or unavailable.")?;
                    job_executor::Credentials::resolve(action, current, server_access, secret)
                })
                .map(Box::new);
            job_executor::JobAuth::Saved(saved)
        } else {
            let batch = store.as_ref().map_err(|error| *error).and_then(|store| {
                job_executor::Credentials::resolve_batch(action, store, server_access, secret)
            });
            job_executor::JobAuth::Batch(batch)
        };
        if action.sudo != SudoMode::Vault {
            return auth;
        }
        auth.with_sudo_passwords(|reference| {
            let group = store
                .as_ref()
                .ok()
                .and_then(|store| store.group_of(reference.id));
            self.scheduler_sudo_password(reference, group)
        })
    }

    fn scheduler_sudo_password(
        &self,
        reference: &scheduler_window::ConnectionRef,
        group: Option<&str>,
    ) -> Result<kortty_vault::Secret, &'static str> {
        self.vault
            .as_ref()
            .ok_or("Unlock the vault for this password-based sudo job.")?
            .scheduler_sudo_password(&reference.target, group)
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

fn mask_webhook_summary(
    vault: &crate::vault_window::VaultWindow,
    policy: &crate::terminal_logging::Policy,
    detail: &str,
) -> Option<String> {
    use crate::terminal_logging::{Redactor, Secrets};
    use zeroize::Zeroizing;

    let compact = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = Zeroizing::new(compact(detail));
    let secrets = Secrets::default();
    vault.add_summary_secrets(detail, &secrets);
    vault.add_summary_secrets(&normalized, &secrets);
    let redactor = Redactor::for_journal(secrets.seal().ok()?, policy.clone());
    let masked = Zeroizing::new(super::terminal_ai::mask::prompt(&redactor, &normalized).ok()?);
    let summary = Zeroizing::new(compact(&masked));
    (!summary.is_empty() && !vault.summary_contains_secret(&summary))
        .then(|| summary.chars().take(1_000).collect())
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
    fn unresolved_policy_model_only_defers_its_uncancelled_scheduled_jobs() {
        use iced::futures::executor::block_on;
        use kortty_jobs::{Job, JobGraph, RunOutcome, Scheduler};
        use serde_json::json;

        let policy = crate::clipboard_policy::parse(
            r#"
                [meta]
                schema-version = 1

                [[ai-profile]]
                id = "policy-scheduler-cold-start-fixture"
                name = "Cold-start scheduler fixture"
                provider = "lm-studio"
                endpoint = "http://127.0.0.1:1"
            "#,
        )
        .unwrap();
        let config = Configuration::default()
            .with_policy_profiles(&policy)
            .unwrap();
        let profile = config.terminal_chat_profile_id().unwrap();
        let legacy_alias = config.legacy_policy_alias(profile).unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy = policy;
        workspace.set_test_ai_configuration(config);
        assert!(workspace.policy_auto_model_pending(profile));

        let agent_action = |profile| -> SshCommand {
            serde_json::from_value(json!({
                "agent": {"kind": "agent", "probe": "proposed", "profile": profile, "goal": "Fixture"}
            }))
            .unwrap()
        };
        let automatic = agent_action(json!("policy-scheduler-cold-start-fixture"));
        let legacy = agent_action(json!(legacy_alias));
        let missing = agent_action(json!("policy-Scheduler-cold-start-fixture"));
        // The missing endpoint makes the ordinary worker stop before any SSH I/O.
        let ordinary: SshCommand = serde_json::from_value(json!({"command": "true"})).unwrap();
        let pending_id = JobId::new("pending").unwrap();
        let legacy_id = JobId::new("legacy").unwrap();
        let missing_id = JobId::new("missing").unwrap();
        let ordinary_id = JobId::new("ordinary").unwrap();
        let cancelled_id = JobId::new("cancelled").unwrap();
        let mut scheduler = Scheduler::new(
            JobGraph::new(vec![
                Job::new(pending_id.clone(), "Pending", automatic.clone()).unwrap(),
                Job::new(legacy_id.clone(), "Legacy pending", legacy).unwrap(),
                Job::new(missing_id.clone(), "Missing", missing).unwrap(),
                Job::new(ordinary_id.clone(), "Ordinary", ordinary).unwrap(),
                Job::new(cancelled_id.clone(), "Cancelled", automatic).unwrap(),
            ])
            .unwrap(),
        );
        let now = SystemTime::now();
        let mut start = |id| {
            scheduler
                .start_manual(id, now)
                .unwrap()
                .started
                .pop()
                .unwrap()
        };
        let pending = start(&pending_id);
        let legacy = start(&legacy_id);
        let missing = start(&missing_id);
        let ordinary = start(&ordinary_id);
        let cancelled = start(&cancelled_id);
        scheduler.cancel(cancelled.id).unwrap();
        let deferred = [pending.id, legacy.id];
        let (missing_run, ordinary_run, cancelled_run) = (missing.id, ordinary.id, cancelled.id);

        let task = workspace.enqueue_jobs(vec![pending, legacy, missing, ordinary, cancelled]);
        assert!(
            workspace
                .scheduler_queue
                .iter()
                .map(|request| request.id)
                .eq(deferred)
        );
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let completions = block_on(async {
            let mut completions = Vec::new();
            while let Some(event) = stream.next().await {
                let iced_runtime::Action::Output(Message::JobFinished(completion, _)) = event
                else {
                    panic!("expected a scheduler job completion");
                };
                completions.push((completion.id, completion.outcome));
            }
            completions
        });
        assert_eq!(completions.len(), 3);
        assert!(completions.contains(&(missing_run, RunOutcome::Blocked)));
        assert!(completions.contains(&(ordinary_run, RunOutcome::Blocked)));
        assert!(completions.contains(&(cancelled_run, RunOutcome::Cancelled)));
    }

    #[test]
    fn webhook_summary_masks_saved_and_pattern_credentials_and_bounds_text() {
        let scratch = Scratch::new();
        let path = scratch.0.join("vault.bin");
        let mut vault = kortty_vault::Vault::create(&path, b"scheduler summary fixture").unwrap();
        for secret in ["fixture-value-which-must-stay-private", "fixture password"] {
            vault
                .save(
                    kortty_vault::CredentialInput {
                        label: "Fixture".into(),
                        ..kortty_vault::CredentialInput::default()
                    },
                    kortty_vault::Secret::new(secret.into()),
                )
                .unwrap();
        }
        let vault = crate::vault_window::VaultWindow::with_unlocked_fixture(path, vault);
        let detail = format!(
            "Exit 17: fixture-value-which-must-stay-private\n token=not-for-delivery\tfixture\u{00a0} password {}",
            "é".repeat(1200),
        );
        let summary =
            mask_webhook_summary(&vault, &crate::terminal_logging::Policy::default(), &detail)
                .unwrap();
        assert!(summary.starts_with("Exit 17: *** token=*** *** "));
        assert!(!summary.contains("fixture-value"));
        assert!(!summary.contains("not-for-delivery"));
        assert_eq!(summary.chars().count(), 1000);
    }

    #[test]
    fn disabled_webhook_records_delivery_status_without_starting_a_worker() {
        use kortty_jobs::{Job, JobGraph, RunOutcome, SchedulerStore};
        use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault, WebhookSettings};

        let scratch = Scratch::new();
        let vault_path = scratch.0.join("vault.bin");
        let mut vault = Vault::create(&vault_path, b"scheduler webhook fixture").unwrap();
        let credential = vault
            .save_webhook(
                CredentialInput {
                    label: "Disabled fixture".into(),
                    kind: CredentialKind::JobWebhookUrl,
                    ..CredentialInput::default()
                },
                Secret::new("https://fixture.invalid/job-hook".into()),
                WebhookSettings {
                    enabled: false,
                    ..WebhookSettings::default()
                },
            )
            .unwrap();
        let jobs_path = scratch.0.join("jobs.json");
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "host": "fixture.invalid", "port": 22, "username": "fixture",
            "key_file": scratch.0.join("private-key"), "command": "exit 17",
            "webhook_targets": [{ "credential": credential, "format": "generic-json" }],
        }))
        .unwrap();
        let graph = JobGraph::new(vec![
            Job::new(JobId::new("delivery-fixture").unwrap(), "Fixture", action).unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&jobs_path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut scheduler = SchedulerWindow::open(jobs_path.clone()).unwrap();
        scheduler.set_execution_allowed(true);
        let mut view = scheduler.new_view();
        let run = scheduler
            .update_view(&mut view, scheduler_window::Message::Run(false))
            .pop()
            .unwrap();
        scheduler.update_runtime(scheduler_window::Message::Finished(
            job_executor::Completion {
                id: run.id,
                outcome: RunOutcome::Failed {
                    exit_code: Some(17),
                },
                detail: None,
                recording: None,
                swarm_report: None,
            },
        ));
        let mut workspace = Workspace {
            scheduler: Some(scheduler),
            vault: Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
                vault_path, vault,
            )),
            ..Workspace::default()
        };
        let vault = workspace
            .vault
            .as_mut()
            .unwrap()
            .take_for_ssh_keys()
            .unwrap();
        assert_eq!(workspace.dispatch_job_webhooks().units(), 0);
        workspace
            .vault
            .as_mut()
            .unwrap()
            .restore_after_ssh_keys(vault);
        assert_eq!(workspace.dispatch_job_webhooks().units(), 0);
        drop(workspace);
        let reopened = SchedulerStore::open(&jobs_path).unwrap();
        assert_eq!(
            reopened.webhook_results(run.id).unwrap()[&credential],
            WebhookOutcome::TargetDisabled
        );
    }

    #[test]
    fn swarm_scheduler_handoff_reuses_clean_tab_and_keeps_unsaved_draft() {
        use crate::ai_manager::ProfileRef;
        use crate::connection_store::{Auth, SavedLogin};
        use crate::scheduler_window::ConnectionRef;
        use crate::ssh_dialog::LoginTarget;

        let scratch = Scratch::new();
        let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
        let mut targets = Vec::new();
        for host in ["one.example.test", "two.example.test"] {
            let id = store
                .save(Connection {
                    id: 0,
                    name: host.into(),
                    tag: String::new(),
                    login: SavedLogin {
                        endpoint: LoginTarget {
                            host: host.into(),
                            port: 22,
                            username: "operator".into(),
                        },
                        auth: Auth::StoredKey(1),
                        keyboard_interactive: false,
                    },
                    jump: None,
                    emulation: "xterm-256color".into(),
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
            targets.push(ConnectionRef::from(store.get(id).unwrap()));
        }
        let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
            "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
            "default":1,"last_id":1
        }})).unwrap();
        let mut scheduler = SchedulerWindow::open(scratch.0.join("jobs.json")).unwrap();
        scheduler.set_connections(Some(&store));
        scheduler.set_agent_profiles(Some(&config));
        let mut workspace = Workspace {
            scheduler: Some(scheduler),
            ..Workspace::default()
        };
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_scheduler(owner);
        let hosted = workspace
            .hosted_tool_in(owner, tool_tabs::Kind::Scheduler)
            .unwrap();
        let runtime = workspace.scheduler.as_mut().unwrap();
        runtime.set_connections(Some(&store));
        runtime.set_agent_profiles(Some(&config));
        let handoff = |name: &str| SwarmHandoff {
            name: name.into(),
            goal: "Check disk space".into(),
            profile: ProfileRef::Numeric(1),
            targets: targets.clone(),
            read_only: true,
            journal: None,
        };
        let _ = workspace
            .place_scheduler_swarm_handoff(owner, handoff("First draft"))
            .unwrap();
        assert_eq!(workspace.scheduler_views.len(), 1);
        assert!(workspace.hosted_scheduler_dirty(hosted));
        let _ = workspace
            .place_scheduler_swarm_handoff(owner, handoff("Second draft"))
            .unwrap();
        assert_eq!(workspace.scheduler_views.len(), 2);
        assert!(workspace.hosted_scheduler_dirty(hosted));
        let native = workspace.native_scheduler_close_targets(Some(owner));
        let [second] = native.as_slice() else {
            panic!("the second handoff should preserve the existing tab");
        };
        assert!(workspace.scheduler_views[second].dirty());
        assert!(!scratch.0.join("jobs.json").exists());
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
