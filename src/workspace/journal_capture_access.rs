//! Owns manual and automatic journal captures for terminal tabs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use async_channel::{Receiver, bounded};
use iced::futures::executor::block_on;
use iced::widget::{button, checkbox, column, pane_grid, row, text, text_editor};
use iced::{Alignment, Element, Task, window};
use kortty_recording::{RecordedCell, RecordedScreen};

use crate::session::{InputError, JournalAttachment, SessionId};
use crate::session_journal::markers::Compiled as MarkerRules;
use crate::session_journal::viewer::Source as ViewerSource;
use crate::session_journal::{
    Capture, MAX_RECORD_BYTES, Metadata, PublishedScreenshot, RecordKind, Status,
};

use super::journal_access::entry_editor::edit_text_limited;
use super::journal_note_translation as note_translation;
use super::{Message as Desktop, PaneSession, Tab, Window, Workspace};

#[derive(Clone, Debug)]
pub enum Message {
    Toggle(window::Id, u64),
    Screenshot(window::Id, u64),
    Note(window::Id, u64, u64, Note),
    NoteTranslated(u64, u64, note_translation::Event),
    Open(window::Id, u64, u64),
    Started(u64, u64),
    Attached(u64, u64, SessionId, Result<(), String>),
    Ended(u64, u64),
    Finalized(u64, u64),
    SourceRetired(u64, u64),
    ScreenshotSaved(
        window::Id,
        u64,
        u64,
        Result<Option<(u64, PublishedScreenshot)>, String>,
    ),
    AutomaticScreenshotAi(window::Id, u64, u64, bool),
}

#[derive(Clone)]
pub enum Note {
    Edit,
    Input(text_editor::Action),
    Translate(note_translation::Event),
    Save,
    Cancel,
}

impl std::fmt::Debug for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Edit => "Edit",
            Self::Input(_) => "Input",
            Self::Translate(_) => "Translate",
            Self::Save => "Save",
            Self::Cancel => "Cancel",
        })
    }
}

#[derive(Default)]
struct QuickNote {
    value: String,
    content: text_editor::Content,
    translation: note_translation::State,
    problem: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabJournalAction {
    Start(Option<u64>),
    Stop(u64),
    Screenshot(Option<u64>),
    Note(Option<u64>),
}

impl TabJournalAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Start(_) => "Start journal",
            Self::Stop(_) => "Stop journal",
            Self::Screenshot(_) => "Add screenshot",
            Self::Note(_) => "Add note",
        }
    }
}

pub(super) struct DockSource {
    pub tab: u64,
    pub run: u64,
    pub title: String,
    pub capture: Arc<Capture>,
}

pub(super) struct Entry {
    run: u64,
    pane: pane_grid::Pane,
    title: String,
    phase: Phase,
    notice: Option<String>,
    note: Option<QuickNote>,
    screenshot: Option<Receiver<()>>,
    automatic_screenshot_ai: Option<u64>,
    automatic_screenshot_ai_disabled: bool,
    automatic: Option<(SessionId, u64)>,
    capture_input: bool,
}

enum Phase {
    Starting(SessionId, Startup),
    Active {
        capture: Arc<Capture>,
        source: Option<Binding>,
        retiring: Option<Receiver<()>>,
    },
    Finishing(Arc<Capture>, Receiver<()>),
    Finished(Option<Arc<Capture>>),
}

struct Binding {
    id: SessionId,
    generation: u64,
    attachment: JournalAttachment,
    note_authority: Arc<()>,
    ready: bool,
}

struct Startup {
    result: Arc<Mutex<StartResult>>,
    done: Receiver<()>,
}

enum StartResult {
    Pending,
    Cancelled,
    Ready(Result<Arc<Capture>, String>),
}

impl Startup {
    fn spawn(
        root: PathBuf,
        metadata: Metadata,
        markers: Arc<MarkerRules>,
    ) -> std::io::Result<Self> {
        let (signal, done) = bounded(1);
        let result = Arc::new(Mutex::new(StartResult::Pending));
        let worker_result = Arc::clone(&result);
        thread::Builder::new()
            .name("kortty-journal-start".into())
            .spawn(move || {
                let started = Capture::start_configured(root, metadata, markers)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let mut slot = worker_result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if matches!(*slot, StartResult::Cancelled) {
                    drop(slot);
                    if let Ok(capture) = started {
                        let _ = capture.finish().recv_blocking();
                    }
                } else {
                    *slot = StartResult::Ready(started);
                }
                drop(signal);
            })?;
        Ok(Self { result, done })
    }

    fn take(&self) -> Option<Result<Arc<Capture>, String>> {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match std::mem::replace(&mut *result, StartResult::Cancelled) {
            StartResult::Ready(result) => Some(result),
            _ => None,
        }
    }
}

impl Entry {
    fn metadata_capture(&self) -> Option<Arc<Capture>> {
        match &self.phase {
            Phase::Active { capture, .. } | Phase::Finishing(capture, _) => {
                Some(Arc::clone(capture))
            }
            Phase::Finished(capture) => capture.clone(),
            Phase::Starting(_, startup) => {
                let result = startup
                    .result
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                match &*result {
                    StartResult::Ready(Ok(capture)) => Some(Arc::clone(capture)),
                    _ => None,
                }
            }
        }
    }
    fn capturing(&self) -> bool {
        matches!(self.phase, Phase::Starting(..) | Phase::Active { .. })
    }

    fn recording(&self) -> Option<&Arc<Capture>> {
        match &self.phase {
            Phase::Active { capture, .. } if capture.status() == Status::Recording => Some(capture),
            _ => None,
        }
    }

    fn writable(&self, tab: &Tab) -> Option<&Capture> {
        let expected = *tab.grid.get(self.pane)?;
        if matches!(&self.phase, Phase::Active { source: Some(source), .. }
            if source.ready && source.id == expected)
        {
            self.recording().map(Arc::as_ref)
        } else {
            None
        }
    }

    fn finish(&mut self) -> Vec<Receiver<()>> {
        self.note = None;
        match std::mem::replace(&mut self.phase, Phase::Finished(None)) {
            Phase::Starting(_, startup) => {
                let mut done = vec![startup.done.clone()];
                if let Some(Ok(capture)) = startup.take() {
                    done.push(capture.finish());
                }
                self.notice = Some("Journal start cancelled.".into());
                done
            }
            Phase::Active {
                capture,
                source,
                retiring,
            } => {
                let done = finish_capture(
                    &capture,
                    source.map(|source| source.attachment),
                    retiring,
                    self.screenshot.take(),
                );
                self.phase = Phase::Finishing(capture, done.clone());
                vec![done]
            }
            phase => {
                self.phase = phase;
                Vec::new()
            }
        }
    }
}

fn finish_capture(
    capture: &Arc<Capture>,
    attachment: Option<JournalAttachment>,
    retiring: Option<Receiver<()>>,
    screenshot: Option<Receiver<()>>,
) -> Receiver<()> {
    if attachment.is_none() && retiring.is_none() && screenshot.is_none() {
        return capture.finish();
    }
    let (signal, done) = bounded(1);
    let worker_capture = Arc::clone(capture);
    let worker = thread::Builder::new()
        .name("kortty-journal-stop".into())
        .spawn(move || {
            if let Some(attachment) = attachment {
                block_on(attachment.detach());
            }
            if let Some(retiring) = retiring {
                let _ = retiring.recv_blocking();
            }
            if let Some(screenshot) = screenshot {
                let _ = screenshot.recv_blocking();
            }
            let _ = worker_capture.finish().recv_blocking();
            drop(signal);
        });
    if worker.is_ok() {
        done
    } else {
        capture.finish()
    }
}

fn when_closed(done: Receiver<()>, message: Message) -> Task<Desktop> {
    Task::future(async move {
        let _ = done.recv().await;
        Desktop::JournalCapture(message)
    })
}

impl Workspace {
    pub(super) fn journal_metadata_captures(&self) -> Vec<Arc<Capture>> {
        self.journal_captures
            .values()
            .filter_map(Entry::metadata_capture)
            .chain(self.retired_journal_captures.iter().cloned())
            .collect()
    }

    pub(super) fn live_journal_entry_secrets(
        &self,
        capture: &Arc<Capture>,
    ) -> Option<crate::terminal_logging::RedactionSecrets> {
        self.journal_captures.values().find_map(|entry| {
            let Phase::Active {
                capture: active,
                source: Some(source),
                ..
            } = &entry.phase
            else {
                return None;
            };
            if !Arc::ptr_eq(capture, active) || !source.ready {
                return None;
            }
            self.sessions
                .get(&source.id)?
                .session
                .journal_redaction_secrets()
        })
    }

    pub(super) fn live_journal_note_authority(
        &self,
        capture: &Arc<Capture>,
    ) -> Option<impl Fn() -> bool + use<>> {
        let binding = self.journal_captures.values().find_map(|entry| {
            let Phase::Active {
                capture: active,
                source: Some(binding),
                retiring: None,
            } = &entry.phase
            else {
                return None;
            };
            (Arc::ptr_eq(capture, active) && binding.ready).then_some(binding)
        })?;
        let pane = self.sessions.get(&binding.id)?;
        if pane.stream_generation != binding.generation {
            return None;
        }
        let terminal = pane.session.journal_note_authority()?;
        let binding = Arc::downgrade(&binding.note_authority);
        let capture = Arc::downgrade(capture);
        Some(move || {
            binding.strong_count() != 0
                && capture
                    .upgrade()
                    .is_some_and(|capture| capture.status() == Status::Recording)
                && terminal()
        })
    }

    pub(super) fn prune_retired_journal_captures(&mut self) {
        self.retired_journal_captures
            .retain(|capture| !capture.done().is_closed());
    }

    pub(super) fn journal_metadata_title_saved(
        &mut self,
        aliases: &[PathBuf],
        expected_id: &str,
        title: &str,
    ) {
        for entry in self.journal_captures.values_mut() {
            if entry.metadata_capture().is_some_and(|capture| {
                capture.id() == expected_id
                    && aliases.iter().any(|path| path == capture.directory())
            }) {
                entry.title = title.to_owned();
            }
        }
        for window in self.windows.values_mut() {
            window
                .journal_dock
                .update_journal_title(aliases, expected_id, title);
        }
    }
    fn is_active_journal_tab(&self, owner: window::Id, tab: u64) -> bool {
        self.windows.get(&owner).and_then(Window::active_tab_id) == Some(tab)
    }

    fn journal_tab(&self, id: u64) -> Option<&Tab> {
        self.windows
            .values()
            .flat_map(Window::terminal_tabs)
            .find(|tab| tab.id == id)
    }

    fn journal_tab_in(&self, owner: window::Id, id: u64) -> Option<&Tab> {
        self.windows
            .get(&owner)?
            .terminal_tabs()
            .find(|tab| tab.id == id)
    }

    fn dock_journal_entry(&self, tab: &Tab) -> Option<&Entry> {
        let entry = self.journal_captures.get(&tab.id)?;
        (self.journal_allowed() && !tab.incognito && tab.primary == entry.pane).then_some(entry)
    }

    fn dock_journal_target(&self, owner: window::Id, id: u64, run: u64) -> Option<(&Tab, &Entry)> {
        let tab = self.journal_tab_in(owner, id)?;
        let entry = self
            .dock_journal_entry(tab)
            .filter(|entry| entry.run == run)?;
        Some((tab, entry))
    }

    pub(super) fn active_dock_journal(&self, owner: window::Id) -> Option<DockSource> {
        let tab = self.windows.get(&owner)?.tab()?;
        let entry = self.dock_journal_entry(tab)?;
        let capture = Arc::clone(entry.recording()?);
        Some(DockSource {
            tab: tab.id,
            run: entry.run,
            title: entry.title.clone(),
            capture,
        })
    }

    pub(in crate::workspace) fn journal_agent_capture(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Option<Arc<Capture>> {
        if self.exiting
            || self.scheduler_draining
            || self.sessions.get(&session)?.stream_generation != stream
        {
            return None;
        }
        let tab = self
            .windows
            .values()
            .flat_map(Window::terminal_tabs)
            .find(|tab| tab.grid.get(tab.primary) == Some(&session))?;
        let entry = self.dock_journal_entry(tab)?;
        match &entry.phase {
            Phase::Active {
                source: Some(source),
                retiring: None,
                ..
            } if source.ready && source.id == session && source.generation == stream => {
                entry.recording().cloned()
            }
            _ => None,
        }
    }

    pub(super) fn dock_journal_live(&self, owner: window::Id, tab: u64, run: u64) -> bool {
        self.dock_journal_target(owner, tab, run)
            .is_some_and(|(_, entry)| entry.recording().is_some())
    }

    pub(super) fn dock_note_enabled(&self, owner: window::Id, tab: u64, run: u64) -> bool {
        self.dock_journal_target(owner, tab, run)
            .is_some_and(|(tab, entry)| entry.writable(tab).is_some())
    }

    pub(super) fn dock_screenshot_enabled(&self, owner: window::Id, tab: u64, run: u64) -> bool {
        self.dock_journal_target(owner, tab, run)
            .and_then(|(tab, entry)| self.journal_screenshot_pane(tab, entry))
            .is_some()
    }

    fn journal_source(&self, tab: &Tab) -> Option<(SessionId, &PaneSession)> {
        if tab.incognito || !self.journal_allowed() {
            return None;
        }
        let id = *tab.grid.get(tab.primary)?;
        let pane = self.sessions.get(&id)?;
        pane.session.journal_available().then_some((id, pane))
    }

    pub(super) fn journal_toggle_enabled(&self, owner: window::Id) -> bool {
        self.windows
            .get(&owner)
            .and_then(Window::tab)
            .is_some_and(|tab| self.journal_toggle_enabled_for(tab))
    }

    fn journal_toggle_enabled_for(&self, tab: &Tab) -> bool {
        if !self.journal_allowed() || tab.incognito {
            return false;
        }
        match self.journal_captures.get(&tab.id).map(|entry| &entry.phase) {
            Some(Phase::Starting(..) | Phase::Active { .. }) => true,
            Some(Phase::Finishing(..)) => false,
            _ => self.journal_source(tab).is_some(),
        }
    }

    pub(super) fn tab_journal_actions(
        &self,
        owner: window::Id,
        id: u64,
    ) -> Option<[(TabJournalAction, bool); 3]> {
        let tab = self.journal_tab_in(owner, id)?;
        if !self.journal_allowed() || tab.incognito {
            return None;
        }
        let entry = self.journal_captures.get(&id);
        let run = entry.map(|entry| entry.run);
        let toggle = entry
            .filter(|entry| entry.capturing())
            .map_or(TabJournalAction::Start(run), |entry| {
                TabJournalAction::Stop(entry.run)
            });
        Some([
            (toggle, self.journal_toggle_enabled_for(tab)),
            (
                TabJournalAction::Screenshot(run),
                run.is_some_and(|run| self.dock_screenshot_enabled(owner, id, run)),
            ),
            (
                TabJournalAction::Note(run),
                run.is_some_and(|run| self.dock_note_enabled(owner, id, run)),
            ),
        ])
    }

    pub(super) fn journal_screenshot_enabled(&self, owner: window::Id) -> bool {
        let Some(tab) = self.windows.get(&owner).and_then(Window::tab) else {
            return false;
        };
        self.dock_journal_entry(tab)
            .and_then(|entry| self.journal_screenshot_pane(tab, entry))
            .is_some()
    }

    fn journal_screenshot_pane(&self, tab: &Tab, entry: &Entry) -> Option<&PaneSession> {
        if entry.screenshot.is_some() {
            return None;
        }
        entry.writable(tab)?;
        let pane = self.sessions.get(&tab.focused_session()?)?;
        (pane
            .session
            .journal_screenshot_allowed(&self.clipboard.policy.terminal_logging, &pane.frame)
            && !pane.frame.screen.cells.is_empty())
        .then_some(pane)
    }

    pub(super) fn toggle_journal_for(&mut self, owner: window::Id, tab_id: u64) -> Task<Desktop> {
        let Some(tab) = self
            .journal_tab_in(owner, tab_id)
            .filter(|tab| self.journal_toggle_enabled_for(tab))
        else {
            return Task::none();
        };
        if self
            .journal_captures
            .get(&tab_id)
            .is_some_and(Entry::capturing)
        {
            self.stop_journal(tab_id);
            return Task::none();
        }
        self.start_journal_for(owner, tab.id, false)
    }

    fn start_journal_for(
        &mut self,
        owner: window::Id,
        tab_id: u64,
        automatic: bool,
    ) -> Task<Desktop> {
        let Some(tab) = self.journal_tab_in(owner, tab_id) else {
            return Task::none();
        };
        let journal_pane = tab.primary;
        let Some((source, pane)) = self.journal_source(tab) else {
            return Task::none();
        };
        let generation = pane.stream_generation;
        let mut config = pane.journal_config;
        if let Some(cap) = self
            .clipboard
            .policy
            .terminal_logging
            .journal_max_log_parts()
        {
            config.max_log_parts = config.max_log_parts.min(cap);
        }
        let connection_name = if let Some(ssh) = &pane.ssh {
            ssh.label()
        } else if let Some(local) = &pane.saved_local {
            local.display_name()
        } else if let Some(serial) = &pane.frame.serial {
            serial.device.display().to_string()
        } else {
            "Local shell".into()
        };
        let title = tab
            .custom_title
            .clone()
            .unwrap_or_else(|| connection_name.clone());
        let mut metadata = Metadata {
            title: title.clone(),
            title_manually_set: tab.custom_title.is_some(),
            connection_name,
            seeded: !automatic,
            connection_config: Some(config),
            log_format: self
                .clipboard
                .policy
                .terminal_logging
                .journal_log_format()
                .unwrap_or(self.window_settings.session_journal.log_format),
            ..Metadata::default()
        };
        if let Some(ssh) = &pane.ssh {
            metadata.host = ssh.host().into();
            metadata.port = Some(ssh.port());
            metadata.username = ssh.username().into();
        }
        let Some(metadata) = pane
            .session
            .journal_metadata(metadata, &self.clipboard.policy.terminal_logging)
        else {
            self.notice(
                owner,
                "Journal metadata could not be prepared safely.".into(),
            );
            return Task::none();
        };
        let title = metadata.title.clone();
        let root = self.journal_directory();
        let markers = self.journal_marker_rules();
        let startup = match root.and_then(|root| Startup::spawn(root, metadata, markers)) {
            Ok(startup) => startup,
            Err(error) => {
                self.notice(owner, format!("Could not start session journal ({error})."));
                return Task::none();
            }
        };
        let run = self.allocate();
        let automatic_screenshot_ai = self
            .journal_automatic_screenshot_analysis_enabled()
            .then(|| self.allocate());
        let task = when_closed(startup.done.clone(), Message::Started(tab_id, run));
        self.journal_captures.insert(
            tab_id,
            Entry {
                run,
                pane: journal_pane,
                title,
                phase: Phase::Starting(source, startup),
                notice: None,
                note: None,
                screenshot: None,
                automatic_screenshot_ai,
                automatic_screenshot_ai_disabled: false,
                automatic: automatic.then_some((source, generation)),
                capture_input: config.capture_input,
            },
        );
        task
    }

    fn journal_started(&mut self, tab_id: u64, run: u64) -> Task<Desktop> {
        let markers = self.journal_marker_rules();
        let Some(entry) = self
            .journal_captures
            .get_mut(&tab_id)
            .filter(|entry| entry.run == run)
        else {
            return Task::none();
        };
        let Phase::Starting(source, startup) = &entry.phase else {
            return Task::none();
        };
        let source = *source;
        let capture = match startup.take() {
            Some(Ok(capture)) => capture,
            result => {
                entry.phase = Phase::Finished(None);
                entry.notice = Some(match result {
                    Some(Err(error)) => error,
                    _ => "Session journal startup stopped.".into(),
                });
                return Task::none();
            }
        };
        capture.set_marker_rules(markers);
        let done = capture.done();
        let summary_config = capture.summary_config();
        let summary_capture = Arc::clone(&capture);
        let summary_title = entry.title.clone();
        entry.phase = Phase::Active {
            capture,
            source: None,
            retiring: None,
        };
        if let Some(config) = summary_config
            && let Some(secrets) = self
                .sessions
                .get(&source)
                .and_then(|pane| pane.session.journal_redaction_secrets())
        {
            let redactor = Arc::new(crate::terminal_logging::Redactor::for_journal(
                secrets,
                self.clipboard.policy.terminal_logging.clone(),
            ));
            self.register_journal_summaries(summary_capture, config, redactor, summary_title);
        }
        Task::batch([
            when_closed(done, Message::Ended(tab_id, run)),
            self.attach_journal_source(tab_id, Some(source)),
        ])
    }

    fn attach_journal_source(&mut self, tab_id: u64, original: Option<SessionId>) -> Task<Desktop> {
        let Some(entry) = self.journal_captures.get(&tab_id) else {
            return Task::none();
        };
        let Some(capture) = entry.recording() else {
            return Task::none();
        };
        let Some((id, pane)) = self
            .journal_tab(tab_id)
            .and_then(|tab| self.journal_source(tab))
            .filter(|(id, pane)| {
                original.is_none_or(|original| original == *id)
                    && entry.automatic.is_none_or(|(source, generation)| {
                        source == *id && generation == pane.stream_generation
                    })
            })
        else {
            if original.is_some() {
                self.fail_journal(
                    tab_id,
                    "The original terminal session ended during startup.".into(),
                );
            }
            return Task::none();
        };
        let sink = capture.sink();
        let generation = pane.stream_generation;
        let result = pane.session.attach_journal_configured(
            sink,
            self.clipboard.policy.terminal_logging.clone(),
            original.is_some() && entry.automatic.is_none(),
            entry.capture_input,
        );
        let (attachment, reply) = match result {
            Ok(result) => result,
            Err(error) => {
                let message = match error {
                    InputError::Busy => "The terminal is busy. Start the journal again.",
                    InputError::Closed => "The terminal session is closed.",
                    InputError::TooLarge => "The journal request exceeds the terminal limit.",
                };
                self.fail_journal(tab_id, message.into());
                return Task::none();
            }
        };
        let entry = self
            .journal_captures
            .get_mut(&tab_id)
            .expect("capture still exists");
        if let Phase::Active { source, .. } = &mut entry.phase {
            *source = Some(Binding {
                id,
                generation,
                attachment,
                note_authority: Arc::new(()),
                ready: false,
            });
        }
        let run = entry.run;
        Task::future(async move {
            let result = reply
                .recv()
                .await
                .map_err(|_| "The terminal session ended before recording could attach.".to_owned())
                .and_then(|result| result.map_err(|error| error.to_string()));
            Desktop::JournalCapture(Message::Attached(tab_id, run, id, result))
        })
    }

    pub(super) fn reconcile_journal_sources(&mut self) -> Task<Desktop> {
        let mut attach = Vec::new();
        let mut retire = Vec::new();
        let mut stop = Vec::new();
        for (&id, entry) in &self.journal_captures {
            let Some(tab) = self.journal_tab(id).filter(|tab| {
                tab.primary == entry.pane && !tab.incognito && self.journal_allowed()
            }) else {
                if entry.capturing() {
                    stop.push(id);
                }
                continue;
            };
            let primary = tab.grid.get(entry.pane).copied().and_then(|id| {
                self.sessions
                    .get(&id)
                    .map(|pane| (id, pane.stream_generation, &pane.frame.status))
            });
            if entry.automatic.is_some_and(|(id, generation)| {
                !primary.is_some_and(|(pane, stream, status)| {
                    id == pane && generation == stream && *status == crate::session::Status::Running
                })
            }) {
                if entry.capturing() {
                    stop.push(id);
                }
                continue;
            }
            let Phase::Active {
                source,
                retiring: None,
                ..
            } = &entry.phase
            else {
                continue;
            };
            match (source, primary.map(|(id, generation, _)| (id, generation))) {
                (Some(bound), current) if Some((bound.id, bound.generation)) != current => {
                    retire.push(id)
                }
                (None, Some(_)) => attach.push(id),
                _ => {}
            }
        }
        for id in stop {
            self.stop_journal(id);
        }
        let mut tasks = self.reconcile_automatic_journals();
        for id in retire {
            let Some(Entry {
                run,
                note,
                phase: Phase::Active {
                    source, retiring, ..
                },
                ..
            }) = self.journal_captures.get_mut(&id)
            else {
                continue;
            };
            if let Some(Binding {
                attachment,
                note_authority,
                ..
            }) = source.take()
            {
                drop(note_authority);
                if let Some(note) = note {
                    note.translation.cancel();
                }
                let (signal, done) = bounded(1);
                let worker = thread::Builder::new()
                    .name("kortty-journal-detach".into())
                    .spawn(move || {
                        block_on(attachment.detach());
                        drop(signal);
                    });
                if let Err(error) = worker {
                    self.fail_journal(id, error.to_string());
                    continue;
                }
                *retiring = Some(done.clone());
                tasks.push(when_closed(done, Message::SourceRetired(id, *run)));
            }
        }
        tasks.extend(
            attach
                .into_iter()
                .map(|id| self.attach_journal_source(id, None)),
        );
        Task::batch(tasks)
    }

    fn reconcile_automatic_journals(&mut self) -> Vec<Task<Desktop>> {
        if !self.journal_allowed() {
            return Vec::new();
        }
        let mut candidates = Vec::new();
        for (&owner, window) in &self.windows {
            for tab in window.terminal_tabs() {
                let Some((session, pane)) = self.journal_source(tab) else {
                    continue;
                };
                if pane.journal_config.auto_start
                    && pane.frame.status == crate::session::Status::Running
                    && pane.journal_attempted != Some(pane.stream_generation)
                {
                    candidates.push((owner, tab.id, session, pane.stream_generation));
                }
            }
        }
        let mut tasks = Vec::new();
        for (owner, tab, session, generation) in candidates {
            let existing = self.journal_captures.get(&tab);
            if existing.is_some_and(|entry| matches!(entry.phase, Phase::Finishing(..))) {
                continue;
            }
            if let Some(pane) = self.sessions.get_mut(&session) {
                pane.journal_attempted = Some(generation);
            }
            if existing.is_none_or(|entry| !entry.capturing()) {
                tasks.push(self.start_journal_for(owner, tab, true));
            }
        }
        tasks
    }

    fn fail_journal(&mut self, tab: u64, message: String) {
        if let Some(entry) = self.journal_captures.get_mut(&tab) {
            entry.notice = Some(message);
        }
        self.stop_journal(tab);
    }

    fn stop_journal(&mut self, tab: u64) {
        if let Some(entry) = self.journal_captures.get_mut(&tab) {
            let done = entry.finish();
            for receiver in done {
                self.track_session_shutdown(receiver);
            }
        }
    }

    pub(super) fn remove_tab_journal(&mut self, tab: u64) {
        self.stop_journal(tab);
        self.prune_retired_journal_captures();
        if let Some(entry) = self.journal_captures.remove(&tab)
            && let Some(capture) = entry.metadata_capture()
            && !capture.done().is_closed()
        {
            self.retired_journal_captures.push(capture);
        }
        self.cancel_obsolete_journal_screenshot_analyses();
    }

    pub(super) fn finish_all_journals(&mut self) {
        for (_, mut entry) in std::mem::take(&mut self.journal_captures) {
            for done in entry.finish() {
                self.track_session_shutdown(done);
            }
        }
    }

    pub(super) fn dock_journal_screenshot(
        &mut self,
        owner: window::Id,
        id: u64,
        run: u64,
    ) -> Task<Desktop> {
        let Some((tab, entry)) = self.dock_journal_target(owner, id, run) else {
            return Task::none();
        };
        let Some(pane) = self.journal_screenshot_pane(tab, entry) else {
            return Task::none();
        };
        let frame = Arc::clone(&pane.frame);
        let grid = pane.grid;
        let analysis_consent = entry
            .automatic_screenshot_ai
            .filter(|_| self.journal_automatic_screenshot_analysis_enabled());
        let Some(sink) = entry.writable(tab).map(Capture::sink) else {
            return Task::none();
        };
        let Some(entry) = self.journal_captures.get_mut(&id) else {
            return Task::none();
        };
        let (send, receive) = bounded(1);
        let (signal, done) = bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-screenshot".into())
            .spawn(move || {
                let screen = &frame.screen;
                let recorded = RecordedScreen {
                    columns: screen.columns,
                    rows: screen.rows,
                    pixels: (grid.pixel_width, grid.pixel_height),
                    cursor: screen.cursor,
                    cursor_shape: screen.cursor_shape,
                    cursor_blinking: screen.cursor_blinking,
                    reverse_video: screen.reverse_video,
                    cells: screen
                        .cells
                        .iter()
                        .map(|cell| RecordedCell {
                            text: if cell.style.hidden {
                                String::new()
                            } else {
                                cell.text.clone()
                            },
                            width: cell.width,
                            style: cell.style,
                        })
                        .collect(),
                };
                let result = crate::recording_window::render::png(&recorded).and_then(|png| {
                    if let Some(consent) = analysis_consent {
                        let receipt = sink
                            .try_screenshot_with_receipt(png)
                            .map_err(|error| error.to_string())?;
                        receipt
                            .recv_blocking()
                            .map_err(|_| "The journal screenshot writer stopped.".to_owned())?
                            .map(|receipt| Some((consent, receipt)))
                    } else {
                        sink.try_screenshot(png)
                            .map(|()| None)
                            .map_err(|error| error.to_string())
                    }
                });
                let _ = send.try_send(result);
                drop(signal);
            });
        if let Err(error) = worker {
            entry.notice = Some(error.to_string());
            return Task::none();
        }
        entry.screenshot = Some(done.clone());
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Screenshot worker stopped.".into()));
            Desktop::JournalCapture(Message::ScreenshotSaved(owner, id, run, result))
        })
    }

    pub(super) fn dock_journal_note(
        &mut self,
        owner: window::Id,
        id: u64,
        run: u64,
        action: Note,
    ) -> Task<Desktop> {
        let Some((tab, entry)) = self.dock_journal_target(owner, id, run) else {
            return Task::none();
        };
        let Some(capture) = entry.writable(tab) else {
            return Task::none();
        };
        let sink = capture.sink();
        let setup = match &action {
            Note::Translate(event) => {
                let recording = entry.recording();
                let source = ViewerSource::capture(capture);
                let source = self.journal_note_entry_source(recording, move || {
                    source.require_current_location()
                });
                Some(
                    self.journal_note_translation_setup(event, recording, MAX_RECORD_BYTES)
                        .with_outbound_source(source),
                )
            }
            _ => None,
        };
        let Some(entry) = self.journal_captures.get_mut(&id) else {
            return Task::none();
        };
        match action {
            Note::Edit => {
                entry.note.get_or_insert_with(QuickNote::default);
            }
            Note::Input(action) => {
                if let Some(note) = entry.note.as_mut()
                    && edit_text_limited(
                        &mut note.content,
                        &mut note.value,
                        action,
                        MAX_RECORD_BYTES,
                    )
                {
                    note.translation.edited();
                    note.problem = None;
                }
            }
            Note::Translate(event) => {
                let (Some(note), Some(setup)) = (&mut entry.note, setup) else {
                    return Task::none();
                };
                let mut update = note
                    .translation
                    .update(event, &note.value, setup, move |event| {
                        Desktop::JournalCapture(Message::NoteTranslated(id, run, event))
                    });
                if let Some(value) = update.take_replacement() {
                    note.content = text_editor::Content::with_text(&value);
                    note.content
                        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                    note.value = value;
                }
                return self.finish_journal_note_translation(update, Some(owner));
            }
            Note::Save => {
                let Some(draft) = entry
                    .note
                    .as_mut()
                    .filter(|note| !note.value.trim().is_empty())
                else {
                    return Task::none();
                };
                draft.translation.cancel();
                let Phase::Active {
                    source: Some(source),
                    ..
                } = &entry.phase
                else {
                    return Task::none();
                };
                let Some(note) = self.sessions.get(&source.id).and_then(|pane| {
                    pane.session
                        .redact_journal_note(&draft.value, &self.clipboard.policy.terminal_logging)
                }) else {
                    draft.problem = Some("Journal note could not be prepared safely.".into());
                    return Task::none();
                };
                match sink.try_write(RecordKind::Note, note, false) {
                    Ok(()) => entry.note = None,
                    Err(error) => draft.problem = Some(error.to_string()),
                }
            }
            Note::Cancel => entry.note = None,
        }
        Task::none()
    }

    pub(super) fn journal_note_draft_open(&self, tab: u64, run: u64) -> bool {
        self.journal_captures
            .get(&tab)
            .is_some_and(|entry| entry.run == run && entry.note.is_some())
    }

    pub(super) fn discard_journal_note(&mut self, tab: u64, run: u64) {
        if let Some(entry) = self
            .journal_captures
            .get_mut(&tab)
            .filter(|entry| entry.run == run)
        {
            entry.note = None;
        }
    }

    pub(super) fn update_journal_capture(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Toggle(owner, tab) if self.is_active_journal_tab(owner, tab) => {
                return self.toggle_journal_for(owner, tab);
            }
            Message::Screenshot(owner, tab) if self.is_active_journal_tab(owner, tab) => {
                if let Some(entry) = self.journal_captures.get(&tab) {
                    return self.dock_journal_screenshot(owner, tab, entry.run);
                }
            }
            Message::Note(owner, tab, run, action) if self.is_active_journal_tab(owner, tab) => {
                return self.dock_journal_note(owner, tab, run, action);
            }
            Message::NoteTranslated(tab, run, event) => {
                if let Some(owner) = self.windows.iter().find_map(|(&owner, window)| {
                    window
                        .terminal_tabs()
                        .any(|item| item.id == tab)
                        .then_some(owner)
                }) {
                    return self.dock_journal_note(owner, tab, run, Note::Translate(event));
                }
            }
            Message::Toggle(..) | Message::Screenshot(..) | Message::Note(..) => {}
            Message::Open(owner, tab, run) => {
                if !self.journal_allowed() || !self.is_active_journal_tab(owner, tab) {
                    return Task::none();
                }
                if let Some(Entry {
                    title,
                    phase: Phase::Finished(Some(capture)),
                    ..
                }) = self
                    .journal_captures
                    .get(&tab)
                    .filter(|entry| entry.run == run)
                {
                    return self.open_journal_viewer(
                        owner,
                        ViewerSource::capture(capture),
                        title.clone(),
                    );
                }
            }
            Message::Started(tab, run) => return self.journal_started(tab, run),
            Message::Attached(tab, run, id, result) => {
                if let Some(Entry {
                    phase:
                        Phase::Active {
                            source: Some(source),
                            ..
                        },
                    ..
                }) = self
                    .journal_captures
                    .get_mut(&tab)
                    .filter(|entry| entry.run == run)
                    && source.id == id
                {
                    match result {
                        Ok(()) => source.ready = true,
                        Err(error) => self.fail_journal(tab, error),
                    }
                }
            }
            Message::Ended(tab, run) => {
                if self
                    .journal_captures
                    .get(&tab)
                    .is_some_and(|entry| entry.run == run)
                {
                    self.stop_journal(tab);
                    if let Phase::Finishing(_, done) = &self.journal_captures[&tab].phase {
                        return when_closed(done.clone(), Message::Finalized(tab, run));
                    }
                }
            }
            Message::Finalized(tab, run) => {
                if let Some(entry) = self
                    .journal_captures
                    .get_mut(&tab)
                    .filter(|entry| entry.run == run)
                    && let Phase::Finishing(capture, _) = &entry.phase
                {
                    entry.phase = Phase::Finished(Some(Arc::clone(capture)));
                }
                return self.update_journal_summary_worker(
                    super::journal_access::summaries::Worker::Pulse,
                );
            }
            Message::SourceRetired(tab, run) => {
                if let Some(Entry {
                    phase: Phase::Active { retiring, .. },
                    ..
                }) = self
                    .journal_captures
                    .get_mut(&tab)
                    .filter(|entry| entry.run == run)
                {
                    *retiring = None;
                }
            }
            Message::AutomaticScreenshotAi(owner, tab, run, enabled) => {
                if (!enabled || self.journal_automatic_screenshot_analysis_enabled())
                    && let Some((session, entry)) = self.dock_journal_target(owner, tab, run)
                    && entry.writable(session).is_some()
                    && if enabled {
                        entry.automatic_screenshot_ai.is_none()
                    } else {
                        !entry.automatic_screenshot_ai_disabled
                    }
                {
                    let consent = enabled.then(|| self.allocate());
                    if let Some(entry) = self.journal_captures.get_mut(&tab) {
                        entry.automatic_screenshot_ai = consent;
                        entry.automatic_screenshot_ai_disabled = !enabled;
                    }
                    if !enabled {
                        self.cancel_obsolete_journal_screenshot_analyses();
                    }
                }
            }
            Message::ScreenshotSaved(owner, tab, run, result) => {
                if let Some(entry) = self
                    .journal_captures
                    .get_mut(&tab)
                    .filter(|entry| entry.run == run)
                {
                    entry.screenshot = None;
                    match result {
                        Err(error) => entry.notice = Some(error),
                        Ok(Some((consent, receipt)))
                            if entry.automatic_screenshot_ai == Some(consent) =>
                        {
                            if let Some(capture) = entry.metadata_capture() {
                                let source = ViewerSource::capture(&capture);
                                return self.analyze_recorded_journal_screenshot(
                                    owner, tab, run, consent, source, receipt,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Task::none()
    }

    pub(super) fn journal_capture_bar(
        &self,
        owner: window::Id,
        tab: &Tab,
    ) -> Option<Element<'_, Desktop>> {
        if self.terminal_only_fullscreen_active(owner) {
            return None;
        }
        let entry = self.journal_captures.get(&tab.id)?;
        let id = tab.id;
        let run = entry.run;
        let note = move |action| Desktop::JournalCapture(Message::Note(owner, id, run, action));
        let toggle = button(if entry.capturing() {
            "Stop journal"
        } else {
            "Start journal"
        })
        .on_press_maybe(
            self.journal_toggle_enabled(owner)
                .then_some(Desktop::JournalCapture(Message::Toggle(owner, id))),
        );
        let screenshot = button("Screenshot").on_press_maybe(
            self.journal_screenshot_enabled(owner)
                .then_some(Desktop::JournalCapture(Message::Screenshot(owner, id))),
        );
        let add_note = button("Note").on_press_maybe(
            self.dock_note_enabled(owner, id, run)
                .then_some(note(Note::Edit)),
        );
        let status = match &entry.phase {
            Phase::Starting(..) => "Starting journal…".into(),
            Phase::Active { .. } if self.journal_source(tab).is_none() => {
                "Journal waiting for connection".into()
            }
            Phase::Active {
                source: Some(source),
                ..
            } if source.ready => "Journal recording".into(),
            Phase::Active { .. } => "Attaching journal…".into(),
            Phase::Finishing(..) => "Finishing journal…".into(),
            Phase::Finished(None) => "Journal stopped".into(),
            Phase::Finished(Some(capture)) => match capture.status() {
                Status::Truncated(reason) | Status::Failed(reason) => reason,
                _ => "Journal saved".into(),
            },
        };
        let mut actions = row![toggle, screenshot, add_note, text(status).size(13)]
            .spacing(6)
            .align_y(Alignment::Center);
        if self.journal_allowed() && matches!(entry.phase, Phase::Finished(Some(_))) {
            actions = actions.push(
                button("Open journal")
                    .on_press(Desktop::JournalCapture(Message::Open(owner, id, run))),
            );
        }
        let mut content = column![actions].spacing(4);
        if let Some(automatic) = self.journal_screenshot_ai_run_control(owner, id, run) {
            content = content.push(automatic);
        }
        if let Some(notice) = &entry.notice {
            content = content.push(text(notice).size(13));
        }
        if let Some(editor) = self.dock_journal_note_editor(owner, id, run, note) {
            content = content.push(editor);
        }
        Some(content.into())
    }

    pub(super) fn journal_screenshot_ai_run_consented(
        &self,
        tab: u64,
        run: u64,
        consent: u64,
    ) -> bool {
        self.journal_captures
            .get(&tab)
            .is_some_and(|entry| entry.run == run && entry.automatic_screenshot_ai == Some(consent))
    }

    pub(super) fn journal_screenshot_ai_failed(
        &mut self,
        tab: u64,
        run: u64,
        consent: u64,
        notice: String,
    ) {
        if self.journal_automatic_screenshot_analysis_enabled()
            && let Some(entry) = self
                .journal_captures
                .get_mut(&tab)
                .filter(|entry| entry.run == run && entry.automatic_screenshot_ai == Some(consent))
        {
            entry.notice = Some(notice);
        }
    }

    pub(super) fn renew_journal_screenshot_ai_consents(&mut self) {
        let consent = self
            .journal_automatic_screenshot_analysis_enabled()
            .then(|| self.allocate());
        for entry in self.journal_captures.values_mut() {
            entry.automatic_screenshot_ai =
                consent.filter(|_| entry.capturing() && !entry.automatic_screenshot_ai_disabled);
        }
    }

    pub(super) fn journal_screenshot_ai_run_control(
        &self,
        owner: window::Id,
        tab: u64,
        run: u64,
    ) -> Option<Element<'_, Desktop>> {
        let (session, entry) = self.dock_journal_target(owner, tab, run)?;
        if !self.journal_automatic_screenshot_analysis_enabled()
            || entry.writable(session).is_none()
        {
            return None;
        }
        Some(
            checkbox(entry.automatic_screenshot_ai.is_some())
                .label("Analyze screenshots in this recording")
                .on_toggle(move |enabled| {
                    Desktop::JournalCapture(Message::AutomaticScreenshotAi(
                        owner, tab, run, enabled,
                    ))
                })
                .into(),
        )
    }

    pub(super) fn dock_journal_note_editor<'a>(
        &'a self,
        owner: window::Id,
        tab: u64,
        run: u64,
        on_note: impl Fn(Note) -> Desktop + Copy + 'a,
    ) -> Option<Element<'a, Desktop>> {
        if self.tab_journal_note_for_owner(owner).is_some() {
            return None;
        }
        self.journal_note_editor(owner, tab, run, on_note, None)
    }

    pub(super) fn journal_note_editor<'a>(
        &'a self,
        owner: window::Id,
        tab: u64,
        run: u64,
        on_note: impl Fn(Note) -> Desktop + Copy + 'a,
        dialog_input: Option<iced::widget::Id>,
    ) -> Option<Element<'a, Desktop>> {
        let (tab, entry) = self.dock_journal_target(owner, tab, run)?;
        entry.writable(tab)?;
        let note = entry.note.as_ref()?;
        let save = on_note(Note::Save);
        let mut input = text_editor(&note.content)
            .placeholder("Journal note")
            .height(104)
            .on_action(move |action| on_note(Note::Input(action)))
            .key_binding(move |key| {
                note.translation
                    .binding(key, |event| on_note(Note::Translate(event)))
            });
        let confirm = if let Some(id) = dialog_input {
            input = input.id(id);
            "OK"
        } else {
            "Add"
        };
        Some(
            column![
                input,
                note.translation.view(
                    &note.value,
                    self.journal_note_translation_view(true),
                    move |event| { on_note(Note::Translate(event)) }
                ),
                note.problem.as_ref().map(|problem| text(problem).size(13)),
                row![
                    button(confirm).on_press_maybe((!note.value.trim().is_empty()).then_some(save)),
                    button("Cancel").on_press(on_note(Note::Cancel)),
                ]
                .spacing(6),
            ]
            .spacing(6)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let folder = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-journal-root-{}",
                crate::private_files::nonce().unwrap()
            ));
            fs::create_dir(&folder).unwrap();
            Self(folder)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[track_caller]
    fn wait(mut finished: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !finished() {
            assert!(Instant::now() < deadline, "journal worker did not finish");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn journal_screenshot_analysis_automatic_receipt_requires_the_original_recording_consent() {
        use std::io::Write as _;

        use crate::local_ai::test_support::{
            accept, exclusive_run, fixture_response, listener, request,
        };
        use crate::session_journal::topic_ai_settings::Preferences;

        let _exclusive = exclusive_run();
        let scratch = Scratch::new();
        let capture =
            Arc::new(Capture::start(scratch.0.join("journals"), Metadata::default()).unwrap());
        let png = tiny_skia::Pixmap::new(3, 2).unwrap().encode_png().unwrap();
        let receipt = capture
            .sink()
            .try_screenshot_with_receipt(png)
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap();
        let _ = capture.finish().recv_blocking();
        let (listener, _) = listener();
        let mut workspace = Workspace {
            window_settings: crate::workspace::tab_titles::WindowSettings {
                journal_ai: Preferences {
                    enabled: true,
                    endpoint: listener.local_addr().unwrap().to_string(),
                    model: "fixture:local".into(),
                    vision_enabled: true,
                    ..Preferences::default()
                },
                ..Default::default()
            },
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        // Test double for the tab/run binding. The capture, receipt and HTTP client are real.
        workspace.add_tab(owner, SessionId(100));
        let tab = workspace.windows[&owner].tab().unwrap();
        let (tab, pane) = (tab.id, tab.primary);
        workspace.journal_captures.insert(
            tab,
            Entry {
                run: 17,
                pane,
                title: "Recorded screenshot".into(),
                phase: Phase::Finished(Some(capture)),
                notice: None,
                note: None,
                screenshot: None,
                automatic_screenshot_ai: Some(19),
                automatic_screenshot_ai_disabled: false,
                automatic: None,
                capture_input: false,
            },
        );
        let obsolete = PublishedScreenshot {
            id: receipt.id.clone(),
            reference: receipt.reference.clone(),
        };
        let stale = workspace.update_journal_capture(Message::ScreenshotSaved(
            owner,
            tab,
            17,
            Ok(Some((18, obsolete))),
        ));
        assert_eq!(stale.units(), 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        // The receipt can still arrive after automatic analysis is switched off.
        workspace
            .window_settings
            .journal_ai
            .auto_screenshot_analysis = false;
        let disabled = workspace.update_journal_capture(Message::ScreenshotSaved(
            owner,
            tab,
            17,
            Ok(Some((19, receipt.clone()))),
        ));
        assert_eq!(disabled.units(), 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        workspace
            .window_settings
            .journal_ai
            .auto_screenshot_analysis = true;

        // Loopback HTTP test double for the configured image-capable model.
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let body = request(&mut stream);
            stream
                .get_mut()
                .write_all(
                    fixture_response(
                        "200 OK",
                        "",
                        r#"{"description":"Terminal","tags":["console"]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
            body
        });
        let current = workspace.update_journal_capture(Message::ScreenshotSaved(
            owner,
            tab,
            17,
            Ok(Some((19, receipt))),
        ));
        let body = server.join().unwrap();
        assert!(current.units() > 0);
        assert!(
            body["messages"][1]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        wait(|| workspace.pending_shutdown.iter().all(Receiver::is_closed));
    }

    #[test]
    fn journal_metadata_retains_a_closing_capture_after_its_tab_is_removed() {
        use iced::futures::{FutureExt as _, StreamExt as _};

        use crate::workspace::journal_access::{Message as JournalMessage, metadata};

        let scratch = Scratch::new();
        let capture =
            Arc::new(Capture::start(scratch.0.join("closing"), Metadata::default()).unwrap());
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        // Test double for a terminal tab whose transport is retiring. The
        // journal and metadata writer below are real filesystem workers.
        workspace.add_tab(owner, SessionId(100));
        let tab = workspace.windows[&owner].tab().unwrap();
        let (tab_id, pane) = (tab.id, tab.primary);
        let (retiring, done) = bounded(1);
        workspace.journal_captures.insert(
            tab_id,
            Entry {
                run: 500,
                pane,
                title: "Closing".into(),
                phase: Phase::Active {
                    capture: Arc::clone(&capture),
                    source: None,
                    retiring: Some(done),
                },
                notice: None,
                note: None,
                screenshot: None,
                automatic_screenshot_ai: None,
                automatic_screenshot_ai_disabled: false,
                automatic: None,
                capture_input: false,
            },
        );
        workspace.remove_tab_journal(tab_id);
        assert!(!workspace.journal_captures.contains_key(&tab_id));
        assert!(
            workspace
                .journal_metadata_captures()
                .iter()
                .any(|found| Arc::ptr_eq(found, &capture))
        );
        let _ =
            workspace.open_journal_viewer(owner, ViewerSource::capture(&capture), "Closing".into());
        let viewer = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery of the actual on-disk document.
        let document = Arc::new(crate::session_journal::read(capture.directory()).unwrap());
        let _ =
            workspace.update_session_journal(viewer, JournalMessage::DocumentLoaded(Ok(document)));
        let _ = workspace
            .update_session_journal(viewer, JournalMessage::Metadata(metadata::Message::Rename));
        let dialog = workspace.journal_edit_for_owner(owner).unwrap();
        let _ = workspace.update_session_journal(
            dialog,
            JournalMessage::Metadata(metadata::Message::Title("Saved after tab close".into())),
        );
        let save = workspace.update_session_journal(
            dialog,
            JournalMessage::Metadata(metadata::Message::SaveTitle),
        );
        let mut save = iced_runtime::task::into_stream(save).expect("scheduled journal rename");
        let mut completion = None;
        wait(|| match save.next().now_or_never() {
            Some(Some(iced_runtime::Action::Output(message))) => {
                completion = Some(message);
                true
            }
            Some(_) => panic!("Journal rename ended without a completion message"),
            None => false,
        });
        let completion = completion.unwrap();
        let Desktop::SessionJournal(
            saved_viewer,
            JournalMessage::Metadata(metadata::Message::Saved(_, result)),
        ) = &completion
        else {
            panic!("Expected journal rename completion");
        };
        assert_eq!(*saved_viewer, viewer);
        result
            .as_ref()
            .expect("saved title while capture was retiring");
        let _ = workspace.update(completion);
        drop(retiring);
        wait(|| capture.done().is_closed());
        wait(|| {
            workspace
                .pending_shutdown
                .iter()
                .all(|done| done.is_closed())
        });
        let document = crate::session_journal::read(capture.directory()).unwrap();
        assert_eq!(document.summary.title, "Saved after tab close");
        assert!(document.summary.ended_at.is_some());
        workspace.prune_retired_journal_captures();
        assert!(workspace.retired_journal_captures.is_empty());
    }

    #[test]
    fn stopped_journal_open_uses_exact_run_and_capture_identity() {
        let scratch = Scratch::new();
        let capture =
            Arc::new(Capture::start(scratch.0.join("journals"), Metadata::default()).unwrap());
        let done = capture.finish();
        wait(|| done.is_closed());
        assert_eq!(capture.status(), Status::Complete);

        // Reuse the XML and path but change the physical journal directory.
        let original = capture.directory();
        let moved = scratch.0.join("moved");
        fs::rename(original, &moved).unwrap();
        fs::create_dir(original).unwrap();
        fs::copy(moved.join("journal.xml"), original.join("journal.xml")).unwrap();
        let replacement = crate::session_journal::read(original).unwrap();
        assert_eq!(replacement.summary.id, capture.id());
        let replacement = ViewerSource::catalog(&replacement.summary).unwrap();
        assert!(!ViewerSource::capture(&capture).same_journal(&replacement));

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        // Test double for an active terminal tab. The completed capture above is real.
        workspace.add_tab(owner, SessionId(100));
        let active = workspace.windows[&owner].tab().unwrap();
        let (tab, pane, run) = (active.id, active.primary, 500);
        workspace.journal_captures.insert(
            tab,
            Entry {
                run,
                pane,
                title: "Original capture".into(),
                phase: Phase::Finished(Some(capture)),
                notice: None,
                note: None,
                screenshot: None,
                automatic_screenshot_ai: None,
                automatic_screenshot_ai_disabled: false,
                automatic: None,
                capture_input: false,
            },
        );
        let _ = workspace.open_journal_viewer(owner, replacement, "Replacement".into());
        assert_eq!(workspace.journals.len(), 1);

        let _ = workspace.update_journal_capture(Message::Open(owner, tab, run - 1));
        assert_eq!(workspace.journals.len(), 1);
        workspace.clipboard.policy.session_journal_allowed = false;
        let _ = workspace.update_journal_capture(Message::Open(owner, tab, run));
        assert_eq!(workspace.journals.len(), 1);
        workspace.clipboard.policy.session_journal_allowed = true;
        let _ = workspace.update_journal_capture(Message::Open(owner, tab, run));
        assert_eq!(workspace.journals.len(), 2);
        let _ = workspace.update_journal_capture(Message::Open(owner, tab, run));
        assert_eq!(workspace.journals.len(), 2);
    }

    #[test]
    fn global_journal_root_and_capture_format_change_only_the_next_real_capture() {
        use crate::session_journal::LogFormat;

        let scratch = Scratch::new();
        let first = scratch.0.join("first");
        let next = scratch.0.join("next");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let grid = crate::session::GridSize::default();
        let session =
            crate::session::Session::start(grid, kortty_terminal::TerminalProfile::Xterm).unwrap();
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(owner, session);
        wait(|| workspace.sessions[&session].session.journal_available());
        let tab = workspace.windows[&owner].active_tab_id().unwrap();
        workspace.window_settings.session_journal.directory = first.to_str().unwrap().into();
        workspace.window_settings.session_journal.log_format = LogFormat::Yaml;

        let start = |workspace: &mut Workspace| {
            let _ = workspace.update_journal_capture(Message::Toggle(owner, tab));
            let entry = &workspace.journal_captures[&tab];
            let Phase::Starting(_, startup) = &entry.phase else {
                panic!("journal did not start")
            };
            wait(|| startup.done.is_closed());
            let run = entry.run;
            // Test double for Iced completion delivery, using the actual startup worker.
            let _ = workspace.update_journal_capture(Message::Started(tab, run));
            workspace.active_dock_journal(owner).unwrap().capture
        };
        let stop = |workspace: &mut Workspace| {
            let _ = workspace.update_journal_capture(Message::Toggle(owner, tab));
            let entry = &workspace.journal_captures[&tab];
            let Phase::Finishing(_, done) = &entry.phase else {
                panic!("journal did not stop")
            };
            wait(|| done.is_closed());
            let run = entry.run;
            let _ = workspace.update_journal_capture(Message::Finalized(tab, run));
        };
        let original = start(&mut workspace);
        assert_eq!(original.directory().parent(), Some(first.as_path()));
        assert_eq!(original.log_format(), LogFormat::Yaml);
        workspace.window_settings.session_journal.directory = next.to_str().unwrap().into();
        let managed = "[session-journal]\nlog-format = 'xml'"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(managed.as_table())
            .unwrap();
        let active = workspace.active_dock_journal(owner).unwrap();
        assert!(Arc::ptr_eq(&original, &active.capture));
        assert_eq!(active.capture.log_format(), LogFormat::Yaml);
        assert!(!next.exists());
        stop(&mut workspace);
        let replacement = start(&mut workspace);
        assert_eq!(replacement.directory().parent(), Some(next.as_path()));
        assert_eq!(replacement.log_format(), LogFormat::Xml);
        stop(&mut workspace);
        assert!(original.directory().join("session-log.yaml.zst").is_file());
        assert!(
            replacement
                .directory()
                .join("session-log.xml.zst")
                .is_file()
        );
        let done = workspace.sessions[&session].session.stop();
        wait(|| done.is_closed());
    }

    #[test]
    fn configured_journal_starts_once_per_stream_without_retroactive_seed() {
        let scratch = Scratch::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.window_settings.session_journal.directory =
            scratch.0.join("journals").to_str().unwrap().into();
        let grid = crate::session::GridSize::default();
        let shell =
            crate::session::Session::start(grid, kortty_terminal::TerminalProfile::Xterm).unwrap();
        let session = workspace.store_session(shell, grid, 14);
        workspace.add_tab(owner, session);
        let pane = workspace.sessions.get_mut(&session).unwrap();
        wait(|| {
            pane.session.journal_available()
                && pane.session.frame().status == crate::session::Status::Running
        });
        pane.frame = pane.session.frame();
        pane.journal_config.auto_start = true;
        let generation = pane.stream_generation;
        let tab = workspace.windows[&owner].active_tab_id().unwrap();
        let _ = workspace.reconcile_journal_sources();
        let entry = &workspace.journal_captures[&tab];
        assert_eq!(entry.automatic, Some((session, generation)));
        assert!(!entry.capture_input);
        let run = entry.run;
        let Phase::Starting(_, startup) = &entry.phase else {
            panic!("automatic capture did not start");
        };
        wait(|| startup.done.is_closed());
        let attach = workspace.update_journal_capture(Message::Started(tab, run));
        assert!(matches!(
            &workspace.journal_captures[&tab].phase,
            Phase::Active {
                source: Some(_),
                ..
            }
        ));
        assert!(
            workspace
                .journal_agent_capture(session, generation)
                .is_none()
        );
        {
            use iced::futures::StreamExt as _;

            let mut events = iced_runtime::task::into_stream(attach).unwrap();
            loop {
                if let iced_runtime::Action::Output(Desktop::JournalCapture(
                    message @ Message::Attached(..),
                )) = block_on(events.next()).expect("the journal attachment must finish")
                {
                    let _ = workspace.update_journal_capture(message);
                    break;
                }
            }
        }
        let capture = workspace.journal_captures[&tab].metadata_capture().unwrap();
        let note_authority = workspace.live_journal_note_authority(&capture).unwrap();
        assert!(note_authority());
        assert!(
            workspace
                .journal_agent_capture(session, generation)
                .is_some_and(|current| { Arc::ptr_eq(&current, &capture) })
        );
        assert!(
            workspace
                .journal_agent_capture(session, generation.wrapping_add(1))
                .is_none()
        );
        workspace.active_tab_mut(owner).unwrap().incognito = true;
        assert!(
            workspace
                .journal_agent_capture(session, generation)
                .is_none()
        );
        workspace.active_tab_mut(owner).unwrap().incognito = false;
        let data = std::fs::read_to_string(capture.directory().join("journal.xml")).unwrap();
        assert!(data.contains("<seeded>false</seeded>"));
        let _ = workspace.reconcile_journal_sources();
        assert_eq!(workspace.journal_captures[&tab].run, run);
        workspace.sessions[&session]
            .session
            .send(b"exit\r".to_vec())
            .unwrap();
        wait(|| {
            workspace.sessions[&session].session.frame().status != crate::session::Status::Running
        });
        assert!(!note_authority());
        let _ = workspace.update(Desktop::SessionUpdate(session));
        let Phase::Finishing(_, done) = &workspace.journal_captures[&tab].phase else {
            panic!("automatic capture did not stop after the shell exited");
        };
        wait(|| done.is_closed());
        let _ = workspace.update_journal_capture(Message::Finalized(tab, run));
        assert!(
            workspace
                .journal_agent_capture(session, generation)
                .is_none()
        );
        let _ = workspace.reconcile_journal_sources();
        assert_eq!(workspace.journal_captures[&tab].run, run);
        let _ = workspace.sessions[&session].session.stop().recv_blocking();
    }

    #[test]
    fn default_screenshot_analysis_applies_to_each_recording_and_honors_opt_out() {
        let scratch = Scratch::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let grid = crate::session::GridSize::default();
        let session =
            crate::session::Session::start(grid, kortty_terminal::TerminalProfile::Xterm).unwrap();
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(owner, session);
        wait(|| workspace.sessions[&session].session.journal_available());
        let tab = workspace.windows[&owner].active_tab_id().unwrap();
        workspace.window_settings.session_journal.directory =
            scratch.0.join("journals").to_str().unwrap().into();
        let preferences = &mut workspace.window_settings.journal_ai;
        preferences.enabled = true;
        preferences.vision_enabled = true;
        preferences.endpoint = "127.0.0.1:11434".into();
        preferences.model = "fixture-local-image".into();

        let _ = workspace.update_journal_capture(Message::Toggle(owner, tab));
        let entry = &workspace.journal_captures[&tab];
        let Phase::Starting(_, startup) = &entry.phase else {
            panic!("journal did not start");
        };
        let run = entry.run;
        let initial = entry.automatic_screenshot_ai.expect("default analysis");
        wait(|| startup.done.is_closed());
        // Test doubles for Iced completion delivery, using a real session and capture.
        let _ = workspace.update_journal_capture(Message::Started(tab, run));
        let _ = workspace.update_journal_capture(Message::Attached(tab, run, session, Ok(())));
        assert!(workspace.dock_note_enabled(owner, tab, run));
        let _ = workspace
            .update_journal_capture(Message::AutomaticScreenshotAi(owner, tab, run, false));
        assert!(!workspace.journal_screenshot_ai_run_consented(tab, run, initial));
        assert!(workspace.journal_captures[&tab].automatic_screenshot_ai_disabled);
        let _ =
            workspace.update_journal_capture(Message::AutomaticScreenshotAi(owner, tab, run, true));
        assert!(!workspace.journal_captures[&tab].automatic_screenshot_ai_disabled);

        workspace
            .window_settings
            .journal_ai
            .auto_screenshot_analysis = false;
        workspace.renew_journal_screenshot_ai_consents();
        assert!(!workspace.journal_automatic_screenshot_analysis_enabled());
        // The user already clicked this run's toggle before the global Save hid it.
        let _ = workspace
            .update_journal_capture(Message::AutomaticScreenshotAi(owner, tab, run, false));
        workspace
            .window_settings
            .journal_ai
            .auto_screenshot_analysis = true;
        workspace.renew_journal_screenshot_ai_consents();
        assert!(
            workspace.journal_captures[&tab]
                .automatic_screenshot_ai
                .is_none()
        );
        let _ =
            workspace.update_journal_capture(Message::AutomaticScreenshotAi(owner, tab, run, true));
        let enabled = workspace.journal_captures[&tab]
            .automatic_screenshot_ai
            .unwrap();
        assert_ne!(initial, enabled);
        workspace.journal_screenshot_ai_failed(tab, run, initial, "Stale result".into());
        workspace.journal_screenshot_ai_failed(tab, run - 1, enabled, "Wrong run".into());
        assert!(workspace.journal_captures[&tab].notice.is_none());
        workspace.journal_screenshot_ai_failed(tab, run, enabled, "Current failure".into());
        assert_eq!(
            workspace.journal_captures[&tab].notice.as_deref(),
            Some("Current failure")
        );

        workspace.window_settings.journal_ai.model = "replacement-local-image".into();
        workspace.renew_journal_screenshot_ai_consents();
        assert!(!workspace.journal_screenshot_ai_run_consented(tab, run, enabled));
        assert!(
            workspace.journal_captures[&tab]
                .automatic_screenshot_ai
                .is_some()
        );
        let _ = workspace
            .update_journal_capture(Message::AutomaticScreenshotAi(owner, tab, run, false));
        workspace.stop_journal(tab);
        wait(|| workspace.pending_shutdown.iter().all(Receiver::is_closed));
        let _ = workspace.update_journal_capture(Message::Finalized(tab, run));

        let _ = workspace.update_journal_capture(Message::Toggle(owner, tab));
        let next = &workspace.journal_captures[&tab];
        assert_ne!(run, next.run);
        assert!(next.automatic_screenshot_ai.is_some());
        assert!(!next.automatic_screenshot_ai_disabled);
        workspace.stop_journal(tab);
        wait(|| workspace.pending_shutdown.iter().all(Receiver::is_closed));
        let done = workspace.sessions[&session].session.stop();
        wait(|| done.is_closed());
    }

    #[test]
    fn dock_journal_target_tracks_exact_run_and_owner_independent_of_selection() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        // Test-only tab and stopped journal metadata exercise routing without workers.
        workspace.add_tab(owner, SessionId(100));
        let active = workspace.windows[&owner].tab().unwrap();
        let (tab, pane, run) = (active.id, active.primary, 500);
        workspace.add_tab(owner, SessionId(101));
        workspace.journal_captures.insert(
            tab,
            Entry {
                run,
                pane,
                title: "Routing fixture".into(),
                phase: Phase::Finished(None),
                notice: None,
                note: None,
                screenshot: None,
                automatic_screenshot_ai: None,
                automatic_screenshot_ai_disabled: false,
                automatic: None,
                capture_input: false,
            },
        );

        assert!(!workspace.is_active_journal_tab(owner, tab));
        assert!(workspace.dock_journal_target(owner, tab, run).is_some());
        assert!(workspace.dock_journal_target(owner, tab, run - 1).is_none());
        assert!(!workspace.dock_journal_live(owner, tab, run));
        assert!(!workspace.dock_note_enabled(owner, tab, run));

        let _ = workspace.transfer(owner, Some(destination), tab);
        assert!(workspace.dock_journal_target(owner, tab, run).is_none());
        assert!(
            workspace
                .dock_journal_target(destination, tab, run)
                .is_some()
        );
        workspace.clipboard.policy.session_journal_allowed = false;
        assert!(
            workspace
                .dock_journal_target(destination, tab, run)
                .is_none()
        );
    }
}
