//! Authority and storage routing for Agent cards and their saved Chat copies.

use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::{Receiver, TryRecvError};
use iced::widget::{button, column, row, text, text_editor};
use iced::{Element, Length, Task, window};
use jiff::Timestamp;

use crate::{
    session::SessionId,
    session_journal::{
        Capture, Status,
        agent_chat::{self, Card, Snapshot},
        viewer::Source,
    },
    terminal_logging::{Redactor, Secrets},
};

use super::{
    Message as Desktop, Workspace,
    saved_chats::store,
    terminal_ai::{
        agent_follow_up::{StepKind, StepStatus},
        mask, metadata,
    },
    tool_tabs::Kind,
};

const WAIT: Duration = Duration::from_secs(3);
const MAX_TRANSCRIPT_TABS: usize = 16;

#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Synchronizations {
    pending: HashMap<window::Id, Synchronization>,
}

struct Synchronization {
    config: Arc<crate::ai_manager::Configuration>,
    policy: crate::terminal_logging::Policy,
    key: store::Key,
    targets: Vec<SyncTarget>,
}

struct SyncTarget {
    capture: Weak<Capture>,
    permit: Arc<AtomicBool>,
}

impl Synchronization {
    fn revoke(&self) {
        for target in &self.targets {
            target.permit.store(false, Ordering::Release);
        }
    }
}

impl Drop for Synchronization {
    fn drop(&mut self) {
        self.revoke();
    }
}

// A timed out/dropped worker cannot leave a queued write authorized while its
// completion waits for the UI. Workspace also owns every flag for revocation.
struct WorkerPermits(Vec<Arc<AtomicBool>>);

impl Drop for WorkerPermits {
    fn drop(&mut self) {
        for permit in &self.0 {
            permit.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone)]
pub(super) struct AgentJournal {
    capture: Weak<Capture>,
    source: Source,
    card: Card,
}

impl AgentJournal {
    pub(super) fn with_card(mut self, card: Card) -> Self {
        self.card = card;
        self
    }

    pub(super) fn reference(&self) -> store::AgentJournalRef {
        store::AgentJournalRef {
            journal_id: self.source.id().to_owned(),
            entry_id: self.card.id.clone(),
        }
    }

    pub(super) fn queue_card(&self) {
        if let Some(capture) = self.capture.upgrade() {
            let _ = capture.ensure_agent_card(self.card.clone());
        }
    }

    pub(super) fn ensure_card(&self, permit: &Arc<AtomicBool>) -> io::Result<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            authorized(permit)?;
            if let Some(capture) = self.capture.upgrade() {
                match capture.status() {
                    Status::Recording => match capture
                        .ensure_agent_card_checked(self.card.clone(), Some(Arc::clone(permit)))
                    {
                        Ok(reply) => return wait_reply(reply, deadline, permit),
                        Err(error) if busy(&error) => {}
                        Err(error) => return Err(error),
                    },
                    Status::Finishing => {}
                    _ => return agent_chat::verify_card(&self.source, &self.card.id),
                }
            } else {
                return agent_chat::verify_card(&self.source, &self.card.id);
            }
            wait_turn(deadline)?;
        }
    }

    pub(super) fn link(
        &self,
        record: &store::Record,
        redactor: &Redactor,
        permit: &Arc<AtomicBool>,
    ) -> io::Result<()> {
        if record.agent_journal() != Some(&self.reference()) {
            return Ok(());
        }
        let snapshot = snapshot(record, redactor, Timestamp::now())?;
        let deadline = Instant::now() + WAIT;
        loop {
            authorized(permit)?;
            if let Some(capture) = self.capture.upgrade() {
                match capture.status() {
                    Status::Recording => match capture.link_agent_chat_checked(
                        self.card.id.clone(),
                        snapshot.clone(),
                        Arc::clone(permit),
                    ) {
                        Ok(reply) => return wait_reply(reply, deadline, permit),
                        Err(error) if busy(&error) => {}
                        Err(error) => return Err(error),
                    },
                    Status::Finishing => {}
                    _ => break,
                }
            } else {
                break;
            }
            wait_turn(deadline)?;
        }
        agent_chat::link_closed(&self.source, &self.card.id, snapshot, || authorized(permit))
    }
}

pub(super) fn authorized(permit: &AtomicBool) -> io::Result<()> {
    if permit.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Agent Chat review expired",
        ))
    }
}

fn busy(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::BrokenPipe
    )
}

fn wait_turn(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the Agent journal write timed out",
        ));
    }
    std::thread::sleep(Duration::from_millis(12));
    Ok(())
}

fn wait_reply(
    reply: Receiver<Result<(), String>>,
    deadline: Instant,
    permit: &AtomicBool,
) -> io::Result<()> {
    loop {
        authorized(permit)?;
        match reply.try_recv() {
            Ok(result) => return result.map_err(io::Error::other),
            Err(TryRecvError::Closed) => {
                return Err(io::Error::other("the Agent journal writer stopped"));
            }
            Err(TryRecvError::Empty) => wait_turn(deadline)?,
        }
    }
}

fn snapshot(
    record: &store::Record,
    redactor: &Redactor,
    captured_at: Timestamp,
) -> io::Result<Snapshot> {
    let masked = |value: &str| mask::prompt(redactor, value).map_err(io::Error::other);
    let mut snapshot = Snapshot {
        chat_id: record.summary.key.id.clone(),
        title: metadata(&masked(&record.summary.title)?, 140),
        captured_at,
        frozen: false,
        messages: Vec::new(),
    };
    for (index, turn) in record.turns.iter().enumerate() {
        let steps = turn
            .agent_steps()
            .iter()
            .map(|step| {
                Ok(agent_chat::Step {
                    kind: match step.kind {
                        StepKind::Command => "ACTION",
                        StepKind::Warning => "ERROR",
                        StepKind::Question => "QUESTION",
                        _ => "MESSAGE",
                    },
                    status: step.status.map(|status| match status {
                        StepStatus::Running => "RUNNING",
                        StepStatus::Completed => "COMPLETED",
                        StepStatus::Cancelled => "CANCELLED",
                        StepStatus::Failed => "FAILED",
                    }),
                    title: masked(&step.title)?,
                    summary: masked(&step.detail)?,
                    command: step.command.as_deref().map(masked).transpose()?,
                    exit_code: step.exit_code.map(|code| code as i32),
                    output: masked(&step.output.join("\n"))?,
                    elapsed_seconds: step.elapsed_seconds,
                    truncated: step.truncated,
                })
            })
            .collect::<io::Result<_>>()?;
        snapshot.messages.extend([
            agent_chat::Message {
                role: agent_chat::Role::User,
                content: masked(turn.question())?,
                profile: String::new(),
                steps: Vec::new(),
            },
            agent_chat::Message {
                role: agent_chat::Role::Assistant,
                content: masked(turn.answer())?,
                profile: String::new(),
                steps,
            },
        ]);
        while snapshot.validate().is_err() && snapshot.messages.len() > 4 {
            snapshot.messages.drain(2..4);
        }
        if let Err(error) = snapshot.validate() {
            if index == 0 {
                return Err(error);
            }
            snapshot.messages.truncate(snapshot.messages.len() - 2);
        }
    }
    snapshot.validate()?;
    Ok(snapshot)
}

#[derive(Clone)]
struct Opening {
    owner: window::Id,
    source: Source,
    entry: String,
    preferred_viewer: Option<window::Id>,
    root: Option<PathBuf>,
    library_revision: u64,
}

#[derive(Clone)]
pub struct Opened {
    request: Opening,
    record: Option<store::Record>,
    transcript: Option<agent_chat::Transcript>,
}

#[derive(Clone)]
pub enum Message {
    Opened(Arc<Opened>),
    Synchronized(window::Id),
    Select(window::Id, text_editor::Action),
    Copy(window::Id),
    Close(window::Id),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opened(..) => f.write_str("Opened"),
            Self::Synchronized(_) => f.write_str("Synchronized"),
            Self::Select(..) => f.write_str("Select"),
            Self::Copy(_) => f.write_str("Copy"),
            Self::Close(_) => f.write_str("Close"),
        }
    }
}

pub(super) struct TranscriptTab {
    title: String,
    content: text_editor::Content,
}

impl TranscriptTab {
    fn new(transcript: agent_chat::Transcript) -> Self {
        Self {
            title: if transcript.title.is_empty() {
                "Recorded AI Chat".into()
            } else {
                transcript.title
            },
            content: text_editor::Content::with_text(&transcript.text),
        }
    }

    pub(super) fn title(&self) -> &str {
        &self.title
    }
}

async fn worker<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (send, reply) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-journal-chat".into())
        .spawn(move || {
            let _ = send.try_send(run());
        })
        .ok()?;
    reply.recv().await.ok()
}

impl Workspace {
    pub(super) fn agent_journal_for(
        &self,
        session: SessionId,
        stream: u64,
        card: Card,
    ) -> Option<AgentJournal> {
        card.validate().ok()?;
        let capture = self.journal_agent_capture(session, stream)?;
        Some(AgentJournal {
            source: Source::capture(&capture),
            capture: Arc::downgrade(&capture),
            card,
        })
    }

    pub(super) fn start_journal_agent_chat(
        &mut self,
        owner: window::Id,
        source: Source,
        entry: String,
        preferred_viewer: Option<window::Id>,
    ) -> Task<Desktop> {
        if self.exiting
            || self.scheduler_draining
            || !self.journal_allowed()
            || !self.palette_can_open(owner)
            || self.palette.is_open(owner)
            || !agent_chat::valid_reference(&entry)
        {
            return Task::none();
        }
        let root = self
            .ai_chat_configuration()
            .and_then(|config| config.saved_root().map(PathBuf::from));
        let request = Opening {
            owner,
            source,
            entry,
            preferred_viewer,
            root,
            library_revision: self.saved_chat_library_revision,
        };
        Task::perform(
            async move {
                let work = request.clone();
                let (record, transcript) = worker(move || {
                    let Ok(chat) = agent_chat::read_linked_chat(&work.source, &work.entry) else {
                        return (None, None);
                    };
                    let record = work.root.as_deref().zip(chat.chat_id.as_deref()).and_then(
                        |(root, chat_id)| {
                            let record = store::load_by_id(root, chat_id).ok()?;
                            let link = record.agent_journal()?;
                            (link.journal_id == work.source.id() && link.entry_id == work.entry)
                                .then_some(record)
                        },
                    );
                    (record, chat.transcript)
                })
                .await
                .unwrap_or_default();
                Arc::new(Opened {
                    request,
                    record,
                    transcript,
                })
            },
            |opened| Desktop::JournalAgentChat(Message::Opened(opened)),
        )
    }

    pub(super) fn update_journal_agent_chat(&mut self, message: Message) -> Task<Desktop> {
        let opened = match message {
            Message::Opened(opened) => opened,
            Message::Synchronized(ticket) => {
                self.journal_chat_sync.pending.remove(&ticket);
                return Task::none();
            }
            Message::Close(id) => {
                return if self.journal_chat_transcripts.contains_key(&id) {
                    self.request_hosted_tool_close(id)
                } else {
                    Task::none()
                };
            }
            Message::Select(id, action) => {
                if !action.is_edit()
                    && self.journal_transcript_interactive(id)
                    && let Some(tab) = self.journal_chat_transcripts.get_mut(&id)
                {
                    tab.content.perform(action);
                }
                return Task::none();
            }
            Message::Copy(id) => {
                return if self.journal_transcript_interactive(id)
                    && let Some(selection) = self
                        .journal_chat_transcripts
                        .get(&id)
                        .and_then(|tab| tab.content.selection())
                {
                    self.write_clipboard(selection)
                } else {
                    Task::none()
                };
            }
        };
        let Opened {
            request,
            record,
            transcript,
        } = Arc::unwrap_or_clone(opened);
        if self.exiting
            || self.scheduler_draining
            || !self.journal_allowed()
            || !self.palette_can_open(request.owner)
            || self.palette.is_open(request.owner)
            || request.preferred_viewer.is_some_and(|viewer| {
                !self.journal_agent_chat_request_current(
                    viewer,
                    request.owner,
                    &request.source,
                    &request.entry,
                )
            })
        {
            return Task::none();
        }
        if let Some(record) = record
            && request.library_revision == self.saved_chat_library_revision
            && !self.terminal_chat_write_pending(&record.summary.key)
            && let Some(root) = request.root
            && self
                .ai_chat_configuration()
                .is_some_and(|config| config.saved_root() == Some(root.as_path()))
        {
            return self.open_stored_terminal_chat(request.owner, root, record);
        }
        let Some(transcript) = transcript else {
            self.notice(
                request.owner,
                "The recorded AI chat is no longer available.".into(),
            );
            return Task::none();
        };
        if self.journal_chat_transcripts.len() >= MAX_TRANSCRIPT_TABS {
            self.notice(
                request.owner,
                "Close a recorded AI Chat tab before opening another.".into(),
            );
            return Task::none();
        }
        let id = window::Id::unique();
        self.journal_chat_transcripts
            .insert(id, TranscriptTab::new(transcript));
        self.host_tool(request.owner, Kind::JournalChatTranscript, id)
    }

    fn journal_transcript_interactive(&self, id: window::Id) -> bool {
        self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && !self.hosted_close_locks(id)
            && self.hosted_tool_owner(id).is_some_and(|owner| {
                self.palette_can_open(owner)
                    && !self.palette.is_open(owner)
                    && matches!(self.windows.get(&owner).and_then(super::Window::active_entry),
                        Some(super::TabEntry::Tool(tool)) if tool.instance == id)
            })
    }

    pub(super) fn journal_transcript_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let tab = self.journal_chat_transcripts.get(&id)?;
        let close = button("Close").on_press(Message::Close(id));
        let body = if self.journal_allowed() {
            column![
                row![
                    text("Recorded AI Chat · Read only")
                        .size(17)
                        .width(Length::Fill),
                    close
                ]
                .spacing(8),
                text_editor(&tab.content)
                    .on_action(move |action| Message::Select(id, action))
                    .key_binding(
                        move |key| match text_editor::Binding::from_key_press(key)? {
                            text_editor::Binding::Copy | text_editor::Binding::Cut => {
                                Some(text_editor::Binding::Custom(Message::Copy(id)))
                            }
                            text_editor::Binding::Paste => None,
                            binding => Some(binding),
                        }
                    )
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .height(Length::Fill),
            ]
        } else {
            column![
                text("Session journals are unavailable under the current policy."),
                close
            ]
        };
        let body: Element<'_, Message> = body.padding(14).spacing(8).into();
        Some(body.map(Desktop::JournalAgentChat))
    }

    pub(super) fn revalidate_agent_journal_synchronizations(&mut self) {
        let config = self.ai_chat_configuration();
        let allowed = !self.exiting && !self.scheduler_draining && self.journal_allowed();
        let captures = self.journal_metadata_captures();
        for work in self.journal_chat_sync.pending.values() {
            if !allowed
                || !config
                    .as_ref()
                    .is_some_and(|config| Arc::ptr_eq(config, &work.config))
                || !self
                    .clipboard
                    .policy
                    .terminal_logging
                    .same_redactions(&work.policy)
            {
                work.revoke();
                continue;
            }
            for target in &work.targets {
                if !target.capture.upgrade().is_some_and(|expected| {
                    captures
                        .iter()
                        .any(|capture| Arc::ptr_eq(capture, &expected))
                }) {
                    target.permit.store(false, Ordering::Release);
                }
            }
        }
    }

    pub(super) fn synchronize_agent_journal_chat(&mut self, key: &store::Key) -> Task<Desktop> {
        // Save/Rename acknowledges a new revision. An older queued copy must
        // lose publication authority before this one is prepared.
        for work in self.journal_chat_sync.pending.values() {
            if work.key.same_chat(key) {
                work.revoke();
            }
        }
        let captures: Vec<_> = self
            .journal_metadata_captures()
            .into_iter()
            .filter(|capture| capture.status() == Status::Recording)
            .collect();
        if captures.is_empty() || !self.journal_allowed() || self.exiting || self.scheduler_draining
        {
            return Task::none();
        }
        let Some((config, root)) = self.ai_chat_configuration().and_then(|config| {
            let root = config.saved_root().map(PathBuf::from)?;
            Some((config, root))
        }) else {
            return Task::none();
        };
        let policy = self.clipboard.policy.terminal_logging.clone();
        let key = key.clone();
        let ticket = window::Id::unique();
        let targets: Vec<_> = captures
            .iter()
            .map(|capture| SyncTarget {
                capture: Arc::downgrade(capture),
                permit: Arc::new(AtomicBool::new(true)),
            })
            .collect();
        let captures: Vec<_> = captures
            .into_iter()
            .zip(targets.iter().map(|target| Arc::clone(&target.permit)))
            .collect();
        self.journal_chat_sync.pending.insert(
            ticket,
            Synchronization {
                config,
                policy: policy.clone(),
                key: key.clone(),
                targets,
            },
        );
        Task::perform(
            async move {
                let permits = WorkerPermits(
                    captures
                        .iter()
                        .map(|(_, permit)| Arc::clone(permit))
                        .collect(),
                );
                let _ = worker(move || -> io::Result<()> {
                    let _permits = permits;
                    let captured_at = Timestamp::now();
                    let record = store::load(&root, &key)?;
                    let Some(link) = record.agent_journal() else {
                        return Ok(());
                    };
                    let Some((capture, permit)) = captures
                        .into_iter()
                        .find(|(capture, _)| capture.id() == link.journal_id)
                    else {
                        return Ok(());
                    };
                    authorized(&permit)?;
                    let redactor = Redactor::for_journal(Secrets::default().seal()?, policy);
                    let snapshot = snapshot(&record, &redactor, captured_at)?;
                    let deadline = Instant::now() + WAIT;
                    while capture.status() == Status::Recording {
                        authorized(&permit)?;
                        match capture.link_agent_chat_checked(
                            link.entry_id.clone(),
                            snapshot.clone(),
                            Arc::clone(&permit),
                        ) {
                            Ok(reply) => return wait_reply(reply, deadline, &permit),
                            Err(error) if busy(&error) => wait_turn(deadline)?,
                            Err(error) => return Err(error),
                        }
                    }
                    Ok(())
                })
                .await;
            },
            move |()| Desktop::JournalAgentChat(Message::Synchronized(ticket)),
        )
    }
}
