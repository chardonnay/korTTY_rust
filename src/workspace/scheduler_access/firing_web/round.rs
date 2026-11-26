//! One reviewed web request at a time for a target in a scheduled firing.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime};

use async_channel::Sender;
use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Length, Task};
use kortty_jobs::RunId;

use crate::clipboard_policy::ClipboardPolicy;
use crate::job_executor::agent::{AdvisoryWebAuthority, ModelWebReview, WEB_VAULT_CHANGED};
use crate::local_ai::web_search::PageUrl;
use crate::scheduled_agent_review::{Key, Phase, Ticket};
use crate::scheduler_window::{SshCommand, agent};
use crate::terminal_agent::usage::{
    ApprovedWebResult, WebOperation, WebProposal, web::require_clean_call, web_error_result,
};
use crate::workspace::terminal_ai::web;

use super::{
    CHANGED, Context, Event as FiringWebEvent, Message, TIMEOUT, Workspace, check_worker,
    review_phase, unattended, wait_for_permit,
};
use kortty_jobs::RunRequest;
use unattended::Credentials;

const EXPIRED: &str = "The scheduled web review expired before it was approved.";

#[derive(Clone)]
pub enum Event {
    View(Ticket, u64, web::Message),
    Authorize(Ticket, u64, Sender<()>),
    Worker(Ticket, web::Message),
    Skip(Ticket, u64),
}

#[derive(Default)]
pub(super) struct State {
    entries: BTreeMap<Key, Entry>,
    model_lookups: BTreeMap<Key, u8>,
}

struct Entry {
    ticket: Ticket,
    review: web::State,
    revision: u64,
    scope: web::Scope,
    page_allowed: bool,
    credentials: Credentials,
    vault: Weak<()>,
    deadline: Instant,
    status: Option<String>,
    model_tool: Option<ModelTool>,
}

struct ModelTool {
    proposal: Arc<WebProposal>,
    approved_input: Option<String>,
    read_only: Option<AdvisoryWebAuthority>,
}

impl Drop for ModelTool {
    fn drop(&mut self) {
        self.proposal
            .reject("The scheduled model web review ended.");
    }
}

impl State {
    fn get(&self, ticket: &Ticket) -> Option<&Entry> {
        self.entries
            .get(&ticket.key())
            .filter(|entry| entry.ticket == *ticket)
    }

    fn remove(&mut self, ticket: &Ticket) {
        if self.get(ticket).is_some() {
            self.entries.remove(&ticket.key());
        }
    }

    pub(super) fn forget(&mut self, id: RunId) {
        self.entries.retain(|key, _| key.run() != id);
        self.model_lookups.retain(|key, _| key.run() != id);
    }

    pub(super) fn model_web_available(&self, key: Key) -> bool {
        self.model_lookups.get(&key).copied().unwrap_or_default()
            < agent::web_lookup::MAX_MODEL_LOOKUPS_PER_TARGET
    }

    pub(super) fn needs_tick(&self) -> bool {
        !self.entries.is_empty()
    }
}

impl Workspace {
    pub(in crate::workspace::scheduler_access) fn begin_swarm_round_web(
        &mut self,
        ticket: &Ticket,
        lookup: web::Lookup,
        value: String,
    ) -> Result<Task<Message>, String> {
        let key = ticket.key();
        if self.scheduler_firing_web.rounds.entries.contains_key(&key) || ticket.round() < 2 {
            return Err(CHANGED.into());
        }
        let context = self.swarm_round_web_context(ticket)?;
        let page_reviewed = self
            .swarm_round_web_request(ticket)
            .and_then(|request| unattended::grant(request))
            .is_some_and(agent::web_lookup::Grant::allows_round_page)
            && context.scope.page_review().is_some();
        if lookup == web::Lookup::Page && !page_reviewed {
            return Err("The scheduled Swarm round has no reviewed page reader.".into());
        }
        let credentials = Credentials::capture(&context.model, &context.scope, page_reviewed)?;
        let page_allowed = page_reviewed && credentials.page_available();
        let (lookup, draft, status) = match lookup {
            web::Lookup::Page if !page_allowed => (
                web::Lookup::Search,
                String::new(),
                Some("The page reader credential is unavailable. Search or skip the lookup."),
            ),
            _ => match reviewed_public_input(
                lookup,
                &value,
                &credentials,
                &self.unattended_web_mask(&context),
            ) {
                Ok(draft) => (lookup, draft, None),
                Err(_) => (
                    lookup,
                    String::new(),
                    Some(match lookup {
                        web::Lookup::Search => "Enter a public query or skip the lookup.",
                        web::Lookup::Page => "Enter a public webpage URL or skip the lookup.",
                    }),
                ),
            },
        };
        let scope = context.scope;
        self.scheduler_firing_web.rounds.entries.insert(
            key,
            Entry {
                ticket: ticket.clone(),
                review: web::State::Draft(scope.clone(), draft, lookup),
                revision: 0,
                scope,
                page_allowed,
                credentials,
                vault: self.vault_change_revision(),
                deadline: Instant::now() + TIMEOUT,
                status: status.map(str::to_owned),
                model_tool: None,
            },
        );
        Ok(Task::none())
    }

    pub(in crate::workspace::scheduler_access) fn begin_model_round_web(
        &mut self,
        ticket: &Ticket,
        proposal: Arc<WebProposal>,
    ) -> Task<Message> {
        if let Err(error) =
            self.install_model_round_web(ticket, &proposal, None, Instant::now() + TIMEOUT)
        {
            proposal.reject(&error);
        }
        Task::none()
    }

    pub(in crate::workspace::scheduler_access) fn begin_read_only_model_web(
        &mut self,
        review: ModelWebReview,
    ) -> Task<Message> {
        let result = (|| {
            let request = &review.authority.scope.request;
            let index = request
                .action
                .targets()
                .position(|target| review.authority.scope.evidence_targets == [target.id])
                .ok_or(CHANGED)?;
            let ticket = Ticket::new(Key::new(request.id, index), 1)?;
            self.install_model_round_web(
                &ticket,
                &review.proposal,
                Some(review.authority),
                review.deadline,
            )
        })();
        if let Err(error) = result {
            review.proposal.reject(&error);
        }
        Task::none()
    }

    fn install_model_round_web(
        &mut self,
        ticket: &Ticket,
        proposal: &Arc<WebProposal>,
        read_only: Option<AdvisoryWebAuthority>,
        deadline: Instant,
    ) -> Result<(), String> {
        if Instant::now() >= deadline {
            return Err(EXPIRED.into());
        }
        if proposal.closed()
            || self
                .scheduler_firing_web
                .rounds
                .entries
                .contains_key(&ticket.key())
        {
            return Err(CHANGED.into());
        }
        let request = Arc::clone(self.model_round_request(ticket, read_only.as_ref())?);
        let context = self.scheduled_model_web_context(&request)?;
        let credentials = Credentials::capture(&context.model, &context.scope, true)?;
        let mask = self.unattended_web_mask(&context);
        let clean = |value: &str| unattended::public_text(value, &credentials, &mask);
        require_clean_call(&proposal.call, &clean)?;
        clean(&proposal.assistant_context)?;
        let vault = self.vault_change_revision();
        let operation = proposal.operation();
        if proposal.limit_reached
            || !self.scheduler_firing_web.model_web_available(ticket.key())
            || operation.is_none()
        {
            let result = if operation.is_none() {
                web_error_result(
                    "unsupported_or_invalid_tool",
                    "The requested tool or arguments are unsupported.",
                )
            } else {
                web_error_result(
                    "tool_round_limit",
                    "The reviewed web lookup budget has been reached.",
                )
            };
            proposal.resolve(ApprovedWebResult::new(result, move || {
                require_advisory_web_authority(read_only.as_ref(), &vault)?;
                check_worker(
                    &request,
                    &vault,
                    deadline,
                    &context.connection_path,
                    &context.trust,
                )?;
                context.scope.require_current()?;
                credentials.check(&context.model)
            }));
            return Ok(());
        }
        let (operation, input) = operation.expect("supported model web operation");
        let lookup = match operation {
            WebOperation::Search => web::Lookup::Search,
            WebOperation::Page => web::Lookup::Page,
        };
        let (draft, status) = match reviewed_public_input(lookup, &input, &credentials, &mask) {
            Ok(input) => (input, None),
            Err(error) => (String::new(), Some(error)),
        };
        let scope = context.scope;
        let revision = self.allocate();
        self.scheduler_firing_web.rounds.entries.insert(
            ticket.key(),
            Entry {
                ticket: ticket.clone(),
                review: web::State::Draft(scope.clone(), draft, lookup),
                revision,
                scope,
                page_allowed: true,
                credentials,
                vault,
                deadline,
                status,
                model_tool: Some(ModelTool {
                    proposal: Arc::clone(proposal),
                    approved_input: None,
                    read_only,
                }),
            },
        );
        Ok(())
    }

    fn model_round_request<'a>(
        &'a self,
        ticket: &Ticket,
        read_only: Option<&'a AdvisoryWebAuthority>,
    ) -> Result<&'a Arc<RunRequest<SshCommand>>, String> {
        if let Some(authority) = read_only {
            let scope = &authority.scope;
            let request = &scope.request;
            if request.id != ticket.run()
                || !request
                    .action
                    .targets()
                    .nth(ticket.target_index())
                    .is_some_and(|target| scope.evidence_targets == [target.id])
                || !request.action.agent.as_ref().is_some_and(|action| {
                    action.kind == agent::Kind::Swarm
                        && action.probe == agent::Probe::SwarmReadOnly
                        && action.approval.as_ref().is_some_and(|approval| {
                            approval
                                .advisory
                                .as_ref()
                                .is_some_and(agent::AdvisoryGrant::is_guided)
                        })
                })
            {
                return Err(CHANGED.into());
            }
            authority.require_policy(&self.clipboard.policy)?;
            authority.require_policy(&ClipboardPolicy::load())?;
            self.scheduler_agent_resend_authority(scope)
                .and_then(|()| self.require_read_only_environment_web(scope))
                .inspect_err(|&reason| authority.block(reason))?;
            return Ok(request);
        }
        let review = self.current_firing(ticket, Phase::Generating)?;
        Ok(review.request())
    }

    pub(in crate::workspace::scheduler_access) fn scheduled_model_web_context(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Context, String> {
        let baseline = self
            .scheduler_firing_web
            .entries
            .get(&request.id)
            .filter(|entry| entry.approved.is_some())
            .ok_or(CHANGED)?;
        let grant = unattended::grant(request)
            .filter(|grant| grant.allows_model_tools())
            .ok_or(CHANGED)?;
        let context = self.current_firing_web_context(baseline, request)?;
        context
            .scope
            .require_unattended_profile(grant, context.redactor.redaction_policy())?;
        Ok(context)
    }

    fn swarm_round_web_context(&self, ticket: &Ticket) -> Result<Context, String> {
        let request = self.swarm_round_web_request(ticket).ok_or(CHANGED)?;
        let scheduler = self
            .scheduler
            .as_ref()
            .filter(|scheduler| scheduler.active_agent_run_approved(request))
            .ok_or(CHANGED)?;
        if !scheduler.agent_predecessors_current(request, SystemTime::now()) {
            return Err(
                "The job prerequisites changed before dispatch. Review the run again.".into(),
            );
        }
        let baseline = self
            .scheduler_firing_web
            .entries
            .get(&ticket.run())
            .filter(|entry| entry.approved.is_some())
            .ok_or(CHANGED)?;
        let grant = unattended::grant(request)
            .filter(|grant| grant.allows_round_lookup())
            .ok_or(CHANGED)?;
        let context = self.current_firing_web_context(baseline, request)?;
        context
            .scope
            .require_unattended_profile(grant, context.redactor.redaction_policy())?;
        Ok(context)
    }

    fn current_swarm_round_web(&self, entry: &Entry) -> Result<Context, String> {
        if Instant::now() >= entry.deadline {
            return Err(EXPIRED.into());
        }
        if !Weak::ptr_eq(&entry.vault, &self.vault_change_revision())
            || entry.vault.strong_count() == 0
        {
            if let Some(authority) = entry
                .model_tool
                .as_ref()
                .and_then(|tool| tool.read_only.as_ref())
            {
                authority.block(WEB_VAULT_CHANGED);
                return Err(WEB_VAULT_CHANGED.into());
            }
            return Err(CHANGED.into());
        }
        let context = if let Some(tool) = &entry.model_tool {
            if tool.proposal.closed() {
                return Err(CHANGED.into());
            }
            self.scheduled_model_web_context(
                self.model_round_request(&entry.ticket, tool.read_only.as_ref())?,
            )?
        } else {
            self.swarm_round_web_context(&entry.ticket)?
        };
        if context.scope != entry.scope {
            return Err(CHANGED.into());
        }
        entry.credentials.check(&context.model)?;
        Ok(context)
    }

    fn round_web_request<'a>(
        &'a self,
        entry: &'a Entry,
    ) -> Option<&'a Arc<RunRequest<SshCommand>>> {
        if let Some(tool) = &entry.model_tool {
            if let Some(authority) = &tool.read_only {
                return Some(&authority.scope.request);
            }
            self.model_round_request(&entry.ticket, None).ok()
        } else {
            self.swarm_round_web_request(&entry.ticket)
        }
    }

    pub(in crate::workspace::scheduler_access) fn expire_swarm_round_web(
        &mut self,
    ) -> Task<Message> {
        let expired = self
            .scheduler_firing_web
            .rounds
            .entries
            .values()
            .filter_map(|entry| {
                self.current_swarm_round_web(entry)
                    .err()
                    .map(|error| (entry.ticket.clone(), error))
            })
            .collect::<Vec<_>>();
        Task::batch(
            expired
                .into_iter()
                .map(|(ticket, error)| self.end_round_web(&ticket, error)),
        )
    }

    pub(super) fn swarm_round_web_view(&self) -> Option<Element<'_, Message>> {
        let entry = self
            .scheduler_firing_web
            .rounds
            .entries
            .values()
            .find(|entry| self.round_web_request(entry).is_some())?;
        let ticket = &entry.ticket;
        let id = ticket.run();
        let request = self.round_web_request(entry)?;
        let route = request.action.targets().nth(ticket.target_index())?;
        let name = self
            .scheduler
            .as_ref()?
            .automation_job_name(&request.job)
            .unwrap_or(request.job.as_str());
        let topic = unattended::grant(request)?.public_topic();
        let action = move |message| event(Event::View(ticket.clone(), entry.revision, message));
        let controls: Element<'_, Message> = match &entry.review {
            web::State::Draft(_, query, web::Lookup::Search) if !entry.page_allowed => column![
                text_input("Enter a public web search query", query)
                    .on_input(move |query| action(web::Message::Edit(query)))
                    .on_submit(action(web::Message::Review)),
                button("Review public query").on_press(action(web::Message::Review)),
            ]
            .spacing(8)
            .into(),
            _ if entry.model_tool.is_some() => entry.review.tool_view()?.map(action),
            _ => entry.review.view()?.map(action),
        };
        Some(
            column![
                row![
                    text(format!("Web research for {name} · run {id} · round {}", ticket.round()))
                        .size(19)
                        .width(Length::Fill),
                    entry.model_tool.is_none().then(|| button("Skip lookup").on_press(event(Event::Skip(ticket.clone(), entry.revision)))),
                ]
                .spacing(8),
                text(format!("Target {} · {}", ticket.target_index() + 1, route.target.host)).size(13),
                text(format!("Reviewed public topic · {topic}")).size(13),
                text("Review the public request and returned excerpts before the next model request for this target.").size(13),
                entry.status.as_ref().map(|status| text(status).size(13)),
                row![controls].height(Length::Fixed(250.0)),
            ]
            .spacing(8)
            .padding(12)
            .into(),
        )
    }

    pub(super) fn update_swarm_round_web(&mut self, message: Event) -> Task<Message> {
        match message {
            Event::Authorize(ticket, nonce, permit) => {
                if self.scheduler_firing_web.rounds.get(&ticket).is_some_and(|entry| {
                    matches!(&entry.review, web::State::Pending { ticket, .. } if *ticket == nonce)
                        && self.current_swarm_round_web(entry).is_ok()
                }) {
                    let _ = permit.try_send(());
                }
                permit.close();
                Task::none()
            }
            Event::Skip(ticket, revision) | Event::View(ticket, revision, web::Message::Close)
                if self.round_web_revision(&ticket) == Some(revision) =>
            {
                self.settle_swarm_round_web(&ticket, None)
            }
            Event::View(ticket, revision, message)
                if self.round_web_revision(&ticket) == Some(revision)
                    && matches!(message, web::Message::PageMode(_) | web::Message::Edit(_) | web::Message::Review | web::Message::Send | web::Message::Back | web::Message::Include) =>
            {
                self.update_swarm_round_web_review(&ticket, message)
            }
            Event::Worker(ticket, message @ web::Message::Finished(nonce, _))
                if self.scheduler_firing_web.rounds.get(&ticket).is_some_and(|entry| {
                    matches!(&entry.review, web::State::Pending { ticket, .. } if *ticket == nonce)
                }) =>
            {
                self.update_swarm_round_web_review(&ticket, message)
            }
            _ => Task::none(),
        }
    }

    #[cfg(all(test, debug_assertions, feature = "dialog-preview"))]
    pub(in crate::workspace::scheduler_access) fn model_web_ticket(
        &self,
        key: Key,
    ) -> Option<Ticket> {
        self.scheduler_firing_web
            .rounds
            .entries
            .get(&key)
            .map(|entry| entry.ticket.clone())
    }

    pub(in crate::workspace::scheduler_access) fn round_web_revision(
        &self,
        ticket: &Ticket,
    ) -> Option<u64> {
        self.scheduler_firing_web
            .rounds
            .get(ticket)
            .map(|entry| entry.revision)
    }

    fn settle_swarm_round_web(
        &mut self,
        ticket: &Ticket,
        evidence: Option<Arc<web::Evidence>>,
    ) -> Task<Message> {
        let entry = self
            .scheduler_firing_web
            .rounds
            .get(ticket)
            .expect("current web review");
        let current = self.current_swarm_round_web(entry);
        if entry.model_tool.is_some() {
            return match current {
                Ok(context) => self.settle_model_round_web(ticket, context, evidence),
                Err(error) => self.end_round_web(ticket, error),
            };
        }
        self.scheduler_firing_web.rounds.remove(ticket);
        match current {
            Ok(_) => self.finish_swarm_round_web(ticket, evidence),
            Err(error) => self.fail_swarm_round_web(ticket, error),
        }
    }

    fn end_round_web(&mut self, ticket: &Ticket, error: String) -> Task<Message> {
        if let Some(tool) = self
            .scheduler_firing_web
            .rounds
            .get(ticket)
            .and_then(|entry| entry.model_tool.as_ref())
        {
            tool.proposal.reject(&error);
            self.scheduler_firing_web.rounds.remove(ticket);
            Task::none()
        } else {
            self.scheduler_firing_web.rounds.remove(ticket);
            self.fail_swarm_round_web(ticket, error)
        }
    }

    fn settle_model_round_web(
        &mut self,
        ticket: &Ticket,
        context: Context,
        evidence: Option<Arc<web::Evidence>>,
    ) -> Task<Message> {
        let entry = self
            .scheduler_firing_web
            .rounds
            .get(ticket)
            .expect("current model review");
        let tool = entry.model_tool.as_ref().expect("current model tool");
        let result = (|| {
            let request = Arc::clone(self.model_round_request(ticket, tool.read_only.as_ref())?);
            let content = match &evidence {
                Some(evidence) => {
                    let mut excerpts = String::new();
                    evidence.append_to(&mut excerpts);
                    let input = tool.approved_input.as_ref().ok_or(CHANGED)?;
                    serde_json::json!({"status":"ok","approved_input":input,"evidence":excerpts})
                        .to_string()
                }
                None => {
                    web_error_result("user_declined", "The user declined this web-tool request.")
                }
            };
            if !tool.proposal.fits_result(&content) {
                return Err("The reviewed excerpts exceed the model's remaining context. Decline the request to continue.".into());
            }
            let vault = entry.vault.clone();
            let deadline = entry.deadline;
            let credentials = entry.credentials.clone();
            let read_only = tool.read_only.clone();
            let mask = self.unattended_web_mask(&context);
            ApprovedWebResult::new(content, move || {
                require_advisory_web_authority(read_only.as_ref(), &vault)?;
                check_worker(
                    &request,
                    &vault,
                    deadline,
                    &context.connection_path,
                    &context.trust,
                )?;
                context.scope.require_current()?;
                credentials.check(&context.model)?;
                if let Some(evidence) = &evidence {
                    evidence.require_mask(|text| {
                        credentials.require_clean(text)?;
                        mask(text)
                    })?;
                }
                Ok(())
            })
        })();
        match result {
            Ok(result) => {
                tool.proposal.resolve(Ok(result));
                self.scheduler_firing_web.rounds.remove(ticket);
                Task::none()
            }
            Err(error) if entry.review.included().is_some() => {
                let entry = self
                    .scheduler_firing_web
                    .rounds
                    .entries
                    .get_mut(&ticket.key())
                    .expect("current model review");
                if let Some((scope, results)) = entry.review.included() {
                    entry.review = web::State::Results(scope.clone(), Arc::clone(results));
                }
                entry.status = Some(error);
                Task::none()
            }
            Err(error) => self.end_round_web(ticket, error),
        }
    }

    fn update_swarm_round_web_review(
        &mut self,
        ticket: &Ticket,
        message: web::Message,
    ) -> Task<Message> {
        let current = self.current_swarm_round_web(
            self.scheduler_firing_web
                .rounds
                .get(ticket)
                .expect("current Swarm web review"),
        );
        let context = match current {
            Ok(context) => context,
            Err(error) => {
                return self.end_round_web(ticket, error);
            }
        };
        match self.apply_swarm_round_web(ticket, message, context) {
            Ok((task, Some(evidence))) => {
                Task::batch([task, self.settle_swarm_round_web(ticket, Some(evidence))])
            }
            Ok((task, None)) => task,
            Err(error) => {
                if let Some(entry) = self
                    .scheduler_firing_web
                    .rounds
                    .entries
                    .get_mut(&ticket.key())
                {
                    entry.status = Some(error);
                }
                Task::none()
            }
        }
    }

    fn apply_swarm_round_web(
        &mut self,
        ticket: &Ticket,
        message: web::Message,
        context: Context,
    ) -> Result<(Task<Message>, Option<Arc<web::Evidence>>), String> {
        let entry = self
            .scheduler_firing_web
            .rounds
            .get(ticket)
            .ok_or(CHANGED)?;
        if matches!(message, web::Message::PageMode(_)) && !entry.page_allowed {
            return Ok((Task::none(), None));
        }
        let request = Arc::clone(self.round_web_request(entry).ok_or(CHANGED)?);
        let mask = self.unattended_web_mask(&context);
        let wire_input = match (&entry.review, &message) {
            (web::State::Draft(_, value, lookup), web::Message::Review)
            | (web::State::Reviewed(_, value, lookup), web::Message::Send) => {
                if *lookup == web::Lookup::Page && !entry.page_allowed {
                    return Err(CHANGED.into());
                }
                let reviewed = reviewed_public_input(*lookup, value, &entry.credentials, &mask)?;
                if matches!(message, web::Message::Send) {
                    if &reviewed != value {
                        return Err(CHANGED.into());
                    }
                    Some(reviewed)
                } else {
                    None
                }
            }
            _ => None,
        };
        let credentials = entry.credentials.clone();
        let include_mask = self.unattended_web_mask(&context);
        let result_credentials = credentials.clone();
        let vault = entry.vault.clone();
        let deadline = entry.deadline;
        let include = matches!(message, web::Message::Include);
        let sending = wire_input.is_some();
        let model_input = if entry.model_tool.is_some() {
            if sending && !self.scheduler_firing_web.model_web_available(ticket.key()) {
                return Err("This target reached its web lookup budget.".into());
            }
            wire_input.clone()
        } else {
            None
        };
        let direct = entry.model_tool.is_some();
        let nonce = sending.then(|| self.allocate());
        let revision = direct.then(|| self.allocate());
        let (dispatches, authorizations) = async_channel::bounded(1);
        let entry = self
            .scheduler_firing_web
            .rounds
            .entries
            .get_mut(&ticket.key())
            .ok_or(CHANGED)?;
        let phase = review_phase(&entry.review);
        let finish_ticket = ticket.clone();
        let result_mask = move |text: &str| {
            result_credentials.require_clean(text)?;
            mask(text)
        };
        let update = entry.review.update(
            message,
            context.scope,
            result_mask,
            nonce,
            move || {
                let check = || {
                    check_worker(
                        &request,
                        &vault,
                        deadline,
                        &context.connection_path,
                        &context.trust,
                    )?;
                    credentials.check(&context.model)?;
                    if let Some(value) = &wire_input {
                        credentials.require_clean(value)?;
                    }
                    Ok::<(), String>(())
                };
                check()?;
                wait_for_permit(&dispatches, &request, deadline)?;
                check()
            },
            move |message| event(Event::Worker(finish_ticket, message)),
        );
        if review_phase(&entry.review) != phase {
            entry.revision = revision.unwrap_or(entry.revision + 1);
        }
        let task = update?;
        if let Some(input) = model_input {
            entry
                .model_tool
                .as_mut()
                .expect("current model tool")
                .approved_input = Some(input);
            *self
                .scheduler_firing_web
                .rounds
                .model_lookups
                .entry(ticket.key())
                .or_default() += 1;
        }
        entry.status = None;
        let evidence = if include && let Some((scope, results)) = entry.review.included() {
            let scope = scope.clone();
            let results = Arc::clone(results);
            let evidence = web::Evidence::new(&scope, &results).and_then(|evidence| {
                evidence.require_current()?;
                evidence.require_mask(|text| {
                    entry.credentials.require_clean(text)?;
                    include_mask(text)
                })?;
                Ok(Arc::new(evidence))
            });
            match evidence {
                Ok(evidence) => Some(evidence),
                Err(error) => {
                    entry.review = web::State::Results(scope, results);
                    return Err(error);
                }
            }
        } else {
            None
        };
        if let Some(nonce) = nonce {
            let ticket = ticket.clone();
            Ok((
                Task::run(authorizations, move |permit| {
                    event(Event::Authorize(ticket.clone(), nonce, permit))
                })
                .chain(task),
                evidence,
            ))
        } else {
            Ok((task, evidence))
        }
    }
}

fn require_advisory_web_authority(
    read_only: Option<&AdvisoryWebAuthority>,
    vault: &Weak<()>,
) -> Result<(), &'static str> {
    if let Some(authority) = read_only {
        if vault.strong_count() == 0 {
            authority.block(WEB_VAULT_CHANGED);
            return Err(WEB_VAULT_CHANGED);
        }
        authority.require_policy(&ClipboardPolicy::load())?;
    }
    Ok(())
}

fn reviewed_public_input(
    lookup: web::Lookup,
    value: &str,
    credentials: &Credentials,
    mask: &impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    match lookup {
        web::Lookup::Search => {
            if !agent::web_lookup::valid_input(value) {
                return Err("Enter a one-line public search query of up to 400 bytes.".into());
            }
            unattended::public_text(value, credentials, mask).map_err(
                |_| "Remove any private or masked text before reviewing the public query.",
            )?;
            Ok(value.to_owned())
        }
        web::Lookup::Page => {
            let page = PageUrl::parse(value.trim())?;
            let decoded = page.decoded()?;
            let private =
                "Remove any private or masked text before reviewing the public webpage URL.";
            unattended::public_text(page.as_str(), credentials, mask).map_err(|_| private)?;
            unattended::public_text(&decoded, credentials, mask).map_err(|_| private)?;
            Ok(page.as_str().to_owned())
        }
    }
}

fn event(message: Event) -> Message {
    Message::ScheduledFiringWeb(FiringWebEvent::Round(message))
}
