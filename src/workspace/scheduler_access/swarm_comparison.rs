//! Reviews and dispatches the optional comparison for one scheduled Swarm run.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_channel::{Sender, TryRecvError};
use iced::{Task, window};
use kortty_jobs::{Cancellation, RunId, RunOutcome, RunRequest, valid_swarm_comparison};

use crate::clipboard_policy::AiMemoryPolicy;
use crate::job_executor::Completion;
use crate::local_ai;
use crate::scheduler_window::agent::knowledge;
use crate::scheduler_window::agent::provider_mask::ProviderMask;
use crate::terminal_agent::swarm::comparison::{
    Report, ScheduledGrant, ScheduledScope, ScheduledTemplate,
};
use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::mask;

use super::{
    ClipboardPolicy, Configuration, ConnectionStore, Message, SshCommand, Workspace, agent,
    job_executor, profile,
};

#[cfg(test)]
mod tests;

const CHANGED: &str = "The scheduled Swarm comparison review changed or is unavailable.";
const UNAVAILABLE: &str = "AI comparison unavailable. Target results were retained.";
const PERMIT_LIMIT: Duration = Duration::from_secs(5);
const WORKER_WAIT_LIMIT: Duration = Duration::from_secs(120);

type Permit = Sender<Result<(), &'static str>>;

#[derive(Clone)]
pub struct Event(Progress);

#[derive(Clone)]
enum Progress {
    Permit(RunId, Permit),
    Finished(RunId, Arc<Result<Report, String>>),
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Scheduled Swarm comparison")
    }
}

#[derive(Default)]
pub(in crate::workspace) struct Runs {
    admitted: BTreeMap<RunId, RunRequest<SshCommand>>,
    pending: BTreeMap<RunId, Pending>,
}

impl Runs {
    pub(super) fn remember(&mut self, request: &RunRequest<SshCommand>) {
        if request.action.agent.as_ref().is_some_and(|action| {
            action.kind == agent::Kind::Swarm
                && action
                    .approval
                    .as_ref()
                    .is_some_and(|approval| approval.comparison.is_some())
        }) {
            self.admitted.insert(
                request.id,
                RunRequest {
                    id: request.id,
                    job: request.job.clone(),
                    action: Arc::clone(&request.action),
                    trigger: request.trigger,
                    started_at: request.started_at,
                    cancellation: request.cancellation.clone(),
                },
            );
        }
    }

    pub(super) fn take_admitted(&mut self, id: RunId) -> Option<RunRequest<SshCommand>> {
        self.admitted.remove(&id)
    }

    pub(super) fn needs_tick(&self) -> bool {
        !self.pending.is_empty()
    }
}

struct Pending {
    request: RunRequest<SshCommand>,
    completion: Completion,
    session_journals: Vec<String>,
    dispatch_deadline: Instant,
    worker: Option<Worker>,
}

struct Worker {
    authority: Arc<Authority>,
    _cancel: local_ai::Cancel,
}

struct Authority {
    profile: agent::ProfileChoice,
    transport: local_ai::Profile,
    provider_mask: ProviderMask,
    context_limit: usize,
    profile_root: PathBuf,
    targets: Vec<agent::Target>,
    goal: String,
    grant: ScheduledGrant,
    redactor: Redactor,
    vault_revision: Option<Weak<()>>,
    memory_policy: AiMemoryPolicy,
    knowledge: Option<Arc<knowledge::Grant>>,
}

fn prepare_template(
    config: &Configuration,
    scope: ScheduledScope<'_>,
    goal: &str,
    root: &std::path::Path,
    memory_policy: &AiMemoryPolicy,
    redactor: &Redactor,
    knowledge: Option<&knowledge::Grant>,
) -> Result<ScheduledTemplate, String> {
    let memory = config.load_memory(Some(root), memory_policy, None, false, |text| {
        mask::prompt(redactor, text)
    })?;
    ScheduledTemplate::prepare_with_knowledge(
        scope,
        goal,
        memory.as_ref().map(|memory| memory.text()),
        knowledge.map(knowledge::Grant::context),
        redactor,
        mask::prompt,
    )
}

impl Authority {
    fn check_disk(&self, cancellation: &Cancellation) -> Result<(), String> {
        let policy = ClipboardPolicy::load();
        job_executor::agent::require_policy(&policy, agent::Kind::Swarm)?;
        if policy.ai_memory != self.memory_policy
            || !self
                .redactor
                .redaction_policy()
                .equivalent_redactions(&policy.terminal_logging)
            || self
                .vault_revision
                .as_ref()
                .is_some_and(|revision| revision.upgrade().is_none())
            || profile::root().ok().as_ref() != Some(&self.profile_root)
        {
            return Err(CHANGED.into());
        }
        let config = Configuration::load_from(&self.profile_root).map_err(|_| CHANGED)?;
        let revision = self.grant.profile_revision();
        if !config.enabled()
            || self.profile.revision(&config, &policy.terminal_logging) != Some(revision)
            || !self
                .profile
                .transport(&config)
                .is_ok_and(|(transport, limit)| {
                    transport == self.transport && limit == self.context_limit
                })
        {
            return Err(CHANGED.into());
        }
        self.provider_mask.check(&self.transport)?;
        if let Some(knowledge) = &self.knowledge {
            knowledge.require_current(&config, &self.profile.id, &self.profile_root, || {
                if cancellation.is_requested()
                    || self
                        .vault_revision
                        .as_ref()
                        .is_some_and(|revision| revision.upgrade().is_none())
                {
                    return Err(CHANGED.into());
                }
                Ok(())
            })?;
        }
        let store = ConnectionStore::open(profile::connections_path_at(&self.profile_root))
            .map_err(|_| CHANGED)?;
        let trust = crate::ssh_connection::trust_store().map_err(|_| CHANGED)?;
        if self.targets.iter().any(|target| {
            let reference = &target.reference;
            policy
                .server_access()
                .first_blocked(reference.target.servers(reference.jump.as_ref()))
                .is_some()
                || store
                    .get(reference.id)
                    .and_then(|connection| {
                        agent::Target::capture(reference, connection, &trust).ok()
                    })
                    .as_ref()
                    != Some(target)
        }) {
            return Err(CHANGED.into());
        }
        let ids: Vec<_> = self
            .targets
            .iter()
            .map(|target| target.reference.id)
            .collect();
        let template = prepare_template(
            &config,
            ScheduledScope {
                profile: &self.transport,
                profile_revision: revision,
                context_limit: self.context_limit,
                targets: &ids,
            },
            &self.goal,
            &self.profile_root,
            &policy.ai_memory,
            &self.redactor,
            self.knowledge.as_deref(),
        )?;
        if !self.grant.matches(&template) {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}

impl Workspace {
    fn prepare_scheduler_comparison(
        &self,
        profile: &agent::ProfileChoice,
        targets: &[agent::Target],
        goal: &str,
        knowledge: Option<&Arc<knowledge::Grant>>,
    ) -> Result<(ScheduledTemplate, Authority), String> {
        let installed = ClipboardPolicy::load();
        let config = self.scheduler_agent_policy(agent::Kind::Swarm, &installed)?;
        let (transport, context_limit) = profile.transport(&config)?;
        let (policy, saved) = self.scheduler_model_policy(
            agent::Kind::Swarm,
            profile,
            &transport,
            context_limit,
            &self.clipboard.policy.terminal_logging,
        )?;
        if self.clipboard.policy.ai_memory != policy.ai_memory {
            return Err(CHANGED.into());
        }
        let revision = self.firing_profile_revision(&profile.id)?;
        let root = saved.saved_root().ok_or(CHANGED)?.to_owned();
        let store =
            ConnectionStore::open(profile::connections_path_at(&root)).map_err(|_| CHANGED)?;
        let trust = crate::ssh_connection::trust_store().map_err(|_| CHANGED)?;
        for target in targets {
            self.scheduler_reviewed_agent_target(target, &store, &trust, &policy)?;
        }
        let (secrets, mut vault_revision) = self.firing_secrets(targets, &store)?;
        if let Some(knowledge) = knowledge {
            super::proposal_knowledge::require_current_vault_mask(self.vault.as_ref(), knowledge)?;
            vault_revision = Some(self.vault_change_revision());
        }
        let provider_mask = ProviderMask::capture(&transport);
        let redactor = provider_mask.apply(&Redactor::for_journal(
            secrets,
            self.clipboard.policy.terminal_logging.clone(),
        ))?;
        let ids: Vec<_> = targets.iter().map(|target| target.reference.id).collect();
        let template = prepare_template(
            &saved,
            ScheduledScope {
                profile: &transport,
                profile_revision: revision,
                context_limit,
                targets: &ids,
            },
            goal,
            &root,
            &policy.ai_memory,
            &redactor,
            knowledge.map(Arc::as_ref),
        )?;
        let authority = Authority {
            profile: profile.clone(),
            transport,
            provider_mask,
            context_limit,
            profile_root: root,
            targets: targets.to_vec(),
            goal: goal.into(),
            grant: template.grant(),
            redactor,
            vault_revision,
            memory_policy: policy.ai_memory,
            knowledge: knowledge.cloned(),
        };
        Ok((template, authority))
    }

    pub(super) fn review_scheduler_swarm_comparison(
        &mut self,
        id: window::Id,
        ticket: &str,
        approving: bool,
    ) -> Task<Message> {
        let result = (|| {
            let (scheduler, view) = self
                .scheduler
                .as_ref()
                .zip(self.scheduler_views.get(&id))
                .ok_or(CHANGED)?;
            let Some(authority) =
                scheduler.swarm_comparison_review_authority(view, ticket, approving)?
            else {
                return Ok(Task::none());
            };
            if !self.scheduler_proposal_current(id, None)? {
                return Err(CHANGED.into());
            }
            let review = scheduler.begin_agent_review(view)?;
            let action = review.job.action().agent.as_ref().ok_or(CHANGED)?;
            let (template, _) = self.prepare_scheduler_comparison(
                authority.profile,
                authority.targets,
                &action.goal,
                authority.knowledge,
            )?;
            let view = self.scheduler_views.get_mut(&id).ok_or(CHANGED)?;
            if approving {
                scheduler.approve_swarm_comparison(view, ticket, &template)
            } else {
                scheduler.finish_swarm_comparison_review(view, ticket, template)
            }
        })();
        match result {
            Ok(task) => task.map(move |message| Message::Scheduler(id, message)),
            Err(error) => {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(error);
                }
                Task::none()
            }
        }
    }

    pub(super) fn scheduler_swarm_comparison_current(&self, id: window::Id) -> Result<(), String> {
        let Some((scheduler, view)) = self.scheduler.as_ref().zip(self.scheduler_views.get(&id))
        else {
            return Ok(());
        };
        let Some(authority) = scheduler.proposal_authority(view)? else {
            return Ok(());
        };
        let Some(grant) = authority.comparison else {
            return Ok(());
        };
        let review = scheduler.begin_agent_review(view)?;
        let reviewed = review.job.action();
        reviewed.require_installed_target_groups()?;
        let action = reviewed.agent.as_ref().ok_or(CHANGED)?;
        let (template, _) = self.prepare_scheduler_comparison(
            authority.profile,
            authority.targets,
            &action.goal,
            authority.knowledge,
        )?;
        if !grant.matches(&template) {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    pub(super) fn compare_scheduled_swarm(
        &mut self,
        request: RunRequest<SshCommand>,
        completion: Completion,
        session_journals: Vec<String>,
    ) -> Task<Message> {
        let id = request.id;
        self.scheduler_swarm_comparisons.pending.insert(
            id,
            Pending {
                request,
                completion,
                session_journals,
                dispatch_deadline: Instant::now() + WORKER_WAIT_LIMIT,
                worker: None,
            },
        );
        let task = self.start_swarm_comparison(id);
        if self.scheduler_swarm_comparisons.pending.contains_key(&id) {
            Task::batch([task, self.enqueue_jobs(Vec::new())])
        } else {
            task
        }
    }

    fn swarm_comparison_active(&self, request: &RunRequest<SshCommand>) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.active_swarm_comparison_approved(request))
    }

    fn check_swarm_comparison_authority(
        &self,
        request: &RunRequest<SshCommand>,
        authority: &Authority,
    ) -> Result<(), String> {
        if !self.swarm_comparison_active(request) {
            return Err(CHANGED.into());
        }
        request.action.require_installed_target_groups()?;
        let (template, current) = self.prepare_scheduler_comparison(
            &authority.profile,
            &authority.targets,
            &authority.goal,
            authority.knowledge.as_ref(),
        )?;
        if !authority.grant.matches(&template)
            || authority.profile_root != current.profile_root
            || authority.transport != current.transport
            || authority.memory_policy != current.memory_policy
            || !authority.redactor.matches_secrets(&current.redactor)
            || authority
                .vault_revision
                .as_ref()
                .is_some_and(|revision| revision.upgrade().is_none())
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    fn start_swarm_comparison(&mut self, id: RunId) -> Task<Message> {
        let Some(mut pending) = self.scheduler_swarm_comparisons.pending.remove(&id) else {
            return Task::none();
        };
        if self.swarm_comparison_cancelled(&pending) {
            cancel_completion(&mut pending.completion);
            return self.commit_scheduler_run(pending.completion, pending.session_journals);
        }
        if Instant::now() >= pending.dispatch_deadline {
            apply_comparison(&mut pending.completion, None);
            return self.commit_scheduler_run(pending.completion, pending.session_journals);
        }
        let started = self.dispatch_swarm_comparison(&pending);
        match started {
            Ok((worker, task)) => {
                pending.worker = Some(worker);
                self.scheduler_swarm_comparisons.pending.insert(id, pending);
                task
            }
            Err(error) if error == local_ai::WORKERS_BUSY => {
                self.scheduler_swarm_comparisons.pending.insert(id, pending);
                Task::none()
            }
            Err(_) => {
                apply_comparison(&mut pending.completion, None);
                self.commit_scheduler_run(pending.completion, pending.session_journals)
            }
        }
    }

    fn dispatch_swarm_comparison(
        &self,
        pending: &Pending,
    ) -> Result<(Worker, Task<Message>), String> {
        let request = &pending.request;
        if !self.swarm_comparison_active(request) {
            return Err(CHANGED.into());
        }
        request.action.require_installed_target_groups()?;
        let action = request.action.agent.as_ref().ok_or(CHANGED)?;
        let approval = action.approval.as_ref().ok_or(CHANGED)?;
        let grant = approval.comparison.as_ref().ok_or(CHANGED)?;
        let (template, authority) = self.prepare_scheduler_comparison(
            &approval.profile,
            &approval.targets,
            &action.goal,
            approval.knowledge.as_ref(),
        )?;
        if !grant.matches(&template) {
            return Err(CHANGED.into());
        }
        let report = pending.completion.swarm_report.as_ref().ok_or(CHANGED)?;
        let model = template.fill(&report.targets, &authority.redactor, mask::prompt)?;
        let authority = Arc::new(authority);
        let guard = Arc::clone(&authority);
        let cancellation = request.cancellation.clone();
        let action = Arc::clone(&request.action);
        let id = request.id;
        let (permits, requests) = async_channel::bounded(1);
        let reviewed_https = (authority.knowledge.is_some() && authority.transport.is_remote())
            .then(|| authority.transport.completion_secret());
        let local_ai::Pending { reply, cancel } = model.start_checked(
            authority.transport.clone(),
            authority.redactor.clone(),
            mask::prompt,
            (reviewed_https, move || {
                await_permit(&permits, &cancellation)?;
                action.require_installed_target_groups()?;
                guard.check_disk(&cancellation)?;
                if cancellation.is_requested() {
                    return Err(CHANGED.into());
                }
                Ok(())
            }),
            |_| Ok(()),
        )?;
        let task = Task::run(requests, move |permit| {
            Message::ScheduledSwarmComparison(Event(Progress::Permit(id, permit)))
        })
        .chain(Task::future(async move {
            let result = reply
                .recv()
                .await
                .unwrap_or_else(|_| Err(UNAVAILABLE.into()));
            Message::ScheduledSwarmComparison(Event(Progress::Finished(id, Arc::new(result))))
        }));
        Ok((
            Worker {
                authority,
                _cancel: cancel,
            },
            task,
        ))
    }

    pub(in crate::workspace) fn update_scheduled_swarm_comparison(
        &mut self,
        event: Event,
    ) -> Task<Message> {
        match event.0 {
            Progress::Permit(id, reply) => {
                let permitted = self
                    .scheduler_swarm_comparisons
                    .pending
                    .get(&id)
                    .is_some_and(|pending| {
                        pending.worker.as_ref().is_some_and(|worker| {
                            self.check_swarm_comparison_authority(
                                &pending.request,
                                &worker.authority,
                            )
                            .is_ok()
                        })
                    });
                let _ = reply.try_send(if permitted { Ok(()) } else { Err(CHANGED) });
                Task::none()
            }
            Progress::Finished(id, result) => {
                let Some(mut pending) = self.scheduler_swarm_comparisons.pending.remove(&id) else {
                    return Task::none();
                };
                if self.swarm_comparison_cancelled(&pending) {
                    cancel_completion(&mut pending.completion);
                } else {
                    let markdown = pending
                        .worker
                        .as_ref()
                        .filter(|worker| {
                            self.check_swarm_comparison_authority(
                                &pending.request,
                                &worker.authority,
                            )
                            .is_ok()
                        })
                        .and_then(|_| result.as_ref().as_ref().ok())
                        .and_then(|report| report.markdown.as_deref().ok());
                    apply_comparison(&mut pending.completion, markdown);
                }
                self.commit_scheduler_run(pending.completion, pending.session_journals)
            }
        }
    }

    fn swarm_comparison_cancelled(&self, pending: &Pending) -> bool {
        self.exiting
            || self.scheduler_draining
            || pending.request.cancellation.is_requested()
            || pending.completion.outcome == RunOutcome::Cancelled
    }

    pub(in crate::workspace) fn expire_swarm_comparisons(&mut self) -> Task<Message> {
        let expired: Vec<_> = self
            .scheduler_swarm_comparisons
            .pending
            .iter()
            .filter_map(|(id, pending)| {
                (self.swarm_comparison_cancelled(pending)
                    || !self.swarm_comparison_active(&pending.request))
                .then_some(*id)
            })
            .collect();
        let tasks = expired
            .into_iter()
            .filter_map(|id| {
                let mut pending = self.scheduler_swarm_comparisons.pending.remove(&id)?;
                drop(pending.worker.take());
                if self.swarm_comparison_cancelled(&pending) {
                    cancel_completion(&mut pending.completion);
                } else {
                    apply_comparison(&mut pending.completion, None);
                }
                Some(self.commit_scheduler_run(pending.completion, pending.session_journals))
            })
            .collect::<Vec<_>>();
        Task::batch(tasks)
    }

    pub(super) fn tick_swarm_comparisons(&mut self) -> Task<Message> {
        let expiry = self.expire_swarm_comparisons();
        let waiting: Vec<_> = self
            .scheduler_swarm_comparisons
            .pending
            .iter()
            .filter_map(|(id, pending)| pending.worker.is_none().then_some(*id))
            .collect();
        Task::batch(
            std::iter::once(expiry).chain(
                waiting
                    .into_iter()
                    .map(|id| self.start_swarm_comparison(id)),
            ),
        )
    }
}

fn await_permit(permits: &Sender<Permit>, cancellation: &Cancellation) -> Result<(), String> {
    let (permit, reply) = async_channel::bounded(1);
    permits.try_send(permit).map_err(|_| CHANGED)?;
    let deadline = Instant::now() + PERMIT_LIMIT;
    while !cancellation.is_requested() && Instant::now() < deadline {
        match reply.try_recv() {
            Ok(result) => return result.map_err(str::to_owned),
            Err(TryRecvError::Closed) => break,
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(CHANGED.into())
}

fn cancel_completion(completion: &mut Completion) {
    completion.outcome = RunOutcome::Cancelled;
    completion.swarm_report = None;
    completion.append_detail("The Swarm comparison was cancelled.");
}

fn apply_comparison(completion: &mut Completion, markdown: Option<&str>) {
    if let Some((report, markdown)) = completion.swarm_report.as_mut().zip(markdown)
        && valid_swarm_comparison(markdown)
    {
        report.comparison = Some(markdown.into());
        completion.append_detail("AI comparison completed.");
    } else {
        completion.append_detail(UNAVAILABLE);
    }
}
