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

pub(crate) mod firing;
pub(super) mod firing_web;
mod ownership;
mod proposal_knowledge;
mod proposal_skills;
mod proposal_web;
mod rsync;
pub(super) mod swarm_chat;
pub(super) mod swarm_comparison;

pub(super) const TITLE: &str = "JobScheduler";
const MAX_RUNNING_JOBS: usize = 4;
const JOURNAL_DIVIDER_SAVE_DELAY: Duration = Duration::from_millis(300);

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
    pub(super) fn scheduler_shared_targets(
        &self,
        store: &ConnectionStore,
    ) -> Result<
        (
            crate::connection_store::teamwork::Catalog,
            scheduler_window::SharedTargets,
        ),
        &'static str,
    > {
        use super::ssh_access::teamwork_auth::{eligible_password, set_username};
        use crate::connection_store::teamwork::{Catalog, DefaultAuth};
        use scheduler_window::{SharedAuth, SharedTargets};

        if !self.teamwork_sync_allowed() {
            return Ok((Catalog::default(), SharedTargets::default()));
        }
        let root = profile::root().map_err(|_| "The Teamwork profile is unavailable.")?;
        let policy = ClipboardPolicy::load();
        let catalog = Catalog::load_for_scheduler(&root, &policy)
            .map_err(|_| "The current Teamwork sources are unavailable.")?;
        if catalog.entries().next().is_none() {
            return Ok((catalog, SharedTargets::default()));
        }
        let ordered = DefaultAuth::load(&root)
            .map_err(|_| "The current Teamwork authentication settings are unavailable.")?
            .ordered();
        let key = ordered.key.filter(|id| {
            crate::ssh_key_catalog::Catalog::open_profile()
                .is_ok_and(|catalog| catalog.get(*id).is_some())
        });
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        let targets = SharedTargets::new(&catalog, store, &trust, |entry| {
            if !entry.default_auth_eligible {
                return None;
            }
            let login = &entry.connection.login.endpoint;
            if let Some(credential) = &ordered.credential
                && let Some(target) = eligible_password(self, credential, login)
            {
                return Some((
                    target,
                    SharedAuth::Password {
                        credential: credential.clone(),
                    },
                ));
            }
            let mut target = login.clone();
            set_username(&mut target, &ordered.username);
            target.validate().ok()?;
            Some((
                target,
                SharedAuth::Key {
                    id: key?,
                    password_preference: ordered.credential.clone(),
                },
            ))
        });
        Ok((catalog, targets))
    }

    fn refresh_scheduler_connections(&mut self, connections: Option<&ConnectionStore>) {
        let targets = connections
            .and_then(|store| self.scheduler_shared_targets(store).ok())
            .map(|(_, targets)| targets)
            .unwrap_or_default();
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.set_shared_targets(targets);
            scheduler.set_connections(connections);
        }
    }

    pub(super) fn scheduler_journal_output_allowed(&self) -> bool {
        self.clipboard.policy.recording_allowed && self.automation_journal_capture_allowed()
    }

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
                scheduler.set_journal_output_allowed(self.scheduler_journal_output_allowed());
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
        let allow_cli = self.scheduler_cli_allowed(&installed);
        self.refresh_scheduler_connections(Some(&connections));
        let scheduler = self.scheduler.as_mut().expect("initialized scheduler");
        scheduler.refresh_snippets();
        scheduler.set_agent_profiles(Some(&config), allow_cli);
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
        self.refresh_scheduler_connections(connections.as_ref());
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.refresh_snippets();
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
        mut view: SchedulerView,
        hosted: bool,
    ) -> Task<Message> {
        view.apply_journal_divider(self.window_settings.scheduler_journal_divider);
        if let Some(scheduler) = &self.scheduler {
            scheduler.apply_diagram_preferences(
                &mut view,
                self.window_settings.scheduler_diagram_horizontal,
                self.window_settings.scheduler_diagram_color_mode,
                self.journal_system_dark(),
            );
        }
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
        let body = view
            .view(
                scheduler,
                self.window_settings.job_menu_status_enabled,
                self.scheduler_privileged_action_allowed(view),
                journal_root.as_deref(),
                self.automation_summary_providers(),
            )
            .map(move |message| Message::Scheduler(id, message));
        Some(if let Some(web) = self.firing_web_view() {
            iced::widget::column![web, body].spacing(8).into()
        } else {
            body
        })
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
            || self.scheduler_firing_web.needs_tick()
            || self.environment_lookups_need_tick()
            || self.swarm_chat_seed_needs_tick()
            || self.scheduler_swarm_comparisons.needs_tick()
            || self
                .scheduler_views
                .values()
                .any(|view| view.active_proposal_web_scope().is_some())
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
        self.revalidate_scheduler_web_reviews();
        let expiry = self.expire_firing();
        let web_expiry = self.expire_pending_firing_web();
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
            web_expiry,
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
            scheduler_window::Message::ResizeJournal(event) => {
                let Some((scheduler, view)) = self
                    .scheduler
                    .as_mut()
                    .zip(self.scheduler_views.get_mut(&id))
                else {
                    return Task::none();
                };
                let previous = view.journal_divider();
                let _ =
                    scheduler.update_view(view, scheduler_window::Message::ResizeJournal(event));
                let current = view.journal_divider();
                if previous == current || self.exiting {
                    return Task::none();
                }
                self.window_settings.scheduler_journal_divider = current;
                let owner = self
                    .scheduler_owners
                    .get(&id)
                    .copied()
                    .or_else(|| self.hosted_tool_owner(id));
                let queued = self
                    .scheduler_journal_divider_save
                    .replace((Instant::now() + JOURNAL_DIVIDER_SAVE_DELAY, owner))
                    .is_some();
                return if queued {
                    Task::none()
                } else {
                    self.delay_scheduler_journal_divider_save(JOURNAL_DIVIDER_SAVE_DELAY)
                };
            }
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
        if view.reviewing_ownership() && matches!(message, scheduler_window::Message::Dismiss) {
            if let Some(view) = self.scheduler_views.get_mut(&id) {
                view.cancel_ownership_review();
            }
            return Task::none();
        }
        if let scheduler_window::Message::Ownership(message) = message {
            if view.browsing_directory() {
                return Task::none();
            }
            return self.update_scheduler_ownership(id, message);
        }
        if (view.browsing_directory() || view.reviewing_ownership())
            && !matches!(
                message,
                scheduler_window::Message::DirectoryBrowser(_)
                    | scheduler_window::Message::Close
                    | scheduler_window::Message::AgentProposed(_, _)
                    | scheduler_window::Message::ProposalQuotaFinished(..)
                    | scheduler_window::Message::ProposalSkillQuotaFinished(..)
                    | scheduler_window::Message::ProposalWeb(
                        _,
                        crate::workspace::terminal_ai::web::Message::Finished(..)
                    )
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
        if let scheduler_window::Message::ReviewUnattendedWeb(ticket, topic) = &message {
            if view.unattended_web_topic(ticket) != Some(topic) {
                return Task::none();
            }
            let reviewed = (|| {
                if !self.scheduler_proposal_current(id, None)? {
                    return Err("The model review is unavailable.".into());
                }
                let scheduler = self
                    .scheduler
                    .as_ref()
                    .ok_or("The scheduler is unavailable.")?;
                let authority = scheduler
                    .proposal_authority(view)?
                    .ok_or("The model review is unavailable.")?;
                let config =
                    Configuration::load().map_err(|_| "The saved AI profile is unavailable.")?;
                let profile = config
                    .resolve_profile_reference(&authority.profile.id)
                    .ok_or("The saved AI profile is unavailable.")?;
                let scope = super::terminal_ai::web::Scope::new(
                    Some(&config),
                    profile,
                    self.scheduler_web_allowed(),
                )
                .filter(super::terminal_ai::web::Scope::supports_unattended_search)
                .ok_or("The saved web search settings are unavailable.")?;
                scope.require_current()?;
                self.require_public_scheduler_topic(
                    topic,
                    authority.transport,
                    &scope,
                    view.unattended_web_includes_page_mask(ticket),
                )
            })();
            if let Err(error) = reviewed {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(error);
                }
                return Task::none();
            }
        }
        if (matches!(message, scheduler_window::Message::Save)
            || matches!(message, scheduler_window::Message::Run(_)) && view.dirty())
            && let Some(grant) = view.new_unattended_web_lookup()
            && let Err(error) = self.require_saved_public_scheduler_topic(grant)
        {
            if let Some(view) = self.scheduler_views.get_mut(&id) {
                view.report_status(error);
            }
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
            scheduler_window::Message::ProposalWeb(ticket, message) => {
                return self.update_scheduler_proposal_web(id, ticket, message);
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
                return self.review_scheduler_rsync(id);
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
            scheduler_window::Message::AutomationJournal(
                scheduler_window::SwarmJournalEdit::UseImportedProfile(ref choice),
            ) if !self
                .automation_summary_providers()
                .contains(choice.as_ref()) =>
            {
                return Task::none();
            }
            scheduler_window::Message::ReviewPrivileged => {
                return self.review_scheduler_privileged(id);
            }
            scheduler_window::Message::ImportedStagingSudo(_)
            | scheduler_window::Message::PrivilegedStanding(true)
            | scheduler_window::Message::AdoptImportedTransfer(
                scheduler_window::imported_transfer::Mode::Staged(_),
            ) if !self.scheduler_privileged_allowed() => {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status("Application policy forbids privileged staging.".into());
                }
                return Task::none();
            }
            scheduler_window::Message::ImportedPrivilegedSudo(_)
            | scheduler_window::Message::AdoptImportedPrivileged
                if !self.scheduler_privileged_action_allowed(view) =>
            {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(
                        "Application policy forbids this Scheduler sudo action.".into(),
                    );
                }
                return Task::none();
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
            | scheduler_window::Message::PrivilegedReplacementPreviewed(_)
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
            scheduler_window::Message::CopyRunDetail(run) => {
                let journal_root = self.journal_directory().ok();
                let selection = self.scheduler.as_ref().and_then(|scheduler| {
                    scheduler.selected_run_detail_text(view, run, journal_root.as_deref())
                });
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.dismiss_run_detail_menu(run);
                }
                return selection.map_or_else(Task::none, |text| self.write_clipboard(text));
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
        if matches!(message, scheduler_window::Message::Confirm)
            && let Some(job) = self
                .scheduler
                .as_ref()
                .zip(self.scheduler_views.get(&id))
                .and_then(|(scheduler, view)| scheduler.confirmed_job_deletion(view))
                .cloned()
            && let Err(error) = self.revoke_deleted_automation_summary_job(&job)
        {
            if let Some(view) = self.scheduler_views.get_mut(&id) {
                view.report_status(format!(
                    "Could not revoke the job's journal approval ({error})."
                ));
            }
            return Task::none();
        }
        let refresh_targets = match &message {
            scheduler_window::Message::RefreshConnectionGroups
            | scheduler_window::Message::TargetPicker(
                scheduler_window::target_picker::Event::Open
                | scheduler_window::target_picker::Event::Apply,
            )
            | scheduler_window::Message::ConnectionGroupAdded(
                scheduler_window::ConnectionGroup::Teamwork(_),
            ) => true,
            scheduler_window::Message::Save
            | scheduler_window::Message::Run(_)
            | scheduler_window::Message::ConnectionGroupRemoved(_) => self
                .scheduler_views
                .get(&id)
                .is_some_and(SchedulerView::has_teamwork_targets),
            _ => false,
        };
        if refresh_targets
            && let Ok(connections) = profile::connections_path().and_then(ConnectionStore::open)
        {
            self.refresh_scheduler_connections(Some(&connections));
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
        let diagram_changed = matches!(
            message,
            scheduler_window::Message::DiagramDirection | scheduler_window::Message::DiagramTheme
        );
        let preview_journals = matches!(
            message,
            scheduler_window::Message::Page(scheduler_window::Page::Journal)
                | scheduler_window::Message::SelectRecord(_)
                | scheduler_window::Message::MarkRecord(_, true)
        );
        scheduler.set_swarm_chats_allowed(swarm_chats_allowed);
        let requests = scheduler.update_view(view, message);
        if diagram_changed {
            let (horizontal, color) = view.diagram_preferences();
            self.window_settings.scheduler_diagram_horizontal = horizontal;
            self.window_settings.scheduler_diagram_color_mode = color;
            let owner = self
                .scheduler_owners
                .get(&id)
                .copied()
                .or_else(|| self.hosted_tool_owner(id));
            self.save_window_settings(owner);
        }
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
            Task::batch([
                task,
                self.expire_pending_firing_web(),
                self.expire_firing(),
                self.expire_swarm_comparisons(),
            ])
        } else {
            task
        }
    }

    fn estimate_automation_journal(&mut self, id: window::Id) -> Task<Message> {
        self.refresh_automation_journal_capture_policy();
        if !self.automation_journal_capture_allowed() {
            return Task::none();
        }
        let model = self.scheduler_views.get(&id).and_then(|view| {
            if view.unresolved_automation_summary_profile() {
                return None;
            }
            let source = view
                .automation_summary_job()
                .cloned()
                .map(crate::session_journal::summary_queue::AutomationSource::Job);
            let selected = view.automation_summary_profile();
            let profile = self.automation_summary_estimate_profile(source.as_ref(), selected)?;
            let limits = self.automation_summary_evidence_limits(source.as_ref(), selected);
            Some((profile, limits))
        });
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
        let Some(((profile, evidence_limits), root)) = model.zip(root) else {
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
                        let (name, budget) = profile.usage_estimate_profile();
                        request.calculate(
                            &root,
                            scheduler_window::AutomationModelEstimate {
                                name,
                                budget,
                                evidence_limits,
                            },
                        )
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

    pub(in crate::workspace) fn invalidate_automation_summary_estimates(&mut self) {
        for view in self.scheduler_views.values_mut() {
            view.clear_automation_estimate();
        }
        self.invalidate_swarm_journal_estimates();
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
        let credentials = self.scheduler_transfer_review_credentials(reference, SudoMode::Disabled);
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
        if view.editing_privileged_file_action() || view.editing_imported_privileged() {
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
            let path = profile::connections_path()
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            reference.require_saved_at(&path)?;
            let trust = crate::ssh_connection::trust_store()
                .map_err(|_| "The SSH host-key store is unavailable.")?;
            let trust = reference.strict_trust(trust)?;
            let pins = scheduler_window::sync::pins(reference, &trust).map_err(
                |_| "Verify the SSH host keys for the complete saved route before review.",
            )?;
            Ok(scheduler_window::sync::fingerprints(&pins))
        });
        match review {
            Ok(Some(preview)) => {
                let credentials = preview.upload().map(|upload| {
                    self.scheduler_transfer_review_credentials(&upload.reference, upload.sudo)
                });
                Task::future(async move {
                    Message::Scheduler(
                        id,
                        scheduler_window::Message::PrivilegedReplacementPreviewed(
                            preview.inspect(credentials).await,
                        ),
                    )
                })
            }
            Ok(None) => Task::none(),
            Err(error) => {
                view.report_status(error);
                Task::none()
            }
        }
    }

    fn scheduler_transfer_review_credentials(
        &self,
        reference: &scheduler_window::ConnectionRef,
        sudo: SudoMode,
    ) -> Result<job_executor::Credentials, &'static str> {
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let (credentials, sudo_group) = if let Some(identity) = reference.shared_identity() {
            let (catalog, shared) = self.scheduler_shared_targets(&store)?;
            shared.require(reference)?;
            let entry = catalog
                .get(identity)
                .ok_or("The Teamwork connection is unavailable.")?;
            (
                self.scheduler_shared_credentials(
                    reference,
                    entry,
                    &store,
                    false,
                    sudo == SudoMode::Vault,
                )?,
                None,
            )
        } else {
            let profile = store
                .get(reference.id)
                .ok_or("The saved SSH connection is missing or unavailable.")?;
            let credentials = job_executor::Credentials::resolve_directory_browser(
                reference,
                profile,
                self.clipboard.policy.server_access(),
                |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
            )?;
            (credentials, store.group_of(reference.id))
        };
        if sudo != SudoMode::Vault {
            return Ok(credentials);
        }
        let job_executor::JobAuth::Saved(saved) = job_executor::JobAuth::Saved(Ok(Box::new(
            credentials,
        )))
        .with_sudo_passwords(|reference| self.scheduler_sudo_password(reference, sudo_group)) else {
            unreachable!();
        };
        saved.map(|value| *value)
    }

    fn refresh_scheduler_agent_profiles(&mut self) {
        let allow_cli = self.scheduler_cli_allowed(&ClipboardPolicy::load());
        let config = self.ai_configuration();
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_agent_profiles(config.as_deref(), allow_cli);
        }
    }

    fn scheduler_cli_allowed(&self, installed: &ClipboardPolicy) -> bool {
        crate::local_ai::cli::allowed(&self.clipboard.policy, false)
            && crate::local_ai::cli::allowed(installed, false)
    }

    fn scheduler_agent_policy(
        &self,
        kind: agent::Kind,
        current: &ClipboardPolicy,
    ) -> Result<std::sync::Arc<crate::ai_manager::Configuration>, &'static str> {
        job_executor::agent::require_policy(&self.clipboard.policy, kind)?;
        job_executor::agent::require_policy(current, kind)?;
        self.ai_configuration()
            .filter(|config| config.agent_execution_enabled())
            .ok_or("AI is unavailable under the current settings.")
    }

    fn review_scheduler_agent(&mut self, id: window::Id) -> Task<Message> {
        let policy_allows_web = self.scheduler_web_allowed();
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
            if profile.reviewed_cli.is_some() && !self.scheduler_cli_allowed(&installed) {
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
                if !profile.matches_transport_model(&transport) {
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
                        .and_then(|prompt| {
                            let memory = prompt.capture_memory(
                                &config,
                                &installed.ai_memory,
                                target.reference.id,
                            )?;
                            let prompt = prompt.with_memory(memory)?.bind_profile(revision);
                            prompt.require_current_memory(
                                &config,
                                &self.clipboard.policy.ai_memory,
                            )?;
                            Ok(prompt)
                        })
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
                if !profile.matches_transport_model(&transport) {
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
                let extended_mask = agent.probe.is_firing() || profile.requires_masked_review();
                let mut redactor = if extended_mask {
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
                let build = |targets, redactor: &crate::terminal_logging::Redactor| {
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
                            let prompt = build(vec![target.reference.id], &redactor)?;
                            let memory = prompt.capture_memory(
                                &config,
                                &installed.ai_memory,
                                target.reference.id,
                            )?;
                            let prompt = prompt.with_memory(memory)?.bind_profile(revision);
                            prompt.require_current_memory(
                                &config,
                                &self.clipboard.policy.ai_memory,
                            )?;
                            Ok::<_, String>(prompt)
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
                    let ordered: Vec<_> = action.targets().map(|reference| reference.id).collect();
                    let load_memory = |prompt: &agent::proposal::Prompt| {
                        ordered
                            .iter()
                            .map(|&id| prompt.capture_memory(&config, &installed.ai_memory, id))
                            .collect::<Result<Vec<_>, _>>()
                    };
                    let mut prompt = build(ordered.clone(), &redactor)?;
                    let mut memory = load_memory(&prompt);
                    if !extended_mask
                        && !memory.as_ref().is_ok_and(|memory| {
                            memory.iter().all(|memory| memory.binding().is_none())
                        })
                    {
                        redactor = self.scheduler_review_redactor(action, &store, &trust)?;
                        prompt = build(ordered.clone(), &redactor)?;
                        memory = load_memory(&prompt);
                    }
                    let memory = memory?;
                    let prompts = if ordered.len() == 1 {
                        let [memory] = <[_; 1]>::try_from(memory)
                            .map_err(|_| "The reviewed AI memory is unavailable.")?;
                        vec![prompt.with_memory(memory)?]
                    } else if memory
                        .iter()
                        .any(agent::memory::Memory::includes_connection)
                        || memory
                            .windows(2)
                            .any(|pair| pair[0].text() != pair[1].text())
                    {
                        ordered
                            .into_iter()
                            .zip(memory)
                            .map(|(id, memory)| build(vec![id], &redactor)?.with_memory(memory))
                            .collect::<Result<Vec<_>, _>>()?
                    } else {
                        vec![prompt.with_shared_memory(&config, &installed.ai_memory, memory)?]
                    };
                    for prompt in &prompts {
                        self.require_scheduler_prompt_memory(prompt, &installed, &config)?;
                    }
                    let (prompts, setup, omitted) = proposal_skills::prepare(
                        std::sync::Arc::clone(&config),
                        &store,
                        &targets,
                        &agent.goal,
                        &redactor,
                        prompts,
                    )?;
                    skill_setup = Some((setup, omitted));
                    prompts
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
                        scheduler.finish_agent_proposal_review(view, review, profile, targets, prompts)?;
                    }
                    let task = scheduler.attach_proposal_skills(view, setup).inspect_err(|_| {
                        view.discard_proposal_review();
                    })?;
                    scheduler.attach_proposal_knowledge(view);
                    scheduler.attach_proposal_web(view, policy_allows_web);
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
                self.require_scheduler_prompt_memory(prompt, &policy, &config)?;
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
        view.proposal_web_vault_revision()?;
        if authority.fresh_web && !self.scheduler_web_allowed() {
            return Err("Managed policy blocks a fresh web lookup at each start.".into());
        }
        if let Some(grant) = authority.web_lookup {
            self.require_saved_public_scheduler_topic(grant)?;
        }
        if let Some(evidence) = authority.prompt.web() {
            if !self.scheduler_web_allowed() {
                return Err("Managed policy blocks use of the reviewed web excerpts.".into());
            }
            evidence.require_current()?;
            proposal_web::require_current_vault_mask(self.vault.as_ref(), evidence)?;
        }
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
            self.require_scheduler_prompt_memory(prompt, &policy, &saved_config)?;
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
            || authority.prompt.web().is_some()
            || authority.prompts.iter().any(|prompt| prompt.has_memory())
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
        let vault_revision = if authority.prompt.web().is_some() {
            view.proposal_web_vault_revision()?
        } else if authority.prompt.knowledge().is_some() {
            view.proposal_knowledge_vault_revision()?
        } else if authority.standing
            || authority.profile.requires_masked_review()
            || authority.prompts.iter().any(|prompt| prompt.has_memory())
        {
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
            current.agent_execution_enabled()
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
        if request.cancellation.is_requested() {
            return Err("The scheduled diagnostics run was cancelled.");
        }
        let action = &request.action;
        let agent = action
            .agent
            .as_ref()
            .ok_or("The scheduled diagnostics action is unavailable.")?;
        if !agent.probe.allows_trigger(request.trigger) {
            return Err("The selected diagnostics workflow cannot use this start trigger.");
        }
        let scheduler = self
            .scheduler
            .as_ref()
            .filter(|scheduler| scheduler.active_agent_run_approved(request))
            .ok_or("Review the exact scheduled AI action before running.")?;
        if !scheduler.agent_predecessors_current(request, SystemTime::now()) {
            return Err("The job prerequisites changed before dispatch. Review the run again.");
        }
        let installed = ClipboardPolicy::load();
        let config = self.scheduler_agent_policy(agent.kind, &installed)?;
        let proposal = scheduler.agent_proposal_run(request)?;
        let approval = agent
            .approval
            .as_ref()
            .ok_or("Review the exact scheduled diagnostics before running.")?;
        if approval.profile.reviewed_cli.is_some()
            && (!self.scheduler_cli_allowed(&installed)
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
        if !approval.profile.matches_transport_model(&transport)
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
        if let Some(evidence) = &approval.web {
            if !self.scheduler_web_allowed() {
                return Err("Managed policy blocks use of the approved web excerpts.");
            }
            evidence
                .require_current()
                .map_err(|_| "The approved web search settings changed. Review this job again.")?;
            proposal_web::require_current_vault_mask(self.vault.as_ref(), evidence)?;
        }
        let evidence_vault_revision = (approval.knowledge.is_some()
            || approval.web.is_some()
            || approval.requires_fresh_web())
        .then(|| self.vault_change_revision());
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
                    if let Some(revision) = &evidence_vault_revision {
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
            fresh_web: None,
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

    pub(super) fn answer_scheduler_agent_resend(
        &mut self,
        resend: job_executor::agent::Resend,
    ) -> Task<Message> {
        match resend {
            job_executor::agent::Resend::Model(resend) => {
                let approval = self
                    .scheduler_agent_resend_authority(&resend.scope)
                    .and_then(|()| self.require_read_only_environment_web(&resend.scope));
                resend.settle(approval);
                Task::none()
            }
            job_executor::agent::Resend::Environment(research) => {
                self.queue_read_only_research(research)
            }
        }
    }

    fn scheduler_agent_resend_authority(
        &self,
        scope: &job_executor::agent::ResendScope,
    ) -> Result<(), &'static str> {
        let request = &scope.request;
        let scheduler = self
            .scheduler
            .as_ref()
            .filter(|scheduler| {
                !self.exiting
                    && !self.scheduler_draining
                    && scheduler.active_agent_run_approved(request)
            })
            .ok_or("The scheduled diagnostics run changed or ended.")?;
        if !scheduler.agent_predecessors_current(request, SystemTime::now()) {
            return Err("The job prerequisites changed before dispatch. Review the run again.");
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
        if self.require_firing_web_evidence(request)?.as_deref()
            != scope
                .advisory_prompt
                .as_deref()
                .and_then(agent::proposal::Prompt::fresh_web)
        {
            return Err("The web excerpts reviewed for this run changed.");
        }
        if let Some(evidence) = &approved.web {
            if !self.scheduler_web_allowed() {
                return Err("Managed policy blocks use of the approved web excerpts.");
            }
            evidence
                .require_current()
                .map_err(|_| "The approved web search settings changed. Review this job again.")?;
            proposal_web::require_current_vault_mask(self.vault.as_ref(), evidence)?;
            if scope
                .advisory_prompt
                .as_deref()
                .and_then(agent::proposal::Prompt::web)
                != Some(evidence.as_ref())
            {
                return Err("The approved web excerpts changed. Review this job again.");
            }
        }
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
                self.require_scheduler_prompt_memory(prompt, &policy, &config)?;
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
        let credentials =
            self.scheduler_transfer_review_credentials(&request.connection, request.sudo);
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
        let journal_output_allowed = self.scheduler_journal_output_allowed();
        let Some(scheduler) = self.scheduler.as_mut() else {
            return Task::none();
        };
        scheduler.set_swarm_chats_allowed(swarm_chats_allowed);
        scheduler.set_journal_output_allowed(journal_output_allowed);
        let requests = scheduler.update_runtime(message);
        self.sync_scheduler_views();
        let task = self.enqueue_jobs(requests);
        #[cfg(target_os = "macos")]
        self.sync_native_jobs_menu();
        task
    }

    pub(super) fn sync_scheduler_views(&mut self) {
        if let Some(scheduler) = &self.scheduler {
            let journal_root = self.journal_directory().ok();
            let system_dark = self.journal_system_dark();
            for view in self.scheduler_views.values_mut() {
                view.retain_run_detail_journal_root(journal_root.as_deref());
                scheduler.apply_diagram_preferences(
                    view,
                    self.window_settings.scheduler_diagram_horizontal,
                    self.window_settings.scheduler_diagram_color_mode,
                    system_dark,
                );
                scheduler.sync_view(view);
            }
        }
    }

    pub(super) fn save_scheduler_journal_divider_when_ready(&mut self) -> Task<Message> {
        let Some((due, _)) = self.scheduler_journal_divider_save else {
            return Task::none();
        };
        let remaining = due.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.flush_scheduler_journal_divider();
            Task::none()
        } else {
            self.delay_scheduler_journal_divider_save(remaining)
        }
    }

    pub(super) fn flush_scheduler_journal_divider(&mut self) {
        if let Some((_, owner)) = self.scheduler_journal_divider_save.take() {
            self.save_window_settings(owner);
        }
    }

    fn delay_scheduler_journal_divider_save(&mut self, delay: Duration) -> Task<Message> {
        let (send, receive) = async_channel::bounded(1);
        if thread::Builder::new()
            .name("kortty-scheduler-journal-divider".into())
            .spawn(move || {
                thread::sleep(delay);
                let _ = send.try_send(());
            })
            .is_err()
        {
            self.flush_scheduler_journal_divider();
            return Task::none();
        }
        Task::future(async move {
            let _ = receive.recv().await;
            Message::SaveSchedulerJournalDivider
        })
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
        mut outcome: job_executor::Completion,
        session_journals: Vec<String>,
    ) -> Task<Message> {
        self.scheduler_firing_web.forget(outcome.id);
        self.scheduler_read_only_research.forget(outcome.id);
        if !self.scheduler_journal_output_allowed()
            || outcome
                .output
                .as_ref()
                .is_some_and(|output| output.current(Some(&self.clipboard.policy)).is_none())
        {
            outcome.output = None;
        }
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
        self.scheduler_firing_web.forget(request.id);
        self.update_scheduler_runtime(scheduler_window::Message::Finished(
            job_executor::Completion {
                id: request.id,
                outcome: kortty_jobs::RunOutcome::Cancelled,
                detail: None,
                recording: None,
                output: None,
                swarm_report: None,
            },
        ))
    }

    pub(super) fn expire_pending_firing_web(&mut self) -> Task<Message> {
        let mut completions = Vec::new();
        for (id, reason) in self.expire_firing_web() {
            if let Some(index) = self
                .scheduler_queue
                .iter()
                .position(|queued| queued.id == id)
                && let Some(request) = self.scheduler_queue.remove(index)
            {
                let outcome = if request.cancellation.is_requested() || self.scheduler_draining {
                    kortty_jobs::RunOutcome::Cancelled
                } else {
                    kortty_jobs::RunOutcome::Blocked
                };
                completions.push(job_executor::Completion {
                    id,
                    outcome,
                    detail: Some(reason),
                    recording: None,
                    output: None,
                    swarm_report: None,
                });
            }
        }
        Task::batch(
            completions
                .into_iter()
                .map(|done| self.commit_scheduler_run(done, Vec::new())),
        )
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
            job_output_allowed: self.scheduler_journal_output_allowed(),
            job_output_redactions: self.clipboard.policy.terminal_logging.clone(),
            server_access: self.clipboard.policy.server_access().clone(),
            automation: None,
            sync: None,
            privileged: None,
            rsync: None,
            rsync_batch_parents: None,
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
                request.action.imported_ai.is_some()
                    || (!self.scheduler_firing_web.waiting(request.id)
                        && (ssh_room || per_firing(request))
                        && (request.cancellation.is_requested()
                            || !request.action.agent.as_ref().is_some_and(|action| {
                                self.ai_configuration()
                                    .and_then(|config| {
                                        config.resolve_profile_reference(&action.profile)
                                    })
                                    .is_some_and(|id| self.ai_model_resolution_pending(id))
                            })))
            });
            let Some(request) = next.and_then(|index| self.scheduler_queue.remove(index)) else {
                break;
            };
            if let Err(error) = request.action.require_executable() {
                let cancelled = request.cancellation.is_requested();
                self.scheduler_workers += 1;
                tasks.push(Task::done(Message::JobFinished(
                    job_executor::Completion {
                        id: request.id,
                        outcome: if cancelled {
                            kortty_jobs::RunOutcome::Cancelled
                        } else {
                            kortty_jobs::RunOutcome::Blocked
                        },
                        detail: (!cancelled).then(|| error.into()),
                        recording: None,
                        output: None,
                        swarm_report: None,
                    },
                    Vec::new(),
                )));
                continue;
            }
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
                        output: None,
                        swarm_report: None,
                    },
                    Vec::new(),
                )));
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
                        output: None,
                        swarm_report: None,
                    },
                    Vec::new(),
                )));
                continue;
            }
            let fresh = (|| {
                if request.cancellation.is_requested() {
                    return Ok(None);
                }
                if request
                    .action
                    .agent
                    .as_ref()
                    .and_then(|agent| agent.approval.as_deref())
                    .is_some_and(agent::Approval::requires_fresh_web)
                    && !self.scheduler_firing_web.contains(id)
                {
                    tasks.push(self.begin_firing_web_review(&request)?);
                    if firing_web::unattended::grant(&request).is_none()
                        && self.scheduler_views.is_empty()
                        && let Some((&owner, _)) = self
                            .windows
                            .iter()
                            .find(|(_, window)| window.focused)
                            .or_else(|| self.windows.iter().next())
                    {
                        self.notice(owner, "JobScheduler is waiting for a web review.".into());
                    }
                    return Ok(None);
                }
                self.require_firing_web_evidence(&request)
                    .map_err(str::to_owned)
            })();
            let fresh = match fresh {
                Ok(_) if self.scheduler_firing_web.waiting(id) => {
                    self.scheduler_queue.push_back(request);
                    continue;
                }
                Ok(evidence) => evidence,
                Err(error) => {
                    self.scheduler_workers += 1;
                    tasks.push(Task::done(Message::JobFinished(
                        job_executor::Completion {
                            id,
                            outcome: kortty_jobs::RunOutcome::Blocked,
                            detail: Some(error),
                            recording: None,
                            output: None,
                            swarm_report: None,
                        },
                        Vec::new(),
                    )));
                    continue;
                }
            };
            self.scheduler_swarm_comparisons.remember(&request);
            if per_firing(&request) {
                tasks.push(self.queue_firing_generation(request));
                continue;
            }
            let (done, automation, journal_warning, resends) = if request.action.agent.is_some() {
                let mut context = self.scheduler_agent_context(&request);
                let (automation, journal_warning) = if let Ok(context) = &mut context {
                    context.fresh_web = fresh;
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
                let elevated_rsync = request
                    .action
                    .remote
                    .as_ref()
                    .and_then(RemoteAction::rsync_transfer)
                    .is_some_and(|(transfer, _)| transfer.uses_passwordless_sudo());
                let elevation_allowed = (request.action.sudo == SudoMode::Disabled
                    && !elevated_rsync)
                    || policy.sudo_allowed;
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
                let rsync = match self.scheduler.as_mut().map_or(Ok(None), |scheduler| {
                    scheduler.try_rsync_run_context(&request)
                }) {
                    Ok(rsync) => rsync,
                    Err(error) => {
                        self.scheduler_workers += 1;
                        tasks.push(Task::done(Message::JobFinished(
                            job_executor::Completion {
                                id,
                                outcome: kortty_jobs::RunOutcome::Blocked,
                                detail: Some(error),
                                recording: None,
                                output: None,
                                swarm_report: None,
                            },
                            Vec::new(),
                        )));
                        continue;
                    }
                };
                let reviewed_rsync =
                    scheduler_window::rsync_authority::uses_reviewed_receiver(&request.action);
                let saved_auth =
                    if reviewed_folder && sync.is_none() || reviewed_rsync && rsync.is_none() {
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
                job_policy.rsync = rsync;
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
                    output: None,
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
        if request.cancellation.is_requested() || request.action.remote.is_none() {
            return Ok(None);
        }
        let remote = request.action.remote_action()?;
        if !file_transfer_allowed && remote.transfers_files() {
            return Ok(None);
        }
        let archive = match remote {
            RemoteAction::Archive(archive) | RemoteAction::ServerArchive(archive) => archive,
            RemoteAction::PrivilegedArchive(archive) => &archive.archive,
            _ => return Ok(None),
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
        if action.has_teamwork_targets() {
            return self.scheduler_teamwork_auth(action);
        }
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

    fn scheduler_teamwork_auth(&self, action: &SshCommand) -> job_executor::JobAuth {
        let prepare = || {
            action.validate_target_selection()?;
            let store = profile::connections_path()
                .and_then(ConnectionStore::open)
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            let (catalog, shared) = self.scheduler_shared_targets(&store)?;
            action.require_current_targets(&store, &shared)?;
            for reference in action
                .targets()
                .filter(|reference| reference.shared_identity().is_none())
            {
                if store
                    .get(reference.id)
                    .map(scheduler_window::ConnectionRef::from)
                    .as_ref()
                    != Some(reference)
                {
                    return Err("A selected local SSH route changed or is unavailable.");
                }
            }
            let server_access = self.clipboard.policy.server_access();
            let secret = |id: &str, kind, endpoint: &crate::ssh_dialog::LoginTarget| {
                self.vault_credential(id, kind, endpoint)
            };
            let credentials = action
                .targets()
                .map(|reference| {
                    if let Some(identity) = reference.shared_identity() {
                        let entry = catalog
                            .get(identity)
                            .ok_or("The Teamwork connection is unavailable.")?;
                        self.scheduler_shared_credentials(
                            reference,
                            entry,
                            &store,
                            action.virtual_terminal.is_some(),
                            action.sudo == SudoMode::Vault,
                        )
                    } else {
                        let current = store
                            .get(reference.id)
                            .ok_or("The saved SSH connection is unavailable.")?;
                        job_executor::Credentials::resolve_directory_browser(
                            reference,
                            current,
                            server_access,
                            secret,
                        )
                    }
                })
                .collect::<Vec<_>>();
            let auth = if action.additional_connections.is_empty() {
                job_executor::JobAuth::Saved(
                    credentials
                        .into_iter()
                        .next()
                        .ok_or("The saved SSH connection is unavailable.")?
                        .map(Box::new),
                )
            } else {
                job_executor::JobAuth::Batch(Ok(credentials))
            };
            Ok(if action.sudo == SudoMode::Vault {
                auth.with_sudo_passwords(|reference| {
                    // Local folder-based sudo scopes do not apply to shared paths.
                    let group = if reference.shared_identity().is_some() {
                        None
                    } else {
                        store.group_of(reference.id)
                    };
                    self.scheduler_sudo_password(reference, group)
                })
            } else {
                auth
            })
        };
        prepare().unwrap_or_else(|error| {
            if action.additional_connections.is_empty() {
                job_executor::JobAuth::Saved(Err(error))
            } else {
                job_executor::JobAuth::Batch(Err(error))
            }
        })
    }

    fn scheduler_shared_credentials(
        &self,
        reference: &scheduler_window::ConnectionRef,
        entry: &crate::connection_store::teamwork::Entry,
        store: &ConnectionStore,
        recording: bool,
        mut uses_vault: bool,
    ) -> Result<job_executor::Credentials, &'static str> {
        let mut credentials = job_executor::Credentials::resolve_shared(
            reference,
            entry,
            store,
            recording,
            self.clipboard.policy.server_access(),
            |id, kind, endpoint| {
                uses_vault = true;
                self.vault_credential(id, kind, endpoint)
            },
        )?;
        if uses_vault {
            let revision = self
                .vault
                .as_ref()
                .and_then(|vault| vault.credential_revision())
                .ok_or(crate::vault_window::VAULT_LOCKED)?;
            credentials.bind_shared_vault_revision(revision);
        }
        Ok(credentials)
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
    fn scheduler_run_detail_copy_uses_internal_clipboard_for_the_current_selection() {
        use iced::widget::text_editor::Action;
        use kortty_jobs::{FinishedRunMetadata, Job, JobGraph, RunOutcome, RunOutput, Scheduler};

        let scratch = Scratch::new();
        let database = scratch.0.join("jobs.json");
        let job = JobId::new("clipboard-fixture").unwrap();
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "host": "fixture.invalid", "port": 22, "username": "operator",
            "key_file": scratch.0.join("unused-key"), "command": "printf ready"
        }))
        .unwrap();
        let graph = JobGraph::new(vec![Job::new(job.clone(), "Fixture", action).unwrap()]).unwrap();
        let mut scheduler = Scheduler::new(graph);
        let started = SystemTime::now();
        let run = scheduler.start_manual(&job, started).unwrap().started[0].id;
        let completed = scheduler
            .complete(run, RunOutcome::Success, started)
            .unwrap();
        let mut store = kortty_jobs::SchedulerStore::open(&database).unwrap();
        store
            .commit_with_actions(scheduler.graph(), &[], &[])
            .unwrap();
        store
            .commit_finished_runs(
                scheduler.graph(),
                &completed.finished,
                FinishedRunMetadata {
                    output: &[(
                        run,
                        RunOutput {
                            stdout: "café".into(),
                            stderr: "警告".into(),
                        },
                    )],
                    ..FinishedRunMetadata::default()
                },
            )
            .unwrap();
        drop(store);

        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        let scheduler = SchedulerWindow::open(database).unwrap();
        let window = window::Id::unique();
        workspace
            .scheduler_views
            .insert(window, scheduler.new_view());
        workspace.scheduler = Some(scheduler);

        use scheduler_window::{Message as ActionMessage, Page};
        let _ = workspace.update_scheduler(window, ActionMessage::Page(Page::Journal));
        let _ = workspace.update_scheduler(window, ActionMessage::SelectRecord(run));
        let _ = workspace.update_scheduler(
            window,
            ActionMessage::SelectRunDetail(run, Action::SelectAll),
        );
        let _ = workspace.update_scheduler(window, ActionMessage::CopyRunDetail(run));
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("stdout\ncafé\n\nstderr\n警告")
        );

        workspace.clipboard.buffer.set("prior".into());
        let _ = workspace.update_scheduler(window, ActionMessage::Page(Page::Action));
        let _ = workspace.update_scheduler(window, ActionMessage::CopyRunDetail(run));
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("prior"));
    }

    #[test]
    fn scheduler_deletion_persists_journal_revocation_before_removing_the_job() {
        use crate::session_journal::summary_queue::{
            AutomaticApproval, AutomationSource, ProfileBinding, ProfileChoice, Queue,
        };

        let scratch = Scratch::new();
        let job = JobId::new("java-fixture").unwrap();
        let source = AutomationSource::Job(job.clone());
        let database = scratch.0.join("jobs.json");
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "host": "fixture.invalid", "port": 22, "username": "operator",
            "key_file": scratch.0.join("unused-key"), "command": "true"
        }))
        .unwrap();
        let graph = kortty_jobs::JobGraph::new(vec![
            kortty_jobs::Job::new(job.clone(), "Imported fixture", action).unwrap(),
        ])
        .unwrap();
        kortty_jobs::SchedulerStore::open(&database)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let grant = AutomaticApproval {
            profile: ProfileChoice {
                id: crate::ai_manager::ProfileRef::Numeric(1),
                model: "fixture".into(),
                destination: "https://fixture.invalid/v1".into(),
                policy_model_fingerprint: None,
                https: Some(ProfileBinding {
                    root: scratch.0.join("journals"),
                    profile_revision: [0; 32],
                    redaction_revision: [0; 32],
                }),
                cli: None,
            },
            since: jiff::Timestamp::now(),
            evidence_limits: None,
            title_since: None,
        };
        let mut queue = Queue::load(&scratch.0).unwrap();
        let mut preferences = queue.preferences().clone();
        preferences
            .set_source_approval(source.clone(), Some((grant.clone(), false)))
            .unwrap();
        preferences
            .set_source_approval(AutomationSource::Swarm, Some((grant, false)))
            .unwrap();
        queue.set_preferences(preferences).unwrap();

        let mut workspace = Workspace::default();
        workspace.journal_summaries.load(&scratch.0);
        let scheduler = SchedulerWindow::open(database).unwrap();
        let window = window::Id::unique();
        workspace
            .scheduler_views
            .insert(window, scheduler.new_view());
        workspace.scheduler = Some(scheduler);
        let _ = workspace.update_scheduler(window, scheduler_window::Message::Delete);

        let directory = crate::private_files::Directory::open(&scratch.0, false).unwrap();
        let lock = directory.open_lock(".journal-summary-jobs.lock").unwrap();
        lock.try_lock().unwrap();
        let _ = workspace.update_scheduler(window, scheduler_window::Message::Confirm);
        lock.unlock().unwrap();
        let scheduler = workspace.scheduler.as_ref().unwrap();
        assert_eq!(
            scheduler.automation_job_name(&job),
            Some("Imported fixture")
        );
        assert_eq!(
            scheduler.confirmed_job_deletion(&workspace.scheduler_views[&window]),
            Some(&job)
        );
        assert!(
            Queue::load(&scratch.0)
                .unwrap()
                .preferences()
                .source_approval(&source)
                .is_some()
        );

        let _ = workspace.update_scheduler(window, scheduler_window::Message::Confirm);
        assert!(
            workspace
                .scheduler
                .as_ref()
                .unwrap()
                .automation_job_name(&job)
                .is_none()
        );
        let queue = Queue::load(&scratch.0).unwrap();
        assert!(queue.preferences().source_approval(&source).is_none());
        assert!(
            queue
                .preferences()
                .source_approval(&AutomationSource::Swarm)
                .is_some()
        );
    }

    #[test]
    fn injected_imported_ai_run_completes_without_opening_a_worker_or_journal() {
        use iced::futures::executor::block_on;
        use kortty_jobs::{Job, JobGraph, RunOutcome, Scheduler};

        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "connection": {"id": 1, "target": {"host": "fixture.invalid", "port": 22, "username": "operator"}},
            "imported_ai": {"kind": "agent", "goal": "Inspect fixture", "parallelism": 1, "swarm_read_only": true}
        })).unwrap();
        let job = JobId::new("draft").unwrap();
        let mut model = Scheduler::new(
            JobGraph::new(vec![
                Job::new(job.clone(), "Draft fixture", action)
                    .unwrap()
                    .with_enabled(false),
            ])
            .unwrap(),
        );
        let request = model
            .start_manual(&job, SystemTime::now())
            .unwrap()
            .started
            .remove(0);
        let expected = request.id;
        let mut workspace = Workspace::default();
        let mut stream =
            iced_runtime::task::into_stream(workspace.enqueue_jobs(vec![request])).unwrap();
        assert!(workspace.scheduler_queue.is_empty());
        let Some(iced_runtime::Action::Output(Message::JobFinished(completion, journals))) =
            block_on(stream.next())
        else {
            panic!("expected a blocked scheduler completion")
        };
        assert_eq!(completion.id, expected);
        assert_eq!(completion.outcome, RunOutcome::Blocked);
        assert_eq!(
            completion.detail.as_deref(),
            Some(scheduler_window::IMPORTED_AI_REVIEW_REQUIRED)
        );
        assert!(completion.recording.is_none() && journals.is_empty());
        assert!(block_on(stream.next()).is_none());
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
        let mut vault =
            kortty_vault::Vault::create(&path, &crate::vault_window::test_vault_password()[..])
                .unwrap();
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
        let mut vault =
            Vault::create(&vault_path, &crate::vault_window::test_vault_password()[..]).unwrap();
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
                output: None,
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
        scheduler.set_agent_profiles(Some(&config), true);
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
        runtime.set_agent_profiles(Some(&config), true);
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

        assert!(workspace.guard_hosted_quit(Default::default()).is_some());
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
