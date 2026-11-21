//! Opens reviewed Chat context from an owned, settled Agent run.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{
    Desktop, LiveBinding, Message, Mode, Phase, ProfileChoice, Run, SessionId, State, Task,
    Workspace, inline_history::FollowUpTrace, mask, window,
};
use crate::workspace::{
    journal_agent_chat::{AgentJournal, authorized},
    saved_chats::store,
    terminal_ai::{
        MAX_QUESTION_BYTES, Turn,
        agent_follow_up::{Origin, Seed},
        metadata,
    },
};
use crate::{
    session_journal::agent_chat::Card,
    terminal_logging::{RedactionSecrets, Redactor},
};

const MAX_RETAINED: usize = 16;

#[derive(Clone)]
pub(super) struct Snapshot {
    id: String,
    source: LiveBinding,
    profile: ProfileChoice,
    label: String,
    trace: Arc<FollowUpTrace>,
    learned_sudo: Option<RedactionSecrets>,
    journal: Option<AgentJournal>,
    duration_ms: Option<u64>,
    tokens: Option<u64>,
}

impl Snapshot {
    fn capture(run: &Run) -> Result<Self, String> {
        if !run.can_chat_handoff() {
            return Err("This Agent run is unavailable for Chat.".into());
        }
        Ok(Self {
            id: run
                .chat_handoff_id
                .clone()
                .ok_or("The Agent run identity is unavailable.")?,
            source: run.binding.live()?,
            profile: run
                .profile()
                .ok_or("The Agent profile is unavailable.")?
                .clone(),
            label: run.label.clone(),
            trace: Arc::new(run.follow_up_trace()?),
            learned_sudo: run.learned_sudo.clone(),
            journal: run.chat_journal.clone(),
            duration_ms: run
                .chat_handoff_elapsed_ms
                .filter(|value| i64::try_from(*value).is_ok()),
            tokens: run
                .presentation
                .model_usage
                .total()
                .filter(|value| i64::try_from(*value).is_ok()),
        })
    }

    fn origin(&self, owner: window::Id) -> Origin {
        Origin {
            owner,
            session: self.source.session,
            stream: self.source.stream,
            id: self.id.clone(),
        }
    }

    fn card(&self, redactor: &Redactor) -> Result<Card, String> {
        let title = metadata(&mask::prompt(redactor, &self.trace.goal)?, 140);
        Ok(Card {
            id: format!("agent-{}", self.id),
            title: if title.is_empty() {
                "AI Agent".into()
            } else {
                title
            },
            answer: mask::prompt(redactor, &self.trace.answer)?,
            model: metadata(&mask::prompt(redactor, &self.profile.label)?, 256),
            duration_ms: self.duration_ms,
            tokens: self.tokens,
        })
    }
}

#[derive(Clone, Copy)]
struct Ticket {
    window: window::Id,
    epoch: u64,
}

pub(super) struct Pending {
    token: window::Id,
    permit: Arc<AtomicBool>,
    config: Arc<crate::ai_manager::Configuration>,
    mask_policy: crate::terminal_logging::Policy,
    library_revision: u64,
    source: LiveBinding,
    ticket: Option<Ticket>,
    question: String,
    root: PathBuf,
    learned_sudo: Option<RedactionSecrets>,
    journal_requested: bool,
}

#[derive(Clone)]
pub struct Loaded {
    id: String,
    token: window::Id,
    result: Result<Box<store::Record>, String>,
    journal_warning: bool,
}

impl Run {
    pub(super) fn is_live_ask_answer(&self) -> bool {
        self.mode == Mode::Ask
            && self.phase == Phase::Finished
            && self.runs_in_chat_tab()
            && !self.reopened_history
    }

    pub(super) fn can_chat_handoff(&self) -> bool {
        !self.closing
            && !self.swarm
            && self.started_order != 0
            && self.chat_handoff_id.is_some()
            && self.inline.is_some()
            && (self.phase.finished() || self.phase == Phase::PlanReady)
    }
}

impl State {
    fn retain_chat_handoff(&mut self, snapshot: Snapshot) {
        self.chat_handoffs
            .retain(|previous| previous.source.session != snapshot.source.session);
        if self.chat_handoffs.len() >= MAX_RETAINED {
            self.chat_handoffs.pop_front();
        }
        self.chat_handoffs.push_back(snapshot);
    }
}

impl Workspace {
    pub(super) fn remember_chat_handoff(&mut self, run: &mut Run) {
        if run.binding.private() {
            return;
        }
        let Ok(mut snapshot) = Snapshot::capture(run) else {
            return;
        };
        if snapshot.journal.is_none()
            && let Ok(redactor) =
                self.handoff_redactor(snapshot.source, snapshot.learned_sudo.as_ref())
            && let Ok(card) = snapshot.card(&redactor)
        {
            snapshot.journal =
                self.agent_journal_for(snapshot.source.session, snapshot.source.stream, card);
            if let Some(journal) = &snapshot.journal {
                journal.queue_card();
            }
            run.chat_journal = snapshot.journal.clone();
        }
        self.terminal_agents.retain_chat_handoff(snapshot);
    }

    fn handoff_redactor(
        &self,
        source: LiveBinding,
        learned: Option<&RedactionSecrets>,
    ) -> Result<Redactor, String> {
        let pane = self
            .sessions
            .get(&source.session)
            .filter(|pane| pane.stream_generation == source.stream)
            .ok_or("The original terminal is unavailable.")?;
        let redactor = self.agent_source_redactor(pane)?;
        if let Some(learned) = learned {
            redactor
                .with_overlay(learned)
                .map_err(|error| error.to_string())
        } else {
            Ok(redactor)
        }
    }

    fn handoff_owner(&self, source: LiveBinding) -> Option<window::Id> {
        let (owner, pane, tab) = self.agent_pane(source.session)?;
        (!self.exiting
            && self.ai_chat_enabled()
            && pane.stream_generation == source.stream
            && tab.incognito == source.private
            && !pane.frame.password_prompt)
            .then_some(owner)
    }

    fn pending_handoff_is_current(&self, id: &str, pending: &Pending) -> bool {
        self.handoff_owner(pending.source).is_some()
            && (!pending.journal_requested || self.journal_allowed())
            && self
                .ai_chat_configuration()
                .is_some_and(|config| Arc::ptr_eq(&config, &pending.config))
            && self
                .clipboard
                .policy
                .terminal_logging
                .same_redactions(&pending.mask_policy)
            && pending.ticket.is_none_or(|ticket| {
                self.terminal_agents
                    .runs
                    .get(&ticket.window)
                    .is_some_and(|run| {
                        run.epoch == ticket.epoch
                            && run.can_chat_handoff()
                            && run.chat_handoff_id.as_deref() == Some(id)
                    })
            })
    }

    pub(in crate::workspace) fn revalidate_agent_chat_handoffs(&mut self) {
        let enabled = self.ai_chat_enabled();
        for run in self.terminal_agents.runs.values_mut() {
            run.chat_handoff_enabled = enabled;
        }
        let invalid: Vec<_> = self
            .terminal_agents
            .pending_chat_handoffs
            .iter()
            .filter(|(id, pending)| !self.pending_handoff_is_current(id, pending))
            .map(|(id, _)| id.clone())
            .collect();
        for id in invalid {
            let pending = self
                .terminal_agents
                .pending_chat_handoffs
                .get_mut(&id)
                .expect("pending handoff");
            pending.permit.store(false, Ordering::Release);
            pending.learned_sudo = None;
        }
        let live: Vec<_> = self
            .terminal_agents
            .chat_handoffs
            .iter()
            .filter_map(|snapshot| {
                let source = snapshot.source;
                self.agent_pane(source.session)
                    .is_some_and(|(_, pane, tab)| {
                        pane.stream_generation == source.stream && tab.incognito == source.private
                    })
                    .then_some(source.session)
            })
            .collect();
        self.terminal_agents
            .chat_handoffs
            .retain(|snapshot| live.contains(&snapshot.source.session));
    }

    pub(in crate::workspace) fn agent_chat_handoff_available(&self, session: SessionId) -> bool {
        self.terminal_agents.chat_handoffs.iter().any(|snapshot| {
            snapshot.source.session == session && self.handoff_owner(snapshot.source).is_some()
        })
    }

    pub(in crate::workspace) fn continue_latest_agent_chat(
        &mut self,
        owner: window::Id,
        session: SessionId,
    ) -> Task<Desktop> {
        if !self.window_contains_visible_pane(owner, session) {
            return Task::none();
        }
        let snapshot = self
            .terminal_agents
            .chat_handoffs
            .iter()
            .rev()
            .find(|snapshot| {
                snapshot.source.session == session
                    && self.handoff_owner(snapshot.source) == Some(owner)
            })
            .cloned();
        snapshot.map_or_else(Task::none, |snapshot| {
            if let Some(id) = self.terminal_agents.runs.iter().find_map(|(&id, run)| {
                (run.is_live_ask_answer() && run.chat_handoff_id.as_deref() == Some(&snapshot.id))
                    .then_some(id)
            }) {
                return self.continue_ask_tab(id, "");
            }
            self.start_agent_chat(snapshot, String::new(), None)
        })
    }

    pub(super) fn agent_chat_action(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        let (Message::ChatFollowUp(epoch, _)
        | Message::ContinueInChat(epoch)
        | Message::DismissChatHandoff(epoch)) = &message
        else {
            return Task::none();
        };
        let Some(run) = self.terminal_agents.runs.get_mut(&id).filter(|run| {
            run.epoch == *epoch && run.can_chat_handoff() && !run.chat_handoff_pending
        }) else {
            return Task::none();
        };
        match message {
            Message::ChatFollowUp(_, text) if text.len() <= MAX_QUESTION_BYTES => {
                run.chat_follow_up = text
            }
            Message::DismissChatHandoff(_) => {
                run.chat_handoff_dismissed = true;
                run.chat_follow_up.clear();
            }
            Message::ContinueInChat(epoch) => {
                let question = run.chat_follow_up.clone();
                if run.is_live_ask_answer() {
                    return self.continue_ask_tab(id, &question);
                }
                let snapshot = match Snapshot::capture(run) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        run.notice = error;
                        return Task::none();
                    }
                };
                return self.start_agent_chat(
                    snapshot,
                    question,
                    Some(Ticket { window: id, epoch }),
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn continue_ask_tab(&mut self, id: window::Id, question: &str) -> Task<Desktop> {
        let result = (|| {
            let run = self
                .terminal_agents
                .runs
                .get(&id)
                .filter(|run| run.is_live_ask_answer() && run.can_chat_handoff())
                .ok_or("The finished Ask tab is unavailable.")?;
            let source = run.binding.live()?;
            let owner = self
                .handoff_owner(source)
                .ok_or("The original terminal or AI Chat is unavailable.")?;
            self.current_agent_pane(source, Mode::Ask)?;
            let redactor = self.handoff_redactor(source, None)?;
            let profile = run.profile().ok_or("The Ask profile is unavailable.")?;
            let (transcript, _) = run
                .status_transcript()
                .ok_or("The Ask response is unavailable.")?;
            let context = mask::prompt(&redactor, run.context.trim())?;
            let attachment = run
                .ask_file
                .snapshot
                .as_ref()
                .map(|file| file.stored().remasked(&redactor))
                .transpose()?;
            let context = attachment
                .as_ref()
                .map_or(context.clone(), |file| file.append_to(&context));
            let turn = Turn::from_parts(
                (!context.trim().is_empty()).then_some(context.as_str()),
                mask::prompt(&redactor, &run.goal)?,
                mask::prompt(&redactor, &transcript.final_answer)?,
            );
            if !turn.is_native_pair() || !turn.valid_initial() {
                return Err("The Ask response does not fit the Chat limits.".into());
            }
            let draft = store::Draft {
                profile: profile.reference.clone(),
                profile_name: metadata(&mask::prompt(&redactor, &profile.label)?, 256),
                connection: metadata(&mask::prompt(&redactor, &run.label)?, 120),
                response_language: None,
                turns: vec![turn],
                attachment,
            };
            let origin = Origin {
                owner,
                session: source.session,
                stream: source.stream,
                id: run
                    .chat_handoff_id
                    .clone()
                    .ok_or("The Ask run identity is unavailable.")?,
            };
            self.replace_ask_with_chat(id, origin, draft, question.trim(), redactor)
        })();
        match result {
            Ok(task) => {
                let pip =
                    self.terminal_agents
                        .runs
                        .remove(&id)
                        .map_or_else(Task::none, |mut run| {
                            self.retain_finished_agent_run(id, &mut run);
                            self.follow_released_agent_pip(&mut run)
                        });
                Task::batch([task, pip, self.reconcile_agent_pip()])
            }
            Err(error) => {
                if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                    run.notice = error;
                }
                Task::none()
            }
        }
    }

    fn start_agent_chat(
        &mut self,
        snapshot: Snapshot,
        question: String,
        ticket: Option<Ticket>,
    ) -> Task<Desktop> {
        if self
            .terminal_agents
            .pending_chat_handoffs
            .contains_key(&snapshot.id)
        {
            return Task::none();
        }
        let Some(owner) = self.handoff_owner(snapshot.source) else {
            self.finish_chat_offer(
                ticket,
                false,
                Some("The original terminal or AI Chat is unavailable.".into()),
            );
            return Task::none();
        };
        let result = (|| {
            let redactor =
                self.handoff_redactor(snapshot.source, snapshot.learned_sudo.as_ref())?;
            let question = mask::prompt(&redactor, question.trim())?;
            if question.len() > MAX_QUESTION_BYTES {
                return Err("Shorten the follow-up question to fit the Chat limit.".into());
            }
            Ok((question, redactor))
        })();
        let (question, redactor) = match result {
            Ok(prepared) => prepared,
            Err(error) => return self.agent_chat_error(owner, ticket, error),
        };
        let has_learned_sudo = snapshot.learned_sudo.is_some();
        if has_learned_sudo && let Err(error) = self.apply_agent_chat_mask(&snapshot.id, &redactor)
        {
            return self.agent_chat_error(owner, ticket, error);
        }
        if let Some((task, staged)) = self.focus_agent_terminal_chat(&snapshot.id, &question) {
            self.agent_chat_staged(owner, ticket, staged);
            return task;
        }
        if !self.agent_chat_has_capacity() {
            return self.agent_chat_error(
                owner,
                ticket,
                "Close an AI Chat tab before opening another.".into(),
            );
        }
        let draft = (|| {
            let turn = Turn::from_agent(
                &snapshot.trace.goal,
                &snapshot.trace.answer,
                &snapshot.trace.steps,
                &redactor,
            )?;
            let title = metadata(turn.question(), store::MAX_TITLE_CHARS);
            let draft = store::Draft {
                profile: snapshot.profile.reference.clone(),
                profile_name: metadata(&mask::prompt(&redactor, &snapshot.profile.label)?, 256),
                connection: metadata(&mask::prompt(&redactor, &snapshot.label)?, 120),
                response_language: None,
                turns: vec![turn],
                attachment: None,
            };
            Ok((title, draft))
        })();
        let (title, draft) = match draft {
            Ok(prepared) => prepared,
            Err(error) => return self.agent_chat_error(owner, ticket, error),
        };
        if snapshot.source.private {
            return match self.install_agent_terminal_chat(
                snapshot.origin(owner),
                Seed::Private(draft),
                &question,
                redactor,
                has_learned_sudo,
            ) {
                Ok((task, staged)) => {
                    self.agent_chat_staged(owner, ticket, staged);
                    task
                }
                Err(error) => self.agent_chat_error(owner, ticket, error),
            };
        }
        let Some((config, root)) = self.ai_chat_configuration().and_then(|config| {
            let root = PathBuf::from(config.saved_root()?);
            Some((config, root))
        }) else {
            return self.agent_chat_error(
                owner,
                ticket,
                "Saved Chat storage is unavailable.".into(),
            );
        };
        if let Some(ticket) = ticket
            && let Some(run) = self.terminal_agents.runs.get_mut(&ticket.window)
        {
            run.chat_handoff_pending = true;
        }
        let token = window::Id::unique();
        let permit = Arc::new(AtomicBool::new(true));
        let mut journals = Vec::new();
        if self.journal_allowed()
            && let Ok(card) = snapshot.card(&redactor)
        {
            journals.extend(
                snapshot
                    .journal
                    .map(|journal| journal.with_card(card.clone())),
            );
            let fallback =
                self.agent_journal_for(snapshot.source.session, snapshot.source.stream, card);
            if let Some(fallback) = fallback
                && journals
                    .iter()
                    .all(|journal| journal.reference() != fallback.reference())
            {
                journals.push(fallback);
            }
        }
        let id = snapshot.id;
        self.terminal_agents.pending_chat_handoffs.insert(
            id.clone(),
            Pending {
                token,
                permit: Arc::clone(&permit),
                config,
                mask_policy: self.clipboard.policy.terminal_logging.clone(),
                library_revision: self.saved_chat_library_revision,
                source: snapshot.source,
                ticket,
                question,
                root: root.clone(),
                learned_sudo: snapshot.learned_sudo,
                journal_requested: !journals.is_empty(),
            },
        );
        let worker_id = id.clone();
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                let worker = std::thread::Builder::new()
                    .name("kortty-agent-chat".into())
                    .spawn(move || {
                        let established = store::load_agent_run(&root, &worker_id)
                            .ok()
                            .and_then(|record| record.agent_journal().cloned());
                        let journal = journals
                            .iter()
                            .filter(|journal| {
                                established
                                    .as_ref()
                                    .is_none_or(|link| journal.reference() == *link)
                            })
                            .find(|journal| journal.ensure_card(&permit).is_ok());
                        let result = store::open_or_seed_agent_linked(
                            &root,
                            &worker_id,
                            title,
                            draft,
                            journal.map(AgentJournal::reference),
                            || authorized(&permit),
                        )
                        .map(Box::new)
                        .map_err(|error| format!("Could not open the Agent chat ({error})."));
                        let journal_warning = result.as_ref().is_ok_and(|record| {
                            if let Some(journal) = journal {
                                journal.link(record, &redactor, &permit).is_err()
                            } else {
                                !journals.is_empty()
                            }
                        });
                        let _ = send.try_send((result, journal_warning));
                    });
                let (result, journal_warning) = match worker {
                    Ok(_) => receive
                        .recv()
                        .await
                        .unwrap_or_else(|_| (Err("The Agent Chat worker stopped.".into()), false)),
                    Err(_) => (Err("The Agent Chat worker could not start.".into()), false),
                };
                Arc::new(Loaded {
                    id,
                    token,
                    result,
                    journal_warning,
                })
            },
            move |reply| Desktop::TerminalAgent(owner, Message::ChatHandoffLoaded(reply)),
        )
    }

    pub(super) fn finish_agent_chat(&mut self, loaded: Arc<Loaded>) -> Task<Desktop> {
        let Loaded {
            id,
            token,
            result,
            journal_warning,
        } = Arc::unwrap_or_clone(loaded);
        if self
            .terminal_agents
            .pending_chat_handoffs
            .get(&id)
            .is_none_or(|pending| pending.token != token)
        {
            return Task::none();
        }
        let Some(pending) = self.terminal_agents.pending_chat_handoffs.remove(&id) else {
            return Task::none();
        };
        if !pending.permit.load(Ordering::Acquire)
            || !self.pending_handoff_is_current(&id, &pending)
        {
            self.finish_chat_offer(
                pending.ticket,
                false,
                Some("The original terminal or AI Chat review changed.".into()),
            );
            return Task::none();
        }
        let Some(owner) = self.handoff_owner(pending.source) else {
            self.finish_chat_offer(
                pending.ticket,
                false,
                Some("The original terminal or AI Chat is unavailable.".into()),
            );
            return Task::none();
        };
        let opened = result.and_then(|record| {
            let record = if pending.library_revision == self.saved_chat_library_revision {
                record
            } else {
                Box::new(store::load(&pending.root, &record.summary.key).map_err(|_| {
                    "The saved chat changed while opening. Continue again to load its current copy."
                        .to_owned()
                })?)
            };
            self.record_saved_chat(&record.summary);
            let redactor = self.handoff_redactor(pending.source, pending.learned_sudo.as_ref())?;
            let origin = Origin {
                owner,
                session: pending.source.session,
                stream: pending.source.stream,
                id,
            };
            self.install_agent_terminal_chat(
                origin,
                Seed::Saved(pending.root, record),
                &pending.question,
                redactor,
                pending.learned_sudo.is_some(),
            )
        });
        match opened {
            Ok((task, staged)) => {
                self.agent_chat_staged(owner, pending.ticket, staged);
                if journal_warning {
                    self.notice(
                        owner,
                        "Chat opened. Its journal copy could not be updated.".into(),
                    );
                }
                task
            }
            Err(error) => self.agent_chat_error(owner, pending.ticket, error),
        }
    }

    fn agent_chat_staged(&mut self, owner: window::Id, ticket: Option<Ticket>, staged: bool) {
        if staged {
            self.finish_chat_offer(ticket, true, None);
        } else {
            let _ = self.agent_chat_error(
                owner,
                ticket,
                "Chat already has a draft or a reviewed request. Your follow-up remains here."
                    .into(),
            );
        }
    }

    fn agent_chat_error(
        &mut self,
        owner: window::Id,
        ticket: Option<Ticket>,
        error: String,
    ) -> Task<Desktop> {
        if ticket.is_some() {
            self.finish_chat_offer(ticket, false, Some(error));
        } else {
            self.notice(owner, error);
        }
        Task::none()
    }

    fn finish_chat_offer(&mut self, ticket: Option<Ticket>, staged: bool, error: Option<String>) {
        if let Some(ticket) = ticket
            && let Some(run) = self
                .terminal_agents
                .runs
                .get_mut(&ticket.window)
                .filter(|run| run.epoch == ticket.epoch)
        {
            run.chat_handoff_pending = false;
            if staged {
                run.chat_handoff_dismissed = true;
                run.chat_follow_up.clear();
            }
            if let Some(error) = error {
                run.notice = error;
            }
        }
    }
}
