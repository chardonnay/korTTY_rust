//! Review one synthesis of the masked results from a completed Swarm round.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task, window};
use zeroize::Zeroizing;

use crate::ai_manager::{Configuration, MemoryPrompt, RetrievedKnowledge};
use crate::clipboard_policy::AiMemoryPolicy;
use crate::local_ai;
use crate::terminal_agent::Cancel;
use crate::terminal_agent::swarm::comparison::{Evidence, Report, Request, transport};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::ai_quota;
use crate::workspace::saved_chats::swarm::{self as store, Record};

use super::super::knowledge_review::{self, system_with_excerpts};
use super::super::policy::Dispatch;
use super::super::request_review::{CliPresentation, OutboundAuthority};
use super::{
    ClipboardPolicy, Desktop, FollowUpResult, Group, Key, ProfileChoice, Queue, Run, RunMessage,
    SwarmMode, Workspace, conversation, mask, require_swarm_policy, snippet, swarm_profile_entries,
};

const PROFILE_CHANGED: &str = "The comparison profile changed. Review the completed round again.";
const CANCELLED: &str = "AI comparison cancelled. The local comparison is available.";

#[derive(Clone)]
pub enum Message {
    Review,
    Send(u64),
    Cancel,
    Quota(u64, String),
    Knowledge(u64, knowledge_review::Message),
    Reply(u64, Arc<Result<Report, String>>),
    Saved(u64, FollowUpResult),
}

#[derive(Default)]
pub(super) struct State {
    round: Option<Round>,
    stage: Stage,
    epoch: u64,
    result: Option<String>,
    saving: bool,
}

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    Review(Box<Review>),
    Waiting {
        authority: Arc<Authority>,
        knowledge: Option<Arc<RetrievedKnowledge>>,
        cancel: Cancel,
        _worker: local_ai::Cancel,
    },
}

struct Round {
    mode: SwarmMode,
    profile: ProfileChoice,
    configuration: Arc<Configuration>,
    transport: (local_ai::Profile, usize),
    question: Zeroizing<String>,
    sources: HashMap<Key, Source>,
    secrets: Secrets,
    saved: Option<SavedRound>,
}

struct Source {
    conclusion: Option<String>,
    dispatch: Arc<Dispatch>,
    directory: Option<String>,
}

#[derive(Clone)]
struct SavedRound {
    root: PathBuf,
    continuation: Arc<conversation::Reviewed>,
    record: Option<Record>,
}

struct Review {
    authority: Arc<Authority>,
    base: Request,
    request: Request,
    destination: String,
    cli: Option<CliPresentation>,
    quota: ai_quota::State,
    knowledge: Option<knowledge_review::Review>,
}

struct Authority {
    mode: SwarmMode,
    profile: ProfileChoice,
    configuration: Arc<Configuration>,
    transport: (local_ai::Profile, usize),
    redactor: Redactor,
    memory_policy: AiMemoryPolicy,
    memory: Option<MemoryPrompt>,
    outbound: Option<OutboundAuthority>,
    routes: Vec<Arc<Dispatch>>,
    saved: Option<SavedRound>,
    vault_revision: std::sync::Weak<()>,
    knowledge_credential: Option<Zeroizing<String>>,
}

impl State {
    pub(super) fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle) || self.saving
    }

    pub(super) fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }

    #[cfg(test)]
    pub(super) fn review(&self) -> Option<(&Request, u64, &ai_quota::State)> {
        let Stage::Review(review) = &self.stage else {
            return None;
        };
        Some((&review.request, self.epoch, &review.quota))
    }

    #[cfg(test)]
    pub(super) fn cli_review(&self) -> Option<&CliPresentation> {
        let Stage::Review(review) = &self.stage else {
            return None;
        };
        review.cli.as_ref()
    }

    pub(super) fn clear_round(&mut self) {
        self.cancel();
        self.round = None;
        self.result = None;
    }

    pub(super) fn reset_target(&mut self, key: Key) {
        self.cancel();
        self.result = None;
        if let Some(round) = &mut self.round {
            round.sources.remove(&key);
        }
    }

    pub(super) fn cancel(&mut self) -> bool {
        let cancelled = !matches!(self.stage, Stage::Idle);
        if let Stage::Waiting { cancel, .. } = &self.stage {
            cancel.request();
        }
        if cancelled {
            self.epoch += 1;
            self.stage = Stage::Idle;
        }
        cancelled
    }

    pub(super) fn record(&mut self, workspace: &Workspace, key: Key, run: &Run) {
        if let Some(round) = &mut self.round {
            run.redactor.add_known_secrets(&round.secrets);
            if let Ok(dispatch) = workspace.agent_dispatch_policy(run) {
                round.sources.insert(
                    key,
                    Source {
                        conclusion: run.swarm_conclusion.clone(),
                        dispatch: Arc::new(dispatch),
                        directory: run
                            .target
                            .as_ref()
                            .map(|target| target.directory().to_owned()),
                    },
                );
            }
        }
    }

    pub(super) fn can_save_initial(&self, queue: &Queue) -> bool {
        queue.complete()
            && queue.rows().iter().all(|row| !row.target.private)
            && self.round.as_ref().is_some_and(|round| {
                round.saved.is_none()
                    && round
                        .sources
                        .values()
                        .any(|source| source.conclusion.is_some())
            })
    }

    pub(super) fn initial_snapshot(
        &self,
        workspace: &Workspace,
        group: &Group,
        queue: &Queue,
    ) -> Result<conversation::Initial, String> {
        if !workspace.ai_chat_enabled() || !self.can_save_initial(queue) {
            return Err("A completed, non-private AI Swarm round is required.".into());
        }
        let round = self.round.as_ref().expect("completed initial round");
        let policy = workspace.swarm_policy_for(round.mode)?;
        let local = &workspace.clipboard.policy.terminal_logging;
        if !policy.ai_chat_allowed || !local.equivalent_redactions(&policy.terminal_logging) {
            return Err("Saved chat policy or the managed redaction rules changed.".into());
        }
        let redactor = Redactor::for_journal(
            round.secrets.seal().map_err(|error| error.to_string())?,
            local.clone(),
        );
        let outbound = round.outbound_authority(workspace);
        let mask_text = |value| mask_evidence(&redactor, value, outbound.as_ref(), workspace);
        let metadata = |value, limit| {
            if mask_text(value).ok().as_deref() == Some(value) {
                conversation::unmasked_metadata(&redactor, value, limit)
            } else {
                None
            }
        };
        let name = swarm_profile_entries(&round.configuration)
            .find_map(|(id, name, _, _)| (id == round.profile.id).then_some(name))
            .ok_or(PROFILE_CHANGED)?;
        let mut routes = Vec::new();
        let mut connections = std::collections::HashSet::new();
        let mut targets = Vec::new();
        for (index, row) in queue.rows().iter().enumerate() {
            let target = index as u64 + 1;
            let source = round.sources.get(&row.target.key);
            let detail = source.and_then(|source| source.conclusion.as_deref());
            let outcome = match row.status {
                super::Status::Succeeded => kortty_jobs::RunOutcome::Success,
                super::Status::Failed => kortty_jobs::RunOutcome::Failed { exit_code: None },
                super::Status::Blocked => kortty_jobs::RunOutcome::Blocked,
                super::Status::Cancelled => kortty_jobs::RunOutcome::Cancelled,
                _ => return Err("The Swarm round is still running.".into()),
            };
            let label = conversation::plain_line(
                &mask_text(&row.target.label)?,
                kortty_jobs::MAX_SWARM_TARGET_LABEL_BYTES,
            );
            let detail = conversation::plain_line(
                &mask_text(detail.unwrap_or_else(|| row.status.label()))?,
                kortty_jobs::MAX_SWARM_TARGET_DETAIL_BYTES,
            );
            targets.push(kortty_jobs::SwarmTargetReport {
                target,
                label: if label.is_empty() {
                    format!("Target {target}")
                } else {
                    label
                },
                outcome,
                detail: if detail.is_empty() {
                    row.status.label().into()
                } else {
                    detail
                },
            });
            if let Some(selected) = group
                .choices
                .iter()
                .find(|choice| choice.target.key == row.target.key)
                .and_then(|choice| group.catalog_source(choice))
                && connections.insert(selected.reference.id)
            {
                routes.push(store::SavedRoute {
                    target,
                    connection: selected.reference.id,
                    directory: source
                        .and_then(|source| source.directory.as_deref())
                        .filter(|value| value.starts_with('/'))
                        .and_then(|value| metadata(value, 4096)),
                    group: selected.group.as_deref().and_then(|value| {
                        metadata(value, crate::connection_store::MAX_GROUP_PATH_BYTES)
                    }),
                });
            }
        }
        let question = mask_text(round.question.trim())?;
        if question.is_empty() || question.len() > kortty_jobs::MAX_SWARM_QUESTION_BYTES {
            return Err(
                "The masked question exceeds the saved conversation limit of 4 KiB.".into(),
            );
        }
        let comparison = self.result().map(mask_text).transpose()?;
        Ok(conversation::Initial {
            profile: round.profile.reference.clone(),
            profile_name: conversation::plain_line(&mask_text(name)?, 256),
            mode: round.mode,
            redactor,
            turn: store::Turn {
                question,
                targets,
                comparison,
                routes: Some(routes),
            },
        })
    }

    pub(super) fn follow_up_saved(&mut self, record: &Record) {
        if let Some(saved) = self.round.as_mut().and_then(|round| round.saved.as_mut()) {
            saved.record = Some(record.clone());
        }
    }

    pub(super) fn conversation_renamed(&mut self, previous: &Record, current: &Record) {
        if let Some(saved) = self.round.as_mut().and_then(|round| round.saved.as_mut())
            && saved
                .record
                .as_ref()
                .is_some_and(|record| record.same_revision(previous))
        {
            saved.record = Some(current.clone());
        }
    }

    pub(super) fn view(&self, can_review: bool) -> Option<Element<'_, Message>> {
        if let Stage::Review(review) = &self.stage {
            let epoch = self.epoch;
            let quota = &review.quota;
            let [system, input] = review.request.messages();
            return Some(
                column![
                    text("Review the AI comparison").size(17),
                    text(&review.destination).size(13),
                    review.knowledge.as_ref().map(|knowledge| knowledge
                        .view(true)
                        .map(move |message| Message::Knowledge(epoch, message))),
                    quota.label().map(|label| text(label).size(13)),
                    scrollable(if let Some(cli) = &review.cli {
                        cli.view()
                    } else {
                        column![
                            text("System instructions").size(13),
                            text(system).wrapping(text::Wrapping::WordOrGlyph),
                            text("Masked results").size(13),
                            text(input)
                                .font(iced::Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        ]
                        .spacing(5)
                        .into()
                    })
                    .height(Length::Fixed(180.0)),
                    row![
                        button(if review.cli.is_some() {
                            "Run reviewed CLI comparison"
                        } else if review.authority.outbound.is_some() {
                            "Send reviewed HTTPS comparison"
                        } else {
                            "Send comparison request"
                        })
                        .on_press_maybe(
                            (!quota.loading() && review.knowledge_ready())
                                .then_some(Message::Send(epoch))
                        ),
                        button("Cancel").on_press(Message::Cancel),
                    ]
                    .spacing(6)
                    .wrap(),
                ]
                .spacing(6)
                .padding(12)
                .into(),
            );
        }
        let panel: Element<'_, Message> = match &self.stage {
            Stage::Waiting { .. } => row![
                text("Comparing the reviewed results…"),
                button("Cancel comparison").on_press(Message::Cancel),
            ]
            .spacing(8)
            .wrap()
            .into(),
            _ => {
                if let Some(result) = self.result() {
                    column![
                        text("AI comparison").size(17),
                        scrollable(
                            text(result)
                                .font(iced::Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph)
                        )
                        .height(Length::Fixed(170.0)),
                    ]
                    .spacing(6)
                    .into()
                } else if can_review && self.round.as_ref().is_some_and(Round::available) {
                    button("Review AI comparison…")
                        .on_press(Message::Review)
                        .into()
                } else {
                    return None;
                }
            }
        };
        Some(column![panel].padding(12).into())
    }
}

impl Round {
    fn outbound_authority(&self, workspace: &Workspace) -> Option<OutboundAuthority> {
        OutboundAuthority::capture(&self.profile, &self.transport.0, workspace.vault.as_ref())
    }

    fn available(&self) -> bool {
        self.saved
            .as_ref()
            .is_none_or(|saved| saved.record.is_some())
    }

    fn prepare(&self, workspace: &Workspace, queue: &Queue) -> Result<Review, String> {
        let policy = workspace.swarm_policy_for(self.mode)?;
        let configuration = workspace.ai_configuration().ok_or(PROFILE_CHANGED)?;
        if configuration.saved_root() != self.configuration.saved_root()
            || configuration.agent_profile_reference_revision(&self.profile.reference)
                != self
                    .configuration
                    .agent_profile_reference_revision(&self.profile.reference)
            || self.profile.transport(&configuration)? != self.transport
        {
            return Err(PROFILE_CHANGED.into());
        }
        let redactor = Redactor::for_journal(
            self.secrets.seal().map_err(|error| error.to_string())?,
            workspace.clipboard.policy.terminal_logging.clone(),
        );
        let outbound = self.outbound_authority(workspace);
        let mask_text = |redactor: &Redactor, value: &str| {
            mask_evidence(redactor, value, outbound.as_ref(), workspace)
        };
        let memory_policy = workspace.clipboard.policy.ai_memory.clone();
        let memory = configuration.load_memory(
            configuration.saved_root(),
            &memory_policy,
            None,
            false,
            |text| mask_text(&redactor, text),
        )?;
        let (name, model, endpoint) = swarm_profile_entries(&configuration)
            .find_map(|(candidate, name, model, endpoint)| {
                (candidate == self.profile.id).then_some((name, model, endpoint))
            })
            .ok_or(PROFILE_CHANGED)?;
        let destination = mask_text(&redactor, &format!("{name} · {model} · {endpoint}"))?;
        let rows = queue
            .rows()
            .iter()
            .map(|row| {
                let source = self.sources.get(&row.target.key);
                Evidence {
                    label: &row.target.label,
                    status: row.status,
                    conclusion: source.and_then(|source| source.conclusion.as_deref()),
                    detail: &row.detail,
                    transcript: source.map(|_| row.transcript.as_str()),
                }
            })
            .collect::<Vec<_>>();
        let request = Request::prepare(
            &self.transport.0,
            &self.question,
            &rows,
            memory.as_ref().map(MemoryPrompt::text),
            self.transport.1,
            &redactor,
            mask_text,
        )?;
        let [system, input] = request.messages();
        let cli = CliPresentation::prepare(&self.transport.0, system, input)?;
        let knowledge_credential = self.transport.0.completion_secret();
        let knowledge = configuration.saved_root().and_then(|root| {
            let masking = knowledge_review::Mask::new(
                redactor.clone(),
                knowledge_credential.clone(),
                workspace.vault.as_ref(),
            );
            knowledge_review::Review::new(
                &configuration,
                self.profile.id,
                root,
                &masking.apply(&self.question).ok()?,
                self.transport.1,
            )
        });
        let authority = Arc::new(Authority {
            mode: self.mode,
            profile: self.profile.clone(),
            configuration,
            transport: self.transport.clone(),
            redactor,
            memory_policy,
            memory,
            outbound,
            routes: self
                .sources
                .values()
                .map(|source| Arc::clone(&source.dispatch))
                .collect(),
            saved: self.saved.clone(),
            vault_revision: workspace.vault_change_revision(),
            knowledge_credential,
        });
        authority.check_policy(&policy)?;
        authority.check_dispatch()?;
        Ok(Review {
            authority,
            base: request.clone(),
            request,
            destination,
            cli,
            quota: ai_quota::State::default(),
            knowledge,
        })
    }
}

fn mask_evidence(
    redactor: &Redactor,
    value: &str,
    outbound: Option<&OutboundAuthority>,
    workspace: &Workspace,
) -> Result<String, String> {
    let value = mask::prompt(redactor, value)?;
    if let Some(outbound) = outbound {
        outbound.mask_text(&value, workspace.vault.as_ref())
    } else {
        Ok(value)
    }
}

impl Authority {
    fn check_knowledge_mask_authority(&self) -> Result<(), String> {
        if self.vault_revision.strong_count() == 0
            || self.transport.0.completion_secret() != self.knowledge_credential
        {
            return Err(
                "The vault or model credential changed. Review the knowledge request again.".into(),
            );
        }
        Ok(())
    }

    fn check_knowledge_dispatch(&self) -> Result<(), String> {
        self.check_knowledge_mask_authority()?;
        self.check_dispatch()
    }

    fn knowledge_mask(&self, workspace: &Workspace) -> knowledge_review::Mask {
        knowledge_review::Mask::new(
            self.redactor.clone(),
            self.knowledge_credential.clone(),
            workspace.vault.as_ref(),
        )
    }

    fn check_policy(&self, policy: &ClipboardPolicy) -> Result<(), String> {
        require_swarm_policy(policy, self.mode)?;
        if let Some(outbound) = &self.outbound {
            outbound.check_policy(policy)?;
        }
        if !self
            .redactor
            .redaction_policy()
            .equivalent_redactions(&policy.terminal_logging)
            || self.memory_policy != policy.ai_memory
        {
            return Err("The comparison masking or memory policy changed. Review it again.".into());
        }
        for route in &self.routes {
            route.check_with(policy)?;
        }
        if let Some(memory) = &self.memory {
            memory.require_current(&policy.ai_memory)?;
        }
        if let Some(saved) = &self.saved {
            saved
                .continuation
                .check(&saved.root)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn check_profile_and_policy(&self) -> Result<(), String> {
        self.transport.0.require_current_policy_authority()?;
        if let Some(outbound) = &self.outbound {
            outbound.check(&self.transport.0)?;
        }
        if self.configuration.saved_root().is_some()
            && !self.configuration.reload_saved().is_ok_and(|config| {
                config == *self.configuration
                    && self
                        .profile
                        .transport(&config)
                        .is_ok_and(|transport| transport == self.transport)
            })
        {
            return Err(PROFILE_CHANGED.into());
        }
        self.check_policy(&ClipboardPolicy::load())
    }

    fn check_dispatch(&self) -> Result<(), String> {
        self.check_profile_and_policy()?;
        if let Some(saved) = &self.saved {
            let record = saved
                .record
                .as_ref()
                .ok_or("Save the reviewed follow-up before comparing it.")?;
            if !store::load(&saved.root, &record.id)
                .is_ok_and(|current| record.same_revision(&current))
            {
                return Err(
                    "The saved Swarm conversation changed. Reopen it before comparing.".into(),
                );
            }
        }
        Ok(())
    }

    fn require_current(&self, workspace: &Workspace) -> Result<(), String> {
        let configuration = workspace.ai_configuration().ok_or(PROFILE_CHANGED)?;
        if *configuration != *self.configuration
            || self.profile.transport(&configuration)? != self.transport
            || self
                .outbound
                .as_ref()
                .is_some_and(|outbound| !outbound.matches_vault(workspace.vault.as_ref()))
        {
            return Err(PROFILE_CHANGED.into());
        }
        self.check_policy(&workspace.clipboard.policy)?;
        self.check_dispatch()
    }
}

impl Review {
    fn knowledge_ready(&self) -> bool {
        self.knowledge
            .as_ref()
            .is_none_or(knowledge_review::Review::ready)
    }

    fn set_knowledge(
        &mut self,
        retrieved: Option<(&RetrievedKnowledge, &knowledge_review::Mask)>,
    ) -> Result<(), String> {
        let [base_system, _] = self.base.messages();
        let transport = &self.authority.transport;
        let request = if let Some((retrieved, masking)) = retrieved {
            let system = system_with_excerpts(
                base_system,
                Some(retrieved),
                transport.0.structured_system_suffix(),
            )
            .into_owned();
            masking.require_current(&system)?;
            self.base
                .with_system(system, transport.1, &self.authority.redactor)?
        } else {
            self.base.clone()
        };
        let [system, input] = request.messages();
        self.cli = CliPresentation::prepare(&transport.0, system, input)?;
        self.request = request;
        Ok(())
    }

    fn forecast(&mut self, id: window::Id, epoch: u64) -> Task<Desktop> {
        self.quota.start_messages(
            epoch,
            &transport(&self.authority.transport.0),
            self.request.messages(),
            move |value| event(id, Message::Quota(epoch, value)),
        )
    }
}

impl Workspace {
    pub(super) fn begin_swarm_comparison(&self, group: &mut Group) {
        let state = &mut group.comparison;
        state.clear_round();
        let Some(queue) = &group.queue else { return };
        if group.mode == SwarmMode::Snippet || queue.rows().iter().any(|row| row.target.private) {
            return;
        }
        let Some(profile) = &group.profile else {
            return;
        };
        let Some(configuration) = self.ai_configuration() else {
            return;
        };
        let Ok(transport) = profile.transport(&configuration) else {
            return;
        };
        let saved = group.conversation.as_ref().and_then(|chat| {
            let active = chat.active.as_ref()?;
            Some(SavedRound {
                root: chat.root.clone(),
                continuation: Arc::clone(&active.reviewed),
                record: None,
            })
        });
        let question = saved.as_ref().map_or(group.goal.as_str(), |saved| {
            saved.continuation.question.as_str()
        });
        let secrets = Secrets::default();
        if transport.0.is_remote()
            && let Some(credential) = transport.0.completion_secret()
        {
            secrets.add(&credential);
        }
        state.round = Some(Round {
            mode: group.mode,
            profile: profile.clone(),
            configuration,
            transport,
            question: Zeroizing::new(question.to_owned()),
            sources: HashMap::new(),
            secrets,
            saved,
        });
    }

    pub(super) fn swarm_comparison_action(
        &mut self,
        id: window::Id,
        group: &mut Group,
        message: Message,
    ) -> Task<Desktop> {
        let can_review = !group.running()
            && group.workflow.is_none()
            && group.save_name.is_none()
            && !group.closing;
        let mut state = std::mem::take(&mut group.comparison);
        let result = self.update_swarm_comparison(id, group, &mut state, message, can_review);
        group.comparison = state;
        match result {
            Ok(task) => task,
            Err(error) => {
                group.notice = error;
                Task::none()
            }
        }
    }

    fn update_swarm_comparison(
        &mut self,
        id: window::Id,
        group: &mut Group,
        state: &mut State,
        message: Message,
        can_review: bool,
    ) -> Result<Task<Desktop>, String> {
        match message {
            Message::Review if can_review && state.result.is_none() => {
                let Some((round, queue)) = state
                    .round
                    .as_ref()
                    .zip(group.queue.as_ref().filter(|queue| queue.complete()))
                else {
                    return Ok(Task::none());
                };
                let mut review = Box::new(round.prepare(self, queue)?);
                state.epoch += 1;
                let task = review.forecast(id, state.epoch);
                state.stage = Stage::Review(review);
                group.notice.clear();
                return Ok(task);
            }
            Message::Quota(epoch, value) if epoch == state.epoch => {
                if let Stage::Review(review) = &mut state.stage {
                    review.quota.complete(epoch, value);
                }
            }
            Message::Knowledge(epoch, message) if epoch == state.epoch && !group.closing => {
                return state.update_knowledge(id, group, self, message);
            }
            Message::Send(epoch)
                if epoch == state.epoch
                    && !group.closing
                    && matches!(state.stage, Stage::Review(_)) =>
            {
                return state.send(id, group, self);
            }
            Message::Cancel if state.cancel() => group.notice = CANCELLED.into(),
            Message::Reply(epoch, result)
                if epoch == state.epoch && matches!(state.stage, Stage::Waiting { .. }) =>
            {
                let Stage::Waiting {
                    authority,
                    knowledge,
                    ..
                } = std::mem::take(&mut state.stage)
                else {
                    return Ok(Task::none());
                };
                authority.require_current(self)?;
                if let Some(knowledge) = &knowledge {
                    knowledge.require_current_guarded(|| authority.check_knowledge_dispatch())?;
                }
                let report = result.as_ref().as_ref().map_err(Clone::clone)?;
                let markdown = report
                    .markdown
                    .as_ref()
                    .map_err(|error| format!("{error} The local comparison is available."))?
                    .clone();
                if let Some(outbound) = &authority.outbound
                    && outbound.mask_text(&markdown, self.vault.as_ref())? != markdown
                {
                    return Err("The AI comparison returned text requiring masking. The local comparison is available.".into());
                }
                let tokens = report.usage.as_ref().and_then(|usage| {
                    usage
                        .total_tokens
                        .or_else(|| Some(usage.input_tokens?.saturating_add(usage.output_tokens?)))
                });
                group.notice = tokens.map_or_else(
                    || "AI comparison ready.".into(),
                    |tokens| format!("AI comparison ready. {tokens} tokens reported."),
                );
                state.result = Some(markdown.clone());
                if let Some(saved) = &authority.saved
                    && let Some(record) = &saved.record
                {
                    let root = saved.root.clone();
                    let record = record.clone();
                    state.saving = true;
                    return Ok(snippet::work(
                        "kortty-swarm-comparison-save",
                        move || {
                            store::set_comparison(&root, &record, markdown, || {
                                let check = || {
                                    if knowledge.is_some() {
                                        authority.check_knowledge_mask_authority()?;
                                    }
                                    authority.check_profile_and_policy()
                                };
                                knowledge.as_ref().map_or_else(check, |knowledge| {
                                    knowledge.require_current_guarded(check)
                                }).map_err(std::io::Error::other)
                            })
                            .map_err(|_| "Could not save the AI comparison. It remains available in this window.")
                        },
                        move |result| event(id, Message::Saved(epoch, FollowUpResult(result))),
                    ));
                }
            }
            Message::Saved(epoch, result) if epoch == state.epoch && state.saving => {
                state.saving = false;
                let record = result.0?;
                if let Some(chat) = &mut group.conversation {
                    self.record_saved_swarm(&chat.root, &record);
                    group.notice = if record.sync_failed {
                        "AI comparison saved. Could not confirm that it reached durable storage."
                    } else {
                        "AI comparison saved."
                    }
                    .into();
                    chat.record = record;
                }
            }
            _ => {}
        }
        Ok(Task::none())
    }
}

impl State {
    fn update_knowledge(
        &mut self,
        id: window::Id,
        group: &mut Group,
        workspace: &mut Workspace,
        message: knowledge_review::Message,
    ) -> Result<Task<Desktop>, String> {
        let Stage::Review(review) = &mut self.stage else {
            return Ok(Task::none());
        };
        let Some(knowledge) = &mut review.knowledge else {
            return Ok(Task::none());
        };
        match message {
            knowledge_review::Message::Select(index) => {
                if !knowledge.select(index) {
                    return Ok(Task::none());
                }
                review.set_knowledge(None)?;
                group.notice.clear();
            }
            knowledge_review::Message::Cancel => {
                if knowledge.cancel() {
                    group.notice = "Knowledge retrieval cancelled.".into();
                }
                return Ok(Task::none());
            }
            knowledge_review::Message::Retrieve => {
                review.authority.require_current(workspace)?;
                let authority = Arc::clone(&review.authority);
                let masking = authority.knowledge_mask(workspace);
                masking.require_current(knowledge.query())?;
                let token = workspace.allocate();
                let epoch = self.epoch;
                let task = knowledge.start(
                    token,
                    workspace.vault.as_ref(),
                    move |value| masking.apply(value),
                    move || authority.check_knowledge_dispatch(),
                    move |message| event(id, Message::Knowledge(epoch, message)),
                )?;
                group.notice.clear();
                return Ok(task);
            }
            knowledge_review::Message::Finished(token, result) => {
                let Some(result) = knowledge.finish(token, result) else {
                    return Ok(Task::none());
                };
                let retrieved = result?;
                review.authority.require_current(workspace)?;
                retrieved
                    .require_current_guarded(|| review.authority.check_knowledge_dispatch())?;
                let masking = review.authority.knowledge_mask(workspace);
                review.set_knowledge(Some((&retrieved, &masking)))?;
                if let Some(knowledge) = &mut review.knowledge {
                    knowledge.accept(retrieved);
                }
                group.notice = "Knowledge is ready. Review the updated comparison request.".into();
            }
        }
        self.epoch += 1;
        Ok(review.forecast(id, self.epoch))
    }

    fn send(
        &mut self,
        id: window::Id,
        group: &mut Group,
        workspace: &Workspace,
    ) -> Result<Task<Desktop>, String> {
        let Stage::Review(mut review) = std::mem::take(&mut self.stage) else {
            return Ok(Task::none());
        };
        if review.quota.loading() || !review.knowledge_ready() {
            self.stage = Stage::Review(review);
            return Ok(Task::none());
        }
        review.authority.require_current(workspace)?;
        if review.quota.stale() {
            self.epoch += 1;
            let task = review.forecast(id, self.epoch);
            self.stage = Stage::Review(review);
            group.notice = "Review the refreshed quota before sending.".into();
            return Ok(task);
        }
        let authority = Arc::clone(&review.authority);
        let knowledge = review
            .knowledge
            .as_ref()
            .and_then(knowledge_review::Review::retrieved);
        let masking = knowledge
            .as_ref()
            .map(|_| authority.knowledge_mask(workspace));
        let reviewed_https = (knowledge.is_some() && authority.transport.0.is_remote())
            .then(|| authority.knowledge_credential.clone());
        let cancel = Cancel::default();
        let worker_cancel = cancel.clone();
        let pending = review.request.clone().start_checked(
            authority.transport.0.clone(),
            authority.redactor.clone(),
            mask::prompt,
            (reviewed_https, {
                let authority = Arc::clone(&authority);
                let knowledge = knowledge.clone();
                move || {
                    let check = || {
                        if worker_cancel.requested() {
                            return Err(CANCELLED.into());
                        }
                        if knowledge.is_some() {
                            authority.check_knowledge_mask_authority()?;
                        }
                        authority.check_dispatch()
                    };
                    knowledge
                        .as_ref()
                        .map_or_else(check, |knowledge| knowledge.require_current_guarded(check))
                }
            }),
            {
                let knowledge = knowledge.clone();
                move |response| {
                    if let Some(masking) = &masking {
                        masking.require_current(response)?;
                    }
                    knowledge.as_ref().map_or(Ok(()), |knowledge| {
                        knowledge.validate_references(std::iter::once(response))
                    })
                }
            },
        );
        let pending = match pending {
            Err(error) if error == local_ai::WORKERS_BUSY => {
                self.epoch += 1;
                let task = review.forecast(id, self.epoch);
                self.stage = Stage::Review(review);
                group.notice =
                    "The AI workers are busy. Review the refreshed quota and try again.".into();
                return Ok(task);
            }
            result => result?,
        };
        self.epoch += 1;
        let epoch = self.epoch;
        self.stage = Stage::Waiting {
            authority,
            knowledge,
            cancel,
            _worker: pending.cancel,
        };
        group.notice.clear();
        Ok(Task::perform(
            async move {
                Arc::new(
                    pending
                        .reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The AI comparison worker stopped.".into())),
                )
            },
            move |result| event(id, Message::Reply(epoch, result)),
        ))
    }
}

fn event(id: window::Id, message: Message) -> Desktop {
    Desktop::TerminalAgent(id, RunMessage::Swarm(super::Message::Comparison(message)))
}
