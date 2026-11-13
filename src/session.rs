use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
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
use kortty_terminal::ScrollbackConfig;
use kortty_terminal::{
    Cell, CommandTimestamp, Hp2392Input, LastOutput, LastOutputRange, MAX_CLIPBOARD_TEXT_BYTES,
    MAX_SEARCH_QUERY_BYTES, PromptDirection, PromptLayout, PromptNavigator, ScreenSnapshot,
    SearchResults, Terminal, TerminalProfile, TextEncoding, TypeAhead,
};

use crate::terminal_effects::Speed;
use crate::terminal_theme::Appearance;

mod activity;
pub(crate) mod agent_takeover;
pub(crate) mod ai_code_guard;
mod clipboard;
mod ibm;
pub(crate) mod inline_history;
mod journal;
mod paste;
pub(crate) use journal::journal_output_observer;
mod recording;
mod serial;
mod shell_activity;
mod typewriter;
#[cfg(windows)]
mod windows_file;
pub(crate) use activity::OutputSample;
pub(crate) use ai_code_guard::{
    CodingAgentDetails, CodingAgentInput, CodingAgentKey, CodingAgentState, CodingAgentStatus,
};
pub use ibm::{IbmAid, IbmEdit, IbmInput, IbmKind};
pub(crate) use recording::TabRecording;
pub use recording::{RecordingCommandError, RecordingStatus};
pub use serial::{
    SerialConnection, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
    SerialStopBits, available_serial_ports,
};
pub(crate) use shell_activity::ShellActivity;
pub(crate) use shell_activity::{CodePrompt, PromptState, ShortcutPrompt};
#[cfg(test)]
pub(crate) use shell_activity::{ReportedDirectory, TypedCd};
mod mcp;
pub(crate) use mcp::{McpGuard, McpIdentity, McpInput};

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

#[derive(Clone)]
pub struct Frame {
    pub screen: ScreenSnapshot,
    pub status: Status,
    pub scroll_offset: usize,
    pub(crate) inline: Option<Arc<inline_history::Frame>>,
    pub(crate) scrollback: Option<kortty_terminal::ScrollbackSnapshot>,
    pub(crate) output_snapshot: Option<Arc<kortty_terminal::OutputSnapshot>>,
    pub(crate) recovery_output: Option<Arc<kortty_terminal::OutputSnapshot>>,
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
    coding_agent: Option<CodingAgentStatus>,
    coding_agent_details: Option<Arc<CodingAgentDetails>>,
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
    pub(crate) fn at_live_viewport(&self) -> bool {
        self.viewport_scroll_offset() == 0
    }

    pub(crate) fn viewport_scroll_offset(&self) -> usize {
        self.inline
            .as_ref()
            .map_or(self.scroll_offset, |view| view.offset())
    }

    pub(crate) fn display_raw_row(&self, row: u16) -> Option<u16> {
        self.inline.as_ref().map_or_else(
            || (row < self.screen.rows).then_some(row),
            |view| view.display_raw_row(row),
        )
    }

    pub(crate) fn raw_display_row(&self, row: u16) -> Option<u16> {
        self.inline.as_ref().map_or_else(
            || (row < self.screen.rows).then_some(row),
            |view| view.raw_display_row(row),
        )
    }

    pub fn search_result(&self) -> Option<&Arc<SearchResponse>> {
        self.search_result.as_ref()
    }

    /// Passive SSH evidence published with the corresponding screen.
    pub(crate) fn shell_activity(&self) -> &ShellActivity {
        &self.shell_activity
    }

    pub(crate) fn coding_agent_visible(&self) -> bool {
        self.at_live_viewport() && ai_code_guard::coding_agent_visible(&self.screen)
    }

    pub(crate) fn approval_prompt_visible(&self) -> bool {
        self.at_live_viewport() && ai_code_guard::approval_prompt_visible(&self.screen)
    }

    pub(crate) fn coding_agent(&self) -> Option<CodingAgentStatus> {
        self.coding_agent
    }

    pub(crate) fn coding_agent_waiting_for_decision(&self) -> bool {
        self.coding_agent()
            .is_some_and(|agent| agent.state == CodingAgentState::Blocked)
    }

    pub(crate) fn coding_agent_details(&self) -> Option<&Arc<CodingAgentDetails>> {
        self.coding_agent_details.as_ref()
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
    inline_anchor: Option<inline_history::Anchor>,
    typeahead: TypeAheadRequest,
    prompt_navigation_enabled: bool,
    prompt_navigator: PromptNavigator,
    appearance: Appearance,
    scrollback: ScrollbackConfig,
    recovery_output_rows: u16,
}

impl ViewRequest {
    fn set_scroll_anchor(&mut self, anchor: Option<kortty_terminal::ScrollbackAnchor>) {
        if self.scroll_anchor != anchor || self.inline_anchor.is_some() {
            self.scroll_anchor = anchor;
            self.inline_anchor = None;
            self.prompt_navigator = PromptNavigator::default();
        }
    }

    fn set_inline_anchor(&mut self, anchor: Option<inline_history::Anchor>) {
        if self.inline_anchor != anchor || self.scroll_anchor.is_some() {
            self.inline_anchor = anchor;
            self.scroll_anchor = None;
            self.prompt_navigator = PromptNavigator::default();
        }
    }

    fn raw_anchor(&self) -> Option<&kortty_terminal::ScrollbackAnchor> {
        self.inline_anchor
            .as_ref()
            .map(|anchor| &anchor.raw)
            .or(self.scroll_anchor.as_ref())
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TypeAheadRequest {
    enabled: bool,
    generation: u64,
}

struct Shared {
    view: Mutex<ViewRequest>,
    inline_updates: Mutex<std::collections::VecDeque<inline_history::Update>>,
    frame: Mutex<Arc<Frame>>,
    redraw: Sender<()>,
    bell_events: AtomicU64,
    program_notifications: Mutex<std::collections::VecDeque<kortty_terminal::ProgramNotification>>,
    output_activity: Arc<activity::Clock>,
    local_shell_pid: AtomicU32,
    sandbox_backend: Mutex<Option<&'static str>>,
    serial: Option<Arc<SerialConnection>>,
    serial_notice: Mutex<Option<Arc<str>>>,
    shell_activity: Option<Mutex<shell_activity::State>>,
    // The low bit is the enabled flag. Each change advances the revision.
    coding_agent_detection: AtomicU64,
    clipboard: Mutex<clipboard::ClipboardBridge>,
    recording: Arc<recording::Control>,
    logfile_warning: crate::terminal_logging::Warning,
    journal_available: AtomicBool,
    journal_input: Arc<AtomicBool>,
    journal_generation: AtomicU64,
    journal_secrets: Mutex<Option<crate::terminal_logging::RedactionSecrets>>,
    // Zero bits mean disabled. Other values are bounded Speed multipliers.
    typewriter_speed: AtomicU64,
    paste_cancellation: Mutex<Weak<paste::Cancellation>>,
}

impl Shared {
    fn coding_agent_waiting_for_decision(&self) -> bool {
        self.frame
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .coding_agent_waiting_for_decision()
    }

    fn typewriter_speed(&self) -> Option<Speed> {
        let speed = self.typewriter_speed.load(Ordering::Relaxed);
        Speed::try_from(f64::from_bits(speed)).ok()
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

pub(crate) struct StreamLiveness(Weak<Shared>);

impl StreamLiveness {
    pub(crate) fn running(&self) -> bool {
        self.0.upgrade().is_some_and(|shared| {
            let frame = shared
                .frame
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            frame.status == Status::Running && !frame.password_prompt
        })
    }

    pub(crate) fn shell_activity(&self) -> Option<ShellActivity> {
        let shared = self.0.upgrade()?;
        let activity = shared
            .shell_activity
            .as_ref()?
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot();
        Some(activity)
    }
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
        exec: Option<crate::ssh_connection::ExecOpener>,
        ssh_connected_at: Option<(Instant, SystemTime)>,
    },
    Ibm(Sender<IbmInput>),
    Offline,
}

enum StreamSource {
    Pty {
        options: LaunchOptions,
        initial_grid: GridSize,
    },
    ClosedLocal(String),
    Ssh(
        Option<crate::ssh_connection::Connected>,
        Option<Box<crate::terminal_logging::Prepared>>,
    ),
    Serial {
        connection: Arc<SerialConnection>,
        connect: bool,
        encoding: TextEncoding,
    },
}

#[derive(Clone, Copy, Default)]
pub(crate) struct StreamOptions {
    pub(crate) strict_escapes: Option<bool>,
    pub(crate) encoding: Option<TextEncoding>,
    pub(crate) scrollback: ScrollbackConfig,
}

enum StreamInput {
    PacedPaste {
        bytes: Vec<u8>,
        clear_typeahead: bool,
        pacing: paste::Pacing,
    },
    Raw {
        bytes: Vec<u8>,
        clear_typeahead: bool,
        mirrored: bool,
    },
    Guarded(GuardedInput, WriteReply),
    Protocol(Vec<u8>, bool),
    Keyboard {
        bytes: Vec<u8>,
        enter: bool,
        generation: Option<u64>,
        physical: bool,
    },
    User {
        bytes: Vec<u8>,
        enter: bool,
        record_enter: bool,
        mirrored: bool,
    },
    Break,
}

enum GuardedInput {
    AiCode {
        code: zeroize::Zeroizing<String>,
        run_at: Option<CodePrompt>,
    },
    CodingAgent(ai_code_guard::InputRequest),
    Mcp(McpInput),
    AgentTakeover(agent_takeover::Request),
}

enum WriteReply {
    AiCode(Sender<TerminalCommandResult>),
    CodingAgent(Sender<Result<usize, &'static str>>),
    Mcp(Sender<Result<usize, &'static str>>, McpGuard),
    AgentTakeover(
        Sender<Result<usize, &'static str>>,
        crate::terminal_agent::Cancel,
    ),
}

struct ApprovedInput {
    bytes: zeroize::Zeroizing<Vec<u8>>,
    output: Option<Instant>,
}

impl WriteReply {
    fn finish(self, result: Result<usize, &'static str>) {
        match self {
            Self::AiCode(reply) => {
                let _ = reply.try_send(TerminalCommandResult::AiCodeWritten(result.map(|_| ())));
            }
            Self::CodingAgent(reply) | Self::Mcp(reply, _) | Self::AgentTakeover(reply, _) => {
                let _ = reply.try_send(result);
            }
        }
    }
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
    ScrollbackChanged,
    CodingAgentPoll,
    Clipboard,
    Output(Vec<u8>, Instant),
    KeyboardInput {
        bytes: Vec<u8>,
        enter: bool,
        sent_at: Instant,
        wall_time: Option<SystemTime>,
        generation: Option<u64>,
        journal: Option<Sender<Option<journal::Permit>>>,
    },
    JournalInput(journal::Permit, zeroize::Zeroizing<Vec<u8>>),
    JournalInputLost(journal::Permit),
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
        paced_reply: Option<Sender<Option<Vec<u8>>>>,
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
    McpPreflight(McpGuard, mcp::Check),
    TerminalCommand(TerminalCommand, Sender<TerminalCommandResult>),
    AuthorizeInput {
        input: GuardedInput,
        reply: Sender<Result<ApprovedInput, &'static str>>,
    },
    LocalShellDirectory(Sender<io::Result<PathBuf>>),
    #[cfg(windows)]
    WindowsTerminalFile(windows_file::Request, Sender<io::Result<PathBuf>>),
    CaptureReconnectHistory(Sender<inline_history::Reconnect>),
    PrependReconnectHistory(Box<inline_history::Reconnect>, Sender<()>),
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
    AiRecentOutput,
    WriteAiCode {
        code: zeroize::Zeroizing<String>,
        run_at: Option<CodePrompt>,
    },
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
    AiCodeWritten(Result<(), &'static str>),
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
            Self::AiCodeWritten(result) => formatter
                .debug_tuple("AiCodeWritten")
                .field(result)
                .finish(),
        }
    }
}

struct ControlRead {
    pane_id: String,
    mode: ReadMode,
    lines: usize,
    reply: Reply,
    filter: Option<mcp::ReadFilter>,
}

impl ControlRead {
    fn answer(self, live: &ScreenSnapshot, terminal: Option<&Terminal>) {
        if !self.reply.active() {
            return;
        }
        if self.filter.is_some() && terminal.is_some_and(Terminal::cursor_line_is_password_prompt) {
            self.reply.send(Err(ControlFault::new(
                ControlError::Busy,
                "The pane is waiting for a password",
            )));
            return;
        }
        let Some((screen, wraps, starts_with_continuation)) = control_screen_rows(
            self.pane_id,
            self.mode,
            self.lines,
            live,
            terminal,
            self.filter.is_some(),
            || self.reply.active(),
        ) else {
            return;
        };
        let result = if let Some(filter) = self.filter {
            filter(screen, wraps, starts_with_continuation)
        } else {
            serde_json::to_value(screen).map_err(|_| {
                ControlFault::new(ControlError::Internal, "Could not encode terminal screen")
            })
        };
        self.reply.send(result);
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
    journal_input: Arc<AtomicBool>,
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

    fn authorize_input(&self, input: GuardedInput) -> Result<ApprovedInput, &'static str> {
        let (reply, receive) = bounded(1);
        if !self.send(Event::AuthorizeInput { input, reply }) {
            return Err("The terminal pane disconnected.");
        }
        // The terminal actor closes input before joining the writer.
        match block_on(select(pin!(self.input.closed()), pin!(receive.recv()))) {
            Either::Right((Ok(result), _)) => result,
            _ => Err("The terminal pane disconnected."),
        }
    }

    fn paced_hp_input(&self, line: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let (reply, receive) = bounded(1);
        // Decide on the actor where the current HP block-mode state is known.
        // The writer is busy with this paste, so send the result back directly.
        if !self.send(Event::HpUserInput {
            bytes: line.to_vec(),
            kind: Hp2392Input::Text,
            record_enter: false,
            mirrored: false,
            paced_reply: Some(reply),
        }) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        match block_on(select(pin!(self.input.closed()), pin!(receive.recv()))) {
            Either::Right((Ok(result), _)) => Ok(result),
            _ => Err(io::ErrorKind::BrokenPipe.into()),
        }
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
    inline: inline_history::History,
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
            StreamOptions::default(),
        )
    }

    pub(crate) fn start_local_with_strict(
        grid: GridSize,
        profile: TerminalProfile,
        options: LaunchOptions,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
        stream: StreamOptions,
    ) -> io::Result<Self> {
        Self::start_stream(
            StreamSource::Pty {
                options,
                initial_grid: grid,
            },
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            stream,
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
            StreamOptions::default(),
        )
    }

    pub(crate) fn ssh_with_logging(
        connected: Option<crate::ssh_connection::Connected>,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
        logging: Option<Box<crate::terminal_logging::Prepared>>,
        stream: StreamOptions,
    ) -> io::Result<Self> {
        Self::start_stream(
            StreamSource::Ssh(connected, logging),
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            stream,
        )
    }

    pub(crate) fn closed_local(
        grid: GridSize,
        profile: TerminalProfile,
        appearance: Appearance,
        error: String,
    ) -> io::Result<Self> {
        Self::start_stream(
            StreamSource::ClosedLocal(error),
            grid,
            profile,
            false,
            appearance,
            StreamOptions::default(),
        )
    }

    fn start_stream(
        source: StreamSource,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: Appearance,
        stream: StreamOptions,
    ) -> io::Result<Self> {
        let (event_send, event_receive) = bounded(32);
        let (input_send, input_receive) = bounded(WRITE_QUEUE_SIZE);
        let (redraw_send, redraw_receive) = bounded(1);
        let (done_send, done_receive) = bounded(1);
        let (sftp, exec, ssh_connected_at) = match &source {
            StreamSource::Ssh(Some(ssh), _) => (
                ssh.sftp_opener(),
                ssh.exec_opener(),
                Some((Instant::now(), SystemTime::now())),
            ),
            _ => (None, None, None),
        };
        let observe_shell = matches!(
            source,
            StreamSource::Pty { .. } | StreamSource::Ssh(Some(_), _)
        );
        let encoding = match &source {
            StreamSource::Serial { encoding, .. } => Some(*encoding),
            _ => stream.encoding,
        };
        let mut terminal = match encoding {
            Some(encoding) => {
                Terminal::with_profile_and_encoding(grid.columns, grid.rows, profile, encoding)
            }
            None => Terminal::with_profile(grid.columns, grid.rows, profile),
        };
        terminal.set_prompt_navigation_enabled(prompt_navigation_enabled);
        terminal.set_scrollback_config(stream.scrollback);
        set_terminal_appearance(&mut terminal, appearance);
        terminal.set_clipboard_protocol_enabled(true);
        terminal.set_clipboard_write_limit(MAX_CLIPBOARD_TEXT_BYTES);
        let strict = stream.strict_escapes.unwrap_or_else(|| match &source {
            StreamSource::Pty { options, .. } => options.sandbox.is_some(),
            StreamSource::Ssh(Some(ssh), _) => ssh.sandbox_backend().is_some(),
            _ => false,
        });
        terminal.set_strict_escape_filter(strict);
        let (serial, status) = match &source {
            StreamSource::Pty { .. } => (None, Status::Starting),
            StreamSource::ClosedLocal(error) => (None, Status::Error(error.clone())),
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
                ..
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
                inline_anchor: None,
                typeahead: TypeAheadRequest::default(),
                prompt_navigation_enabled,
                prompt_navigator: PromptNavigator::default(),
                appearance,
                scrollback: stream.scrollback,
                recovery_output_rows: 0,
            }),
            inline_updates: Mutex::default(),
            frame: Mutex::new(Arc::new(frame)),
            logfile_warning: crate::terminal_logging::Warning::new(redraw_send.clone()),
            redraw: redraw_send,
            bell_events: AtomicU64::new(0),
            program_notifications: Mutex::default(),
            output_activity: Arc::default(),
            local_shell_pid: AtomicU32::new(0),
            sandbox_backend: Mutex::default(),
            serial,
            serial_notice: Mutex::new(None),
            shell_activity: observe_shell.then(Mutex::default),
            coding_agent_detection: AtomicU64::new(0),
            clipboard: Mutex::new(clipboard::ClipboardBridge::with_strict(strict)),
            recording: Arc::default(),
            journal_available: AtomicBool::new(false),
            journal_input: Arc::new(AtomicBool::new(false)),
            journal_generation: AtomicU64::new(0),
            journal_secrets: Mutex::new(None),
            typewriter_speed: AtomicU64::new(0),
            paste_cancellation: Mutex::default(),
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
                        inline: last.inline.clone(),
                        scrollback: last.scrollback.clone(),
                        output_snapshot: last.output_snapshot.clone(),
                        recovery_output: last.recovery_output.clone(),
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
                        coding_agent: None,
                        coding_agent_details: None,
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
                exec,
                ssh_connected_at,
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

    #[cfg(test)]
    pub(crate) fn set_mirror_test_frame(&self, state: Option<CodingAgentState>) {
        let mut frame = self.shared.frame.lock().unwrap();
        let frame = Arc::make_mut(&mut frame);
        frame.status = Status::Running;
        frame.ibm = None;
        frame.coding_agent = state.map(|state| CodingAgentStatus {
            id: "gemini-cli",
            name: "Gemini CLI",
            state,
            since: Instant::now(),
            instance: 1,
        });
    }

    pub(crate) fn stream_liveness(&self) -> Option<StreamLiveness> {
        matches!(self.backend, Backend::Stream { .. })
            .then(|| StreamLiveness(Arc::downgrade(&self.shared)))
    }

    pub(crate) fn agent_shortcut_prompt(&self, frame: &Frame) -> Option<ShortcutPrompt> {
        if frame.status != Status::Running || frame.screen.alternate_screen {
            return None;
        }
        self.shared
            .shell_activity
            .as_ref()?
            .lock()
            .ok()?
            .agent_shortcut_prompt()
            .filter(|prompt| prompt.0 == frame.shell_activity.prompt)
    }

    pub(crate) fn replay_agent_shortcut(
        &self,
        prompt: ShortcutPrompt,
        bytes: Vec<u8>,
    ) -> Result<(), InputError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(InputError::TooLarge);
        }
        let Backend::Stream { input, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        let request = self.typeahead_request();
        let mut activity = self
            .shared
            .shell_activity
            .as_ref()
            .ok_or(InputError::Closed)?
            .lock()
            .map_err(|_| InputError::Closed)?;
        if activity.agent_shortcut_prompt() != Some(prompt) {
            return Err(InputError::Closed);
        }
        // The actor cannot replace the prompt between validation and enqueue.
        send_input(
            input,
            StreamInput::Keyboard {
                bytes,
                enter: false,
                generation: request.enabled.then_some(request.generation),
                physical: true,
            },
        )?;
        activity.note_code_input();
        Ok(())
    }

    pub(crate) fn local_shell_pid(&self) -> Option<u32> {
        let Backend::Stream { events, .. } = &self.backend else {
            return None;
        };
        let pid = self.shared.local_shell_pid.load(Ordering::Acquire);
        (pid != 0 && !events.is_closed() && self.frame().status == Status::Running).then_some(pid)
    }

    pub(crate) fn set_coding_agent_detection(&self, enabled: bool) {
        if self
            .shared
            .coding_agent_detection
            .try_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
                ((revision & 1 != 0) != enabled).then_some(revision.wrapping_add(1))
            })
            .is_ok()
            && let Backend::Stream { actor, .. } = &self.backend
        {
            actor.unpark();
        }
    }

    #[cfg(test)]
    pub(crate) fn coding_agent_detection_requested(&self) -> bool {
        self.shared.coding_agent_detection.load(Ordering::Acquire) & 1 != 0
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
        self.control_read_with(pane_id, mode, lines, reply, None)
    }

    pub(crate) fn control_read_masked(
        &self,
        pane_id: String,
        mode: ReadMode,
        lines: usize,
        reply: Reply,
        filter: impl FnOnce(ControlScreen, Vec<bool>, bool) -> Result<serde_json::Value, ControlFault>
        + Send
        + 'static,
    ) -> Result<(), ControlFault> {
        self.control_read_with(pane_id, mode, lines, reply, Some(Box::new(filter)))
    }

    fn control_read_with(
        &self,
        pane_id: String,
        mode: ReadMode,
        lines: usize,
        reply: Reply,
        filter: Option<mcp::ReadFilter>,
    ) -> Result<(), ControlFault> {
        if !reply.active() {
            return Ok(());
        }
        let request = ControlRead {
            pane_id,
            mode,
            lines,
            reply,
            filter,
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

    pub(crate) fn exec_opener(&self) -> Option<crate::ssh_connection::ExecOpener> {
        let Backend::Stream { exec, events, .. } = &self.backend else {
            return None;
        };
        exec.as_ref()
            .filter(|opener| !events.is_closed() && !opener.is_closed())
            .cloned()
    }

    pub(crate) fn ssh_connected_at(&self) -> Option<(Instant, SystemTime)> {
        let Backend::Stream {
            exec: Some(exec),
            events,
            ssh_connected_at,
            ..
        } = &self.backend
        else {
            return None;
        };
        if events.is_closed() || exec.is_closed() {
            return None;
        }
        *ssh_connected_at
    }

    pub fn take_bell_events(&self) -> u64 {
        self.shared.bell_events.swap(0, Ordering::Relaxed)
    }

    pub(crate) fn last_mirrored_input(&self) -> Option<Instant> {
        self.shared.output_activity.last_mirrored_input()
    }

    pub(crate) fn take_program_notifications(&self) -> Vec<kortty_terminal::ProgramNotification> {
        self.shared
            .program_notifications
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect()
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

    pub(crate) fn can_record_in_tab(&self) -> bool {
        matches!(self.backend, Backend::Stream { .. } | Backend::Ibm(_))
            && self.frame().status == Status::Running
    }

    pub(crate) fn attach_tab_recording(&self, id: SessionId, recording: &TabRecording) {
        let pane = recording.pane(id);
        if let Backend::Stream { events, actor, .. } = &self.backend {
            self.shared.recording.set_tab(Some(pane));
            let _ = events.try_send(Event::ViewChanged);
            actor.unpark();
        } else if matches!(self.backend, Backend::Ibm(_)) {
            let frame = self
                .shared
                .frame
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.shared.recording.set_tab(Some(pane.clone()));
            if frame.status == Status::Running {
                let view = requested_view(&self.shared);
                pane.frame(frame.screen.clone(), view.grid, view.appearance);
            }
        }
    }

    pub(crate) fn detach_tab_recording(&self, recording: &TabRecording) {
        if self
            .shared
            .recording
            .tab_pane()
            .is_some_and(|pane| pane.belongs_to(recording))
        {
            self.shared.recording.set_tab(None);
        }
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

    pub(crate) fn journal_note_authority(&self) -> Option<impl Fn() -> bool + use<>> {
        let secrets = self.journal_redaction_secrets()?;
        let shared = Arc::downgrade(&self.shared);
        Some(move || {
            shared.upgrade().is_some_and(|shared| {
                shared.journal_available.load(Ordering::Acquire)
                    && shared.journal_secrets.lock().is_ok_and(|current| {
                        current
                            .as_ref()
                            .is_some_and(|current| secrets.matches(current))
                    })
            })
        })
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
            && frame.at_live_viewport()
            && !frame.screen.alternate_screen
    }

    pub(crate) fn attach_journal_configured(
        &self,
        sink: crate::session_journal::Sink,
        policy: crate::terminal_logging::Policy,
        seed: bool,
        capture_input: bool,
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
                capture_input,
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

    pub(crate) fn send_coding_agent_input(
        &self,
        expected: CodingAgentStatus,
        expected_revision: u64,
        input: CodingAgentInput,
    ) -> Result<Receiver<Result<usize, &'static str>>, InputError> {
        let Backend::Stream { input: queue, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        if input.exceeds_limit() {
            return Err(InputError::TooLarge);
        }
        let (reply, receive) = bounded(1);
        send_input(
            queue,
            StreamInput::Guarded(
                GuardedInput::CodingAgent(ai_code_guard::InputRequest {
                    expected,
                    expected_revision,
                    input,
                }),
                WriteReply::CodingAgent(reply),
            ),
        )?;
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
        self.send_keyboard_from(bytes, enter, true)
    }

    pub(crate) fn journal_input_enabled(&self) -> bool {
        self.shared.journal_input.load(Ordering::Acquire)
    }

    pub(crate) fn send_keyboard_from(
        &self,
        bytes: Vec<u8>,
        enter: bool,
        physical: bool,
    ) -> Result<(), InputError> {
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
                physical,
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

    pub(crate) fn set_typewriter_speed(&self, speed: Option<Speed>) {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return;
        };
        let speed = speed.map_or(0, |speed| speed.multiplier().to_bits());
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
                physical: true,
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
        if self.is_pacing_paste() {
            return Err(InputError::Busy);
        }
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
                paced_reply: None,
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
        if !matches!(
            command,
            StreamInput::PacedPaste { .. } | StreamInput::Protocol(..)
        ) && self.is_pacing_paste()
        {
            return Err(InputError::Busy);
        }
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
        if !matches!(command, StreamInput::Protocol(..) | StreamInput::Break)
            && let Some(activity) = &self.shared.shell_activity
        {
            activity
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .note_code_input();
        }
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
        if let Some(presentation) = &frame.inline {
            self.update_view(|view| {
                let current = if view.inline_anchor.is_some() {
                    presentation.offset_of(view.inline_anchor.as_ref())
                } else if view.scroll_anchor.is_some() {
                    presentation.offset()
                } else {
                    0
                };
                let offset = current
                    .saturating_add_signed(lines as isize)
                    .min(presentation.history());
                view.set_inline_anchor(presentation.anchor(offset));
            });
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

    pub(crate) fn scroll_inline_to(&self, anchor: Option<inline_history::Anchor>) {
        self.update_view(|view| view.set_inline_anchor(anchor));
    }

    pub(crate) fn publish_agent_history(&self, update: inline_history::Update) {
        if let Backend::Stream { events, actor, .. } = &self.backend
            && !events.is_closed()
        {
            inline_history::queue(
                &mut self
                    .shared
                    .inline_updates
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                update,
            );
            let _ = events.try_send(Event::ViewChanged);
            actor.unpark();
        }
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

    pub(crate) fn set_scrollback_config(&self, config: ScrollbackConfig) {
        self.update_view(|view| view.scrollback = config.bounded());
    }

    #[cfg(test)]
    pub(crate) fn scrollback_config_requested(&self) -> ScrollbackConfig {
        requested_scrollback(&self.shared)
    }

    pub(crate) fn set_terminal_appearance(&self, appearance: impl Into<Appearance>) {
        self.update_view(|view| view.appearance = appearance.into());
    }

    pub(crate) fn set_recovery_output_rows(&self, rows: u16) {
        self.update_view(|view| view.recovery_output_rows = rows);
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
                    coordinates.offset(view.raw_anchor()),
                );
                // PromptNavigator's consecutive-jump state belongs to this operation.
                view.scroll_anchor = coordinates.anchor(offset);
                view.inline_anchor = None;
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
                self.cancel_paced_paste();
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
        if view.grid != previous.grid
            || view.scroll_anchor != previous.scroll_anchor
            || view.inline_anchor != previous.inline_anchor
        {
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
        profile: TerminalProfile,
    ) -> io::Result<(Self, StreamReader, StreamWriter)> {
        match source {
            StreamSource::Pty {
                options,
                initial_grid,
            } => {
                let mut pty = Pty::spawn_with_options(
                    initial_grid.columns,
                    initial_grid.rows,
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
            StreamSource::ClosedLocal(error) => Err(io::Error::other(error)),
            StreamSource::Serial {
                connection,
                connect: true,
                ..
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
    shared: &Arc<Shared>,
    event_send: &Sender<Event>,
    events: &Receiver<Event>,
    input_send: &Sender<StreamInput>,
    inputs: &Receiver<StreamInput>,
) -> io::Result<()> {
    if events.is_closed() {
        return Ok(());
    }
    let mut state = EmulationState {
        search: SearchState::default(),
        inline: inline_history::History::default(),
        recording: recording::Actor::new(Arc::clone(&shared.recording), shared.redraw.clone()),
        logfile: None,
        journal: journal::Actor::default(),
        serial_released: None,
    };
    let disconnected = match &source {
        StreamSource::Ssh(None, _) | StreamSource::Serial { connect: false, .. } => {
            Some(Status::Disconnected)
        }
        StreamSource::ClosedLocal(error) => Some(Status::Error(error.clone())),
        _ => None,
    };
    if let Some(status) = disconnected {
        let mut grid = requested_view(shared).grid;
        inputs.close();
        resize_terminal(&mut terminal, grid);
        show_ended_session(
            &mut terminal,
            shared,
            events,
            &status,
            (&mut state.search, &mut state.inline),
            &mut grid,
            None,
        );
        return Ok(());
    }
    let logging = match &mut source {
        StreamSource::Pty { .. } => {
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
    let (mut transport, reader, writer) = StreamTransport::open(source, terminal.profile())?;
    if events.is_closed() {
        return transport.close();
    }
    // Layout may have measured the pane while its transport was opening.
    let mut grid = requested_view(shared).grid;
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
        journal_input: Arc::clone(&shared.journal_input),
    };
    let reader_events = worker_events.clone();
    let read_thread = thread::Builder::new()
        .name("kortty-stream-read".into())
        .spawn(move || {
            read_output(reader, &reader_events);
        })?;
    let writer_inputs = inputs.clone();
    let writer_shared = Arc::clone(shared);
    let write_thread = thread::Builder::new()
        .name("kortty-stream-write".into())
        .spawn(move || {
            write_input(
                writer,
                &writer_inputs,
                &worker_events,
                &writer_shared,
                serial,
            );
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
            if let Some(secrets) = &state.journal.secrets {
                *shared
                    .journal_secrets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(secrets.clone());
                shared
                    .journal_available
                    .store(terminal.supports_journal_output(), Ordering::Release);
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
    shared.journal_input.store(false, Ordering::Release);
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
        let released = state.serial_released.take();
        show_ended_session(
            &mut terminal,
            shared,
            events,
            &status,
            (&mut state.search, &mut state.inline),
            &mut grid,
            released,
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
    shared: &Shared,
    serial: Option<(Arc<SerialPort>, Arc<SerialConnection>)>,
) {
    while !inputs.is_closed() {
        let Ok(command) = inputs.recv_blocking() else {
            break;
        };
        if inputs.is_closed() {
            break;
        }
        let (recording_generation, tab_recording) = if matches!(command, StreamInput::Protocol(..))
        {
            (0, None)
        } else {
            (events.recording.generation(), events.recording.tab_pane())
        };
        let mirrored = matches!(
            &command,
            StreamInput::Raw { mirrored: true, .. }
                | StreamInput::User { mirrored: true, .. }
                | StreamInput::Protocol(_, true)
        );
        if mirrored && shared.coding_agent_waiting_for_decision() {
            continue;
        }
        let user_input = !matches!(&command, StreamInput::Protocol(..) | StreamInput::Break);
        let mut write_reply = None;
        let mut approved = None;
        let mut journal_receipt = None;
        let mut journal_enter = false;
        let mut pacing = None;
        let (bytes, before_write) = match command {
            StreamInput::PacedPaste {
                bytes,
                clear_typeahead,
                pacing: value,
            } => {
                pacing = Some(value);
                let event = if events.observe_shell {
                    Some(Event::UntrackedShellInput { clear_typeahead })
                } else {
                    clear_typeahead.then_some(Event::TypeAheadClear)
                };
                (bytes, event)
            }
            StreamInput::Protocol(bytes, _) => (bytes, None),
            StreamInput::Raw {
                bytes,
                clear_typeahead,
                ..
            } => {
                let before_write = if events.observe_shell {
                    Some(Event::UntrackedShellInput { clear_typeahead })
                } else {
                    clear_typeahead.then_some(Event::TypeAheadClear)
                };
                (bytes, before_write)
            }
            StreamInput::Guarded(input, reply) => match events.authorize_input(input) {
                Ok(input) => {
                    write_reply = Some(reply);
                    approved = Some(input);
                    (Vec::new(), None)
                }
                Err(error) => {
                    reply.finish(Err(error));
                    continue;
                }
            },
            StreamInput::Keyboard {
                bytes,
                enter,
                generation,
                physical,
            } => {
                let journal = if physical && events.journal_input.load(Ordering::Acquire) {
                    let (reply, receipt) = bounded(1);
                    journal_receipt = Some(receipt);
                    journal_enter = enter;
                    Some(reply)
                } else {
                    None
                };
                let event = Event::KeyboardInput {
                    bytes: bytes.clone(),
                    enter,
                    sent_at: Instant::now(),
                    wall_time: enter.then(SystemTime::now),
                    generation,
                    journal,
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
        let bytes = approved
            .as_ref()
            .map_or(bytes.as_slice(), |input| input.bytes.as_slice());
        let permit = journal_receipt.and_then(|receipt| {
            match block_on(select(pin!(events.input.closed()), pin!(receipt.recv()))) {
                Either::Right((Ok(permit), _)) => permit,
                _ => None,
            }
        });
        if inputs.is_closed() {
            break;
        }
        let permit = match permit {
            Some(mut permit) if events.output_activity.last_output() != permit.output => {
                permit.confirmation.take();
                events.send(Event::JournalInputLost(permit));
                None
            }
            permit => permit,
        };
        if let Some(input) = &approved
            && matches!(
                &write_reply,
                Some(
                    WriteReply::CodingAgent(_)
                        | WriteReply::Mcp(..)
                        | WriteReply::AgentTakeover(..)
                )
            )
            && events.output_activity.last_output() != input.output
        {
            if let Some(reply) = write_reply {
                reply.finish(Err(
                    "The agent or terminal screen changed. Choose the action again.",
                ));
            }
            continue;
        }
        if let Some(WriteReply::Mcp(_, permit)) = &write_reply
            && !permit.active()
        {
            if let Some(reply) = write_reply {
                reply.finish(Err("request_cancelled"));
            }
            continue;
        }
        if let Some(WriteReply::AgentTakeover(_, permit)) = &write_reply
            && permit.requested()
        {
            if let Some(reply) = write_reply {
                reply.finish(Err(
                    "The terminal changed before the command could be inserted.",
                ));
            }
            continue;
        }
        // Output can change the pane while this write waits for the actor's event queue.
        if mirrored && shared.coding_agent_waiting_for_decision() {
            continue;
        }
        if user_input && !bytes.is_empty() {
            events.output_activity.note_input();
        }
        let write_result = if let Some(pacing) = pacing {
            pacing.write(
                bytes,
                || inputs.is_closed(),
                |line| {
                    if pacing.hp2392
                        && let Some(response) = events.paced_hp_input(line)?
                    {
                        return write_terminal_bytes(&mut writer, &response);
                    }
                    if let Some((_, connection)) = &serial {
                        let (line, echo) = connection.prepare_input(line.to_vec(), false);
                        if let Some(echo) = echo {
                            let _ = events.send(Event::Echo(echo));
                        }
                        write_terminal_bytes(&mut writer, &line)
                    } else {
                        write_terminal_bytes(&mut writer, line)
                    }
                },
            )
        } else {
            write_terminal_bytes(&mut writer, bytes)
        };
        if let Err(error) = write_result {
            drop(permit);
            if let Some(reply) = write_reply {
                let error = match &reply {
                    WriteReply::AiCode(_) => "Could not write the code to the terminal pane.",
                    WriteReply::CodingAgent(_) => "Could not write the input to the coding agent.",
                    WriteReply::Mcp(..) => "terminal_write_failed",
                    WriteReply::AgentTakeover(..) => {
                        "Could not insert the command into the terminal."
                    }
                };
                reply.finish(Err(error));
            }
            events.send(Event::IoError(error.to_string()));
            inputs.close();
            break;
        }
        if let Some(reply) = write_reply {
            reply.finish(Ok(bytes.len()));
        }
        if let Some(mut permit) = permit {
            if let Some(confirmation) = permit.confirmation.take() {
                let _ = confirmation.try_send(());
            } else if !journal_enter {
                let _ = events.send(Event::JournalInput(
                    permit,
                    zeroize::Zeroizing::new(bytes.to_vec()),
                ));
            }
        }
        if !bytes.is_empty() {
            if mirrored {
                events.output_activity.mirrored_write(Instant::now());
            }
            events.recording_activity(recording_generation);
            if let Some(pane) = tab_recording {
                pane.activity();
            }
        }
    }
    // Drop replies in abandoned inputs so a waiting chat can report closure.
    inputs.close();
    while inputs.try_recv().is_ok() {}
}

fn write_terminal_bytes(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()?;
    #[cfg(target_os = "macos")]
    if !bytes.is_empty() {
        kortty_macos::record_terminal_activity(Instant::now());
    }
    Ok(())
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
        inline,
        recording,
        logfile,
        journal,
        serial_released,
    } = state;
    apply_view_preferences(terminal, &requested_view(shared));
    publish(shared, make_frame(terminal, Status::Running, 0, search));
    let mut last_frame = Instant::now();
    let mut applied_output = None;
    let mut status = Status::Running;
    let mut pack_at = None;
    let mut typeahead = TypeAhead::default();
    let mut typeahead_request = TypeAheadRequest::default();
    let mut pacer = (terminal.profile() == TerminalProfile::Xterm).then(typewriter::Pacer::default);
    let mut coding_agent = match transport {
        StreamTransport::Pty(pty) => pty.job_process_probe().map(ai_code_guard::Monitor::new),
        _ => None,
    };
    while let Some(first) = next_stream_event(
        terminal,
        shared,
        events,
        &mut pack_at,
        typeahead.deadline(),
        pacer.as_mut(),
        coding_agent.as_ref(),
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
            if view.inline_anchor.is_some() {
                1
            } else {
                terminal.scrollback_offset(view.raw_anchor())
            },
            *grid,
        );
        let queued =
            std::iter::from_fn(|| poll_stream_event(shared, events, pacer.as_mut(), true).ok());
        for event in std::iter::once(first).chain(queued).take(32) {
            match event {
                Event::CodingAgentPoll => continue,
                Event::Output(bytes, received_at) => {
                    if let Some(capture) = logfile {
                        capture.advance(&bytes);
                    }
                    if !bytes.is_empty() {
                        search.invalidate();
                        screen_changed = true;
                    }
                    let current = requested_view(shared);
                    if sync_scrollback(terminal, &mut pack_at, current.scrollback) {
                        search.invalidate();
                        screen_changed = true;
                    }
                    apply_view_preferences(terminal, &current);
                    terminal.advance_timed(&bytes, SystemTime::now(), received_at);
                    applied_output = Some(received_at);
                    let shell_events = terminal.take_shell_events();
                    journal.output(terminal, &shell_events);
                    if let Some(activity) = &shared.shell_activity {
                        activity
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .output(
                                !terminal.alternate_screen_active(),
                                shell_events,
                                received_at,
                            );
                    }
                    shared.report_bells(terminal.take_bell_events());
                    let requests = terminal.take_program_notifications();
                    if !requests.is_empty() {
                        let mut pending = shared
                            .program_notifications
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        for request in requests {
                            if pending.len() == 32 {
                                pending.pop_front();
                            }
                            pending.push_back(request);
                        }
                    }
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
                    if applied.fallback.is_some() {
                        journal.suppress_input();
                    }
                    if applied.fallback.is_some()
                        && let Some(activity) = &shared.shell_activity
                    {
                        activity
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .note_code_input();
                    }
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
                    journal: journal_reply,
                } => {
                    if let Some(reply) = journal_reply {
                        let prompt = shared
                            .shell_activity
                            .as_ref()
                            .and_then(|state| state.lock().ok()?.journal_prompt());
                        let permit = journal.permit(
                            terminal,
                            prompt,
                            applied_output,
                            shared.output_activity.last_output() == applied_output,
                            &bytes,
                            enter,
                        );
                        let _ = reply.try_send(permit);
                    } else {
                        journal.suppress_input();
                    }
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
                Event::JournalInput(permit, bytes) => journal.written(permit, &bytes),
                Event::JournalInputLost(permit) => journal.missed_input(permit),
                Event::UntrackedShellInput { clear_typeahead } => {
                    journal.suppress_input();
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
                    paced_reply,
                } => {
                    if record_enter {
                        terminal.record_command_enter(SystemTime::now(), Instant::now());
                    }
                    let local = terminal.hp2392_local_input(&bytes, kind);
                    if local {
                        search.invalidate();
                        screen_changed = true;
                        recording.activity(shared.recording.generation(), Instant::now());
                        if let Some(pane) = shared.recording.tab_pane() {
                            pane.activity();
                        }
                    }
                    if let Some(reply) = paced_reply {
                        let _ = reply.try_send(local.then(|| terminal.take_responses()));
                        continue;
                    }
                    let sent = if local {
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
                        recording.tab_frame(terminal, *grid, appearance, screen_changed);
                        screen_changed = false;
                    }
                    recording.command(command, terminal, *grid, appearance);
                }
                Event::Journal(command) => {
                    let prompt = shared
                        .shell_activity
                        .as_ref()
                        .and_then(|state| state.lock().ok()?.journal_prompt());
                    journal.command(terminal, command, prompt);
                    shared
                        .journal_input
                        .store(journal.captures_input(), Ordering::Release);
                }
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
                Event::ScrollbackChanged => {
                    search.invalidate();
                    screen_changed = true;
                }
                Event::LocalShellDirectory(reply) => {
                    if let StreamTransport::Pty(pty) = transport {
                        let _ = reply.try_send(pty.shell_directory());
                    }
                    continue;
                }
                #[cfg(windows)]
                Event::WindowsTerminalFile(request, reply) => {
                    let result = match transport {
                        StreamTransport::Pty(pty)
                            if status == Status::Running
                                && !terminal.alternate_screen_active()
                                && !terminal.cursor_line_is_password_prompt()
                                && shared.output_activity.last_output() == applied_output =>
                        {
                            windows_file::resolve(request, pty, terminal, shared)
                        }
                        _ => Err(windows_file::unverified()),
                    };
                    let _ = reply.try_send(result);
                    continue;
                }
                Event::Search(next) => request = Some(next),
                Event::ControlRead(request) => {
                    control_reads.push(request);
                    continue;
                }
                Event::TerminalCommand(command, reply) => {
                    if let TerminalCommand::WriteAiCode { code, run_at } = command {
                        if let Err(error) =
                            queue_ai_code(shared, input, &status, code, run_at, reply.clone())
                        {
                            let _ =
                                reply.try_send(TerminalCommandResult::AiCodeWritten(Err(error)));
                        }
                        refresh_frame = true;
                        continue;
                    }
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
                Event::McpPreflight(guard, reply) => {
                    let _ = reply.try_send(guard.check(mcp::Live {
                        terminal,
                        shared,
                        status: &status,
                        transport,
                        output: applied_output,
                        revision: search.revision,
                    }));
                }
                Event::AuthorizeInput { input, reply } => {
                    let run = matches!(
                        &input,
                        GuardedInput::AiCode {
                            run_at: Some(_),
                            ..
                        }
                    ) || matches!(&input, GuardedInput::Mcp(input) if input.submits);
                    let changes_prompt =
                        matches!(&input, GuardedInput::Mcp(input) if input.changes_prompt);
                    let bytes = match input {
                        GuardedInput::AiCode { code, run_at } => authorize_ai_code(
                            terminal,
                            shared,
                            &status,
                            &code,
                            run_at,
                            applied_output,
                            |screen, run| match transport {
                                StreamTransport::Pty(pty) => pty.any_job_process(|image, args| {
                                    ai_code_guard::process_blocks_action(screen, run, image, args)
                                }),
                                _ => false,
                            },
                        )
                        .map(zeroize::Zeroizing::new),
                        GuardedInput::CodingAgent(request) => ai_code_guard::authorize_agent_input(
                            terminal,
                            shared,
                            &status,
                            coding_agent.as_mut(),
                            applied_output,
                            search.revision,
                            request,
                        ),
                        GuardedInput::Mcp(input) => input
                            .guard
                            .check(mcp::Live {
                                terminal,
                                shared,
                                status: &status,
                                transport,
                                output: applied_output,
                                revision: search.revision,
                            })
                            .map(|()| input.bytes),
                        GuardedInput::AgentTakeover(request) => request.authorize(mcp::Live {
                            terminal,
                            shared,
                            status: &status,
                            transport,
                            output: applied_output,
                            revision: search.revision,
                        }),
                    };
                    if bytes.is_ok() {
                        if changes_prompt && let Some(activity) = &shared.shell_activity {
                            activity
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .untracked_input();
                        }
                        journal.suppress_input();
                        typeahead.clear();
                        if run {
                            terminal.record_command_enter(SystemTime::now(), Instant::now());
                        }
                    }
                    let _ = reply.try_send(bytes.map(|bytes| ApprovedInput {
                        bytes,
                        output: applied_output,
                    }));
                }
                Event::CaptureReconnectHistory(reply) => {
                    sync_scrollback(terminal, &mut pack_at, requested_scrollback(shared));
                    refresh_frame |= inline.apply(terminal, take_inline_updates(shared));
                    let _ = reply.try_send(inline.capture_reconnect(terminal));
                    continue;
                }
                Event::PrependReconnectHistory(history, reply) => {
                    sync_scrollback(terminal, &mut pack_at, requested_scrollback(shared));
                    inline.apply(terminal, take_inline_updates(shared));
                    inline.prepend_reconnect(terminal, *history);
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
        if sync_scrollback(terminal, &mut pack_at, view.scrollback) {
            search.invalidate();
            screen_changed = true;
        }
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
        recording.tab_frame(terminal, *grid, view.appearance, screen_changed);
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
        if let Some(monitor) = &mut coding_agent {
            refresh_frame |= monitor.tick(
                shared.coding_agent_detection.load(Ordering::Acquire),
                screen_changed,
                terminal,
                Instant::now(),
            );
        }
        refresh_frame |= inline.apply(terminal, take_inline_updates(shared));
        if !refresh_frame && !screen_changed {
            answer_control_reads(terminal, None, control_reads);
            continue;
        }
        let view = requested_view(shared);
        apply_view_preferences(terminal, &view);
        let mut frame = inline.frame(terminal, status.clone(), &view, search);
        if view.recovery_output_rows != 0
            && !frame.screen.alternate_screen
            && !frame.password_prompt
        {
            frame.recovery_output = terminal
                .recovery_output_snapshot(usize::from(view.recovery_output_rows))
                .map(Arc::new);
        }
        let offset = if frame.at_live_viewport() { 0 } else { 1 };
        let can_predict =
            sync_typeahead(&mut typeahead, &mut typeahead_request, &view, offset, *grid);
        typeahead.expire(Instant::now());
        if let Some(monitor) = &coding_agent {
            frame.coding_agent = monitor.status();
            if !frame.password_prompt {
                frame.coding_agent_details = monitor.details();
            }
        }
        let live = frame.at_live_viewport().then_some(&frame.screen);
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
            view.inline_anchor = None;
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
        TerminalCommand::AiRecentOutput => match terminal.recent_visible_output() {
            Ok((text, truncated)) => TerminalCommandResult::Copied { text, truncated },
            Err(status) => TerminalCommandResult::LastOutput(status),
        },
        TerminalCommand::WriteAiCode { .. } => TerminalCommandResult::AiCodeWritten(Err(
            "This terminal does not accept AI code input.",
        )),
        TerminalCommand::CopySelection { revision, range } => {
            if revision != search.revision || !requested_view(shared).prompt_navigation_enabled {
                return TerminalCommandResult::SelectionChanged;
            }
            copy_terminal_output(terminal, range)
        }
    }
}

fn queue_ai_code(
    shared: &Shared,
    input: &Sender<StreamInput>,
    status: &Status,
    code: zeroize::Zeroizing<String>,
    run_at: Option<CodePrompt>,
    reply: Sender<TerminalCommandResult>,
) -> Result<(), &'static str> {
    if *status != Status::Running || shared.serial.is_some() {
        return Err("The terminal is disconnected or does not accept AI code input.");
    }
    if !input.is_empty() {
        return Err("The terminal pane is busy.");
    }
    if let Some(expected) = run_at {
        let activity = shared
            .shell_activity
            .as_ref()
            .ok_or("Could not verify the terminal input state.")?;
        let current = activity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .code_prompt();
        if current != expected || expected.state == PromptState::Busy {
            return Err("The terminal input or prompt changed. Review the command again.");
        }
    }
    input
        .try_send(StreamInput::Guarded(
            GuardedInput::AiCode { code, run_at },
            WriteReply::AiCode(reply),
        ))
        .map_err(|error| match error {
            TrySendError::Full(_) => "The terminal input queue is full.",
            TrySendError::Closed(_) => "The terminal pane disconnected.",
        })
}

fn authorize_ai_code(
    terminal: &Terminal,
    shared: &Shared,
    status: &Status,
    code: &str,
    run_at: Option<CodePrompt>,
    applied_output: Option<Instant>,
    local_process_blocks: impl FnOnce(&ScreenSnapshot, bool) -> bool,
) -> Result<Vec<u8>, &'static str> {
    if *status != Status::Running || shared.serial.is_some() {
        return Err("The terminal is disconnected or does not accept AI code input.");
    }
    if terminal.alternate_screen_active()
        || terminal.cursor_line_is_password_prompt()
        || shared.typewriter_speed.load(Ordering::Relaxed) != 0
    {
        return Err("The terminal pane is busy.");
    }
    const OUTPUT_CHANGED: &str = "The terminal output changed. Choose the action again.";
    if shared.output_activity.last_output() != applied_output {
        return Err(OUTPUT_CHANGED);
    }
    let screen = terminal.snapshot();
    if ai_code_guard::approval_prompt_visible(&screen) {
        return Err("The terminal is waiting for a coding-agent approval.");
    }
    if run_at.is_some() {
        if ai_code_guard::coding_agent_visible(&screen) {
            return Err("A coding agent is using this terminal pane.");
        }
        if !valid_ai_run_line(code) {
            return Err("Run requires one shell line without control characters.");
        }
    }
    if local_process_blocks(&screen, run_at.is_some()) {
        return Err(if run_at.is_some() {
            "A coding agent is using this terminal pane."
        } else {
            "The terminal is waiting for a coding-agent approval."
        });
    }
    let bracketed = run_at.is_none() && terminal.bracketed_paste();
    if code.contains(['\n', '\t']) && !bracketed {
        return Err("Bracketed paste is no longer active. Review the code again.");
    }
    let mut bytes = crate::input::encode_stream_paste(&screen, code, bracketed);
    // The stream encoders preserve literal question marks and use this byte for
    // unsupported characters. A replacement can change the meaning of shell code.
    if bytes.iter().filter(|&&byte| byte == b'?').count()
        != code.bytes().filter(|&byte| byte == b'?').count()
    {
        return Err("The terminal encoding cannot represent this code exactly.");
    }
    if run_at.is_some() {
        bytes.extend(
            screen
                .encode_key(kortty_terminal::TerminalKey::Enter, Default::default())
                .unwrap_or_else(|| vec![b'\r']),
        );
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("The code exceeds the terminal input limit.");
    }
    let mut prompt = shared
        .shell_activity
        .as_ref()
        .map(|activity| activity.lock().unwrap_or_else(|error| error.into_inner()));
    if let Some(expected) = run_at
        && (expected.state == PromptState::Busy
            || prompt
                .as_ref()
                .is_none_or(|state| state.code_prompt() != expected))
    {
        return Err("The terminal input or prompt changed. Review the command again.");
    }
    // The reader can queue new output while the actor inspects local processes.
    if shared.output_activity.last_output() != applied_output {
        return Err(OUTPUT_CHANGED);
    }
    if let Some(state) = &mut prompt {
        state.untracked_input();
    }
    Ok(bytes)
}

pub(crate) fn valid_ai_run_line(code: &str) -> bool {
    !code.trim().is_empty()
        && !code.contains("***")
        && code.chars().all(|character| {
            !character.is_control()
                && crate::session_journal::is_display_character(character)
                && !matches!(character, '\u{2028}' | '\u{2029}')
        })
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
    presentation: (&mut SearchState, &mut inline_history::History),
    grid: &mut GridSize,
    serial_released: Option<Sender<()>>,
) {
    let (search, inline) = presentation;
    let view = requested_view(shared);
    apply_view_preferences(terminal, &view);
    inline.apply(terminal, take_inline_updates(shared));
    publish(
        shared,
        inline.frame(terminal, status.clone(), &view, search),
    );
    // Both I/O threads have returned, so the exclusive serial handle has been released.
    drop(serial_released);
    let mut pack_at = Some(Instant::now() + SCROLLBACK_IDLE);
    while let Some(event) =
        next_stream_event(terminal, shared, events, &mut pack_at, None, None, None)
    {
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
                sync_scrollback(terminal, &mut pack_at, requested_scrollback(shared));
                let changed = inline.apply(terminal, take_inline_updates(shared));
                let _ = reply.try_send(inline.capture_reconnect(terminal));
                if !changed {
                    continue;
                }
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
            Event::ScrollbackChanged => search.invalidate(),
            _ => continue,
        }
        inline.apply(terminal, take_inline_updates(shared));
        publish(
            shared,
            inline.frame(terminal, status.clone(), &requested_view(shared), search),
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

#[cfg(test)]
fn control_screen(
    pane_id: String,
    mode: ReadMode,
    limit: usize,
    live: &ScreenSnapshot,
    terminal: Option<&Terminal>,
    active: impl Fn() -> bool,
) -> Option<ControlScreen> {
    control_screen_rows(pane_id, mode, limit, live, terminal, false, active)
        .map(|(screen, _, _)| screen)
}

fn control_screen_rows(
    pane_id: String,
    mode: ReadMode,
    limit: usize,
    live: &ScreenSnapshot,
    terminal: Option<&Terminal>,
    mask_context: bool,
    active: impl Fn() -> bool,
) -> Option<(ControlScreen, Vec<bool>, bool)> {
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
    let mut wraps = Vec::new();
    let mut first_row = 0;
    let wrapped_columns = |row: usize| {
        if row < history {
            terminal.and_then(|terminal| terminal.history_wrapped_columns(row))
        } else {
            live.wrapped_columns((row - history) as u16)
                .map(usize::from)
        }
    };
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
        let wrap = mask_context.then(|| wrapped_columns(row)).flatten();
        let line = if row < history {
            let Some(cells) = terminal.and_then(|terminal| terminal.history_line(row)) else {
                continue;
            };
            control_line(&cells, wrap)
        } else {
            let Some(cells) = live.row((row - history) as u16) else {
                continue;
            };
            control_line(cells, wrap)
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
        if mask_context {
            wraps.push(wrap.is_some());
        }
        first_row = row;
    }
    lines.reverse();
    wraps.reverse();
    let starts_with_continuation = mask_context
        && (first_row.checked_sub(1).and_then(wrapped_columns).is_some()
            || (first_row == 0 && history == 0 && live.starts_with_continuation()));
    Some((
        ControlScreen {
            pane_id,
            mode: mode.name().into(),
            lines,
            columns: live.columns,
            rows: live.rows,
            alternate_screen: !recent && live.alternate_screen,
            osc_title: (!live.title.is_empty()).then(|| live.title.clone()),
            truncated,
        },
        wraps,
        starts_with_continuation,
    ))
}

fn control_line(cells: &[Cell], wrap: Option<usize>) -> String {
    let mut line = String::new();
    let mut spaces = 0;
    for cell in cells.iter().take(wrap.unwrap_or(cells.len())) {
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
    if wrap.is_some() {
        line.extend(std::iter::repeat_n(' ', spaces));
        return line;
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
    coding_agent: Option<&ai_code_guard::Monitor>,
) -> Option<Event> {
    while !events.is_closed() {
        if sync_scrollback(terminal, pack_at, requested_scrollback(shared)) {
            return Some(Event::ScrollbackChanged);
        }
        match poll_stream_event(shared, events, pacer.as_deref_mut(), false) {
            Ok(event) => return Some(event),
            Err(TryRecvError::Closed) => return None,
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        let agent_deadline = coding_agent.and_then(|monitor| {
            monitor.deadline(shared.coding_agent_detection.load(Ordering::Acquire), now)
        });
        if agent_deadline.is_some_and(|deadline| deadline <= now) {
            return Some(Event::CodingAgentPoll);
        }
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
            .chain(agent_deadline)
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

fn sync_scrollback(
    terminal: &mut Terminal,
    pack_at: &mut Option<Instant>,
    config: ScrollbackConfig,
) -> bool {
    let changed = terminal.set_scrollback_config(config);
    if !config.compression {
        *pack_at = None;
    } else if changed {
        *pack_at = Some(Instant::now() + SCROLLBACK_IDLE);
    }
    changed
}

fn requested_scrollback(shared: &Shared) -> ScrollbackConfig {
    shared
        .view
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .scrollback
}

fn poll_stream_event(
    shared: &Shared,
    events: &Receiver<Event>,
    pacer: Option<&mut typewriter::Pacer>,
    batch_continuation: bool,
) -> Result<Event, TryRecvError> {
    // Order the next transport event with any concurrent strict-mode request.
    let bridge = clipboard::lock(&shared.clipboard);
    if bridge.has_commands(batch_continuation) {
        return Ok(Event::Clipboard);
    }
    match pacer {
        Some(pacer) => pacer.poll(events, shared.typewriter_speed(), Instant::now()),
        None => events.try_recv(),
    }
}

fn requested_offset(shared: &Shared, terminal: &Terminal) -> usize {
    terminal.scrollback_offset(requested_view(shared).raw_anchor())
}

fn take_inline_updates(shared: &Shared) -> std::collections::VecDeque<inline_history::Update> {
    std::mem::take(
        &mut *shared
            .inline_updates
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    )
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
        inline: None,
        scrollback: Some(terminal.scrollback_snapshot()),
        output_snapshot: Some(Arc::new(terminal.output_snapshot(scroll_offset != 0))),
        recovery_output: None,
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
        coding_agent: None,
        coding_agent_details: None,
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
    let mut previous = shared
        .frame
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if frame.ibm.is_some()
        && frame.status == Status::Running
        && let Some(pane) = shared.recording.tab_pane()
    {
        let view = requested_view(shared);
        pane.frame(frame.screen.clone(), view.grid, view.appearance);
    }
    *previous = Arc::new(frame);
    drop(previous);
    let _ = shared.redraw.try_send(());
}

#[cfg(test)]
mod search_tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(8);

    #[test]
    fn mirrored_input_writer_drops_a_blocked_pane_and_resumes_when_idle() {
        struct Wire(std::sync::mpsc::Sender<Vec<u8>>);

        impl Write for Wire {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0
                    .send(bytes.to_vec())
                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let session = Session::disconnected_ibm(IbmKind::Tn3270e);
        session.set_mirror_test_frame(Some(CodingAgentState::Blocked));
        let shared = Arc::clone(&session.shared);
        let (input, inputs) = bounded(4);
        let (sender, _events) = bounded(4);
        let events = WorkerEvents {
            sender,
            input: input.clone(),
            actor: thread::current(),
            recording: Arc::clone(&shared.recording),
            output_activity: Arc::clone(&shared.output_activity),
            observe_shell: false,
            journal_input: Arc::new(AtomicBool::new(false)),
        };
        let (wire, writes) = std::sync::mpsc::channel();
        let writer =
            thread::spawn(move || write_input(Wire(wire), &inputs, &events, &shared, None));
        let queue = |bytes: &[u8], mirrored| {
            input
                .try_send(StreamInput::Raw {
                    bytes: bytes.to_vec(),
                    clear_typeahead: false,
                    mirrored,
                })
                .is_ok()
        };
        let blocked = queue(b"mirrored while blocked", true);
        let direct = queue(b"direct", false);
        let first = writes.recv_timeout(TIMEOUT);
        let blocked_activity = session.last_mirrored_input();
        session.set_mirror_test_frame(Some(CodingAgentState::Idle));
        let resumed = queue(b"mirrored when idle", true);
        let second = writes.recv_timeout(TIMEOUT);
        input.close();
        writer.join().unwrap();

        assert!(blocked && direct && resumed);
        assert_eq!(first.unwrap(), b"direct");
        assert!(blocked_activity.is_none());
        assert_eq!(second.unwrap(), b"mirrored when idle");
        assert!(session.last_mirrored_input().is_some());
    }

    #[test]
    fn scrollback_compression_actor_cancels_and_resumes_the_idle_deadline() {
        let mut terminal = Terminal::new(16, 4);
        let before = Instant::now();
        let mut deadline = Some(before);
        let config = ScrollbackConfig::default();
        sync_scrollback(
            &mut terminal,
            &mut deadline,
            ScrollbackConfig {
                compression: false,
                ..config
            },
        );
        assert!(deadline.is_none());
        assert!(!terminal.set_scrollback_compression_enabled(false));

        sync_scrollback(&mut terminal, &mut deadline, config);
        let resumed = deadline.unwrap();
        assert!(resumed >= before + SCROLLBACK_IDLE);
        assert!(!terminal.set_scrollback_compression_enabled(true));
        sync_scrollback(&mut terminal, &mut deadline, config);
        assert_eq!(deadline, Some(resumed));
    }

    #[cfg(unix)]
    #[test]
    fn active_pty_applies_latest_scrollback_limits_without_more_output() {
        let config = ScrollbackConfig {
            lines: 200,
            ..ScrollbackConfig::default()
        };
        let session = Session::start_local_with_strict(
            GridSize {
                columns: 64,
                rows: 4,
                ..GridSize::default()
            },
            TerminalProfile::Xterm,
            LaunchOptions {
                command: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    concat!(
                        r#"i=0; while [ "$i" -lt 250 ]; do printf 'row\n'; i=$((i+1)); done; "#,
                        "printf 'history-ready\\n'; IFS= read -r gate",
                    )
                    .into(),
                ]),
                ..LaunchOptions::default()
            },
            true,
            Appearance::default(),
            StreamOptions {
                scrollback: config,
                ..StreamOptions::default()
            },
        )
        .unwrap();
        let initial = wait_for_frame(&session, |frame| visible_text(frame, "history-ready"));
        assert_eq!(initial.screen.scrollback_lines, 200);
        session.scroll(180);
        wait_for_frame(&session, |frame| frame.scroll_offset == 180);

        session.set_scrollback_config(ScrollbackConfig {
            lines: 150,
            ..config
        });
        session.set_scrollback_config(ScrollbackConfig {
            lines: 100,
            ..config
        });
        let trimmed = wait_for_frame(&session, |frame| frame.screen.scrollback_lines == 100);
        assert_eq!(trimmed.scroll_offset, 100);
        let _ = session.stop().recv_blocking();
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
                inline_anchor: None,
                typeahead: TypeAheadRequest::default(),
                prompt_navigation_enabled: true,
                prompt_navigator: PromptNavigator::default(),
                appearance: Appearance::default(),
                scrollback: ScrollbackConfig::default(),
                recovery_output_rows: 0,
            }),
            inline_updates: Mutex::default(),
            frame: Mutex::new(Arc::new(frame)),
            logfile_warning: crate::terminal_logging::Warning::new(redraw.clone()),
            redraw,
            bell_events: AtomicU64::new(0),
            program_notifications: Mutex::default(),
            output_activity: Arc::default(),
            local_shell_pid: AtomicU32::new(0),
            sandbox_backend: Mutex::default(),
            serial: None,
            serial_notice: Mutex::new(None),
            shell_activity: None,
            coding_agent_detection: AtomicU64::new(0),
            clipboard: Mutex::default(),
            recording: Arc::default(),
            journal_available: AtomicBool::new(false),
            journal_input: Arc::new(AtomicBool::new(false)),
            journal_generation: AtomicU64::new(0),
            journal_secrets: Mutex::new(None),
            typewriter_speed: AtomicU64::new(0),
            paste_cancellation: Mutex::default(),
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

    #[test]
    fn ai_code_insertion_checks_live_brackets_and_never_adds_enter() {
        let session = Session::disconnected_ibm(IbmKind::Tn3270e);
        let mut terminal = Terminal::new(80, 24);
        let insert = |terminal: &Terminal, code| {
            authorize_ai_code(
                terminal,
                &session.shared,
                &Status::Running,
                code,
                None,
                None,
                |_, _| false,
            )
        };
        assert_eq!(
            authorize_ai_code(
                &terminal,
                &session.shared,
                &Status::Running,
                "line",
                None,
                None,
                |_, run| !run
            ),
            Err("The terminal is waiting for a coding-agent approval.")
        );
        assert!(insert(&terminal, "first\nsecond").is_err());
        assert_eq!(insert(&terminal, "single line").unwrap(), b"single line");
        terminal.advance(b"\x1b[?2004h");
        assert_eq!(
            insert(&terminal, "first\nsecond").unwrap(),
            b"\x1b[200~first\nsecond\x1b[201~"
        );
        terminal.advance(b"\x1b[?2004l");
        assert!(insert(&terminal, "first\nsecond").is_err());
        terminal.advance(b"\x1b[?1049h");
        assert!(insert(&terminal, "single line").is_err());
        let legacy = Terminal::with_profile(80, 24, TerminalProfile::Vt220);
        assert!(insert(&legacy, "printf 🧪").is_err());
    }

    #[test]
    fn ai_code_insertion_refuses_output_arriving_during_process_inspection() {
        let session = Session::disconnected_ibm(IbmKind::Tn3270e);
        let terminal = Terminal::new(80, 24);
        let shared = &session.shared;
        assert_eq!(
            authorize_ai_code(
                &terminal,
                shared,
                &Status::Running,
                "line",
                None,
                None,
                |_, _| {
                    shared.output_activity.output(Instant::now());
                    false
                }
            ),
            Err("The terminal output changed. Choose the action again.")
        );
    }

    #[test]
    fn batched_pty_output_follows_the_strict_mode_change() {
        let session = Session::disconnected_ibm(IbmKind::Tn3270e);
        let shared = &session.shared;
        let (sender, events) = bounded(2);
        let mut pacer = typewriter::Pacer::default();
        sender.try_send(Event::ViewChanged).unwrap();
        assert!(matches!(
            poll_stream_event(shared, &events, Some(&mut pacer), false),
            Ok(Event::ViewChanged)
        ));

        assert!(clipboard::lock(&shared.clipboard).request_strict(true));
        sender
            .try_send(Event::Output(
                b"\x1b_Ga=T,i=74,f=24,s=1,v=1,q=2,C=1;AP8A\x1b\\".to_vec(),
                Instant::now(),
            ))
            .unwrap();
        assert!(matches!(
            poll_stream_event(shared, &events, Some(&mut pacer), true),
            Ok(Event::Clipboard)
        ));

        let mut terminal = Terminal::new(12, 2);
        clipboard::lock(&shared.clipboard).apply(&mut terminal);
        let Ok(Event::Output(bytes, _)) =
            poll_stream_event(shared, &events, Some(&mut pacer), true)
        else {
            panic!("expected queued PTY output");
        };
        terminal.advance(&bytes);
        assert!(terminal.snapshot().images.is_empty());
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
        session.send_keyboard(b"A".to_vec(), false).unwrap();
        session.send_paste_with_delay(b"B\rC".to_vec(), 25).unwrap();
        wait_for_frame(&session, |frame| {
            !session.is_pacing_paste() && visible_text(frame, "HP:ABC  OK")
        });
        session.send_paste(b"\r".to_vec()).unwrap();
        session.send_shifted_hp_enter(b"\r".to_vec()).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "HP:ABC  OK"));
        assert_eq!(session.frame().status, Status::Running);
        session.send_keyboard(b"\x1b[Z".to_vec(), false).unwrap();
        session.send_keyboard(b"\r".to_vec(), true).unwrap();
        let completed = wait_for_frame(&session, |frame| matches!(frame.status, Status::Exited(_)));
        assert!(visible_text(&completed, "HP-HOST:ABC "));
        assert!(!visible_text(&completed, "HP-HOST:ABCABC"));
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

    #[track_caller]
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
    fn agent_cursor_takeover_revalidates_native_prompt_and_writes_only_the_draft() {
        use crate::session::agent_takeover::{Destination, Request};
        use crate::terminal_agent::{Cancel, LocalDirectory};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let other = root.join("different");
        std::fs::create_dir(&other).unwrap();
        let directory = LocalDirectory::capture(root.clone()).unwrap();
        let session = Session::start_local(
            GridSize::default(),
            TerminalProfile::Xterm,
            LaunchOptions {
                working_directory: Some(root),
                ..LaunchOptions::default()
            },
            true,
            None,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        session.send(concat!(
            r#"stty -echo; expected=$(printf '\033[200~printf "takeover draft"\033[201~'); "#,
            r#"printf '\033[?2004h\033]133;A\007draft-ready\033]133;B\007'; "#,
            r#"IFS= read -r draft; if [ "$draft" = "$expected" ]; then printf '\n%s-%s\n' draft matches; "#,
            r#"else printf '\n%s-%s\n' draft differs; fi"#,
            "\r"
        ).as_bytes().to_vec()).unwrap();
        let frame = wait_for_frame(&session, |frame| {
            visible_text(frame, "draft-ready") && session.agent_shortcut_prompt(frame).is_some()
        });
        let prompt = session.agent_shortcut_prompt(&frame).unwrap();
        let revision = session.mcp_input_revision();
        let request = |directory, revision, permit, command: &str| {
            Request::new(
                Destination::Local(directory),
                prompt,
                frame.search_revision,
                revision,
                permit,
                command.into(),
            )
        };
        let assert_rejected = |request| {
            let receipt = session.take_over_agent_command(request).unwrap();
            assert!(receipt.recv_blocking().unwrap().is_err());
        };
        let command = "printf \"takeover draft\"";
        assert!(
            request(
                directory.clone(),
                revision,
                Cancel::default(),
                "echo a\necho b"
            )
            .is_err()
        );
        let revoked = Cancel::default();
        revoked.request();
        assert_rejected(request(directory.clone(), revision, revoked, command).unwrap());
        assert_rejected(
            request(
                directory.clone(),
                revision.wrapping_add(1),
                Cancel::default(),
                command,
            )
            .unwrap(),
        );
        assert_rejected(
            request(
                LocalDirectory::capture(other).unwrap(),
                revision,
                Cancel::default(),
                command,
            )
            .unwrap(),
        );

        let receipt = session
            .take_over_agent_command(
                request(directory, revision, Cancel::default(), command).unwrap(),
            )
            .unwrap();
        assert_eq!(receipt.recv_blocking().unwrap(), Ok(command.len() + 12));
        thread::sleep(FRAME_INTERVAL * 3);
        assert!(!visible_text(&session.frame(), "draft-matches"));
        assert!(!visible_text(&session.frame(), "draft-differs"));
        session.send_keyboard(b"\r".to_vec(), true).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "draft-matches"));
        let _ = session.stop().recv_blocking();
    }

    #[cfg(unix)]
    #[test]
    fn coding_agent_monitor_requires_a_live_process_and_rule_then_clears_on_exit() {
        use std::os::unix::fs::DirBuilderExt as _;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-agent-monitor-{}",
            crate::private_files::nonce().unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let scratch = Scratch(root);
        let executable = scratch.0.join("gemini");
        let script_path = scratch.0.join("monitor.sh");
        let answer_gate = scratch.0.join("answer");
        // Keep a native Gemini process in the foreground group while the
        // system shell runs the fixture script.
        let shell = if cfg!(target_os = "macos") {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        #[cfg(not(target_os = "macos"))]
        std::fs::copy(shell, &executable).unwrap();
        #[cfg(target_os = "macos")]
        {
            let source = scratch.0.join("gemini.c");
            std::fs::write(
                &source,
                r#"#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) {
        execl("/bin/bash", "bash", argv[1], argv[2], (char *)0);
        _exit(127);
    }
    int status;
    if (waitpid(child, &status, 0) != child) return 4;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 5;
}
"#,
            )
            .unwrap();
            let build = std::process::Command::new("/usr/bin/cc")
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(
                build.status.success(),
                "could not build the coding agent fixture: {}",
                String::from_utf8_lossy(&build.stderr)
            );
        }
        let script = concat!(
            r#"saved=$(stty -g); trap 'stty "$saved"' EXIT; stty -echo; "#,
            r#"printf '\033[2J\033[H%s%s\n' 'monitor-' 'ready'; while IFS= read -r action; do case "$action" in "#,
            r#"block|renew) printf '\033[2J\033[Hquestion-%s\n%s\n%s\n' "$action" '│ How would you like to authenticate for this project? │' '│ (Use Enter to select) │';; "#,
            r#"YES) while [ ! -f "$1" ]; do sleep .05; done; printf '\033[2J\033[Hanswer-accepted\nType your message or @path/to/file\n';; "#,
            r#"'hello agent') printf '\033[2J\033[Hprompt-accepted\nType your message or @path/to/file\n';; "#,
            r#"password) printf '\033[2J\033[HPassword: ';; "#,
            r#"plain) printf '\033[2J\033[Hmonitor-fallback\n';; quit) break;; esac; done"#,
        );
        // Keep rule evidence out of the launch line echoed by the interactive shell.
        std::fs::write(&script_path, script).unwrap();
        let session = Session::start_local(
            GridSize::default(),
            TerminalProfile::Xterm,
            LaunchOptions {
                command: Some(vec![shell.into(), "-i".into()]),
                working_directory: Some(scratch.0.clone()),
                ..LaunchOptions::default()
            },
            true,
            None,
        )
        .unwrap();
        wait_for_frame(&session, |frame| frame.status == Status::Running);
        assert!(!session.coding_agent_detection_requested());
        session.set_coding_agent_detection(true);
        let send = |line: &str| session.send(format!("{line}\r").into_bytes()).unwrap();
        send("./gemini ./monitor.sh ./answer");
        let deadline = Instant::now() + TIMEOUT;
        let ready = loop {
            let frame = session.frame();
            if visible_text(&frame, "monitor-ready") {
                break frame;
            }
            if Instant::now() >= deadline {
                let screen = (0..frame.screen.rows)
                    .filter_map(|row| frame.screen.row(row))
                    .map(|cells| {
                        cells
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                panic!(
                    "coding agent fixture did not start: {:?}\n{}",
                    frame.status,
                    screen.join("\n")
                );
            }
            thread::sleep(FRAME_INTERVAL);
        };
        assert!(ready.coding_agent().is_none());
        send("block");
        #[track_caller]
        fn read_state(session: &Session, state: CodingAgentState) -> CodingAgentStatus {
            wait_for_frame(session, |frame| {
                frame
                    .coding_agent()
                    .is_some_and(|agent| agent.state == state)
            })
            .coding_agent()
            .unwrap()
        }
        let blocked = read_state(&session, CodingAgentState::Blocked);
        assert_eq!((blocked.id, blocked.name), ("gemini-cli", "Gemini CLI"));
        let frame = session.frame();
        let details = frame.coding_agent_details().unwrap();
        assert_ne!(details.pid, 0);
        assert_eq!(details.command.as_ref(), "gemini");
        assert!(details.started_at.is_some());
        let rule = details.rule.as_ref().unwrap();
        assert_eq!(rule.id.as_ref(), "auth-prompt");
        assert!(rule.evidence.contains("How would you like to authenticate"));
        let initial_privacy_generation = details.privacy_generation;
        let revision = session.frame().search_revision;
        let write = |status, revision, input| {
            session
                .send_coding_agent_input(status, revision, input)
                .unwrap()
                .recv_blocking()
                .unwrap()
        };
        let prompt = || CodingAgentInput::Prompt(zeroize::Zeroizing::new("hello agent".into()));
        assert_eq!(
            write(blocked, revision, prompt()),
            Err("The agent is waiting for a decision. Answer it before sending a prompt.")
        );
        session
            .send_interactive(b"plain\r".to_vec(), Hp2392Input::Key { enter: true }, true)
            .unwrap();
        send("renew");
        let renewed = wait_for_frame(&session, |frame| visible_text(frame, "question-renew"));
        assert_eq!(renewed.coding_agent(), Some(blocked));
        assert!(session.last_mirrored_input().is_none());
        let yes = || CodingAgentInput::Key(CodingAgentKey::Yes);
        assert!(write(blocked, revision, yes()).is_err());
        assert_eq!(write(blocked, renewed.search_revision, yes()), Ok(4));
        assert_eq!(
            write(blocked, renewed.search_revision, yes()),
            Err("This decision was already sent. Wait for the agent to respond.")
        );
        std::fs::write(answer_gate, []).unwrap();
        wait_for_frame(&session, |frame| visible_text(frame, "answer-accepted"));
        let idle = read_state(&session, CodingAgentState::Idle);
        assert_eq!(
            write(idle, session.frame().search_revision, prompt()),
            Ok(12)
        );
        wait_for_frame(&session, |frame| visible_text(frame, "prompt-accepted"));
        session
            .send_interactive(b"plain\r".to_vec(), Hp2392Input::Key { enter: true }, true)
            .unwrap();
        wait_for_frame(&session, |frame| {
            visible_text(frame, "monitor-fallback") && session.last_mirrored_input().is_some()
        });
        send("block");
        read_state(&session, CodingAgentState::Blocked);
        send("plain");
        let fallback = read_state(&session, CodingAgentState::Idle);
        assert_eq!(fallback.instance, blocked.instance);
        assert!(fallback.since > blocked.since);
        assert!(
            session
                .frame()
                .coding_agent_details()
                .unwrap()
                .rule
                .is_none()
        );
        send("password");
        let password = wait_for_frame(&session, |frame| frame.password_prompt);
        assert!(password.coding_agent_details().is_none());
        send("block");
        assert_eq!(
            read_state(&session, CodingAgentState::Blocked).instance,
            blocked.instance
        );
        assert_ne!(
            session
                .frame()
                .coding_agent_details()
                .unwrap()
                .privacy_generation,
            initial_privacy_generation
        );
        session.set_coding_agent_detection(false);
        let disabled = wait_for_frame(&session, |frame| frame.coding_agent().is_none());
        assert!(disabled.coding_agent_details().is_none());
        session.set_coding_agent_detection(true);
        let reenabled = read_state(&session, CodingAgentState::Blocked);
        assert_ne!(reenabled.instance, blocked.instance);
        session.set_coding_agent_detection(false);
        session.set_coding_agent_detection(true);
        wait_for_frame(&session, |frame| {
            frame.coding_agent().is_some_and(|agent| {
                agent.state == CodingAgentState::Blocked && agent.instance != reenabled.instance
            })
        });
        send("quit");
        wait_for_frame(&session, |frame| {
            frame.status == Status::Running && frame.coding_agent().is_none()
        });
        let _ = session.stop().recv_blocking();
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
            .attach_journal_configured(capture.sink(), Default::default(), true, false)
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
