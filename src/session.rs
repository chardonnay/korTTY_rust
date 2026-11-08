use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use async_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use iced::futures::executor::block_on;
use iced::futures::future::{Either, select};
use kortty_control::{
    ErrorCode as ControlError, Fault as ControlFault, ReadMode, Reply, Screen as ControlScreen,
};
use kortty_pty::{LaunchOptions, Pty};
use kortty_recording::CaptureOptions;
use kortty_serial::SerialPort;
use kortty_ssh::WindowSize;
use kortty_terminal::{
    Cell, CommandTimestamp, Hp2392Input, LastOutput, LastOutputRange, MAX_CLIPBOARD_TEXT_BYTES,
    MAX_SEARCH_QUERY_BYTES, PromptDirection, PromptLayout, PromptNavigator, ReconnectHistory,
    ScreenSnapshot, SearchResults, Terminal, TerminalProfile, TypeAhead,
};

use crate::terminal_theme::Appearance;

mod activity;
mod clipboard;
mod ibm;
mod journal;
mod recording;
mod serial;
mod shell_activity;
mod typewriter;
pub(crate) use activity::OutputSample;
pub use ibm::{IbmAid, IbmEdit, IbmInput, IbmKind};
pub use recording::{RecordingCommandError, RecordingStatus};
pub use serial::{
    SerialConnection, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
    SerialStopBits, available_serial_ports,
};
#[cfg(test)]
pub(crate) use shell_activity::ReportedDirectory;
pub(crate) use shell_activity::ShellActivity;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SCROLLBACK_IDLE: Duration = Duration::from_millis(1500);
const SCROLLBACK_SLICE_INTERVAL: Duration = Duration::from_millis(500);
const WRITE_QUEUE_SIZE: usize = 64;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const READ_CHUNK_SIZE: usize = 8 * 1024;
const MAX_CONTROL_READ_LINES: usize = 10_000;
// The wire response is capped at 512 KiB. Leave room for the JSON-RPC envelope
// and metadata. Wait-output can also repeat the matched line and a substring.
const CONTROL_LINE_BUDGET: usize = 504 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub columns: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Default for GridSize {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
            cell_width: 0,
            cell_height: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Disconnected,
    Starting,
    Running,
    Exited(Option<i32>),
    Error(String),
}

pub struct Frame {
    pub screen: ScreenSnapshot,
    pub status: Status,
    pub scroll_offset: usize,
    pub(crate) scrollback: Option<kortty_terminal::ScrollbackSnapshot>,
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub(crate) password_prompt: bool,
    pub command_timestamps: Vec<CommandTimestamp>,
    pub ibm: Option<ibm::Presentation>,
    pub serial: Option<Arc<SerialConnection>>,
    pub serial_notice: Option<Arc<str>>,
    /// Live rendering only. The authoritative grid remains in `screen`.
    pub typeahead: Option<Arc<TypeAheadFrame>>,
    prompts: Option<Arc<PromptLayout>>,
    pub has_prompt_marks: bool,
    pub has_finished_command: bool,
    shell_activity: ShellActivity,
    /// The terminal content revision that owns this viewport's absolute search coordinates.
    pub search_revision: u64,
    search_result: Option<Arc<SearchResponse>>,
}

pub struct TypeAheadFrame {
    pub row: u16,
    pub cursor_column: u16,
    pub cells: Vec<Cell>,
}

impl Frame {
    pub fn search_result(&self) -> Option<&Arc<SearchResponse>> {
        self.search_result.as_ref()
    }

    /// Passive SSH evidence published with the corresponding screen.
    pub(crate) fn shell_activity(&self) -> &ShellActivity {
        &self.shell_activity
    }
}

#[derive(Debug)]
pub struct SearchResponse {
    pub request: u64,
    pub revision: u64,
    pub results: SearchResults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    InvalidQuery,
    Unsupported,
    Busy,
    Closed,
}

#[derive(Clone, PartialEq, Eq)]
struct ViewRequest {
    grid: GridSize,
    scroll_anchor: Option<kortty_terminal::ScrollbackAnchor>,
    typeahead: TypeAheadRequest,
    prompt_navigation_enabled: bool,
    prompt_navigator: PromptNavigator,
    appearance: Appearance,
}

impl ViewRequest {
    fn set_scroll_anchor(&mut self, anchor: Option<kortty_terminal::ScrollbackAnchor>) {
        if self.scroll_anchor != anchor {
            self.scroll_anchor = anchor;
            self.prompt_navigator = PromptNavigator::default();
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TypeAheadRequest {
    enabled: bool,
    generation: u64,
}

struct Shared {
    view: Mutex<ViewRequest>,
    frame: Mutex<Arc<Frame>>,
    redraw: Sender<()>,
    bell_events: AtomicU64,
    output_activity: Arc<activity::Clock>,
    local_shell_pid: AtomicU32,
    sandbox_backend: Mutex<Option<&'static str>>,
    serial: Option<Arc<SerialConnection>>,
    serial_notice: Mutex<Option<Arc<str>>>,
    shell_activity: Option<Mutex<shell_activity::State>>,
    clipboard: Mutex<clipboard::ClipboardBridge>,
    recording: Arc<recording::Control>,
    logfile_warning: crate::terminal_logging::Warning,
    journal_available: AtomicBool,
    journal_generation: AtomicU64,
    journal_secrets: Mutex<Option<crate::terminal_logging::RedactionSecrets>>,
    scrollback_compression_enabled: AtomicBool,
    typewriter_speed: AtomicU8,
}

impl Shared {
    fn typewriter_speed(&self) -> Option<u8> {
        let speed = self.typewriter_speed.load(Ordering::Relaxed);
        (speed != 0).then_some(speed)
    }

    fn report_bells(&self, count: u32) {
        if count != 0 {
            self.bell_events
                .fetch_add(u64::from(count), Ordering::Relaxed);
        }
    }
}

pub struct Session {
    shared: Arc<Shared>,
    backend: Backend,
    redraw: Receiver<()>,
    done: Receiver<()>,
}

/// An actor-stamped journal attachment. It can be detached after its pane leaves the workspace.
#[derive(Clone)]
pub(crate) struct JournalAttachment {
    id: u64,
    events: Sender<Event>,
    actor: thread::Thread,
    done: Receiver<()>,
}

impl JournalAttachment {
    pub(crate) async fn detach(self) {
        let (reply, received) = bounded(1);
        if self
            .events
            .send(Event::Journal(journal::Command::Detach {
                id: self.id,
                reply,
            }))
            .await
            .is_ok()
        {
            self.actor.unpark();
            let _ = select(pin!(received.recv()), pin!(self.done.recv())).await;
        }
    }
}

enum Backend {
    Stream {
        events: Sender<Event>,
        input: Sender<StreamInput>,
        actor: thread::Thread,
        next_search: AtomicU64,
        sftp: Option<crate::ssh_connection::SftpOpener>,
    },
    Ibm(Sender<IbmInput>),
    Offline,
}

enum StreamSource {
    Pty(LaunchOptions),
    Ssh(
        Option<crate::ssh_connection::Connected>,
        Option<Box<crate::terminal_logging::Prepared>>,
    ),
    Serial {
        connection: Arc<SerialConnection>,
        connect: bool,
    },
}

enum StreamInput {
    Raw {
        bytes: Vec<u8>,
        clear_typeahead: bool,
        mirrored: bool,
    },
    Protocol(Vec<u8>, bool),
    Keyboard {
        bytes: Vec<u8>,
        enter: bool,
        generation: Option<u64>,
    },
    User {
        bytes: Vec<u8>,
        enter: bool,
        record_enter: bool,
        mirrored: bool,
    },
    Break,
}

enum StreamTransport {
    Pty(Pty),
    Ssh(crate::ssh_connection::Connected),
    Serial {
        port: Arc<SerialPort>,
        connection: Arc<SerialConnection>,
    },
}

type StreamReader = Box<dyn Read + Send>;
type StreamWriter = Box<dyn Write + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Busy,
    Closed,
    TooLarge,
}

enum Event {
    ViewChanged,
    Clipboard,
    Output(Vec<u8>, Instant),
    KeyboardInput {
        bytes: Vec<u8>,
        enter: bool,
        sent_at: Instant,
        wall_time: Option<SystemTime>,
        generation: Option<u64>,
    },
    CommandEnter(SystemTime, Instant),
    CommandTimestampQuiet,
    UntrackedShellInput {
        clear_typeahead: bool,
    },
    TypeAheadClear,
    Echo(Vec<u8>),
    HpUserInput {
        bytes: Vec<u8>,
        kind: Hp2392Input,
        record_enter: bool,
        mirrored: bool,
    },
    Recording(recording::Command),
    Journal(journal::Command),
    RecordingActivity {
        generation: u64,
        at: Instant,
    },
    SerialNotice(Arc<str>),
    DisconnectSerial(Sender<()>),
    Search(SearchRequest),
    ControlRead(ControlRead),
    TerminalCommand(TerminalCommand, Sender<TerminalCommandResult>),
    LocalShellDirectory(Sender<io::Result<PathBuf>>),
    CaptureReconnectHistory(Sender<ReconnectHistory>),
    PrependReconnectHistory(Box<ReconnectHistory>, Sender<()>),
    RevealSearch {
        revision: u64,
        row: usize,
    },
    ReadClosed,
    IoError(String),
}

pub(crate) enum TerminalCommand {
    ClearBuffer,
    SelectLastOutput,
    CopyLastOutput,
    CopySelection {
        revision: u64,
        range: LastOutputRange,
    },
}

#[derive(Clone)]
pub(crate) enum TerminalCommandResult {
    Cleared,
    LastOutput(LastOutput),
    Selected {
        revision: u64,
        range: LastOutputRange,
    },
    Copied {
        text: String,
        truncated: bool,
    },
    SelectionChanged,
    OutputTooLarge,
}

impl std::fmt::Debug for TerminalCommandResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cleared => formatter.write_str("Cleared"),
            Self::LastOutput(status) => formatter.debug_tuple("LastOutput").field(status).finish(),
            Self::Selected { revision, range } => formatter
                .debug_struct("Selected")
                .field("revision", revision)
                .field("range", range)
                .finish(),
            Self::Copied { truncated, .. } => formatter
                .debug_struct("Copied")
                .field("truncated", truncated)
                .finish_non_exhaustive(),
            Self::SelectionChanged => formatter.write_str("SelectionChanged"),
            Self::OutputTooLarge => formatter.write_str("OutputTooLarge"),
        }
    }
}

struct ControlRead {
    pane_id: String,
    mode: ReadMode,
    lines: usize,
    reply: Reply,
}

impl ControlRead {
    fn answer(self, live: &ScreenSnapshot, terminal: Option<&Terminal>) {
        if !self.reply.active() {
            return;
        }
        let Some(screen) =
            control_screen(self.pane_id, self.mode, self.lines, live, terminal, || {
                self.reply.active()
            })
        else {
            return;
        };
        self.reply.send(serde_json::to_value(screen).map_err(|_| {
            ControlFault::new(ControlError::Internal, "Could not encode terminal screen")
        }));
    }
}

#[derive(Clone)]
struct WorkerEvents {
    sender: Sender<Event>,
    input: Sender<StreamInput>,
    actor: thread::Thread,
    recording: Arc<recording::Control>,
    output_activity: Arc<activity::Clock>,
    observe_shell: bool,
}

impl WorkerEvents {
    fn send(&self, event: Event) -> bool {
        if self.input.is_closed() {
            return false;
        }
        let sent = match self.sender.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Closed(_)) => false,
            Err(TrySendError::Full(event)) => {
                // The session joins its I/O workers before publishing an ended frame.
                // A full event queue must not keep a worker alive after input closes.
                matches!(
                    block_on(select(
                        pin!(self.input.closed()),
                        pin!(self.sender.send(event))
                    )),
                    Either::Right((Ok(()), _))
                )
            }
        };
        if sent {
            self.actor.unpark();
        }
        sent
    }

    fn recording_activity(&self, generation: u64) {
        if generation == 0 || self.recording.generation() != generation {
            return;
        }
        let event = Event::RecordingActivity {
            generation,
            at: Instant::now(),
        };
        if matches!(self.sender.try_send(event), Err(TrySendError::Full(_))) {
            self.recording.overflow(generation);
        }
        self.actor.unpark();
    }
}

struct SearchRequest {
    id: u64,
    query: String,
    ignore_case: bool,
}

#[derive(Default)]
struct SearchState {
    revision: u64,
    result: Option<Arc<SearchResponse>>,
}

struct EmulationState {
    search: SearchState,
    recording: recording::Actor,
    logfile: Option<crate::terminal_logging::Capture>,
    journal: journal::Actor,
    serial_released: Option<Sender<()>>,
}

impl SearchState {
    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn find(&mut self, terminal: &Terminal, request: SearchRequest) {
        self.result = Some(Arc::new(SearchResponse {
            request: request.id,
            revision: self.revision,
            results: terminal.search(&request.query, request.ignore_case),
        }));
    }

    fn reveal(&self, shared: &Shared, revision: u64, row: usize) {
        if revision != self.revision {
            return;
        }
        let frame = shared
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let history = frame.screen.scrollback_lines;
        let rows = usize::from(frame.screen.rows);
        let coordinates = frame.scrollback.clone();
        drop(frame);
        let Some(coordinates) = coordinates else {
            return;
        };
        let mut view = shared
            .view
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let offset = reveal_offset(
            history,
            rows,
            coordinates.offset(view.scroll_anchor.as_ref()),
            row,
        );
        view.set_scroll_anchor(coordinates.anchor(offset));
    }
}

fn reveal_offset(history: usize, rows: usize, offset: usize, row: usize) -> usize {
    let offset = offset.min(history);
    let first = history.saturating_sub(offset);
    if row >= history.saturating_add(rows) || (first..first.saturating_add(rows)).contains(&row) {
        return offset;
    }
    history.saturating_sub(row.saturating_sub(rows / 2))
}

impl Session {
    #[cfg(test)]
    pub fn start(grid: GridSize, profile: TerminalProfile) -> io::Result<Self> {
        Self::start_local(grid, profile, LaunchOptions::default(), true, None)
    }

    #[cfg(test)]
    pub(crate) fn start_local(
        grid: GridSize,
        profile: TerminalProfile,
        options: LaunchOptions,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
    ) -> io::Result<Self> {
        Self::start_local_with_strict(
            grid,
            profile,
            options,
            prompt_navigation_enabled,
            appearance,
            false,
        )
    }

    pub(crate) fn start_local_with_strict(
        grid: GridSize,
        profile: TerminalProfile,
        options: LaunchOptions,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
        strict_escapes: bool,
    ) -> io::Result<Self> {
        Self::start_stream(
            StreamSource::Pty(options),
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            strict_escapes,
        )
    }

    pub fn ssh(
        connected: Option<crate::ssh_connection::Connected>,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
    ) -> io::Result<Self> {
        Self::ssh_with_logging(
            connected,
            grid,
            profile,
            prompt_navigation_enabled,
            appearance,
            None,
            false,
        )
    }

    pub(crate) fn ssh_with_logging(
        connected: Option<crate::ssh_connection::Connected>,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
        logging: Option<Box<crate::terminal_logging::Prepared>>,
        strict_escapes: bool,
    ) -> io::Result<Self> {
        Self::start_stream(
            StreamSource::Ssh(connected, logging),
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            strict_escapes,
        )
    }

    fn start_stream(
        source: StreamSource,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: Appearance,
        strict_escapes: bool,
    ) -> io::Result<Self> {
        let (event_send, event_receive) = bounded(32);
        let (input_send, input_receive) = bounded(WRITE_QUEUE_SIZE);
        let (redraw_send, redraw_receive) = bounded(1);
        let (done_send, done_receive) = bounded(1);
        let sftp = match &source {
            StreamSource::Ssh(Some(ssh), _) => ssh.sftp_opener(),
            _ => None,
        };
        let observe_shell = matches!(source, StreamSource::Ssh(Some(_), _));
        let mut terminal = Terminal::with_profile(grid.columns, grid.rows, profile);
        terminal.set_prompt_navigation_enabled(prompt_navigation_enabled);
        set_terminal_appearance(&mut terminal, appearance);
        terminal.set_clipboard_protocol_enabled(true);
        terminal.set_clipboard_write_limit(MAX_CLIPBOARD_TEXT_BYTES);
        let strict = strict_escapes
            || match &source {
                StreamSource::Pty(options) => options.sandbox.is_some(),
                StreamSource::Ssh(Some(ssh), _) => ssh.sandbox_backend().is_some(),
                _ => false,
            };
        terminal.set_strict_escape_filter(strict);
        let (serial, status) = match &source {
            StreamSource::Pty(_) => (None, Status::Starting),
            StreamSource::Ssh(connected, _) => (
                None,
                if connected.is_some() {
                    Status::Starting
                } else {
                    Status::Disconnected
                },
            ),
            StreamSource::Serial {
                connection,
                connect,
            } => (
                Some(Arc::clone(connection)),
                if *connect {
                    Status::Starting
                } else {
                    Status::Disconnected
                },
            ),
        };
        let mut frame = make_frame(&terminal, status, 0, &SearchState::default());
        frame.serial = serial.clone();
        let shared = Arc::new(Shared {
            view: Mutex::new(ViewRequest {
                grid,
                scroll_anchor: None,
                typeahead: TypeAheadRequest::default(),
                prompt_navigation_enabled,
                prompt_navigator: PromptNavigator::default(),
                appearance,
            }),
            frame: Mutex::new(Arc::new(frame)),
            logfile_warning: crate::terminal_logging::Warning::new(redraw_send.clone()),
            redraw: redraw_send,
            bell_events: AtomicU64::new(0),
            output_activity: Arc::default(),
            local_shell_pid: AtomicU32::new(0),
            sandbox_backend: Mutex::default(),
            serial,
            serial_notice: Mutex::new(None),
            shell_activity: observe_shell.then(Mutex::default),
            clipboard: Mutex::new(clipboard::ClipboardBridge::with_strict(strict)),
            recording: Arc::default(),
            journal_available: AtomicBool::new(false),
            journal_generation: AtomicU64::new(0),
            journal_secrets: Mutex::new(None),
            scrollback_compression_enabled: AtomicBool::new(true),
            typewriter_speed: AtomicU8::new(0),
        });
        let worker_shared = Arc::clone(&shared);
        let worker_events = event_send.clone();
        let worker_input = input_send.clone();
        let actor = thread::Builder::new()
            .name("kortty-session".into())
            .spawn(move || {
                let status = run_session(
                    source,
                    terminal,
                    &worker_shared,
                    &worker_events,
                    &event_receive,
                    &worker_input,
                    &input_receive,
                );
                worker_events.close();
                worker_input.close();
                worker_shared
                    .recording
                    .terminal_ended(&worker_shared.redraw);
                if let Err(error) = status {
                    let mut frame = worker_shared
                        .frame
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let last = &**frame;
                    *frame = Arc::new(Frame {
                        screen: last.screen.clone(),
                        status: Status::Error(error.to_string()),
                        scroll_offset: last.scroll_offset,
                        scrollback: last.scrollback.clone(),
                        application_cursor_keys: last.application_cursor_keys,
                        bracketed_paste: last.bracketed_paste,
                        password_prompt: false,
                        command_timestamps: last.command_timestamps.clone(),
                        ibm: None,
                        serial: last.serial.clone(),
                        serial_notice: last.serial_notice.clone(),
                        typeahead: None,
                        prompts: last.prompts.clone(),
                        has_prompt_marks: last.has_prompt_marks,
                        has_finished_command: last.has_finished_command,
                        shell_activity: last.shell_activity.clone(),
                        search_revision: last.search_revision,
                        search_result: last.search_result.clone(),
                    });
                    let _ = worker_shared.redraw.try_send(());
                }
                drop(done_send);
            })?;
        Ok(Self {
            shared,
            backend: Backend::Stream {
                events: event_send,
                input: input_send,
                actor: actor.thread().clone(),
                next_search: AtomicU64::new(1),
                sftp,
            },
            redraw: redraw_receive,
            done: done_receive,
        })
    }

    pub fn frame(&self) -> Arc<Frame> {
        Arc::clone(
            &self
                .shared
                .frame
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    pub(crate) fn local_shell_pid(&self) -> Option<u32> {
        let Backend::Stream { events, .. } = &self.backend else {
            return None;
        };
        let pid = self.shared.local_shell_pid.load(Ordering::Acquire);
        (pid != 0 && !events.is_closed() && self.frame().status == Status::Running).then_some(pid)
    }

    pub(crate) fn local_shell_directory(
        &self,
    ) -> Result<Receiver<io::Result<PathBuf>>, SearchError> {
        let (reply, received) = bounded(1);
        self.send_search_event(Event::LocalShellDirectory(reply))?;
        Ok(received)
    }

    pub(crate) fn sandbox_backend(&self) -> Option<&'static str> {
        *self
            .shared
            .sandbox_backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Reads the authoritative terminal without changing the displayed viewport.
    pub(crate) fn control_read(
        &self,
        pane_id: String,
        mode: ReadMode,
        lines: usize,
        reply: Reply,
    ) -> Result<(), ControlFault> {
        if !reply.active() {
            return Ok(());
        }
        let request = ControlRead {
            pane_id,
            mode,
            lines,
            reply,
        };
        if let Backend::Stream { events, actor, .. } = &self.backend {
            events
                .try_send(Event::ControlRead(request))
                .map_err(|error| match error {
                    TrySendError::Full(_) => {
                        ControlFault::new(ControlError::Busy, "Terminal read queue is full")
                    }
                    TrySendError::Closed(_) => ControlFault::new(
                        ControlError::Unavailable,
                        "Terminal reader is unavailable",
                    ),
                })?;
            actor.unpark();
        } else {
            let frame = self.frame();
            if frame.ibm.is_none() || frame.scroll_offset != 0 {
                return Err(ControlFault::new(
                    ControlError::Unsupported,
                    "This pane has no readable screen",
                ));
            }
            request.answer(&frame.screen, None);
        }
        Ok(())
    }

    pub fn updates(&self) -> Receiver<()> {
        self.redraw.clone()
    }

    pub fn sftp_opener(&self) -> Option<crate::ssh_connection::SftpOpener> {
        let Backend::Stream { sftp, events, .. } = &self.backend else {
            return None;
        };
        sftp.as_ref()
            .filter(|opener| !events.is_closed() && !opener.is_closed())
            .cloned()
    }

    pub fn take_bell_events(&self) -> u64 {
        self.shared.bell_events.swap(0, Ordering::Relaxed)
    }

    pub fn recording_status(&self) -> RecordingStatus {
        let status = self.shared.recording.status();
        if status == RecordingStatus::Idle && self.frame().status != Status::Running {
            return RecordingStatus::Unavailable;
        }
        if !matches!(self.backend, Backend::Stream { .. }) {
            return RecordingStatus::Unavailable;
        }
        status
    }

    pub(crate) fn logfile_notice(&self) -> Option<Arc<str>> {
        self.shared.logfile_warning.current()
    }

    pub(crate) fn journal_available(&self) -> bool {
        self.shared.journal_available.load(Ordering::Acquire)
            && self.frame().status == Status::Running
    }

    pub(crate) fn journal_metadata(
        &self,
        metadata: crate::session_journal::Metadata,
        policy: &crate::terminal_logging::Policy,
    ) -> Option<crate::session_journal::Metadata> {
        if !self.journal_available() {
            return None;
        }
        let secrets = self.shared.journal_secrets.lock().ok()?.clone()?;
        journal::redact_metadata(metadata, secrets, policy)
    }

    pub(crate) fn redact_journal_note(
        &self,
        note: &str,
        policy: &crate::terminal_logging::Policy,
    ) -> Option<zeroize::Zeroizing<String>> {
        let secrets = self.journal_redaction_secrets()?;
        let redactor = crate::terminal_logging::Redactor::for_journal(secrets, policy.clone());
        redactor
            .journal_text(note, Instant::now() + Duration::from_millis(8))
            .filter(|text| text.len() <= crate::session_journal::MAX_RECORD_BYTES)
    }

    pub(crate) fn journal_redaction_secrets(
        &self,
    ) -> Option<crate::terminal_logging::RedactionSecrets> {
        self.shared.journal_secrets.lock().ok()?.clone()
    }

    pub(crate) fn journal_screenshot_allowed(
        &self,
        policy: &crate::terminal_logging::Policy,
        frame: &Frame,
    ) -> bool {
        if !self.journal_available()
            || policy.has_redactions()
            || !self.shared.journal_secrets.lock().is_ok_and(|secrets| {
                secrets
                    .as_ref()
                    .is_some_and(crate::terminal_logging::RedactionSecrets::is_empty)
            })
        {
            return false;
        }
        frame.status == Status::Running
            && frame.scroll_offset == 0
            && !frame.screen.alternate_screen
    }

    pub(crate) fn attach_journal(
        &self,
        sink: crate::session_journal::Sink,
        policy: crate::terminal_logging::Policy,
        seed: bool,
    ) -> Result<(JournalAttachment, Receiver<io::Result<()>>), InputError> {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        if !self.journal_available() {
            return Err(InputError::Closed);
        }
        let id = self
            .shared
            .journal_generation
            .fetch_add(1, Ordering::Relaxed);
        let (reply, received) = bounded(1);
        events
            .try_send(Event::Journal(journal::Command::Attach {
                id,
                sink,
                policy,
                seed,
                reply,
            }))
            .map_err(|error| match error {
                TrySendError::Full(_) => InputError::Busy,
                TrySendError::Closed(_) => InputError::Closed,
            })?;
        actor.unpark();
        Ok((
            JournalAttachment {
                id,
                events: events.clone(),
                actor: actor.clone(),
                done: self.done.clone(),
            },
            received,
        ))
    }

    pub fn start_recording(
        &self,
        path: PathBuf,
        options: CaptureOptions,
    ) -> Result<(), RecordingCommandError> {
        if self.frame().status != Status::Running {
            return Err(RecordingCommandError::Unavailable);
        }
        self.shared
            .recording
            .request_start(path, options, |command| self.recording_command(command))
    }

    pub fn stop_recording(&self) -> Result<(), RecordingCommandError> {
        self.shared
            .recording
            .request_stop(|command| self.recording_command(command))
    }

    fn recording_command(&self, command: recording::Command) -> Result<(), RecordingCommandError> {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return Err(RecordingCommandError::Unavailable);
        };
        events
            .try_send(Event::Recording(command))
            .map_err(|error| match error {
                TrySendError::Full(_) => RecordingCommandError::Busy,
                TrySendError::Closed(_) => RecordingCommandError::Closed,
            })?;
        actor.unpark();
        Ok(())
    }

    pub fn supports_search(&self) -> bool {
        matches!(&self.backend, Backend::Stream { events, .. } if !events.is_closed())
    }

    pub(crate) fn terminal_command(
        &self,
        command: TerminalCommand,
    ) -> Result<Receiver<TerminalCommandResult>, SearchError> {
        let (send, receive) = bounded(1);
        self.send_search_event(Event::TerminalCommand(command, send))?;
        Ok(receive)
    }

    /// Queues a literal search and returns its ticket. Results arrive with a session frame.
    pub fn search(&self, query: &str, ignore_case: bool) -> Result<u64, SearchError> {
        let Backend::Stream { next_search, .. } = &self.backend else {
            return Err(SearchError::Unsupported);
        };
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(SearchError::InvalidQuery);
        }
        let id = next_search.fetch_add(1, Ordering::Relaxed);
        self.send_search_event(Event::Search(SearchRequest {
            id,
            query: query.to_owned(),
            ignore_case,
        }))?;
        Ok(id)
    }

    /// Reveals an absolute match row if its revision is still current on the worker.
    pub fn reveal_search_match(&self, revision: u64, row: usize) -> Result<(), SearchError> {
        self.send_search_event(Event::RevealSearch { revision, row })
    }

    fn send_search_event(&self, event: Event) -> Result<(), SearchError> {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return Err(SearchError::Unsupported);
        };
        events.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => SearchError::Busy,
            TrySendError::Closed(_) => SearchError::Closed,
        })?;
        actor.unpark();
        Ok(())
    }

    pub fn send(&self, bytes: Vec<u8>) -> Result<(), InputError> {
        self.send_raw(bytes, false)
    }

    fn send_raw(&self, bytes: Vec<u8>, mirrored: bool) -> Result<(), InputError> {
        self.send_stream(StreamInput::Raw {
            bytes,
            clear_typeahead: self.typeahead_request().enabled,
            mirrored,
        })
    }

    pub(crate) fn take_output_activity(&self) -> OutputSample {
        self.shared.output_activity.take()
    }

    pub fn send_keyboard(&self, bytes: Vec<u8>, enter: bool) -> Result<(), InputError> {
        if enter
            && (self.shared.serial.is_some()
                || self.frame().screen.profile == TerminalProfile::Hp2392)
        {
            return self.send_physical_enter(bytes, false);
        }
        let request = self.typeahead_request();
        if enter
            || request.enabled
            || (self.shared.shell_activity.is_some()
                && self.frame().screen.profile != TerminalProfile::Hp2392)
        {
            self.send_stream(StreamInput::Keyboard {
                bytes,
                enter,
                generation: request.enabled.then_some(request.generation),
            })
        } else {
            self.send_user_input(bytes, enter)
        }
    }

    pub fn set_typeahead_enabled(&self, enabled: bool) {
        if self.shared.serial.is_some() || self.frame().screen.profile != TerminalProfile::Xterm {
            return;
        }
        self.update_view(|view| {
            if view.typeahead.enabled != enabled {
                view.typeahead.enabled = enabled;
                view.typeahead.generation = view.typeahead.generation.wrapping_add(1);
            }
        });
    }

    pub(crate) fn set_typewriter_speed(&self, speed: Option<u8>) {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return;
        };
        let speed = speed.map_or(0, |speed| speed.clamp(1, 99));
        if self.shared.typewriter_speed.swap(speed, Ordering::Relaxed) != speed {
            let _ = events.try_send(Event::ViewChanged);
            actor.unpark();
        }
    }

    fn typeahead_request(&self) -> TypeAheadRequest {
        requested_view(&self.shared).typeahead
    }

    pub fn send_user_input(&self, bytes: Vec<u8>, enter: bool) -> Result<(), InputError> {
        self.send_interactive(bytes, Hp2392Input::Key { enter }, false)
    }

    pub fn send_paste(&self, bytes: Vec<u8>) -> Result<(), InputError> {
        self.send_interactive(bytes, Hp2392Input::Text, false)
    }

    pub(crate) fn send_interactive(
        &self,
        bytes: Vec<u8>,
        kind: Hp2392Input,
        mirrored: bool,
    ) -> Result<(), InputError> {
        if self.frame().screen.profile == TerminalProfile::Hp2392 {
            self.send_hp_input(bytes, kind, false, mirrored)
        } else if self.shared.serial.is_some() {
            self.send_stream(StreamInput::User {
                bytes,
                enter: matches!(
                    kind,
                    Hp2392Input::Key { enter: true } | Hp2392Input::ShiftEnter
                ),
                record_enter: false,
                mirrored,
            })
        } else {
            self.send_raw(bytes, mirrored)
        }
    }

    pub fn send_shifted_hp_enter(&self, bytes: Vec<u8>) -> Result<(), InputError> {
        self.send_hp_input(bytes, Hp2392Input::ShiftEnter, false, false)
    }

    pub(crate) fn send_physical_enter(
        &self,
        bytes: Vec<u8>,
        shifted: bool,
    ) -> Result<(), InputError> {
        if self.frame().screen.profile == TerminalProfile::Hp2392 {
            let kind = if shifted {
                Hp2392Input::ShiftEnter
            } else {
                Hp2392Input::Key { enter: true }
            };
            self.send_hp_input(bytes, kind, true, false)
        } else if self.shared.serial.is_some() {
            self.send_stream(StreamInput::User {
                bytes,
                enter: true,
                record_enter: true,
                mirrored: false,
            })
        } else {
            self.send_stream(StreamInput::Keyboard {
                bytes,
                enter: true,
                generation: None,
            })
        }
    }

    fn send_hp_input(
        &self,
        bytes: Vec<u8>,
        kind: Hp2392Input,
        record_enter: bool,
        mirrored: bool,
    ) -> Result<(), InputError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(InputError::TooLarge);
        }
        let Backend::Stream {
            events,
            input,
            actor,
            ..
        } = &self.backend
        else {
            return Err(InputError::Closed);
        };
        if input.is_closed() || self.frame().status != Status::Running {
            return Err(InputError::Closed);
        }
        send_input(
            events,
            Event::HpUserInput {
                bytes,
                kind,
                record_enter,
                mirrored,
            },
        )?;
        actor.unpark();
        Ok(())
    }

    pub fn send_serial_break(&self) -> Result<(), InputError> {
        if self.shared.serial.is_none() {
            return Err(InputError::Closed);
        }
        self.send_stream(StreamInput::Break)
    }

    pub(crate) fn disconnect_serial(&self) -> Result<Receiver<()>, InputError> {
        if self.shared.serial.is_none() {
            return Err(InputError::Closed);
        }
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        let (released, done) = bounded(1);
        send_input(events, Event::DisconnectSerial(released))?;
        actor.unpark();
        Ok(done)
    }

    fn send_stream(&self, command: StreamInput) -> Result<(), InputError> {
        if matches!(&command,
            StreamInput::Raw { bytes, .. } | StreamInput::Protocol(bytes, _)
                | StreamInput::Keyboard { bytes, .. } | StreamInput::User { bytes, .. }
            if bytes.len() > MAX_INPUT_BYTES)
        {
            return Err(InputError::TooLarge);
        }
        if self.shared.serial.is_some() && self.frame().status != Status::Running {
            return Err(InputError::Closed);
        }
        let Backend::Stream { input, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        send_input(input, command)
    }

    pub fn send_ibm(&self, input: IbmInput) -> Result<(), InputError> {
        if matches!(&input, IbmInput::Text(text) if text.len() > ibm::MAX_TEXT_BYTES) {
            return Err(InputError::TooLarge);
        }
        let Backend::Ibm(sender) = &self.backend else {
            return Err(InputError::Closed);
        };
        send_input(sender, input)
    }

    pub fn resize(&self, grid: GridSize) {
        self.update_view(|view| view.grid = grid);
    }

    pub fn scroll(&self, lines: i32) {
        let frame = self.frame();
        if frame.screen.alternate_screen {
            return;
        }
        let Some(coordinates) = &frame.scrollback else {
            return;
        };
        self.update_view(|view| {
            let offset = coordinates
                .offset(view.scroll_anchor.as_ref())
                .saturating_add_signed(lines as isize)
                .min(frame.screen.scrollback_lines);
            view.set_scroll_anchor(coordinates.anchor(offset));
        });
    }

    pub(crate) fn scroll_to(&self, anchor: Option<kortty_terminal::ScrollbackAnchor>) {
        self.update_view(|view| view.set_scroll_anchor(anchor));
    }

    pub fn follow_output(&self) {
        if !self.frame().screen.alternate_screen {
            self.update_view(|view| view.set_scroll_anchor(None));
        }
    }

    pub fn set_prompt_navigation_enabled(&self, enabled: bool) {
        self.update_view(|view| {
            view.prompt_navigation_enabled = enabled;
            view.prompt_navigator = PromptNavigator::default();
        });
    }

    pub(crate) fn set_scrollback_compression_enabled(&self, enabled: bool) {
        let Backend::Stream { actor, .. } = &self.backend else {
            return;
        };
        if self
            .shared
            .scrollback_compression_enabled
            .swap(enabled, Ordering::Relaxed)
            != enabled
        {
            actor.unpark();
        }
    }

    #[cfg(test)]
    pub(crate) fn scrollback_compression_requested(&self) -> bool {
        self.shared
            .scrollback_compression_enabled
            .load(Ordering::Relaxed)
    }

    pub(crate) fn set_terminal_appearance(&self, appearance: impl Into<Appearance>) {
        self.update_view(|view| view.appearance = appearance.into());
    }

    /// Returns whether the prompt shortcut belongs to this pane.
    pub fn navigate_prompt(&self, direction: PromptDirection) -> bool {
        let frame = self.frame();
        let Some(prompts) = frame.prompts.as_deref() else {
            return false;
        };
        let Some(coordinates) = &frame.scrollback else {
            return false;
        };
        let mut consumed = false;
        self.update_view(|view| {
            if view.prompt_navigation_enabled {
                let offset = view.prompt_navigator.navigate(
                    prompts,
                    direction,
                    coordinates.offset(view.scroll_anchor.as_ref()),
                );
                // PromptNavigator's consecutive-jump state belongs to this operation.
                view.scroll_anchor = coordinates.anchor(offset);
                consumed = true;
            }
        });
        consumed
    }

    /// Completes when the session worker has released its transport.
    pub(crate) fn shutdown_handle(&self) -> Receiver<()> {
        self.done.clone()
    }

    pub fn stop(&self) -> Receiver<()> {
        match &self.backend {
            Backend::Stream {
                events,
                input,
                actor,
                ..
            } => {
                input.close();
                events.close();
                self.shared.local_shell_pid.store(0, Ordering::Release);
                actor.unpark();
            }
            Backend::Ibm(input) => {
                input.close();
            }
            Backend::Offline => {}
        }
        self.shutdown_handle()
    }

    pub(crate) fn reconnect_history_to(
        &self,
        target: &Self,
    ) -> Option<impl Future<Output = bool> + Send + 'static> {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return None;
        };
        let Backend::Stream {
            events: next_events,
            actor: next_actor,
            ..
        } = &target.backend
        else {
            return None;
        };
        let (events, actor, done) = (events.clone(), actor.clone(), self.done.clone());
        let (next_events, next_actor, next_done) =
            (next_events.clone(), next_actor.clone(), target.done.clone());
        if done.is_closed() {
            return None;
        }
        Some(async move {
            let (reply, history) = bounded(1);
            if events
                .send(Event::CaptureReconnectHistory(reply))
                .await
                .is_err()
            {
                return false;
            }
            actor.unpark();
            let Either::Left((Ok(history), _)) =
                select(pin!(history.recv()), pin!(done.recv())).await
            else {
                return false;
            };
            let (reply, ready) = bounded(1);
            if next_events
                .send(Event::PrependReconnectHistory(Box::new(history), reply))
                .await
                .is_err()
            {
                return false;
            }
            next_actor.unpark();
            matches!(
                select(pin!(ready.recv()), pin!(next_done.recv())).await,
                Either::Left((Ok(()), _))
            )
        })
    }

    fn update_view(&self, update: impl FnOnce(&mut ViewRequest)) {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return;
        };
        let mut view = self
            .shared
            .view
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = view.clone();
        update(&mut view);
        if view.grid != previous.grid || view.scroll_anchor != previous.scroll_anchor {
            view.typeahead.generation = view.typeahead.generation.wrapping_add(1);
        }
        if *view != previous {
            let _ = events.try_send(Event::ViewChanged);
            actor.unpark();
        }
    }
}

fn send_input<T>(sender: &Sender<T>, value: T) -> Result<(), InputError> {
    sender.try_send(value).map_err(|error| match error {
        TrySendError::Full(_) => InputError::Busy,
        TrySendError::Closed(_) => InputError::Closed,
    })
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StreamTransport {
    fn open(
        source: StreamSource,
        grid: GridSize,
        profile: TerminalProfile,
    ) -> io::Result<(Self, StreamReader, StreamWriter)> {
        match source {
            StreamSource::Pty(options) => {
                let mut pty = Pty::spawn_with_options(
                    grid.columns,
                    grid.rows,
                    profile.term_name(),
                    &options,
                )?;
                let reader = pty
                    .take_reader()
                    .ok_or_else(|| io::Error::other("PTY output handle missing"))?;
                let writer = pty
                    .take_writer()
                    .ok_or_else(|| io::Error::other("PTY input handle missing"))?;
                Ok((Self::Pty(pty), Box::new(reader), Box::new(writer)))
            }
            StreamSource::Ssh(Some(mut ssh), _) => {
                let reader = ssh
                    .take_reader()
                    .ok_or_else(|| io::Error::other("SSH output handle missing"))?;
                let writer = ssh
                    .take_writer()
                    .ok_or_else(|| io::Error::other("SSH input handle missing"))?;
                Ok((Self::Ssh(ssh), reader, writer))
            }
            StreamSource::Ssh(None, _) => Err(io::Error::other("SSH session is disconnected")),
            StreamSource::Serial {
                connection,
                connect: true,
            } => {
                let mut port = connection.open()?;
                let reader = port
                    .take_reader()
                    .ok_or_else(|| io::Error::other("serial output handle missing"))?;
                let writer = port
                    .take_writer()
                    .ok_or_else(|| io::Error::other("serial input handle missing"))?;
                Ok((
                    Self::Serial {
                        port: Arc::new(port),
                        connection,
                    },
                    Box::new(reader),
                    Box::new(writer),
                ))
            }
            StreamSource::Serial { connect: false, .. } => {
                Err(io::Error::other("serial session is disconnected"))
            }
        }
    }

    fn exit_code(&mut self) -> io::Result<Option<i32>> {
        match self {
            Self::Pty(pty) => Ok(pty.try_wait()?.and_then(|exit| exit.code())),
            Self::Ssh(ssh) => Ok(ssh.exit_code().and_then(|code| i32::try_from(code).ok())),
            Self::Serial { .. } => Ok(None),
        }
    }

    fn close(&mut self) -> io::Result<()> {
        match self {
            Self::Pty(pty) => pty.shutdown(),
            Self::Ssh(ssh) => ssh.close(),
            Self::Serial { port, .. } => port.close(),
        }
    }

    fn apply_size(&mut self, terminal: &mut Terminal, grid: GridSize) -> io::Result<()> {
        match self {
            Self::Pty(pty) => pty.resize_with_pixels(
                grid.columns,
                grid.rows,
                grid.pixel_width,
                grid.pixel_height,
            )?,
            Self::Ssh(ssh) => {
                if let Err(error) = ssh.resize(ssh_size(grid))
                    && error.kind() != io::ErrorKind::BrokenPipe
                {
                    return Err(error);
                }
            }
            Self::Serial { .. } => {}
        }
        resize_terminal(terminal, grid);
        Ok(())
    }
}

struct LiveShellPid<'a>(&'a AtomicU32);

impl Drop for LiveShellPid<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}

fn run_session(
    mut source: StreamSource,
    mut terminal: Terminal,
    shared: &Shared,
    event_send: &Sender<Event>,
    events: &Receiver<Event>,
    input_send: &Sender<StreamInput>,
    inputs: &Receiver<StreamInput>,
) -> io::Result<()> {
    if events.is_closed() {
        return Ok(());
    }
    let mut grid = shared
        .view
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .grid;
    let mut state = EmulationState {
        search: SearchState::default(),
        recording: recording::Actor::new(Arc::clone(&shared.recording), shared.redraw.clone()),
        logfile: None,
        journal: journal::Actor::default(),
        serial_released: None,
    };
    if matches!(
        source,
        StreamSource::Ssh(None, _) | StreamSource::Serial { connect: false, .. }
    ) {
        inputs.close();
        resize_terminal(&mut terminal, grid);
        show_ended_session(
            &mut terminal,
            shared,
            events,
            &Status::Disconnected,
            &mut state.search,
            &mut grid,
            None,
        );
        return Ok(());
    }
    let logging = match &mut source {
        StreamSource::Pty(_) => {
            state.journal.secrets = crate::terminal_logging::Secrets::default().seal().ok();
            None
        }
        StreamSource::Ssh(_, logging) => {
            state.journal.secrets = logging
                .as_ref()
                .and_then(|prepared| prepared.secrets().seal().ok());
            logging.take()
        }
        _ => None,
    };
    let (mut transport, reader, writer) = StreamTransport::open(source, grid, terminal.profile())?;
    if events.is_closed() {
        return transport.close();
    }
    transport.apply_size(&mut terminal, grid)?;
    if let Some(logging) = logging.filter(|logging| logging.enabled()) {
        match logging.start(shared.logfile_warning.clone()) {
            Ok(capture) => state.logfile = Some(capture),
            Err(error) => shared
                .logfile_warning
                .report(format!("Terminal logging could not start ({error}).")),
        }
    }
    let serial = match &transport {
        StreamTransport::Pty(_) | StreamTransport::Ssh(_) => None,
        StreamTransport::Serial { port, connection } => {
            Some((Arc::clone(port), Arc::clone(connection)))
        }
    };
    let worker_events = WorkerEvents {
        sender: event_send.clone(),
        input: input_send.clone(),
        actor: thread::current(),
        recording: Arc::clone(&shared.recording),
        output_activity: Arc::clone(&shared.output_activity),
        observe_shell: shared.shell_activity.is_some(),
    };
    let reader_events = worker_events.clone();
    let read_thread = thread::Builder::new()
        .name("kortty-stream-read".into())
        .spawn(move || {
            read_output(reader, &reader_events);
        })?;
    let writer_inputs = inputs.clone();
    let write_thread = thread::Builder::new()
        .name("kortty-stream-write".into())
        .spawn(move || {
            write_input(writer, &writer_inputs, &worker_events, serial);
        });
    let published_pid = LiveShellPid(&shared.local_shell_pid);
    if write_thread.is_ok() {
        let backend = match &transport {
            StreamTransport::Pty(pty) => {
                shared
                    .local_shell_pid
                    .store(pty.shell_pid(), Ordering::Release);
                pty.sandbox_backend()
            }
            StreamTransport::Ssh(ssh) => ssh.sandbox_backend(),
            StreamTransport::Serial { .. } => None,
        };
        *shared
            .sandbox_backend
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = backend;
    }
    let result = match &write_thread {
        Ok(_) => {
            if terminal.supports_journal_output()
                && let Some(secrets) = &state.journal.secrets
            {
                *shared
                    .journal_secrets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(secrets.clone());
                shared.journal_available.store(true, Ordering::Release);
            }
            emulate(
                &mut terminal,
                shared,
                events,
                input_send,
                &mut transport,
                &mut grid,
                &mut state,
            )
        }
        Err(error) => Err(io::Error::other(error.to_string())),
    };

    // Unpublish before any wait or shutdown can reap the child and release its PID.
    drop(published_pid);
    inputs.close();
    shared.journal_available.store(false, Ordering::Release);
    *shared
        .journal_secrets
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    state.journal.stop(&mut terminal);
    state.journal.secrets = None;
    state.recording.stop(None);
    let logfile = state
        .logfile
        .take()
        .map(crate::terminal_logging::Capture::finish);
    let shutdown = transport.close();
    // A cancelling read or write may still hold the exclusive serial handle.
    // Release it before exposing a status that allows reconnecting.
    let _ = read_thread.join();
    if let Ok(write_thread) = write_thread {
        let _ = write_thread.join();
    }
    if let Some(logfile) = logfile
        && let Err(error) = logfile.wait()
    {
        shared.logfile_warning.report(format!(
            "Terminal logfile could not be finalized ({error})."
        ));
    }
    drop(state.recording);
    shared.recording.terminal_ended(&shared.redraw);
    clipboard::lock(&shared.clipboard).revoke();
    if !events.is_closed() {
        let status = match (result, shutdown) {
            (Ok(Status::Exited(None)), Ok(())) => {
                Status::Exited(transport.exit_code().ok().flatten())
            }
            (Ok(status), Ok(())) => status,
            (Err(error), _) | (_, Err(error)) => Status::Error(error.to_string()),
        };
        // The process has ended, but the pane can still inspect its output.
        show_ended_session(
            &mut terminal,
            shared,
            events,
            &status,
            &mut state.search,
            &mut grid,
            state.serial_released.take(),
        );
    }
    events.close();
    Ok(())
}

fn read_output(mut reader: impl Read, events: &WorkerEvents) {
    let mut buffer = [0; READ_CHUNK_SIZE];
    loop {
        let event = match reader.read(&mut buffer) {
            Ok(0) => Event::ReadClosed,
            Ok(size) => {
                let now = Instant::now();
                events.output_activity.output(now);
                Event::Output(buffer[..size].to_vec(), now)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => Event::IoError(error.to_string()),
        };
        let finished = !matches!(event, Event::Output(..));
        if !events.send(event) {
            break;
        }
        if finished {
            break;
        }
    }
}

fn write_input(
    mut writer: impl Write,
    inputs: &Receiver<StreamInput>,
    events: &WorkerEvents,
    serial: Option<(Arc<SerialPort>, Arc<SerialConnection>)>,
) {
    while !inputs.is_closed() {
        let Ok(command) = inputs.recv_blocking() else {
            break;
        };
        if inputs.is_closed() {
            break;
        }
        let recording_generation = if matches!(command, StreamInput::Protocol(..)) {
            0
        } else {
            events.recording.generation()
        };
        let mirrored = matches!(
            &command,
            StreamInput::Raw { mirrored: true, .. }
                | StreamInput::User { mirrored: true, .. }
                | StreamInput::Protocol(_, true)
        );
        let (bytes, before_write) = match command {
            StreamInput::Protocol(bytes, _) => (bytes, None),
            StreamInput::Raw {
                bytes,
                clear_typeahead,
                ..
            } => (
                bytes,
                if events.observe_shell {
                    Some(Event::UntrackedShellInput { clear_typeahead })
                } else {
                    clear_typeahead.then_some(Event::TypeAheadClear)
                },
            ),
            StreamInput::Keyboard {
                bytes,
                enter,
                generation,
            } => {
                let event = Event::KeyboardInput {
                    bytes: bytes.clone(),
                    enter,
                    sent_at: Instant::now(),
                    wall_time: enter.then(SystemTime::now),
                    generation,
                };
                (bytes, Some(event))
            }
            StreamInput::User {
                bytes,
                enter,
                record_enter,
                ..
            } => {
                if record_enter
                    && !events.send(Event::CommandEnter(SystemTime::now(), Instant::now()))
                {
                    break;
                }
                if let Some((_, connection)) = &serial {
                    let (bytes, echo) = connection.prepare_input(bytes, enter);
                    (bytes, echo.map(Event::Echo))
                } else {
                    (
                        bytes,
                        events.observe_shell.then_some(Event::UntrackedShellInput {
                            clear_typeahead: false,
                        }),
                    )
                }
            }
            StreamInput::Break => {
                if let Some((port, _)) = &serial {
                    let notice = match port.send_break(serial::BREAK_DURATION) {
                        Ok(()) => Arc::from("Break sent."),
                        Err(error) => Arc::from(format!("Serial break failed ({error})")),
                    };
                    if !events.send(Event::SerialNotice(notice)) {
                        break;
                    }
                }
                continue;
            }
        };
        if let Some(event) = before_write {
            // Queue the prediction before writing: even an immediate PTY echo
            // cannot overtake the input that produced it.
            if !events.send(event) {
                break;
            }
        }
        if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
            events.send(Event::IoError(error.to_string()));
            inputs.close();
            break;
        }
        if !bytes.is_empty() {
            if mirrored {
                events.output_activity.mirrored_write(Instant::now());
            }
            events.recording_activity(recording_generation);
        }
    }
}

fn emulate(
    terminal: &mut Terminal,
    shared: &Shared,
    events: &Receiver<Event>,
    input: &Sender<StreamInput>,
    transport: &mut StreamTransport,
    grid: &mut GridSize,
    state: &mut EmulationState,
) -> io::Result<Status> {
    let EmulationState {
        search,
        recording,
        logfile,
        journal,
        serial_released,
    } = state;
    apply_view_preferences(terminal, &requested_view(shared));
    publish(shared, make_frame(terminal, Status::Running, 0, search));
    let mut last_frame = Instant::now();
    let mut status = Status::Running;
    let mut pack_at = None;
    let mut typeahead = TypeAhead::default();
    let mut typeahead_request = TypeAheadRequest::default();
    let mut pacer = (terminal.profile() == TerminalProfile::Xterm).then(typewriter::Pacer::default);
    while let Some(first) = next_stream_event(
        terminal,
        shared,
        events,
        &mut pack_at,
        typeahead.deadline(),
        pacer.as_mut(),
    ) {
        // Keep the most recent frame without rendering every transport read.
        if let Some(delay) = FRAME_INTERVAL.checked_sub(last_frame.elapsed()) {
            thread::sleep(delay);
        }
        let mut request = None;
        let mut reveal = None;
        let mut screen_changed = false;
        let mut refresh_frame = false;
        let mut control_reads = Vec::new();
        let mut command_replies = Vec::new();
        let mut history_replies = Vec::new();
        let mut view = requested_view(shared);
        if pacer.is_some() && shared.typewriter_speed().is_some() {
            view.typeahead.enabled = false;
        }
        let can_predict = sync_typeahead(
            &mut typeahead,
            &mut typeahead_request,
            &view,
            terminal.scrollback_offset(view.scroll_anchor.as_ref()),
            *grid,
        );
        let queued = std::iter::from_fn(|| poll_stream_event(shared, events, pacer.as_mut()).ok());
        for event in std::iter::once(first).chain(queued).take(32) {
            match event {
                Event::Output(bytes, received_at) => {
                    if let Some(capture) = logfile {
                        capture.advance(&bytes);
                    }
                    if !bytes.is_empty() {
                        search.invalidate();
                        screen_changed = true;
                    }
                    apply_view_preferences(terminal, &requested_view(shared));
                    terminal.advance_timed(&bytes, SystemTime::now(), received_at);
                    if let Some(activity) = &shared.shell_activity {
                        activity
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .output(
                                !terminal.alternate_screen_active(),
                                terminal.take_shell_events(),
                                received_at,
                            );
                    }
                    shared.report_bells(terminal.take_bell_events());
                    let clipboard_replies = clipboard::lock(&shared.clipboard).capture(terminal);
                    if can_predict {
                        typeahead.on_output(terminal, received_at);
                    }
                    let mut responses = terminal.take_responses();
                    responses.extend(clipboard_replies);
                    if !queue_protocol_reply(input, responses) {
                        status = Status::Error("Terminal reply queue is full".into());
                        break;
                    }
                }
                Event::Clipboard => {
                    let applied = clipboard::lock(&shared.clipboard).apply(terminal);
                    typeahead.clear();
                    let fallback_sent = applied.fallback.is_none_or(|bytes| {
                        !matches!(
                            input.try_send(StreamInput::User {
                                bytes,
                                enter: false,
                                record_enter: false,
                                mirrored: false,
                            }),
                            Err(TrySendError::Full(_))
                        )
                    });
                    if !fallback_sent || !queue_protocol_reply(input, applied.bytes) {
                        status = Status::Error("Terminal reply queue is full".into());
                        break;
                    }
                }
                Event::KeyboardInput {
                    bytes,
                    enter,
                    sent_at,
                    wall_time,
                    generation,
                } => {
                    if let Some(wall_time) = wall_time {
                        terminal.record_command_enter(wall_time, sent_at);
                    }
                    if let Some(activity) = &shared.shell_activity {
                        activity
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .keyboard(!terminal.alternate_screen_active(), &bytes, enter, sent_at);
                    }
                    if can_predict && generation == Some(typeahead_request.generation) {
                        typeahead.on_input(terminal, &bytes, sent_at);
                    }
                }
                Event::UntrackedShellInput { clear_typeahead } => {
                    if let Some(activity) = &shared.shell_activity {
                        activity
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .untracked_input();
                    }
                    if clear_typeahead {
                        typeahead.clear();
                    }
                }
                Event::TypeAheadClear => {
                    typeahead.clear();
                }
                Event::CommandEnter(wall, now) => terminal.record_command_enter(wall, now),
                Event::CommandTimestampQuiet => {
                    terminal.finish_quiet_command(SystemTime::now(), Instant::now());
                }
                Event::Echo(bytes) => {
                    search.invalidate();
                    terminal.echo_serial_input(&bytes);
                    screen_changed = true;
                }
                Event::HpUserInput {
                    bytes,
                    kind,
                    record_enter,
                    mirrored,
                } => {
                    if record_enter {
                        terminal.record_command_enter(SystemTime::now(), Instant::now());
                    }
                    let sent = if terminal.hp2392_local_input(&bytes, kind) {
                        search.invalidate();
                        screen_changed = true;
                        recording.activity(shared.recording.generation(), Instant::now());
                        queue_reply(input, terminal.take_responses(), mirrored)
                    } else {
                        let enter = matches!(
                            kind,
                            Hp2392Input::Key { enter: true } | Hp2392Input::ShiftEnter
                        );
                        !matches!(
                            input.try_send(StreamInput::User {
                                bytes,
                                enter,
                                record_enter: false,
                                mirrored,
                            }),
                            Err(TrySendError::Full(_))
                        )
                    };
                    if !sent {
                        status = Status::Error("Terminal input queue is full".into());
                        break;
                    }
                }
                Event::Recording(command) => {
                    let appearance = requested_view(shared).appearance;
                    if screen_changed || recording.appearance_changed(appearance) {
                        recording.frame(terminal, *grid, appearance);
                        screen_changed = false;
                    }
                    recording.command(command, terminal, *grid, appearance);
                }
                Event::Journal(command) => journal.command(terminal, command),
                Event::RecordingActivity { generation, at } => recording.activity(generation, at),
                Event::SerialNotice(notice) => {
                    *shared
                        .serial_notice
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(notice);
                }
                Event::DisconnectSerial(released) => {
                    *serial_released = Some(released);
                    status = Status::Disconnected;
                    break;
                }
                Event::ViewChanged => {}
                Event::LocalShellDirectory(reply) => {
                    if let StreamTransport::Pty(pty) = transport {
                        let _ = reply.try_send(pty.shell_directory());
                    }
                    continue;
                }
                Event::Search(next) => request = Some(next),
                Event::ControlRead(request) => {
                    control_reads.push(request);
                    continue;
                }
                Event::TerminalCommand(command, reply) => {
                    let result = apply_terminal_command(terminal, shared, search, command);
                    if matches!(result, TerminalCommandResult::Cleared) {
                        typeahead.clear();
                        screen_changed = true;
                    }
                    if matches!(result, TerminalCommandResult::Copied { .. }) {
                        let _ = reply.try_send(result);
                        continue;
                    }
                    command_replies.push((reply, result));
                }
                Event::CaptureReconnectHistory(reply) => {
                    let _ = reply.try_send(terminal.reconnect_history());
                    continue;
                }
                Event::PrependReconnectHistory(history, reply) => {
                    terminal.prepend_reconnect_history(*history);
                    search.invalidate();
                    screen_changed = true;
                    history_replies.push(reply);
                }
                Event::RevealSearch { revision, row } => reveal = Some((revision, row)),
                Event::ReadClosed => {
                    // Querying a local exit status can reap the shell.
                    shared.local_shell_pid.store(0, Ordering::Release);
                    status = Status::Exited(transport.exit_code().ok().flatten());
                    break;
                }
                Event::IoError(message) => {
                    status = Status::Error(message);
                    break;
                }
            }
            refresh_frame = true;
        }
        let view = requested_view(shared);
        if status == Status::Running && *grid != view.grid {
            transport.apply_size(terminal, view.grid)?;
            if (grid.columns, grid.rows) != (view.grid.columns, view.grid.rows) {
                search.invalidate();
            }
            *grid = view.grid;
            screen_changed = true;
        }
        recording.check_overflow();
        if screen_changed || recording.appearance_changed(view.appearance) {
            recording.frame(terminal, *grid, view.appearance);
        }
        if let Some((revision, row)) = reveal {
            search.reveal(shared, revision, row);
        }
        if let Some(request) = request {
            search.find(terminal, request);
        }
        if status != Status::Running {
            answer_control_reads(terminal, None, control_reads);
            answer_terminal_commands(command_replies);
            break;
        }
        if !refresh_frame && !screen_changed {
            answer_control_reads(terminal, None, control_reads);
            continue;
        }
        let view = requested_view(shared);
        apply_view_preferences(terminal, &view);
        let offset = terminal.scrollback_offset(view.scroll_anchor.as_ref());
        let can_predict =
            sync_typeahead(&mut typeahead, &mut typeahead_request, &view, offset, *grid);
        typeahead.expire(Instant::now());
        let mut frame = make_frame(terminal, status.clone(), offset, search);
        let live = (frame.scroll_offset == 0).then_some(&frame.screen);
        answer_control_reads(terminal, live, control_reads);
        if can_predict && let Some(overlay) = typeahead.overlay() {
            frame.typeahead = Some(Arc::new(TypeAheadFrame {
                row: overlay.row,
                cursor_column: overlay.cursor_column,
                cells: overlay.cells.to_vec(),
            }));
        }
        publish(shared, frame);
        answer_terminal_commands(command_replies);
        for reply in history_replies {
            let _ = reply.try_send(());
        }
        last_frame = Instant::now();
        pack_at = Some(last_frame + SCROLLBACK_IDLE);
    }
    Ok(status)
}

fn apply_terminal_command(
    terminal: &mut Terminal,
    shared: &Shared,
    search: &mut SearchState,
    command: TerminalCommand,
) -> TerminalCommandResult {
    terminal.set_prompt_navigation_enabled(requested_view(shared).prompt_navigation_enabled);
    match command {
        TerminalCommand::ClearBuffer if terminal.clear_buffer() => {
            let mut view = shared
                .view
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            view.scroll_anchor = None;
            view.prompt_navigator = PromptNavigator::default();
            search.invalidate();
            TerminalCommandResult::Cleared
        }
        TerminalCommand::ClearBuffer => {
            TerminalCommandResult::LastOutput(LastOutput::AlternateScreen)
        }
        TerminalCommand::SelectLastOutput => match terminal.last_output() {
            LastOutput::Found(range) => TerminalCommandResult::Selected {
                revision: search.revision,
                range,
            },
            other => TerminalCommandResult::LastOutput(other),
        },
        TerminalCommand::CopyLastOutput => match terminal.last_output() {
            LastOutput::Found(range) => copy_terminal_output(terminal, range),
            other => TerminalCommandResult::LastOutput(other),
        },
        TerminalCommand::CopySelection { revision, range } => {
            if revision != search.revision || !requested_view(shared).prompt_navigation_enabled {
                return TerminalCommandResult::SelectionChanged;
            }
            copy_terminal_output(terminal, range)
        }
    }
}

fn copy_terminal_output(terminal: &Terminal, range: LastOutputRange) -> TerminalCommandResult {
    match terminal.last_output_text(range) {
        Some(text) if !text.is_empty() => TerminalCommandResult::Copied {
            text,
            truncated: range.truncated,
        },
        Some(_) => TerminalCommandResult::LastOutput(LastOutput::NoOutput),
        None => TerminalCommandResult::OutputTooLarge,
    }
}

fn answer_terminal_commands(
    replies: impl IntoIterator<Item = (Sender<TerminalCommandResult>, TerminalCommandResult)>,
) {
    for (reply, result) in replies {
        let _ = reply.try_send(result);
    }
}

fn sync_typeahead(
    predictor: &mut TypeAhead,
    previous: &mut TypeAheadRequest,
    view: &ViewRequest,
    scroll_offset: usize,
    current_grid: GridSize,
) -> bool {
    let live = scroll_offset == 0 && view.grid == current_grid;
    if *previous != view.typeahead || !live {
        predictor.clear();
        *previous = view.typeahead;
    }
    live && view.typeahead.enabled
}

fn queue_protocol_reply(input: &Sender<StreamInput>, response: Vec<u8>) -> bool {
    queue_reply(input, response, false)
}

fn queue_reply(input: &Sender<StreamInput>, response: Vec<u8>, mirrored: bool) -> bool {
    response.is_empty()
        || !matches!(
            input.try_send(StreamInput::Protocol(response, mirrored)),
            Err(TrySendError::Full(_))
        )
}

fn show_ended_session(
    terminal: &mut Terminal,
    shared: &Shared,
    events: &Receiver<Event>,
    status: &Status,
    search: &mut SearchState,
    grid: &mut GridSize,
    serial_released: Option<Sender<()>>,
) {
    apply_view_preferences(terminal, &requested_view(shared));
    publish(
        shared,
        make_frame(
            terminal,
            status.clone(),
            requested_offset(shared, terminal),
            search,
        ),
    );
    // Both I/O threads have returned, so the exclusive serial handle has been released.
    drop(serial_released);
    let mut pack_at = Some(Instant::now() + SCROLLBACK_IDLE);
    while let Some(event) = next_stream_event(terminal, shared, events, &mut pack_at, None, None) {
        let mut command_reply = None;
        match event {
            Event::Journal(command) => {
                command.unavailable();
                continue;
            }
            Event::Clipboard => {
                clipboard::lock(&shared.clipboard).apply(terminal);
            }
            Event::DisconnectSerial(released) => {
                // The port may have closed naturally before this request was processed.
                drop(released);
                continue;
            }
            Event::Search(request) => search.find(terminal, request),
            Event::ControlRead(request) => {
                answer_control_reads(terminal, None, std::iter::once(request));
                continue;
            }
            Event::TerminalCommand(command, reply) => {
                let result = apply_terminal_command(terminal, shared, search, command);
                command_reply = Some((reply, result));
            }
            Event::CaptureReconnectHistory(reply) => {
                let _ = reply.try_send(terminal.reconnect_history());
                continue;
            }
            Event::RevealSearch { revision, row } => search.reveal(shared, revision, row),
            Event::ViewChanged => {
                let view = requested_view(shared);
                apply_view_preferences(terminal, &view);
                let requested = view.grid;
                if requested != *grid {
                    if (grid.columns, grid.rows) != (requested.columns, requested.rows) {
                        search.invalidate();
                    }
                    resize_terminal(terminal, requested);
                    *grid = requested;
                }
            }
            _ => continue,
        }
        publish(
            shared,
            make_frame(
                terminal,
                status.clone(),
                requested_offset(shared, terminal),
                search,
            ),
        );
        answer_terminal_commands(command_reply);
        pack_at = Some(Instant::now() + SCROLLBACK_IDLE);
    }
}

fn answer_control_reads(
    terminal: &Terminal,
    live: Option<&ScreenSnapshot>,
    requests: impl IntoIterator<Item = ControlRead>,
) {
    let mut requests = requests
        .into_iter()
        .filter(|request| request.reply.active())
        .peekable();
    if requests.peek().is_none() {
        return;
    }
    let snapshot;
    let live = match live {
        Some(live) => live,
        None => {
            snapshot = terminal.snapshot();
            &snapshot
        }
    };
    for request in requests {
        request.answer(live, Some(terminal));
    }
}

fn control_screen(
    pane_id: String,
    mode: ReadMode,
    limit: usize,
    live: &ScreenSnapshot,
    terminal: Option<&Terminal>,
    active: impl Fn() -> bool,
) -> Option<ControlScreen> {
    let recent = mode == ReadMode::Recent;
    let history = if recent && terminal.is_some() {
        live.scrollback_lines
    } else {
        0
    };
    let rows = usize::from(live.rows);
    let max_lines = if recent {
        limit.clamp(1, MAX_CONTROL_READ_LINES)
    } else {
        rows
    };
    let mut lines = Vec::new();
    let mut size = 0;
    let mut longest = 0;
    let mut truncated = false;
    for row in (0..history + rows).rev() {
        if row.is_multiple_of(32) && !active() {
            return None;
        }
        if lines.len() == max_lines {
            break;
        }
        let line = if row < history {
            let Some(cells) = terminal.and_then(|terminal| terminal.history_line(row)) else {
                continue;
            };
            control_line(&cells)
        } else {
            let Some(cells) = live.row((row - history) as u16) else {
                continue;
            };
            control_line(cells)
        };
        // Recent output excludes the blank padding after the last content row.
        if recent && lines.is_empty() && line.is_empty() {
            continue;
        }
        let cost = json_line_cost(&line);
        longest = longest.max(cost);
        if size + cost + 2 * longest > CONTROL_LINE_BUDGET {
            truncated = true;
            break;
        }
        size += cost;
        lines.push(line);
    }
    lines.reverse();
    Some(ControlScreen {
        pane_id,
        mode: mode.name().into(),
        lines,
        columns: live.columns,
        rows: live.rows,
        alternate_screen: !recent && live.alternate_screen,
        osc_title: (!live.title.is_empty()).then(|| live.title.clone()),
        truncated,
    })
}

fn control_line(cells: &[Cell]) -> String {
    let mut line = String::new();
    let mut spaces = 0;
    for cell in cells {
        if cell.width == 0 {
            continue;
        }
        if cell.style.hidden || cell.text.is_empty() {
            spaces += usize::from(cell.width);
        } else {
            line.extend(std::iter::repeat_n(' ', spaces));
            line.push_str(&cell.text);
            spaces = 0;
        }
    }
    // Match Java String.stripTrailing, which leaves non-breaking spaces intact.
    let trimmed = line.trim_end_matches(|character: char| {
        character.is_whitespace()
            && !matches!(character, '\u{00a0}' | '\u{0085}' | '\u{2007}' | '\u{202f}')
    });
    line.truncate(trimmed.len());
    line
}

fn json_line_cost(line: &str) -> usize {
    // Quoting and a comma, plus serde_json's escaping for each byte.
    3 + line
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

fn next_stream_event(
    terminal: &mut Terminal,
    shared: &Shared,
    events: &Receiver<Event>,
    pack_at: &mut Option<Instant>,
    typeahead_deadline: Option<Instant>,
    mut pacer: Option<&mut typewriter::Pacer>,
) -> Option<Event> {
    while !events.is_closed() {
        sync_scrollback_compression(
            terminal,
            pack_at,
            shared
                .scrollback_compression_enabled
                .load(Ordering::Relaxed),
        );
        if clipboard::lock(&shared.clipboard).has_commands() {
            return Some(Event::Clipboard);
        }
        match poll_stream_event(shared, events, pacer.as_deref_mut()) {
            Ok(event) => return Some(event),
            Err(TryRecvError::Closed) => return None,
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if typeahead_deadline.is_some_and(|deadline| deadline <= now) {
            return Some(Event::TypeAheadClear);
        }
        let timestamp_deadline = terminal.command_timestamp_deadline();
        if timestamp_deadline.is_some_and(|deadline| deadline <= now) {
            return Some(Event::CommandTimestampQuiet);
        }
        if pack_at.is_some_and(|deadline| deadline <= now) {
            let offset = requested_offset(shared, terminal);
            let more = terminal.compress_idle_scrollback(offset);
            *pack_at = more.then(|| Instant::now() + SCROLLBACK_SLICE_INTERVAL);
        } else if !events.is_empty() {
            continue;
        } else if let Some(deadline) = (*pack_at)
            .into_iter()
            .chain(typeahead_deadline)
            .chain(timestamp_deadline)
            .chain(pacer.as_deref().and_then(typewriter::Pacer::deadline))
            .min()
        {
            thread::park_timeout(deadline.saturating_duration_since(now));
        } else {
            // A send or close unparks this actor, even if it happens before we park.
            thread::park();
        }
    }
    None
}

fn sync_scrollback_compression(
    terminal: &mut Terminal,
    pack_at: &mut Option<Instant>,
    enabled: bool,
) {
    let changed = terminal.set_scrollback_compression_enabled(enabled);
    if !enabled {
        *pack_at = None;
    } else if changed {
        *pack_at = Some(Instant::now() + SCROLLBACK_IDLE);
    }
}

fn poll_stream_event(
    shared: &Shared,
    events: &Receiver<Event>,
    pacer: Option<&mut typewriter::Pacer>,
) -> Result<Event, TryRecvError> {
    match pacer {
        Some(pacer) => pacer.poll(events, shared.typewriter_speed(), Instant::now()),
        None => events.try_recv(),
    }
}

fn requested_offset(shared: &Shared, terminal: &Terminal) -> usize {
    terminal.scrollback_offset(requested_view(shared).scroll_anchor.as_ref())
}

fn apply_view_preferences(terminal: &mut Terminal, view: &ViewRequest) {
    terminal.set_prompt_navigation_enabled(view.prompt_navigation_enabled);
    set_terminal_appearance(terminal, view.appearance);
}

fn set_terminal_appearance(terminal: &mut Terminal, appearance: Appearance) {
    terminal.set_display_colors(
        Some(appearance.foreground.bytes()),
        Some(appearance.background.bytes()),
    );
    terminal.set_display_palette(Some(appearance.cursor.bytes()), appearance.palette);
    terminal.set_sgr_colors_enabled(appearance.sgr_enabled);
    terminal.set_default_cursor_style(appearance.cursor_shape, appearance.cursor_blink);
    terminal.set_cursor_blink_allowed(appearance.cursor_blink);
}

fn requested_view(shared: &Shared) -> ViewRequest {
    shared
        .view
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn resize_terminal(terminal: &mut Terminal, grid: GridSize) {
    terminal.resize(grid.columns, grid.rows);
    terminal.set_pixel_size(
        grid.cell_width,
        grid.cell_height,
        grid.pixel_width,
        grid.pixel_height,
    );
}

pub fn ssh_size(grid: GridSize) -> WindowSize {
    WindowSize {
        columns: grid.columns.into(),
        rows: grid.rows.into(),
        pixel_width: grid.pixel_width.into(),
        pixel_height: grid.pixel_height.into(),
    }
}

fn make_frame(
    terminal: &Terminal,
    status: Status,
    scroll_offset: usize,
    search: &SearchState,
) -> Frame {
    let screen = terminal.viewport_snapshot(scroll_offset);
    let scroll_offset = scroll_offset.min(screen.scrollback_lines);
    let prompts = terminal.prompt_layout().map(Arc::new);
    let has_finished_command = terminal.has_finished_command();
    let has_prompt_marks = has_finished_command
        || prompts.is_some()
        || (screen.alternate_screen && terminal.has_prompt_marks());
    Frame {
        screen,
        status,
        scroll_offset,
        scrollback: Some(terminal.scrollback_snapshot()),
        application_cursor_keys: terminal.application_cursor_keys(),
        bracketed_paste: terminal.bracketed_paste(),
        password_prompt: terminal.cursor_line_is_password_prompt(),
        command_timestamps: terminal.command_timestamps(scroll_offset),
        ibm: None,
        serial: None,
        serial_notice: None,
        typeahead: None,
        has_prompt_marks,
        has_finished_command,
        shell_activity: ShellActivity::default(),
        prompts,
        search_revision: search.revision,
        search_result: search.result.clone(),
    }
}

fn publish(shared: &Shared, mut frame: Frame) {
    if let Some(activity) = &shared.shell_activity {
        frame.shell_activity = activity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot();
    }
    if let Some(serial) = &shared.serial {
        frame.serial = Some(Arc::clone(serial));
        frame.serial_notice = shared
            .serial_notice
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
    }
    *shared
        .frame
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Arc::new(frame);
    let _ = shared.redraw.try_send(());
}

#[cfg(test)]
mod search_tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(8);

    #[test]
    fn scrollback_compression_actor_cancels_and_resumes_the_idle_deadline() {
        let mut terminal = Terminal::new(16, 4);
        let before = Instant::now();
        let mut deadline = Some(before);
        sync_scrollback_compression(&mut terminal, &mut deadline, false);
        assert!(deadline.is_none());
        assert!(!terminal.set_scrollback_compression_enabled(false));

        sync_scrollback_compression(&mut terminal, &mut deadline, true);
        let resumed = deadline.unwrap();
        assert!(resumed >= before + SCROLLBACK_IDLE);
        assert!(!terminal.set_scrollback_compression_enabled(true));
        sync_scrollback_compression(&mut terminal, &mut deadline, true);
        assert_eq!(deadline, Some(resumed));
    }

    #[cfg(unix)]
    #[test]
    fn held_pty_viewport_survives_output_delayed_thumb_and_alternate_input() {
        let session = Session::start(
            GridSize {
                columns: 64,
                rows: 4,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        let command = concat!(
            r#"sh -c 'saved=$(stty -g); trap "stty $saved" EXIT; stty -echo; "#,
            r#"i=0; while [ "$i" -lt 40 ]; do printf "held-row-%02d\n" "$i"; i=$((i+1)); done; "#,
            r#"printf "%s%s\n" "held-" "ready"; IFS= read -r gate; "#,
            r#"i=0; while [ "$i" -lt 80 ]; do printf "new-row-%02d\n" "$i"; i=$((i+1)); done; "#,
            r#"printf "%s%s\n" "growth-" "complete"; IFS= read -r gate; "#,
            r#"printf "\033[?1049h%s%s" "alternate-" "ready"; IFS= read -r gate; "#,
            r#"printf "\033[?1049l"; IFS= read -r gate'"#,
            "\r"
        );
        session.send(command.as_bytes().to_vec()).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "held-ready"));
        session.scroll(10);
        let first = wait_for_frame(&session, |frame| frame.scroll_offset == 10);
        let first_text = frame_first_row(&first);
        let delayed_thumb = first.scrollback.as_ref().unwrap().anchor(10);
        session.scroll(10);
        let held = wait_for_frame(&session, |frame| frame.scroll_offset == 20);
        let held_text = frame_first_row(&held);
        let initial_history = held.screen.scrollback_lines;
        session.send(b"grow\r".to_vec()).unwrap();
        let grown = wait_for_frame(&session, |frame| {
            frame.screen.scrollback_lines >= initial_history + 80
        });
        assert_eq!(frame_first_row(&grown), held_text);
        assert!(grown.scroll_offset >= 100);

        session.scroll_to(delayed_thumb);
        wait_for_frame(&session, |frame| frame_first_row(frame) == first_text);
        session.send(b"alternate\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            frame.screen.alternate_screen && visible_text(frame, "alternate-ready")
        });
        session.follow_output();
        session.scroll(-1);
        session.send(b"return\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            !frame.screen.alternate_screen && frame_first_row(frame) == first_text
        });
        session.follow_output();
        wait_for_frame(&session, |frame| {
            frame.scroll_offset == 0 && visible_text(frame, "growth-complete")
        });
        let _ = session.stop().recv_blocking();
    }

    #[cfg(unix)]
    fn frame_first_row(frame: &Frame) -> String {
        frame
            .screen
            .row(0)
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn prompt_navigation_uses_real_local_pty_output() {
        let session = Session::start(
            GridSize {
                rows: 4,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert!(!session.navigate_prompt(PromptDirection::Previous));
        session
            .send(
                concat!(
                    r"printf '\033]133;A\007%s-one\033]133;C\007\n\n\n\n\n' 'native-prompt'; ",
                    r"printf '\033]133;A\007%s-two\033]133;C\007\n\n\n\n\n' 'native-prompt'; ",
                    r"printf '%s-live\n' 'native-prompt'",
                    "\r",
                )
                .as_bytes()
                .to_vec(),
            )
            .unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "native-prompt-live"));
        for name in ["native-prompt-two", "native-prompt-one"] {
            assert!(session.navigate_prompt(PromptDirection::Previous));
            wait_for_frame(&session, |frame| {
                frame.scroll_offset > 0
                    && frame.screen.row(0).is_some_and(|cells| {
                        cells
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            == name
                    })
            });
        }
        assert!(session.navigate_prompt(PromptDirection::Previous));
        assert!(session.navigate_prompt(PromptDirection::Next));
        assert!(session.navigate_prompt(PromptDirection::Next));
        wait_for_frame(&session, |frame| frame.scroll_offset == 0);
        session.set_prompt_navigation_enabled(false);
        assert!(!session.navigate_prompt(PromptDirection::Previous));
        wait_for_frame(&session, |frame| frame.prompts.is_none());
        session.set_prompt_navigation_enabled(true);
        wait_for_frame(&session, |frame| frame.prompts.is_some());
        assert!(session.navigate_prompt(PromptDirection::Previous));
        session.send(b"exit\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
    }

    #[test]
    fn control_screen_masks_hidden_cells_and_uses_active_buffer_geometry() {
        let mut terminal = Terminal::new(16, 3);
        terminal.advance("\x1b]2;read-title\x07a界\x1b[8m隠x\x1b[28mz   ".as_bytes());
        let visible = read_test_screen(&terminal, ReadMode::Visible, 1);
        assert_eq!(visible.pane_id, "pane-test");
        assert_eq!(visible.mode, "visible");
        assert_eq!((visible.columns, visible.rows), (16, 3));
        assert_eq!(visible.osc_title.as_deref(), Some("read-title"));
        assert_eq!(visible.lines, ["a界   z", "", ""]);

        terminal.advance(b"\r\nhistory\r\nnew\r\nlast\r\n");
        let recent = read_test_screen(&terminal, ReadMode::Recent, 3);
        assert_eq!(recent.lines, ["history", "new", "last"]);
        assert!(!recent.truncated);
        assert!(!recent.alternate_screen);

        terminal.advance(b"\x1b[?1049h\x1b[Halternate");
        let visible = read_test_screen(&terminal, ReadMode::Visible, 1);
        let recent = read_test_screen(&terminal, ReadMode::Recent, 200);
        assert!(visible.alternate_screen);
        assert!(!recent.alternate_screen);
        assert_eq!(recent.lines, ["alternate"]);
    }

    #[test]
    fn control_screen_truncates_at_the_escaped_wire_budget() {
        let escaped = "a\u{0000}\u{0008}\u{000c}\n\r\t\"\\界";
        assert_eq!(
            json_line_cost(escaped),
            serde_json::to_vec(escaped).unwrap().len() + 1
        );
        let mut terminal = Terminal::new(240, 40);
        let glyph = format!("\"{}", "\u{0301}".repeat(40));
        let content = glyph.repeat(220);
        for row in 0..40 {
            let line = format!("{row:02}{content}");
            terminal.advance(line.as_bytes());
            if row < 39 {
                terminal.advance(b"\r\n");
            }
        }
        let screen = read_test_screen(&terminal, ReadMode::Visible, 1);
        assert!(screen.truncated);
        assert!(!screen.lines.first().unwrap().starts_with("00"));
        assert!(screen.lines.last().unwrap().starts_with("39"));
        let longest = screen
            .lines
            .iter()
            .max_by_key(|line| json_line_cost(line))
            .unwrap();
        let wrapped = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"screen": &screen, "match": longest, "line": longest}
        });
        assert!(serde_json::to_vec(&wrapped).unwrap().len() <= 512 * 1024);
    }

    fn read_test_screen(terminal: &Terminal, mode: ReadMode, lines: usize) -> ControlScreen {
        control_screen(
            "pane-test".into(),
            mode,
            lines,
            &terminal.snapshot(),
            Some(terminal),
            || true,
        )
        .unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires Linux Landlock ABI 6 and seccomp"]
    fn sandboxed_local_shell_proves_containment_and_starts_strict() {
        use std::os::unix::fs::DirBuilderExt as _;

        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let root = Fixture(PathBuf::from("/tmp").join(format!(
            "kortty-session-sandbox-{:016x}",
            u64::from_ne_bytes(random)
        )));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root.0)
            .unwrap();
        let profile_root = root.0.join("profile");
        let workspace = root.0.join("workspace");
        std::fs::create_dir(&profile_root).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let protected = profile_root.join("protected");
        std::fs::write(&protected, "synthetic-profile-secret").unwrap();

        let session = Session::start_local(
            GridSize::default(),
            TerminalProfile::Xterm,
            LaunchOptions {
                working_directory: Some(workspace),
                sandbox: Some(kortty_pty::Sandbox { profile_root }),
                ..LaunchOptions::default()
            },
            true,
            None,
        )
        .unwrap();
        let started = wait_for_frame(&session, |frame| frame.status != Status::Starting);
        assert_eq!(started.status, Status::Running);
        assert_eq!(session.sandbox_backend(), Some("landlock"));
        let command = format!(
            "if /bin/cat '{}'; then printf '%s%s\\n' profile- exposed; else printf '%s%s\\n' profile- protected; fi; \
             printf '\\033]52;c;cHJpdmF0ZQ==\\007'; printf '%s%s\\n' strict- startup\r",
            protected.display(),
        );
        session.send(command.into_bytes()).unwrap();
        let frame = wait_for_frame(&session, |frame| visible_text(frame, "strict-startup"));
        assert!(visible_text(&frame, "profile-protected"));
        assert!(!visible_text(&frame, "synthetic-profile-secret"));
        assert!(session.take_clipboard_events().is_empty());
        let _ = session.stop().recv_blocking();
    }

    #[cfg(unix)]
    #[test]
    fn control_read_uses_live_pty_while_scrolled_and_after_exit() {
        struct TemporaryControlProfile(PathBuf);
        impl Drop for TemporaryControlProfile {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        use std::os::unix::fs::DirBuilderExt as _;
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            PathBuf::from("/tmp").join(format!("kortty-ctl-{:016x}", u64::from_ne_bytes(random)));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let profile = TemporaryControlProfile(path);
        let server = kortty_control::Server::start(&profile.0).unwrap();
        let session = Session::start(
            GridSize {
                rows: 6,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert!(session.local_shell_pid().is_some());
        session
            .send(
                concat!(
                    r"printf '%s%s\n\n\n\n\n\n\n\n\n\n' 'control-' 'old'; ",
                    r"printf '\033[8m%s%s\033[28m %s%s\n' 'private-' 'cell' 'control-' 'live'",
                    "\r",
                )
                .as_bytes()
                .to_vec(),
            )
            .unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "control-live"));
        session.scroll(i32::MAX);
        wait_for_frame(&session, |frame| frame.scroll_offset != 0);

        let visible = read_control(&server, &profile.0, &session, ReadMode::Visible);
        assert!(
            visible
                .lines
                .iter()
                .any(|line| line.contains("control-live"))
        );
        assert!(
            !visible
                .lines
                .iter()
                .any(|line| line.contains("control-old"))
        );
        assert!(
            !visible
                .lines
                .iter()
                .any(|line| line.contains("private-cell"))
        );
        let recent = read_control(&server, &profile.0, &session, ReadMode::Recent);
        assert!(recent.lines.iter().any(|line| line == "control-old"));
        assert!(
            recent
                .lines
                .iter()
                .any(|line| line.contains("control-live"))
        );
        assert!(
            !recent
                .lines
                .iter()
                .any(|line| line.contains("private-cell"))
        );
        assert_ne!(session.frame().scroll_offset, 0);

        session.send(b"exit\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert_eq!(session.local_shell_pid(), None);
        let exited = read_control(&server, &profile.0, &session, ReadMode::Recent);
        assert!(exited.lines.iter().any(|line| line == "control-old"));
        let ibm = Session::disconnected_ibm(IbmKind::Tn3270e);
        assert_eq!(ibm.local_shell_pid(), None);
        let screen = read_control(&server, &profile.0, &ibm, ReadMode::Visible);
        assert_eq!((screen.columns, screen.rows), (80, 24));
        assert_eq!(screen.lines, vec![String::new(); 24]);
        server.stop();
    }

    #[cfg(unix)]
    fn read_control(
        server: &kortty_control::Server,
        profile: &std::path::Path,
        session: &Session,
        mode: ReadMode,
    ) -> ControlScreen {
        thread::scope(|scope| {
            let client = scope.spawn(move || {
                let mut client = kortty_control::Client::connect(profile, TIMEOUT).unwrap();
                client.call("pane.read", &serde_json::json!({"pane": "pane-test", "mode": mode.name(), "lines": 200})).unwrap()
            });
            let deadline = Instant::now() + TIMEOUT;
            let requests = server.requests();
            let request = loop {
                match requests.try_recv() {
                    Ok(request) => break request,
                    Err(TryRecvError::Empty) if Instant::now() < deadline => {
                        thread::sleep(FRAME_INTERVAL)
                    }
                    Err(error) => panic!("control request did not arrive: {error}"),
                }
            };
            assert!(request.method == kortty_control::Method::Read);
            session
                .control_read("pane-test".into(), mode, 200, request.reply)
                .unwrap();
            serde_json::from_value(client.join().unwrap()).unwrap()
        })
    }

    #[test]
    fn reveal_centers_offscreen_history_and_preserves_visible_matches() {
        assert_eq!(reveal_offset(100, 10, 0, 15), 90);
        assert_eq!(reveal_offset(100, 10, 90, 15), 90);
        assert_eq!(reveal_offset(100, 10, 90, 105), 0);
        assert_eq!(reveal_offset(100, 10, 0, 0), 100);
        assert_eq!(reveal_offset(100, 10, 90, 110), 90);
    }

    #[test]
    fn search_reveal_checks_the_content_revision() {
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(b"needle\r\nsecond\r\nthird\r\nfourth");
        let mut search = SearchState::default();
        search.find(
            &terminal,
            SearchRequest {
                id: 4,
                query: "needle".into(),
                ignore_case: false,
            },
        );
        let frame = make_frame(&terminal, Status::Running, 0, &search);
        let response = frame.search_result().unwrap();
        assert_eq!(
            (response.request, response.revision),
            (4, frame.search_revision)
        );
        let row = response.results.matches[0].start.row;
        assert_eq!(row, 0);
        let (redraw, _) = bounded(1);
        let shared = Shared {
            view: Mutex::new(ViewRequest {
                grid: GridSize::default(),
                scroll_anchor: None,
                typeahead: TypeAheadRequest::default(),
                prompt_navigation_enabled: true,
                prompt_navigator: PromptNavigator::default(),
                appearance: Appearance::default(),
            }),
            frame: Mutex::new(Arc::new(frame)),
            logfile_warning: crate::terminal_logging::Warning::new(redraw.clone()),
            redraw,
            bell_events: AtomicU64::new(0),
            output_activity: Arc::default(),
            local_shell_pid: AtomicU32::new(0),
            sandbox_backend: Mutex::default(),
            serial: None,
            serial_notice: Mutex::new(None),
            shell_activity: None,
            clipboard: Mutex::default(),
            recording: Arc::default(),
            journal_available: AtomicBool::new(false),
            journal_generation: AtomicU64::new(0),
            journal_secrets: Mutex::new(None),
            scrollback_compression_enabled: AtomicBool::new(true),
            typewriter_speed: AtomicU8::new(0),
        };
        search.reveal(&shared, search.revision, row);
        assert_eq!(requested_offset(&shared, &terminal), 2);
        search.invalidate();
        search.reveal(&shared, search.revision - 1, 3);
        assert_eq!(requested_offset(&shared, &terminal), 2);
        search.reveal(&shared, search.revision, 3);
        assert_eq!(requested_offset(&shared, &terminal), 0);
    }

    #[test]
    fn local_pty_search_survives_exit_and_reveals_history() {
        let grid = GridSize {
            rows: 8,
            ..GridSize::default()
        };
        let session = Session::start(grid, TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);

        #[cfg(unix)]
        let command = concat!(
            r"printf '%s%s\n\n\n\n\n\n\n\n\n\n\n\n%s%s\n' 'kortty_search_' '3149_old' 'kortty_search_' '3149_live'",
            "\r"
        )
        .as_bytes();
        #[cfg(windows)]
        let command = b"[Console]::WriteLine('kortty_search_'+'3149_old'); 1..12 | ForEach-Object {[Console]::WriteLine('')}; [Console]::WriteLine('kortty_search_'+'3149_live')\r";
        session.send(command.to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            visible_text(frame, "kortty_search_3149_live")
        });

        let request = session.search("kortty_search_3149", false).unwrap();
        let running = wait_for_result(&session, request);
        let result = running.search_result().unwrap();
        assert_eq!(result.results.matches.len(), 2);
        assert!(result.results.matches[0].start.row < running.screen.scrollback_lines);

        #[cfg(unix)]
        let exit = b"printf '%s\\n' 'ending-search-test'; exit\r";
        #[cfg(windows)]
        let exit = b"[Console]::WriteLine('ending-search-test'); exit\r";
        session.send(exit.to_vec()).unwrap();
        wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert!(session.supports_search());
        session
            .reveal_search_match(result.revision, result.results.matches[0].start.row)
            .unwrap();
        let request = session.search("KORTTY_SEARCH_3149", true).unwrap();
        let exited = wait_for_result(&session, request);
        let fresh = exited.search_result().unwrap();
        assert_ne!(result.revision, fresh.revision);
        assert_eq!(exited.scroll_offset, 0, "stale reveal moved the viewport");
        assert_eq!(fresh.results.matches.len(), 2);
        session
            .reveal_search_match(fresh.revision, fresh.results.matches[0].start.row)
            .unwrap();
        let revealed = wait_for_frame(&session, |frame| frame.scroll_offset != 0);
        assert_eq!(revealed.search_revision, fresh.revision);
        assert!(visible_text(&revealed, "kortty_search_3149_old"));

        let done = session.stop();
        let deadline = Instant::now() + TIMEOUT;
        while !done.is_closed() {
            assert!(
                Instant::now() < deadline,
                "session worker did not shut down"
            );
            thread::sleep(FRAME_INTERVAL);
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_terminal_commands_use_one_pane_without_sending_host_input() {
        let session = Session::start(
            GridSize {
                rows: 6,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session.send(concat!(
            r#"printf 'old-history\n\n\n\n\n\n\n\n\n\n'; "#,
            r#"printf '\033]133;A\007$ command\n\033]133;C\007%s%s\n%s%s\n\033]133;D;0\007\033]133;A\007%s%s' 'retained-' 'one' 'retained-' 'two' 'WAIT-' 'HERE'; "#,
            r#"IFS= read -r response; printf 'actor-after-%s-%s\n' "${#response}" "$response""#,
            "\r",
        ).as_bytes().to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            frame.has_finished_command && visible_text(frame, "WAIT-HERE")
        });
        let invoke = |command| {
            let reply = session.terminal_command(command).unwrap();
            let deadline = Instant::now() + TIMEOUT;
            loop {
                match reply.try_recv() {
                    Ok(result) => return result,
                    Err(TryRecvError::Empty) if Instant::now() < deadline => {
                        thread::sleep(FRAME_INTERVAL);
                    }
                    Err(error) => panic!("terminal command did not finish: {error}"),
                }
            }
        };

        let TerminalCommandResult::Selected { revision, range } =
            invoke(TerminalCommand::SelectLastOutput)
        else {
            panic!("finished OSC 133 output could not be selected");
        };
        session.scroll(i32::MAX);
        let scrolled = wait_for_frame(&session, |frame| frame.scroll_offset != 0);
        assert_eq!(scrolled.search_revision, revision);
        for command in [
            TerminalCommand::CopyLastOutput,
            TerminalCommand::CopySelection { revision, range },
        ] {
            let TerminalCommandResult::Copied { text, truncated } = invoke(command) else {
                panic!("finished OSC 133 output could not be copied");
            };
            assert_eq!(text, "retained-one\nretained-two");
            assert!(!truncated);
            assert_eq!(session.frame().scroll_offset, scrolled.scroll_offset);
        }
        assert!(matches!(
            invoke(TerminalCommand::ClearBuffer),
            TerminalCommandResult::Cleared
        ));
        let cleared = session.frame();
        assert_eq!(cleared.scroll_offset, 0);
        assert_eq!(cleared.screen.scrollback_lines, 0);
        assert_eq!(cleared.screen.cursor.row, 0);
        assert!(!cleared.has_finished_command);
        assert!(visible_text(&cleared, "WAIT-HERE"));
        assert!(!visible_text(&cleared, "retained-two"));
        assert!(matches!(
            invoke(TerminalCommand::CopySelection { revision, range }),
            TerminalCommandResult::SelectionChanged,
        ));
        session.send(b"confirmed\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            visible_text(frame, "actor-after-9-confirmed")
        });
        session.send(b"exit\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_typeahead_is_opt_in_and_only_changes_the_overlay() {
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert!(!session.typeahead_request().enabled);

        // The child fixture measures two delayed echoes, then withholds the
        // third until another byte arrives. Echo stays off at the PTY layer.
        let command = concat!(
            r#"sh -c 'saved=$(stty -g); trap "stty $saved" EXIT; stty -echo -icanon min 1 time 0; "#,
            r#"printf "%s%s" "kortty_typeahead_" "ready> "; for i in 1 2; do "#,
            r#"byte=$(dd bs=1 count=1 2>/dev/null); sleep .2; printf "%s" "$byte"; done; "#,
            r#"first=$(dd bs=1 count=1 2>/dev/null); last=$(dd bs=1 count=1 2>/dev/null); "#,
            r#"printf "%s%s\n" "$first" "$last"'"#,
            "\r"
        );
        session.send(command.as_bytes().to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            visible_text(frame, "kortty_typeahead_ready> ")
        });
        session.set_typeahead_enabled(true);
        for (byte, expected) in [
            (b'a', "kortty_typeahead_ready> a"),
            (b'b', "kortty_typeahead_ready> ab"),
        ] {
            session.send_keyboard(vec![byte], false).unwrap();
            let echoed = wait_for_frame(&session, |frame| visible_text(frame, expected));
            assert!(echoed.typeahead.is_none());
        }

        session.send_keyboard(vec![b'c'], false).unwrap();
        session.follow_output();
        let frame = wait_for_frame(&session, |frame| frame.typeahead.is_some());
        let overlay = frame.typeahead.as_ref().unwrap();
        let column = usize::from(frame.screen.cursor.column);
        assert_eq!(overlay.row, frame.screen.cursor.row);
        assert_eq!(usize::from(overlay.cursor_column), column + 1);
        assert_eq!(overlay.cells[column].text, "c");
        assert!(
            frame.screen.row(overlay.row).unwrap()[column]
                .text
                .is_empty()
        );

        session.set_typeahead_enabled(false);
        let cleared = wait_for_frame(&session, |frame| frame.typeahead.is_none());
        assert!(!visible_text(&cleared, "kortty_typeahead_ready> abc"));
        session.send_keyboard(vec![b'd'], false).unwrap();
        let echoed = wait_for_frame(&session, |frame| {
            visible_text(frame, "kortty_typeahead_ready> abcd")
        });
        assert!(echoed.typeahead.is_none());

        let done = session.stop();
        let deadline = Instant::now() + TIMEOUT;
        while !done.is_closed() {
            assert!(
                Instant::now() < deadline,
                "typeahead test session did not shut down"
            );
            thread::sleep(FRAME_INTERVAL);
        }
    }

    fn wait_for_result(session: &Session, request: u64) -> Arc<Frame> {
        wait_for_frame(session, |frame| {
            frame
                .search_result()
                .is_some_and(|result| result.request == request)
        })
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_bells_survive_the_latest_frame_until_consumed() {
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session
            .send(b"printf '\\033]0;quiet'; sleep 0.05; printf '\\007\\007\\007'; exit\r".to_vec())
            .unwrap();
        wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert_eq!(session.take_bell_events(), 2);
        assert_eq!(session.take_bell_events(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_kitty_graphics_survive_scrollback_and_respect_strict_mode() {
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session
            .send(
                concat!(
                    r"printf '\033[2J\033[H%s%s\033_Ga=T,i=73,f=24,s=1,v=1,c=8,r=3,q=2,C=1;/wAA\033\\\n' 'kitty-' 'anchor'",
                    "\r",
                )
                .as_bytes()
                .to_vec(),
            )
            .unwrap();
        let placed = wait_for_frame(&session, |frame| !frame.screen.images.is_empty());
        let placement = &placed.screen.images[0].placement;
        assert_eq!(placement.image.id, 73);
        assert_eq!(placement.image.rgba.as_ref(), &[255, 0, 0, 255]);
        assert_eq!(usize::from(placement.column), "kitty-anchor".len());

        session
            .send(
                b"for n in $(seq 1 40); do printf '\\n'; done; printf '%s%s\\n' 'kitty-' 'live'\r"
                    .to_vec(),
            )
            .unwrap();
        let live = wait_for_frame(&session, |frame| visible_text(frame, "kitty-live"));
        assert!(live.screen.images.is_empty());
        let request = session.search("kitty-anchor", false).unwrap();
        let search_frame = wait_for_result(&session, request);
        let search = search_frame.search_result().unwrap();
        let [anchor] = search.results.matches.as_slice() else {
            panic!("expected one Kitty image anchor in scrollback");
        };
        session
            .reveal_search_match(search.revision, anchor.start.row)
            .unwrap();
        let history = wait_for_frame(&session, |frame| frame.scroll_offset > 0);
        assert!(visible_text(&history, "kitty-anchor"));
        assert_eq!(history.screen.images[0].placement.image.id, 73);

        session.follow_output();
        session.set_strict_escapes(true);
        session
            .send(
                concat!(
                    r"printf '\033[3J\033_Ga=T,i=74,f=24,s=1,v=1,q=2,C=1;AP8A\033\\%s%s\n' 'kitty-' 'strict'",
                    "\r",
                )
                .as_bytes()
                .to_vec(),
            )
            .unwrap();
        let strict = wait_for_frame(&session, |frame| visible_text(frame, "kitty-strict"));
        assert!(strict.screen.images.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_receives_native_renderer_colors_with_both_osc_terminators() {
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        let query = concat!(
            "stty -echo -icanon min 1 time 0; ",
            "printf '\\033]10;?\\007\\033]11;?\\033\\\\'; ",
            "got=$(dd bs=1 count=49 2>/dev/null); ",
            "want=$(printf '\\033]10;rgb:ffff/ffff/ffff\\007\\033]11;rgb:1e1e/1e1e/1e1e\\033\\\\'); ",
            "[ \"$got\" = \"$want\" ]; exit\r"
        );
        session.send(query.as_bytes().to_vec()).unwrap();
        let frame = wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert_eq!(frame.status, Status::Exited(Some(0)));
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_reports_themed_colors_palette_and_live_cursor_defaults() {
        use crate::terminal_theme::{Colors, CursorStyle, Rgb, Scheme};
        use kortty_terminal::CursorShape;

        let scheme = Scheme {
            foreground: Rgb::from_hex(0xD8DEE9),
            background: Rgb::from_hex(0x2E3440),
            cursor_style: CursorStyle::SteadyBar,
            ..Scheme::default()
        };
        let session = Session::start_local(
            GridSize::default(),
            TerminalProfile::Xterm,
            LaunchOptions::default(),
            true,
            Some(scheme),
        )
        .unwrap();
        let initial = wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert_eq!(initial.screen.cursor_shape, CursorShape::Bar);
        assert!(!initial.screen.cursor_blinking);

        let mut colors = Colors {
            cursor_blink: Some(false),
            sgr_enabled: false,
            ..Colors::default()
        };
        colors.ansi[1] = Some(Rgb::from_hex(0x123456));
        session.set_terminal_appearance(colors.appearance(Some(Scheme {
            foreground: Rgb::from_hex(0x24292F),
            background: Rgb::from_hex(0xFFFFFF),
            cursor: Rgb::from_hex(0x765432),
            cursor_style: CursorStyle::BlinkUnderline,
            ..scheme
        })));
        let updated = wait_for_frame(&session, |frame| {
            frame.screen.cursor_shape == CursorShape::Underline
        });
        assert!(!updated.screen.cursor_blinking);
        let query = concat!(
            "stty -echo -icanon min 1 time 0; ",
            "printf '\\033]10;?\\007\\033]11;?\\033\\\\\\033]12;?\\007\\033]4;1;?\\033\\\\'; ",
            "want=$(printf '\\033]10;rgb:2424/2929/2f2f\\007\\033]11;rgb:ffff/ffff/ffff\\033\\\\\\033]12;rgb:7676/5454/3232\\007\\033]4;1;rgb:1212/3434/5656\\033\\\\'); ",
            "got=$(dd bs=1 count=\"${#want}\" 2>/dev/null); ",
            "[ \"$got\" = \"$want\" ]; exit\r"
        );
        session.send(query.as_bytes().to_vec()).unwrap();
        let frame = wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert_eq!(frame.status, Status::Exited(Some(0)));
    }

    #[cfg(unix)]
    #[test]
    fn hp2392a_block_form_reaches_the_pty_only_on_enter() {
        let session = Session::start(GridSize::default(), TerminalProfile::Hp2392).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        // The real shell puts its PTY in no-echo mode, paints a form, and
        // reports the one line received from the terminal's Enter key.
        let host = concat!(
            "stty -echo; printf '\\033h\\033&k1B\\033&s1HHP:\\033[    \\033] OK\\033W'; ",
            "IFS= read -r value; printf '\\033&k0B\\033X\\r\\nHP-HOST:%s\\r\\n' \"$value\"; exit",
            "\r"
        );
        session.send(host.as_bytes().to_vec()).unwrap();
        wait_for_frame(&session, |frame| {
            frame.screen.cursor.column == 3 && visible_text(frame, "HP:     OK")
        });
        session.send_keyboard(b"AB".to_vec(), false).unwrap();
        session.send_paste(b"\r".to_vec()).unwrap();
        session.send_shifted_hp_enter(b"\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "HP:AB   OK"));
        assert_eq!(session.frame().status, Status::Running);
        session.send_keyboard(b"\x1b[Z".to_vec(), false).unwrap();
        session.send_keyboard(b"\r".to_vec(), true).unwrap();
        let completed = wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert!(visible_text(&completed, "HP-HOST:AB  "));
        assert!(!visible_text(&completed, "HP-HOST:ABAB"));
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_recording_saves_styled_output_on_terminal_shutdown() {
        use kortty_recording::Replay;
        use kortty_terminal::Color;

        struct TemporaryRecording(PathBuf);
        impl Drop for TemporaryRecording {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let mut suffix = [0; 8];
        getrandom::fill(&mut suffix).unwrap();
        let file = TemporaryRecording(std::env::temp_dir().join(format!(
            "kortty-pty-capture-{:016x}.korttyrec",
            u64::from_ne_bytes(suffix)
        )));
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session
            .start_recording(
                file.0.clone(),
                CaptureOptions {
                    include_style: true,
                    idle_after: None,
                },
            )
            .unwrap();
        wait_for_frame(&session, |_| {
            matches!(
                session.recording_status(),
                RecordingStatus::Capturing { .. }
            )
        });
        session
            .send(b"printf '\\033[31m%s%s\\033[0m\\n' 'capture-' 'marker'\n".to_vec())
            .unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "capture-marker"));

        let done = session.stop();
        let deadline = Instant::now() + TIMEOUT;
        while !done.is_closed() {
            assert!(
                Instant::now() < deadline,
                "recording session did not finish"
            );
            thread::sleep(FRAME_INTERVAL);
        }
        assert_eq!(
            session.recording_status(),
            RecordingStatus::Saved(file.0.clone())
        );
        let mut replay = Replay::open(&file.0).unwrap();
        assert!(replay.is_complete());
        assert!(replay.frame_count() >= 2);
        let mut found = false;
        for index in 0..replay.frame_count() {
            let frame = replay.frame(index).unwrap().unwrap();
            found |= frame
                .screen
                .cells
                .windows("capture-marker".len())
                .any(|cells| {
                    cells
                        .iter()
                        .all(|cell| cell.style.foreground == Color::Indexed(1))
                        && cells
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            == "capture-marker"
                });
        }
        assert!(found, "no recorded frame contains the colored PTY output");
    }

    fn wait_for_frame(session: &Session, check: impl Fn(&Frame) -> bool) -> Arc<Frame> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let frame = session.frame();
            if check(&frame) {
                return frame;
            }
            assert!(Instant::now() < deadline, "timed out in {:?}", frame.status);
            thread::sleep(FRAME_INTERVAL);
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_timestamps_follow_physical_enter_and_quiet_output() {
        let session = Session::start(GridSize::default(), TerminalProfile::Xterm).unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session
            .send(b"printf '%s%s\\n' 'timestamp-' 'raw'\r".to_vec())
            .unwrap();
        let raw = wait_for_frame(&session, |frame| visible_text(frame, "timestamp-raw"));
        assert!(raw.command_timestamps.is_empty());

        session
            .send_paste(b"printf '%s%s\\n' 'timestamp-' 'keyboard'".to_vec())
            .unwrap();
        session.send_physical_enter(b"\r".to_vec(), false).unwrap();
        let marked = wait_for_frame(&session, |frame| {
            frame.command_timestamps.len() == 2 && visible_text(frame, "timestamp-keyboard")
        });
        let [start, end] = marked.command_timestamps.as_slice() else {
            panic!("expected one command start and one quiet completion");
        };
        assert!(start.row < end.row);
        assert_eq!(end.previous, Some(start.time));
        assert_eq!(start.status, None);
        assert_eq!(end.status, None);
        let _ = session.stop().recv_blocking();
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_manual_journal_seeds_then_captures_only_output_and_flushes_tail() {
        use crate::session_journal::{
            Capture, CaptureKind, Metadata, RecordKind, Status as JournalStatus, capture_page,
        };
        use std::os::unix::fs::DirBuilderExt as _;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-pty-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let scratch = Scratch(root);
        let session = Session::start(
            GridSize {
                rows: 6,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert!(session.journal_available());
        session.send(concat!(
            r"stty -echo; printf '%s%s\n\n\n\n\n\n\n\n\n' 'journal-' 'seeded'; ",
            r"printf '%s%s\n' 'seed-' 'ready'; IFS= read -r ignored; ",
            r"printf '\033[?1049h%s%s\033[?1049l\033[8m%s%s\033[28m' 'alt-' 'only' 'hidden-' 'only'; ",
            r"printf '%s%s\n%s%s' 'journal-' 'live' 'journal-' 'tail'; IFS= read -r ignored",
            "\r"
        ).as_bytes().to_vec()).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "seed-ready"));
        let capture = Capture::start(
            scratch.0.clone(),
            Metadata {
                seeded: true,
                ..Metadata::default()
            },
        )
        .unwrap();
        let (attachment, attached) = session
            .attach_journal(capture.sink(), Default::default(), true)
            .unwrap();
        attached.recv_blocking().unwrap().unwrap();
        let mut note_policy = crate::terminal_logging::Policy::default();
        let rules = r#"
[session-journal]
[[session-journal.replace]]
pattern = 'note-secret'
replacement = '***'
"#
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
        note_policy.read_rule(rules.as_table()).unwrap();
        assert!(!session.journal_screenshot_allowed(&note_policy, &session.frame()));
        let note = session
            .redact_journal_note("manual note-secret", &note_policy)
            .unwrap();
        capture
            .sink()
            .try_write(RecordKind::Note, note, false)
            .unwrap();
        session
            .send_keyboard(b"raw-keyboard-sentinel\r".to_vec(), true)
            .unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "journal-tail"));
        block_on(attachment.detach());
        let _ = capture.finish().recv_blocking();
        let _ = session.stop().recv_blocking();
        assert_eq!(capture.status(), JournalStatus::Complete);

        let page = capture_page(capture.directory(), None, None, None).unwrap();
        let journal = crate::session_journal::read(capture.directory()).unwrap();
        assert_eq!(journal.timeline[0].text, "manual ***");
        assert!(
            page.records
                .iter()
                .any(|record| record.text == "manual ***")
        );
        let seed = page
            .records
            .iter()
            .position(|record| {
                record.kind == CaptureKind::Seed && record.text.contains("journal-seeded")
            })
            .unwrap();
        let live = page
            .records
            .iter()
            .position(|record| {
                record.kind == CaptureKind::Output && record.text.contains("journal-live")
            })
            .unwrap();
        let tail = page
            .records
            .iter()
            .position(|record| {
                record.kind == CaptureKind::Output
                    && record.partial
                    && record.text == "journal-tail"
            })
            .unwrap();
        assert!(seed < live && live < tail);
        assert!(page.records.iter().all(|record| {
            record.kind != CaptureKind::Input
                && ![
                    "raw-keyboard-sentinel",
                    "alt-only",
                    "hidden-only",
                    "note-secret",
                ]
                .iter()
                .any(|text| record.text.contains(text))
        }));
    }

    fn visible_text(frame: &Frame, expected: &str) -> bool {
        (0..frame.screen.rows).any(|row| {
            frame.screen.row(row).is_some_and(|cells| {
                cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains(expected)
            })
        })
    }
}
