//! Reviewed model and SSH work for active scheduled Agent and Swarm targets.

use std::fmt;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_channel::{Sender, TryRecvError};
use iced::{Task, window};
use kortty_jobs::{RunId, RunOutcome, RunRequest};

use crate::ai_manager::{Configuration, ProfileRef, SkillUsageSurface};
use crate::job_executor::agent::{FiringPermit, FiringResult, FiringWork};
use crate::job_executor::automation::IterativeCapture;
use crate::job_executor::{self, Completion};
use crate::scheduled_agent_review::{EndReason, Key, Phase, Review, Ticket};
use crate::scheduler_window::{SshCommand, agent, firing};
use crate::terminal_logging::{RedactionSecrets, Redactor, Secrets};

use super::{ConnectionStore, Message, Workspace, profile, proposal_skills};
use agent::proposal::{Decision, Item, Prompt};

mod sequence;
pub(in crate::workspace) use sequence::Sequence;

#[cfg(test)]
mod tests;

const GENERATION_LIMIT: Duration = Duration::from_secs(150);
const REVIEW_LIMIT: Duration = Duration::from_secs(300);
const PERMIT_LIMIT: Duration = Duration::from_secs(5);
// Writers may stay open while a target awaits its next model or command review.
const MAX_TARGET_JOURNALS: usize = 4;

type FiringReview = Review<SshCommand, firing::Context, Vec<Item>>;
type Permit = Sender<Result<(), &'static str>>;

#[derive(Clone)]
pub struct Event(Progress);

#[derive(Clone)]
enum Progress {
    StartFailed(RunId, usize, u8, RunOutcome, String),
    ModelDispatch(Ticket, Sender<()>),
    Generated(Ticket, Result<Decision, String>),
    ExecutionPermit(Ticket, FiringPermit, Permit),
    Executed(Ticket, FiringResult),
    JournalClosed(RunId, usize, RunOutcome, String, bool),
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Scheduled Agent firing")
    }
}

impl Workspace {
    pub(super) fn firing_profile_revision(
        &self,
        reference: &ProfileRef,
    ) -> Result<[u8; 32], &'static str> {
        let config = self
            .ai_configuration()
            .ok_or("The reviewed model profile is unavailable.")?;
        let profile = agent::ProfileChoice::for_reference(&config, reference)
            .ok_or("The reviewed model profile is unavailable.")?;
        let revision = profile
            .revision(&config, &self.clipboard.policy.terminal_logging)
            .ok_or("The reviewed model profile is unavailable.")?;
        if Configuration::load().ok().and_then(|saved| {
            profile.revision(
                &saved,
                &crate::clipboard_policy::ClipboardPolicy::load().terminal_logging,
            )
        }) != Some(revision)
        {
            return Err("The saved model profile settings changed. Review again.");
        }
        Ok(revision)
    }

    pub(super) fn queue_firing_generation(
        &mut self,
        request: RunRequest<SshCommand>,
    ) -> Task<Message> {
        let cancelled = request.cancellation.is_requested();
        let parallelism = request
            .action
            .agent
            .as_ref()
            .map_or(1, agent::Action::swarm_parallelism);
        self.scheduler_agent_sequences
            .entry(request.id)
            .or_insert_with(|| Sequence::new(request, parallelism));
        if cancelled {
            self.expire_firing()
        } else {
            Task::none()
        }
    }

    pub(super) fn start_ready_firing(&mut self, ssh_room: bool) -> Option<Task<Message>> {
        let scheduler = self.scheduler.as_ref()?;
        let model_room = self.scheduler_firing_retry_at.is_none()
            && scheduler.firing_reviews.generating_count() < crate::local_ai::MAX_WORKERS;
        let journal_room = self.has_firing_journal_room();
        let (id, index) = self
            .scheduler_agent_sequences
            .iter()
            .find_map(|(&id, run)| {
                run.ready_indices()
                    .find(|&index| {
                        if run.needs_initial_probe(index) {
                            ssh_room && (journal_room || !run.needs_journal_slot(index))
                        } else {
                            model_room && (journal_room || !run.needs_model_journal_slot(index))
                        }
                    })
                    .map(|index| (id, index))
            })?;
        Some(self.start_firing_target(id, index))
    }

    fn start_firing_target(&mut self, id: RunId, index: usize) -> Task<Message> {
        let Some((_, request)) = self
            .scheduler_agent_sequences
            .get_mut(&id)
            .and_then(|run| run.claim(index))
        else {
            return Task::none();
        };
        let round = self.scheduler_agent_sequences[&id]
            .current_round(index)
            .expect("claimed firing");
        if request.cancellation.is_requested() {
            return Task::done(Message::ScheduledAgent(Event(Progress::StartFailed(
                id,
                index,
                round,
                RunOutcome::Cancelled,
                "Cancelled before the model request.".into(),
            ))));
        }
        match self.prepare_firing_generation(request, index, round) {
            Ok(task) => task,
            Err(error) if error == crate::local_ai::WORKERS_BUSY => {
                if let Some(run) = self.scheduler_agent_sequences.get_mut(&id) {
                    run.defer(index);
                }
                self.scheduler_firing_retry_at = Some(Instant::now() + Duration::from_secs(1));
                Task::none()
            }
            Err(error) => Task::done(Message::ScheduledAgent(Event(Progress::StartFailed(
                id,
                index,
                round,
                RunOutcome::Blocked,
                error,
            )))),
        }
    }

    pub(super) fn firing_secrets(
        &self,
        targets: &[agent::Target],
        store: &ConnectionStore,
    ) -> Result<(RedactionSecrets, Option<Weak<()>>), &'static str> {
        let secrets = Secrets::scheduled_targets(targets.len());
        let mut vault_revision = None;
        // This resolution only collects local mask inputs. Network authority is
        // checked independently for the exact target selected by the ticket.
        let local_mask_access = crate::clipboard_policy::ServerAccess::default();
        for target in targets {
            let reference = &target.reference;
            let connection = store
                .get(reference.id)
                .ok_or("A reviewed SSH connection is unavailable for masking.")?;
            let credentials = job_executor::Credentials::resolve_directory_browser(
                reference,
                connection,
                &local_mask_access,
                |id, kind, endpoint| {
                    let vault = self
                        .vault
                        .as_ref()
                        .ok_or(crate::vault_window::VAULT_LOCKED)?;
                    vault_revision = Some(
                        vault
                            .credential_revision()
                            .ok_or(crate::vault_window::VAULT_LOCKED)?,
                    );
                    vault.resolve(id, kind, endpoint)
                },
            )?;
            credentials.add_journal_secrets(&secrets);
        }
        let sealed = secrets
            .seal()
            .map_err(|_| "Could not verify the reviewed SSH secret mask.")?;
        Ok((sealed, vault_revision))
    }

    fn prepare_firing_generation(
        &mut self,
        request: Arc<RunRequest<SshCommand>>,
        target_index: usize,
        round: u8,
    ) -> Result<Task<Message>, String> {
        let scheduler = self
            .scheduler
            .as_ref()
            .ok_or("The scheduler is unavailable.")?;
        if !scheduler.active_agent_run_approved(&request) {
            return Err("Review the scheduled model request before running this job.".into());
        }
        let action = &request.action;
        let agent = action
            .agent
            .as_ref()
            .ok_or("The scheduled AI action is unavailable.")?;
        let approval = agent
            .approval
            .as_ref()
            .ok_or("The model request is not approved.")?;
        let directory = action
            .working_directory
            .as_deref()
            .ok_or("The reviewed working directory is unavailable.")?;
        let target = approval
            .targets
            .get(target_index)
            .ok_or("The reviewed SSH target is unavailable.")?;
        let context = self.scheduler_agent_context(&request)?;
        let (policy, saved_config) = self.scheduler_model_policy(
            agent.kind,
            &approval.profile,
            &context.transport,
            context.context_limit,
            &context.log_policy,
        )?;
        let store = ConnectionStore::open(context.connection_store.clone())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let (secrets, mut vault_revision) = self.firing_secrets(&approval.targets, &store)?;
        let revision = self.firing_profile_revision(&agent.profile)?;
        let mut prompt = Prompt::new(
            context.transport,
            context.context_limit,
            &agent.goal,
            directory,
            vec![target.reference.id],
            Redactor::for_journal(secrets.clone(), context.log_policy),
            context.mask,
        )?
        .bind_profile(revision);
        if agent.probe == agent::Probe::SwarmIterative {
            prompt = prompt.for_swarm();
            if approval.has_initial_swarm_probe() {
                let command = agent::initial_swarm_probe(directory)
                    .ok_or("The reviewed Swarm environment probe is unavailable.")?;
                prompt = prompt.with_swarm_probe(&command);
            }
        }
        if let Some(guidance) = approval.firing_skill_guidance_for(target_index) {
            prompt = prompt.with_reviewed_firing_skill_guidance(guidance)?;
            let ids = approval.firing_skill_ids();
            if !ids.is_empty() {
                prompt = prompt.with_firing_skill_classification(ids)?;
            }
        }
        if let Some(knowledge) = &approval.knowledge {
            super::proposal_knowledge::require_current_vault_mask(self.vault.as_ref(), knowledge)?;
            vault_revision = Some(self.vault_change_revision());
            prompt = prompt.with_knowledge(Some(Arc::clone(knowledge)), true)?;
        }
        let guidance = proposal_skills::require_firing_guidance(
            &saved_config,
            &store,
            &approval.targets,
            target,
            &prompt,
        )?;
        if !approval.generation_matches_for(target_index, &prompt) {
            return Err("The exact masked model request changed. Review this job again.".into());
        }
        let mut initial_probe = false;
        if prompt.is_swarm_iterative() {
            let sequence = self
                .scheduler_agent_sequences
                .get_mut(&request.id)
                .ok_or("The iterative Swarm run is unavailable.")?;
            let question = prompt
                .masked_question()
                .ok_or("The reviewed Swarm question is unavailable.")?;
            if !sequence.record_masked_question(&question) {
                return Err("The reviewed Swarm question changed.".into());
            }
            let prior = sequence
                .swarm_prior(target_index)
                .ok_or("The Swarm target history is unavailable.")?;
            let environment = sequence.swarm_environment(target_index);
            if prompt.swarm_probe_command().is_some() {
                if environment.is_some() {
                    prompt = prompt.with_swarm_round_and_probe(round, prior, environment)?;
                } else if round == 1 && prior.is_empty() {
                    initial_probe = true;
                } else {
                    return Err("The reviewed Swarm environment evidence is unavailable.".into());
                }
            } else {
                prompt = prompt.with_swarm_round(round, prior)?;
            }
        }
        context
            .targets
            .get(target_index)
            .ok_or("The reviewed SSH target is unavailable.")?
            .as_ref()
            .map_err(|&error| error)?;
        self.scheduler_reviewed_agent_target(target, &store, &context.trust, &policy)?;
        let agent_journal = (agent.probe == agent::Probe::PerFiring
            && action
                .automation_journal
                .as_ref()
                .is_some_and(|options| options.enabled))
        .then(|| {
            (
                target.clone(),
                context.connection_store.clone(),
                context.trust.clone(),
                vault_revision.clone(),
                secrets.clone(),
            )
        });
        let mut review_context = firing::Context {
            prompt: Arc::new(prompt),
            initial_probe,
            shared_secrets: Some(secrets),
            connection_store: context.connection_store,
            vault_revision,
            _cancel: None,
            skill_usage: None,
        };
        if initial_probe {
            let now = Instant::now();
            self.scheduler
                .as_mut()
                .expect("checked scheduler")
                .firing_reviews
                .begin_preapproved_probe(
                    request,
                    target_index,
                    review_context,
                    Vec::new(),
                    now,
                    now + REVIEW_LIMIT,
                )?;
            return Ok(Task::none());
        }
        let (dispatches, updates) = async_channel::bounded(1);
        review_context.skill_usage = saved_config.skill_usage(
            &guidance,
            &review_context.prompt.transport,
            SkillUsageSurface::Agent,
            false,
        );
        let guard = (review_context.prompt.firing_skill_guidance().is_some()
            || review_context.prompt.knowledge().is_some())
        .then(|| proposal_skills::FiringDispatchGuard {
            targets: approval.targets.clone(),
            target_index,
            profile: approval.profile.clone(),
            connection_path: review_context.connection_store.clone(),
            trust: context.trust,
            vault_revision: review_context.vault_revision.clone(),
            cancellation: request.cancellation.clone(),
        });
        let pending = Arc::clone(&review_context.prompt).start_firing(dispatches, {
            let action = Arc::clone(&request.action);
            move |prompt, policy| {
                action.require_installed_target_groups()?;
                guard
                    .as_ref()
                    .map_or(Ok(()), |guard| guard.verify(prompt, policy))
            }
        })?;
        review_context._cancel = Some(pending.cancel);
        let now = Instant::now();
        let ticket = self
            .scheduler
            .as_mut()
            .expect("checked scheduler")
            .firing_reviews
            .begin(
                Arc::clone(&request),
                target_index,
                round,
                review_context,
                now,
                now + GENERATION_LIMIT,
            )?;
        if let Some((target, store, trust, vault_revision, secrets)) = agent_journal
            && let Some(capture) = self.firing_journal_capture(&request, target_index)
        {
            capture.prepare_status_target(target, store, trust, vault_revision, secrets);
        }
        let dispatch_ticket = ticket.clone();
        Ok(Task::run(updates, move |permit| {
            Message::ScheduledAgent(Event(Progress::ModelDispatch(
                dispatch_ticket.clone(),
                permit,
            )))
        })
        .chain(Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The model worker ended without a proposal.".into()))
            },
            move |result| {
                Message::ScheduledAgent(Event(Progress::Generated(ticket.clone(), result)))
            },
        )))
    }

    fn current_firing(&self, ticket: &Ticket, phase: Phase) -> Result<&FiringReview, &'static str> {
        self.firing_authority(ticket, phase, false)
    }

    fn firing_authority(
        &self,
        ticket: &Ticket,
        phase: Phase,
        for_journal: bool,
    ) -> Result<&FiringReview, &'static str> {
        let scheduler = self
            .scheduler
            .as_ref()
            .ok_or("The scheduler is unavailable.")?;
        let review = if for_journal {
            scheduler.firing_reviews.by_ticket(ticket)
        } else {
            scheduler.firing_reviews.get(ticket, Instant::now())
        }
        .filter(|review| review.phase() == phase)
        .ok_or("The scheduled proposal is no longer current.")?;
        // Closing accepted output can outlive cancellation of SSH execution.
        let approved = if for_journal {
            self.automation_journal_capture_allowed()
                && review.request().action.supports_automation_journal()
                && review
                    .request()
                    .action
                    .automation_journal
                    .as_ref()
                    .is_some_and(|options| options.enabled)
                && scheduler.agent_run_approved(review.request())
        } else {
            !self.scheduler_draining && scheduler.active_agent_run_approved(review.request())
        };
        if self.exiting || !approved {
            return Err("The scheduled Agent run changed or ended.");
        }
        let action = review
            .request()
            .action
            .agent
            .as_ref()
            .ok_or("The scheduled Agent action changed.")?;
        let approved = action
            .approval
            .as_ref()
            .ok_or("The scheduled model request approval changed.")?;
        let context = review.context();
        if self
            .scheduler_agent_sequences
            .get(&ticket.run())
            .is_none_or(|run| {
                !run.is_current(ticket.target_index())
                    || run.current_round(ticket.target_index()) != Some(ticket.round())
            })
        {
            return Err("The scheduled proposal target or round changed.");
        }
        if context
            .vault_revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
        {
            return Err("The SSH credential vault changed or was locked.");
        }
        let prompt = &context.prompt;
        let target = approved
            .targets
            .get(ticket.target_index())
            .ok_or("The reviewed SSH target is unavailable.")?;
        if !approved.generation_matches_for(ticket.target_index(), prompt) {
            return Err("The exact masked model request changed. Review this job again.");
        }
        if Some(self.firing_profile_revision(&action.profile)?) != prompt.profile_revision() {
            return Err("The reviewed model profile settings changed. Review again.");
        }
        let (policy, saved_config) = self.scheduler_model_policy(
            action.kind,
            &approved.profile,
            &prompt.transport,
            prompt.context_limit,
            prompt.redaction_policy(),
        )?;
        let store = profile::connections_path()
            .ok()
            .filter(|path| path == &context.connection_store)
            .and_then(|path| ConnectionStore::open(path).ok())
            .ok_or("The saved SSH connections changed or are unavailable.")?;
        if !for_journal {
            review
                .request()
                .action
                .require_current_target_groups(&store)?;
        }
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        self.scheduler_reviewed_agent_target(target, &store, &trust, &policy)?;
        if !for_journal {
            proposal_skills::require_firing_guidance(
                &saved_config,
                &store,
                &approved.targets,
                target,
                prompt,
            )?;
        }
        if let Some(sealed) = &context.shared_secrets {
            let (current, _) = self.firing_secrets(&approved.targets, &store)?;
            if !sealed.matches(&current) {
                return Err("The reviewed SSH secret mask changed.");
            }
        } else if approved.targets.len() > 1 {
            return Err("The reviewed SSH secret mask is unavailable.");
        }
        Ok(review)
    }

    pub(in crate::workspace) fn update_firing(&mut self, event: Event) -> Task<Message> {
        match event.0 {
            Progress::StartFailed(id, index, round, outcome, detail) => {
                if self.scheduler.as_ref().is_some_and(|scheduler| {
                    scheduler
                        .firing_reviews
                        .by_key(Key::new(id, index))
                        .is_some()
                }) || self.scheduler_agent_sequences.get(&id).is_none_or(|run| {
                    !run.is_current(index) || run.current_round(index) != Some(round)
                }) {
                    return Task::none();
                }
                self.finish_firing_target(id, index, outcome, detail)
            }
            Progress::ModelDispatch(ticket, permit) => {
                if self.current_firing(&ticket, Phase::Generating).is_ok() {
                    let _ = permit.try_send(());
                }
                permit.close();
                Task::none()
            }
            Progress::Generated(ticket, result) => self.publish_firing(ticket, result),
            Progress::ExecutionPermit(ticket, purpose, permit) => {
                let _ = permit.try_send(
                    self.firing_authority(
                        &ticket,
                        Phase::Executing,
                        matches!(purpose, FiringPermit::Journal),
                    )
                    .map(|_| ()),
                );
                Task::none()
            }
            Progress::Executed(ticket, completion) => self.firing_executed(&ticket, completion),
            Progress::JournalClosed(id, index, outcome, detail, complete) => {
                if !self
                    .scheduler_agent_sequences
                    .get_mut(&id)
                    .is_some_and(|run| run.journal_closed(index, complete))
                {
                    return Task::none();
                }
                self.settle_firing_target(id, index, outcome, detail)
            }
        }
    }

    fn publish_firing(
        &mut self,
        ticket: Ticket,
        result: Result<Decision, String>,
    ) -> Task<Message> {
        let expiry = self.expire_firing();
        let review = match self.current_firing(&ticket, Phase::Generating) {
            Ok(review) => review,
            Err(error) => {
                return Task::batch([
                    expiry,
                    self.settle_firing(
                        &ticket,
                        Phase::Generating,
                        RunOutcome::Blocked,
                        error.into(),
                    ),
                ]);
            }
        };
        let mut skill_usage = review.context().skill_usage.clone();
        let accepted = result.is_ok();
        let task = match result {
            Ok(Decision::Commands { commands }) => {
                let target = review
                    .request()
                    .action
                    .targets()
                    .nth(ticket.target_index())
                    .expect("current reviewed target");
                let directory = review
                    .request()
                    .action
                    .working_directory
                    .as_deref()
                    .expect("validated directory");
                if commands.iter().any(|item| {
                    let command = agent::checked_proposal(&item.command, directory);
                    item.target != target.id
                        || crate::terminal_agent::wrap_ssh_command(directory, &command).is_err()
                }) {
                    return Task::batch([
                        expiry,
                        self.settle_firing(
                            &ticket,
                            Phase::Generating,
                            RunOutcome::Blocked,
                            "The proposed commands do not match the reviewed SSH target or limits."
                                .into(),
                        ),
                    ]);
                }
                let now = Instant::now();
                if !self.scheduler.as_mut().is_some_and(|scheduler| {
                    scheduler
                        .firing_reviews
                        .publish(&ticket, commands, now, now + REVIEW_LIMIT)
                }) {
                    skill_usage = None;
                }
                self.sync_scheduler_views();
                self.enqueue_jobs(Vec::new())
            }
            Ok(Decision::Done {}) => self.settle_firing(
                &ticket,
                Phase::Generating,
                RunOutcome::Success,
                "The model reported that no SSH commands were needed.".into(),
            ),
            Ok(Decision::Blocked {}) => self.settle_firing(
                &ticket,
                Phase::Generating,
                RunOutcome::Blocked,
                "The model reported that it could not propose SSH commands.".into(),
            ),
            Ok(Decision::SwarmDone { summary }) => {
                self.settle_firing(&ticket, Phase::Generating, RunOutcome::Success, summary)
            }
            Ok(Decision::SwarmBlocked { summary }) => {
                self.settle_firing(&ticket, Phase::Generating, RunOutcome::Blocked, summary)
            }
            Err(error) => self.settle_firing(
                &ticket,
                Phase::Generating,
                RunOutcome::Failed { exit_code: None },
                error,
            ),
        };
        if accepted && let Some(usage) = skill_usage {
            usage.record();
        }
        Task::batch([expiry, task])
    }

    pub(super) fn decide_firing(
        &mut self,
        id: window::Id,
        ticket: Ticket,
        approved: bool,
    ) -> Task<Message> {
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Task::none();
        };
        if !scheduler.can_decide_proposal(view, &ticket) || view.close_pending() {
            return self.expire_firing();
        }
        if !approved {
            let declined =
                scheduler
                    .firing_reviews
                    .by_ticket(&ticket)
                    .map_or(RunOutcome::Blocked, |review| {
                        if review.request().action.additional_connections.is_empty() {
                            RunOutcome::Cancelled
                        } else {
                            RunOutcome::Blocked
                        }
                    });
            return self.settle_firing(
                &ticket,
                Phase::AwaitingReview,
                declined,
                "The reviewed commands were declined.".into(),
            );
        }
        if let Err(error) = self.current_firing(&ticket, Phase::AwaitingReview) {
            return self.settle_firing(
                &ticket,
                Phase::AwaitingReview,
                RunOutcome::Blocked,
                error.into(),
            );
        }
        if !self
            .scheduler
            .as_mut()
            .expect("checked scheduler")
            .firing_reviews
            .authorize(&ticket, Instant::now())
        {
            return self.expire_firing();
        }
        self.sync_scheduler_views();
        self.enqueue_jobs(Vec::new())
    }

    fn has_firing_journal_room(&self) -> bool {
        !self.automation_journal_capture_allowed()
            || self
                .scheduler_agent_sequences
                .values()
                .map(Sequence::journal_slot_count)
                .sum::<usize>()
                < MAX_TARGET_JOURNALS
    }

    fn firing_journal_capture(
        &mut self,
        request: &RunRequest<SshCommand>,
        index: usize,
    ) -> Option<IterativeCapture> {
        let plan = self
            .scheduler_agent_sequences
            .get(&request.id)?
            .journal_plan_needed()
            .then(|| self.scheduler_automation_capture(request));
        let capture_allowed = self.automation_journal_capture_allowed();
        let run = self.scheduler_agent_sequences.get_mut(&request.id)?;
        if let Some(plan) = plan {
            run.set_journal_plan(plan);
        }
        if capture_allowed {
            run.capture_target(index)
        } else {
            None
        }
    }

    pub(super) fn start_firing_execution(&mut self) -> Option<Task<Message>> {
        let now = Instant::now();
        let journal_room = self.has_firing_journal_room();
        let ticket = self
            .scheduler
            .as_ref()?
            .firing_reviews
            .next_authorized(now, |key| {
                self.scheduler_agent_sequences
                    .get(&key.run())
                    .is_some_and(|run| journal_room || !run.needs_journal_slot(key.target_index()))
            })?;
        let target_index = ticket.target_index();
        let review = self.scheduler.as_ref()?.firing_reviews.by_ticket(&ticket)?;
        let probe_prompt = review
            .context()
            .initial_probe
            .then(|| Arc::clone(&review.context().prompt));
        let mut shared_secrets = None;
        let context = self
            .current_firing(&ticket, Phase::Authorized)
            .and_then(|review| {
                shared_secrets = review.context().shared_secrets.clone();
                let mut context = self.scheduler_agent_context(review.request())?;
                if review.request().action.supports_automation_journal() {
                    context
                        .target_vault_revisions
                        .resize(context.targets.len(), None);
                    let revision = context
                        .target_vault_revisions
                        .get_mut(target_index)
                        .ok_or("The scheduled proposal target changed.")?;
                    *revision = review.context().vault_revision.clone();
                }
                Ok(context)
            });
        let Some(execution) = self
            .scheduler
            .as_mut()?
            .firing_reviews
            .claim(&ticket, Instant::now())
        else {
            return Some(self.expire_firing());
        };
        let (permits, requests) = async_channel::bounded(1);
        let cancel = execution.request.cancellation.clone();
        let id = ticket.run();
        let capture = context
            .is_ok()
            .then(|| self.firing_journal_capture(&execution.request, target_index))
            .flatten();
        let completion_ticket = ticket.clone();
        let work = if let Some(prompt) = probe_prompt {
            FiringWork::InitialProbe(prompt)
        } else {
            FiringWork::Commands(
                execution
                    .proposal
                    .into_iter()
                    .map(|item| item.command)
                    .collect(),
            )
        };
        let done = job_executor::agent::spawn_firing(
            execution.request,
            target_index,
            context,
            shared_secrets,
            capture,
            work,
            move |purpose| {
                let (send, reply) = async_channel::bounded(1);
                permits
                    .try_send((ticket.clone(), purpose, send))
                    .map_err(|_| "Scheduled execution authority is unavailable.")?;
                let deadline = Instant::now() + PERMIT_LIMIT;
                let cancelled =
                    || matches!(purpose, FiringPermit::Execute) && cancel.is_requested();
                loop {
                    if cancelled() || Instant::now() >= deadline {
                        return Err("Scheduled execution authority expired or was cancelled.");
                    }
                    match reply.try_recv() {
                        Ok(result) => {
                            return if cancelled() || Instant::now() >= deadline {
                                Err("Scheduled execution authority expired or was cancelled.")
                            } else {
                                result
                            };
                        }
                        Err(TryRecvError::Closed) => {
                            return Err("Scheduled execution authority is unavailable.");
                        }
                        Err(TryRecvError::Empty) => {}
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            },
        );
        self.scheduler_workers += 1;
        Some(
            Task::run(requests, |(ticket, purpose, permit)| {
                Message::ScheduledAgent(Event(Progress::ExecutionPermit(ticket, purpose, permit)))
            })
            .chain(Task::future(async move {
                let result = done.recv().await.unwrap_or_else(|_| FiringResult {
                    completion: Completion {
                        id,
                        outcome: RunOutcome::Failed { exit_code: None },
                        detail: Some("The scheduled Agent worker ended without a result.".into()),
                        recording: None,
                        swarm_report: None,
                    },
                    evidence: None,
                });
                Message::ScheduledAgent(Event(Progress::Executed(completion_ticket, result)))
            })),
        )
    }

    pub(in crate::workspace) fn expire_firing(&mut self) -> Task<Message> {
        let Some(scheduler) = &mut self.scheduler else {
            return Task::none();
        };
        let ended = scheduler.firing_reviews.settle_inactive(Instant::now());
        let open_cancelled = self
            .scheduler_agent_sequences
            .iter_mut()
            .flat_map(|(&id, run)| {
                run.claim_cancelled_journals()
                    .into_iter()
                    .map(move |index| (id, index))
            })
            .collect::<Vec<_>>();
        let cancelled = self
            .scheduler_agent_sequences
            .values_mut()
            .filter_map(Sequence::cancel_ready)
            .collect::<Vec<_>>();
        let mut tasks = Vec::new();
        for completion in cancelled {
            tasks.push(self.complete_firing_run(completion));
        }
        for (id, index) in open_cancelled {
            tasks.push(self.finish_firing_target(
                id,
                index,
                RunOutcome::Cancelled,
                "Cancelled before the next Swarm round.".into(),
            ));
        }
        for entry in ended {
            let (outcome, detail) = match entry.reason {
                EndReason::Cancelled => (RunOutcome::Cancelled, "Cancelled before SSH execution."),
                EndReason::Expired if entry.phase == Phase::Generating => {
                    (RunOutcome::Blocked, "The model proposal deadline expired.")
                }
                EndReason::Expired => (
                    RunOutcome::Blocked,
                    "The command review expired before SSH execution.",
                ),
            };
            tasks.push(self.finish_firing_target(
                entry.ticket.run(),
                entry.ticket.target_index(),
                outcome,
                detail.into(),
            ));
        }
        Task::batch(tasks)
    }

    fn settle_firing(
        &mut self,
        ticket: &Ticket,
        phase: Phase,
        outcome: RunOutcome,
        detail: String,
    ) -> Task<Message> {
        if self
            .scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.firing_reviews.settle(ticket, phase))
            .is_none()
        {
            return Task::none();
        }
        self.finish_firing_target(ticket.run(), ticket.target_index(), outcome, detail)
    }

    fn firing_executed(&mut self, ticket: &Ticket, result: FiringResult) -> Task<Message> {
        let FiringResult {
            completion,
            evidence,
        } = result;
        let initial_probe = self
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.firing_reviews.by_ticket(ticket))
            .is_some_and(|review| review.context().initial_probe);
        if ticket.run() != completion.id
            || self
                .scheduler
                .as_mut()
                .and_then(|scheduler| scheduler.firing_reviews.settle(ticket, Phase::Executing))
                .is_none()
        {
            return Task::none();
        }
        self.scheduler_workers = self.scheduler_workers.saturating_sub(1);
        let index = ticket.target_index();
        if initial_probe {
            if completion.outcome == RunOutcome::Success
                && let Some(evidence) = evidence
                && let Ok([evidence]) = <[_; 1]>::try_from(evidence)
                && let Some(run) = self.scheduler_agent_sequences.get_mut(&ticket.run())
                && run.accept_initial_probe(index, evidence)
            {
                self.sync_scheduler_views();
                return self.enqueue_jobs(Vec::new());
            }
            let outcome = if completion.outcome == RunOutcome::Success {
                RunOutcome::Blocked
            } else {
                completion.outcome
            };
            let detail = if completion.outcome == RunOutcome::Success {
                "The reviewed environment probe produced no usable masked evidence.".into()
            } else {
                completion
                    .detail
                    .unwrap_or_else(|| "The reviewed environment probe did not finish.".into())
            };
            return self.finish_firing_target(ticket.run(), index, outcome, detail);
        }
        let iterative = self
            .scheduler_agent_sequences
            .get(&ticket.run())
            .is_some_and(|run| run.swarm_prior(index).is_some());
        if matches!(
            completion.outcome,
            RunOutcome::Success | RunOutcome::Failed { .. }
        ) && let Some(evidence) = evidence
            && let Some(run) = self.scheduler_agent_sequences.get_mut(&ticket.run())
            && run.continue_swarm(index, ticket.round(), evidence)
        {
            self.sync_scheduler_views();
            return self.enqueue_jobs(Vec::new());
        }
        if iterative && completion.outcome == RunOutcome::Success {
            return self.finish_firing_target(
                ticket.run(),
                index,
                RunOutcome::Blocked,
                "The reviewed commands finished without usable masked continuation evidence."
                    .into(),
            );
        }
        self.finish_firing_target(
            ticket.run(),
            index,
            completion.outcome,
            completion.detail.unwrap_or_default(),
        )
    }

    fn finish_firing_target(
        &mut self,
        id: RunId,
        index: usize,
        outcome: RunOutcome,
        detail: String,
    ) -> Task<Message> {
        let journal_allowed = !self.exiting && self.journal_allowed();
        let Some(run) = self.scheduler_agent_sequences.get_mut(&id) else {
            return Task::none();
        };
        if !run.is_current(index) {
            return Task::none();
        }
        let (outcome, detail) = if run.cancellation_requested() && outcome != RunOutcome::Cancelled
        {
            (
                RunOutcome::Cancelled,
                "Cancelled before another scheduler step.".into(),
            )
        } else {
            (outcome, detail)
        };
        if let Some(capture) = run.begin_journal_close(index) {
            if !journal_allowed
                || self
                    .scheduler
                    .as_ref()
                    .is_none_or(|scheduler| !scheduler.agent_run_approved(run.request()))
            {
                capture.stop();
            }
            let close = Task::future(async move {
                let complete = capture.finish(outcome).await;
                Message::ScheduledAgent(Event(Progress::JournalClosed(
                    id, index, outcome, detail, complete,
                )))
            });
            return Task::batch([close, self.enqueue_jobs(Vec::new())]);
        }
        self.settle_firing_target(id, index, outcome, detail)
    }

    fn settle_firing_target(
        &mut self,
        id: RunId,
        index: usize,
        outcome: RunOutcome,
        detail: String,
    ) -> Task<Message> {
        match self
            .scheduler_agent_sequences
            .get_mut(&id)
            .and_then(|run| run.finish(index, outcome, detail))
        {
            None => Task::none(),
            Some(None) => {
                self.sync_scheduler_views();
                self.enqueue_jobs(Vec::new())
            }
            Some(Some(completion)) => self.complete_firing_run(completion),
        }
    }

    fn complete_firing_run(&mut self, mut completion: Completion) -> Task<Message> {
        let (plan, warning) = self
            .scheduler_agent_sequences
            .remove(&completion.id)
            .map_or((None, None), |mut run| run.take_journals());
        if let Some(warning) = warning {
            completion.append_detail(&warning);
        }
        let journals = if let Some(plan) = plan {
            self.automation_scheduler_finished(completion.id);
            plan.take_completed()
        } else {
            Vec::new()
        };
        self.complete_scheduler_run(completion, journals)
    }
}
