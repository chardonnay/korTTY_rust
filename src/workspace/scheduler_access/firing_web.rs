//! Single-firing consent for the exact web lookup and resulting masked evidence.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_channel::{Sender, TryRecvError};
use iced::widget::{button, column, row, text};
use iced::{Element, Length, Task};
use kortty_jobs::{RunId, RunRequest};

use crate::clipboard_policy::ClipboardPolicy;
use crate::scheduler_window::{SshCommand, agent};
use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::web;

use super::{ConnectionStore, Message, Workspace, job_executor, proposal_web};

pub(super) mod round;
pub(super) mod unattended;

const TIMEOUT: Duration = Duration::from_secs(300);
const PERMIT_TIMEOUT: Duration = Duration::from_secs(5);
const CHANGED: &str =
    "The scheduled web review changed or ended. Start a new firing and review it again.";

#[derive(Clone)]
pub enum Event {
    View(RunId, u64, web::Message),
    Cancel(RunId, u64),
    Worker(RunId, web::Message),
    Authorize(RunId, u64, Sender<()>),
    UnattendedQuery(RunId, u64, Result<String, String>),
    UnattendedFollowUp(RunId, u64, Result<unattended::FollowUp, String>),
    UnattendedResults(RunId, u64, Result<Arc<web::Results>, String>),
    Round(round::Event),
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Scheduled firing web review")
    }
}

#[derive(Default)]
pub(in crate::workspace) struct State {
    entries: BTreeMap<RunId, Entry>,
    rounds: round::State,
}

struct Entry {
    request: Arc<RunRequest<SshCommand>>,
    review: web::State,
    scope: web::Scope,
    provider_mask: agent::provider_mask::ProviderMask,
    vault: Weak<()>,
    deadline: Instant,
    status: Option<String>,
    approved: Option<Arc<web::Evidence>>,
    review_revision: u64,
    unattended: Option<unattended::Lookup>,
}

impl Entry {
    fn same_request(&self, request: &RunRequest<SshCommand>) -> bool {
        let reviewed = &self.request;
        reviewed.id == request.id
            && reviewed.job == request.job
            && reviewed.action == request.action
            && reviewed.trigger == request.trigger
            && reviewed.started_at == request.started_at
            && reviewed
                .cancellation
                .shares_signal_with(&request.cancellation)
    }
}

impl State {
    pub(super) fn review_revision(&self, id: RunId) -> Option<u64> {
        self.entries
            .get(&id)
            .filter(|entry| entry.approved.is_none())
            .map(|entry| entry.review_revision)
    }

    pub(in crate::workspace) fn waiting(&self, id: RunId) -> bool {
        self.review_revision(id).is_some()
    }

    pub(super) fn contains(&self, id: RunId) -> bool {
        self.entries.contains_key(&id)
    }

    pub(super) fn forget(&mut self, id: RunId) {
        self.entries.remove(&id);
        self.rounds.forget(id);
    }

    pub(super) fn needs_tick(&self) -> bool {
        self.entries.values().any(|entry| entry.approved.is_none()) || self.rounds.needs_tick()
    }

    #[cfg(test)]
    pub(super) fn expire_fixture(&mut self, id: RunId) {
        self.entries.get_mut(&id).unwrap().deadline = Instant::now();
    }
}

pub(super) struct Context {
    pub(super) scope: web::Scope,
    provider_mask: agent::provider_mask::ProviderMask,
    redactor: Redactor,
    pub(super) model: crate::local_ai::Profile,
    pub(super) context_limit: usize,
    pub(super) connection_path: PathBuf,
    pub(super) trust: kortty_ssh::TrustStore,
    mask: job_executor::agent::Mask,
}

impl Workspace {
    pub(super) fn environment_web_context(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Context, String> {
        let entry = self
            .scheduler_firing_web
            .entries
            .get(&request.id)
            .ok_or(CHANGED)?;
        if entry.approved.is_none()
            || !unattended::grant(request)
                .is_some_and(agent::web_lookup::Grant::allows_environment_lookup)
        {
            return Err(CHANGED.into());
        }
        self.current_firing_web_context(entry, request)
    }

    pub(super) fn begin_firing_web_review(
        &mut self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Task<Message>, String> {
        let context = self.firing_web_context(request)?;
        context.scope.require_current()?;
        let review = web::State::Draft(context.scope.clone(), String::new(), web::Lookup::Search);
        let entry = Entry {
            request: Arc::new(RunRequest {
                id: request.id,
                job: request.job.clone(),
                action: Arc::clone(&request.action),
                trigger: request.trigger,
                started_at: request.started_at,
                cancellation: request.cancellation.clone(),
            }),
            review,
            scope: context.scope.clone(),
            provider_mask: context.provider_mask.clone(),
            vault: self.vault_change_revision(),
            deadline: Instant::now() + TIMEOUT,
            status: None,
            approved: None,
            review_revision: 0,
            unattended: None,
        };
        self.scheduler_firing_web.entries.insert(request.id, entry);
        if let Some(grant) = unattended::grant(request) {
            let task = self.start_unattended_query(request.id, grant, &context);
            if task.is_err() {
                self.scheduler_firing_web.forget(request.id);
            }
            task
        } else {
            Ok(Task::none())
        }
    }

    pub(super) fn firing_web_view(&self) -> Option<Element<'_, Message>> {
        let waiting = self
            .scheduler_firing_web
            .entries
            .iter()
            .find(|(_, entry)| entry.approved.is_none() && entry.unattended.is_none());
        let Some((&id, entry)) = waiting else {
            return self.swarm_round_web_view();
        };
        let name = self
            .scheduler
            .as_ref()?
            .automation_job_name(&entry.request.job)
            .unwrap_or(entry.request.job.as_str());
        let revision = entry.review_revision;
        let action = move |message| Message::ScheduledFiringWeb(Event::View(id, revision, message));
        let review = entry
            .review
            .view()
            .map(|view| row![view.map(action)].height(Length::Fixed(255.0)));
        Some(column![
            row![
                text(format!("Web evidence for {name} · run {id}")).size(19).width(Length::Fill),
                button("Cancel run").on_press(Message::ScheduledFiringWeb(Event::Cancel(id, revision))),
            ].spacing(8),
            text("Review a search or webpage and its excerpts for this run before its model requests start.").size(13),
            entry.status.as_ref().map(|status| text(status).size(13)),
            review,
            (!entry.review.active()).then(|| button("Review fresh web request…")
                .on_press(action(web::Message::Open))),
        ].spacing(10).padding(12).into())
    }

    pub(in crate::workspace) fn update_firing_web(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Authorize(id, ticket, permit) => {
                if let Some(entry) = self.scheduler_firing_web.entries.get(&id)
                    && (matches!(&entry.review, web::State::Pending { ticket: current, .. } if *current == ticket)
                        || entry
                            .unattended
                            .as_ref()
                            .is_some_and(|lookup| lookup.matches(ticket)))
                    && self
                        .current_firing_web_context(entry, &entry.request)
                        .is_ok()
                {
                    let _ = permit.try_send(());
                }
                permit.close();
                Task::none()
            }
            Event::Cancel(id, revision)
                if self.scheduler_firing_web.review_revision(id) == Some(revision) =>
            {
                if let Some(scheduler) = self.scheduler.as_ref() {
                    scheduler.cancel_firing_web_run(id);
                }
                Task::batch([
                    self.expire_pending_firing_web(),
                    self.enqueue_jobs(Vec::new()),
                ])
            }
            Event::View(id, revision, message)
                if self.scheduler_firing_web.review_revision(id) == Some(revision)
                    && self.scheduler_firing_web.entries[&id].unattended.is_none()
                    && !matches!(message, web::Message::Finished(..)) =>
            {
                self.handle_firing_web_message(id, message)
            }
            Event::Worker(id, message @ web::Message::Finished(..))
                if self.scheduler_firing_web.waiting(id) =>
            {
                self.handle_firing_web_message(id, message)
            }
            Event::UnattendedQuery(id, ticket, result) => {
                self.finish_unattended_query(id, ticket, result)
            }
            Event::UnattendedFollowUp(id, ticket, result) => {
                self.finish_unattended_follow_up(id, ticket, result)
            }
            Event::UnattendedResults(id, ticket, result) => {
                self.finish_unattended_lookup(id, ticket, result)
            }
            Event::Round(event) => self.update_swarm_round_web(event),
            _ => Task::none(),
        }
    }

    fn handle_firing_web_message(&mut self, id: RunId, message: web::Message) -> Task<Message> {
        match self.apply_firing_web(id, message) {
            Ok(task) if !self.scheduler_firing_web.waiting(id) => {
                Task::batch([task, self.enqueue_jobs(Vec::new())])
            }
            Ok(task) => task,
            Err(error) => {
                if let Some(entry) = self.scheduler_firing_web.entries.get_mut(&id) {
                    entry.status = Some(error);
                }
                Task::none()
            }
        }
    }

    fn apply_firing_web(
        &mut self,
        id: RunId,
        message: web::Message,
    ) -> Result<Task<Message>, String> {
        let Some(entry) = self.scheduler_firing_web.entries.get(&id) else {
            return Ok(Task::none());
        };
        if let web::Message::Finished(ticket, _) = &message
            && !matches!(&entry.review, web::State::Pending { ticket: current, .. } if current == ticket)
        {
            return Ok(Task::none());
        }
        let request = Arc::clone(&entry.request);
        let context = self.current_firing_web_context(entry, &request)?;
        let vault = proposal_web::vault_secrets(self.vault.as_ref());
        let revision = entry.vault.clone();
        let mask = {
            let redactor = Arc::new(context.redactor);
            let mask = context.mask;
            move |text: &str| {
                let redactor = proposal_web::vault_overlay(&vault, &redactor, text)?;
                mask(&redactor, text)
            }
        };
        let deadline = entry.deadline;
        let scope = context.scope;
        let model = context.model;
        let credential = entry.provider_mask.clone();
        let connection_path = context.connection_path;
        let trust = context.trust;
        let include = matches!(message, web::Message::Include);
        let token = matches!(message, web::Message::Send).then(|| self.allocate());
        let (dispatches, authorizations) = async_channel::bounded(1);
        let entry = self
            .scheduler_firing_web
            .entries
            .get_mut(&id)
            .expect("current scheduled web review");
        let phase = review_phase(&entry.review);
        let task = entry.review.update(
            message,
            scope,
            mask.clone(),
            token,
            move || {
                check_worker(&request, &revision, deadline, &connection_path, &trust)?;
                credential.check(&model)?;
                wait_for_permit(&dispatches, &request, deadline)
            },
            move |message| Message::ScheduledFiringWeb(Event::Worker(id, message)),
        );
        if review_phase(&entry.review) != phase {
            entry.review_revision += 1;
        }
        let task = task?;
        entry.status = None;
        if include && let Some((scope, results)) = entry.review.included() {
            let evidence = web::Evidence::new(scope, results).and_then(|evidence| {
                evidence.require_current()?;
                evidence.require_mask(mask)?;
                Ok(evidence)
            });
            match evidence {
                Ok(evidence) => entry.approved = Some(Arc::new(evidence)),
                Err(error) => {
                    entry.review = web::State::Closed;
                    return Err(error);
                }
            }
        }
        if let Some(ticket) = token {
            Ok(Task::run(authorizations, move |permit| {
                Message::ScheduledFiringWeb(Event::Authorize(id, ticket, permit))
            })
            .chain(task))
        } else {
            Ok(task)
        }
    }

    pub(super) fn require_firing_web_evidence(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Option<Arc<web::Evidence>>, &'static str> {
        if !fresh_web(request) {
            return Ok(None);
        }
        let entry = self
            .scheduler_firing_web
            .entries
            .get(&request.id)
            .ok_or(CHANGED)?;
        let evidence = entry
            .approved
            .as_ref()
            .ok_or("Review and include fresh web excerpts for this run.")?;
        let context = self
            .current_firing_web_context(entry, request)
            .map_err(|_| CHANGED)?;
        evidence.require_current().map_err(|_| CHANGED)?;
        let vault = proposal_web::vault_secrets(self.vault.as_ref());
        evidence
            .require_mask(|text| {
                let redactor = proposal_web::vault_overlay(&vault, &context.redactor, text)?;
                (context.mask)(&redactor, text)
            })
            .map_err(|_| "The reviewed web excerpts no longer match the scheduled input mask.")?;
        Ok(Some(Arc::clone(evidence)))
    }

    pub(super) fn expire_firing_web(&mut self) -> Vec<(RunId, String)> {
        let expired = self
            .scheduler_firing_web
            .entries
            .iter()
            .filter(|(_, entry)| entry.approved.is_none())
            .filter_map(|(&id, entry)| {
                self.current_firing_web_context(entry, &entry.request)
                    .err()
                    .map(|error| (id, error))
            })
            .collect::<Vec<_>>();
        for (id, _) in &expired {
            self.scheduler_firing_web.forget(*id);
        }
        expired
    }

    fn current_firing_web_context(
        &self,
        entry: &Entry,
        request: &RunRequest<SshCommand>,
    ) -> Result<Context, String> {
        if !entry.same_request(request)
            || !Weak::ptr_eq(&entry.vault, &self.vault_change_revision())
            || entry.vault.strong_count() == 0
        {
            return Err(CHANGED.into());
        }
        if entry.approved.is_none() && Instant::now() >= entry.deadline {
            return Err("The scheduled web review expired before it was approved.".into());
        }
        let context = self.firing_web_context(request)?;
        if entry.scope != context.scope {
            return Err(CHANGED.into());
        }
        entry.provider_mask.check(&context.model)?;
        context.scope.require_current()?;
        if let Some(grant) = unattended::grant(request) {
            context
                .scope
                .require_unattended_profile(grant, context.redactor.redaction_policy())?;
        }
        if let Some(lookup) = &entry.unattended {
            lookup.check_credential(&context.model)?;
        }
        Ok(context)
    }

    fn firing_web_context(&self, request: &RunRequest<SshCommand>) -> Result<Context, String> {
        if self.exiting || self.scheduler_draining || !self.scheduler_web_allowed() {
            return Err(
                "Managed policy or scheduler state blocks the scheduled web review.".into(),
            );
        }
        let agent = request
            .action
            .agent
            .as_ref()
            .filter(|_| fresh_web(request))
            .ok_or(CHANGED)?;
        let approval = agent.approval.as_ref().ok_or(CHANGED)?;
        let context = self.scheduler_agent_context(request)?;
        let (policy, config) = self.scheduler_model_policy(
            agent.kind,
            &approval.profile,
            &context.transport,
            context.context_limit,
            &context.log_policy,
        )?;
        let store = ConnectionStore::open(context.connection_store.clone())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        for target in &approval.targets {
            self.scheduler_reviewed_agent_target(target, &store, &context.trust, &policy)?;
        }
        let (secrets, _) = self.firing_secrets(&approval.targets, &store)?;
        let redactor = Redactor::for_journal(secrets, context.log_policy);
        let provider_mask = agent::provider_mask::ProviderMask::capture(&context.transport);
        let redactor = provider_mask.apply(&redactor)?;
        let profile = config
            .resolve_profile_reference(&approval.profile.id)
            .ok_or(CHANGED)?;
        let scope = web::Scope::new(Some(&config), profile, policy.ai_web_search_allowed)
            .ok_or("The approved profile has no current web search configuration.")?
            .with_redactor(&redactor);
        Ok(Context {
            scope,
            provider_mask,
            redactor,
            model: context.transport,
            context_limit: context.context_limit,
            connection_path: context.connection_store,
            trust: context.trust,
            mask: context.mask,
        })
    }
}

fn review_phase(review: &web::State) -> (std::mem::Discriminant<web::State>, bool) {
    (
        std::mem::discriminant(review),
        matches!(review, web::State::Draft(_, _, web::Lookup::Page)),
    )
}

fn fresh_web(request: &RunRequest<SshCommand>) -> bool {
    request
        .action
        .agent
        .as_ref()
        .and_then(|agent| agent.approval.as_ref())
        .is_some_and(|approval| approval.requires_fresh_web())
}

pub(super) fn wait_for_permit(
    dispatches: &Sender<Sender<()>>,
    request: &RunRequest<SshCommand>,
    deadline: Instant,
) -> Result<(), String> {
    let (permit, reply) = async_channel::bounded(1);
    dispatches.try_send(permit).map_err(|_| CHANGED)?;
    let deadline = deadline.min(Instant::now() + PERMIT_TIMEOUT);
    loop {
        if request.cancellation.is_requested() || Instant::now() >= deadline {
            return Err(CHANGED.into());
        }
        match reply.try_recv() {
            Ok(()) => return Ok(()),
            Err(TryRecvError::Closed) => return Err(CHANGED.into()),
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

pub(super) fn check_worker(
    request: &RunRequest<SshCommand>,
    vault: &Weak<()>,
    deadline: Instant,
    connection_path: &std::path::Path,
    trust: &kortty_ssh::TrustStore,
) -> Result<(), String> {
    if request.cancellation.is_requested()
        || vault.strong_count() == 0
        || Instant::now() >= deadline
    {
        return Err(CHANGED.into());
    }
    let agent = request.action.agent.as_ref().ok_or(CHANGED)?;
    let approval = agent.approval.as_ref().ok_or(CHANGED)?;
    let policy = ClipboardPolicy::load();
    job_executor::agent::require_policy(&policy, agent.kind)?;
    let store = ConnectionStore::open(connection_path.to_owned())
        .map_err(|_| "The saved SSH connections are unavailable.")?;
    request.action.require_installed_target_groups()?;
    for target in &approval.targets {
        let reference = &target.reference;
        if policy
            .server_access()
            .first_blocked(reference.target.servers(reference.jump.as_ref()))
            .is_some()
            || store
                .get(reference.id)
                .and_then(|connection| agent::Target::capture(reference, connection, trust).ok())
                .as_ref()
                != Some(target)
        {
            return Err("The reviewed SSH routes or host keys changed. Start a new firing.".into());
        }
    }
    Ok(())
}
