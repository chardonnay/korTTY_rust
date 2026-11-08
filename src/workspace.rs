use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;

use async_channel::Receiver;
use iced::event::{self, Status as EventStatus};
use iced::futures::{StreamExt, stream::BoxStream};
use iced::keyboard::{self, Key, Location, Modifiers, key::Named};
use iced::widget::{
    Column, button, checkbox, column, container, pane_grid, pick_list, row, scrollable, space,
    text, text_input, tooltip,
};
use iced::{Alignment, Element, Event, Length, Size, Subscription, Task, window};
use kortty_ibm::{DeviceName5250, Plaintext};
use kortty_jobs::RunRequest;
use kortty_pty::LocalShell;
use kortty_terminal::{Hp2392Input, PromptDirection, TerminalProfile};

use crate::connection_store::Store;
use crate::diagram_window::{self, DiagramWindow};
use crate::editor_window::{self, EditorWindow};
use crate::input::{encode_ibm_key, encode_stream_key, encode_stream_paste, sanitize_paste};
use crate::job_executor;
use crate::profile::{self, PaneLayout, WindowLayout, WindowPlacement, WorkspaceLayout};
use crate::scheduler_window::{self, SchedulerWindow, SshCommand};
use crate::serial_dialog::{self, SerialDialog};
use crate::session::{
    Frame, GridSize, IbmInput, IbmKind, InputError, SerialConnection, Session, SessionId, Status,
};
use crate::session_security::{IsolationLevel, LocalSettings};
use crate::sftp_window::{self, SftpWindow};
use crate::terminal_search::{self, TerminalSearch};
use crate::terminal_view::{self, SearchHighlight, Selection, TerminalView};

mod ai_access;
mod appearance;
mod ascii_art;
mod backup_automatic;
pub(crate) mod backup_data;
mod backup_ui;
mod bell;
mod chrome;
pub(crate) use chrome::ToolIcon;
pub(crate) use chrome::popup as native_popup;
pub(crate) use snippet_access::SnippetReportAction;
mod clipboard_access;
mod close_all_dialog;
mod control_access;
mod dashboard;
mod dialog_placement;
mod effects;
pub(crate) mod environment_access;
mod journal_access;
mod journal_appearance;
mod journal_appearance_access;
mod journal_capture_access;
mod journal_dock;
mod journal_image_menu;
mod journal_note_translation;
mod journal_thumbnail;
mod key_bindings;
mod known_hosts;
mod local_dock;
mod logfile_access;
mod multi_exec;
mod multi_exec_access;
mod open_recent;
mod openpgp_window;
mod os_badge_manager;
mod palette;
mod paste_protection;
mod profile_tools;
mod project_access;
mod quick_connect;
mod recently_closed;
mod reconnect;
mod recording_access;
mod remote_dock;
mod scheduler_access;
mod session_processes;
mod session_restore;
mod session_security;
mod settings_window;
mod sftp_access;
mod shell_integration;
mod snippet_access;
mod split_connection;
mod ssh_access;
mod ssh_keys_ui;
mod tab_context;
mod tab_drag;
mod tab_titles;
mod tab_watch;
mod terminal_commands;
pub(crate) mod terminal_context;
mod terminal_effect_access;
mod terminal_file_access;
pub(crate) mod terminal_link_dialog;
mod tool_tabs;
mod transparency;
mod tunnel_access;
mod window_lifecycle;
mod window_navigation;
pub use ssh_access::SshTarget;

const DEFAULT_FONT: u8 = 14;
const DEFAULT_WINDOW: Size = Size::new(1000.0, 640.0);

#[derive(Debug, Clone)]
pub enum Message {
    Window(window::Id, window::Event),
    WindowGeometryChecked(window::Id, u64, Option<f32>, kortty_windowing::WindowMode),
    DialogPlacementChecked(window::Id, u64, Option<WindowPlacement>),
    #[cfg(target_os = "linux")]
    DialogOpeningRedraw(window::Id),
    DialogPlacementClosing(window::Id, Option<WindowPlacement>),
    MainPlacementClosing(
        window::Id,
        bool,
        Option<(kortty_windowing::WindowMode, Option<WindowPlacement>)>,
    ),
    ExitPlacementChecked(
        Vec<(
            window::Id,
            kortty_windowing::WindowMode,
            Option<WindowPlacement>,
        )>,
    ),
    SaveLastWindow,
    WindowPlacementPoll(window::Id, u64),
    TerminalOnlyInspected(window::Id, u64, kortty_windowing::WindowMode),
    TerminalOnlyDeadline(window::Id, u64, kortty_windowing::WindowMode),
    Transparency(transparency::Message),
    #[cfg(target_os = "macos")]
    DockReopen(bool),
    #[cfg(target_os = "macos")]
    NativeMenu(u16),
    #[cfg(target_os = "macos")]
    NativeRecentlyClosed(u64),
    #[cfg(target_os = "macos")]
    NativeOpenRecent(u64),
    #[cfg(target_os = "macos")]
    NativeOpenRecentShowing,
    Chrome(chrome::Message),
    Palette(window::Id, palette::Message),
    Dashboard(window::Id, dashboard::Message),
    QuickConnect(window::Id, quick_connect::Message),
    KnownHosts(window::Id, known_hosts::Message),
    SshKeys(window::Id, ssh_keys_ui::Message),
    Backup(window::Id, backup_ui::Message),
    OpenPgp(window::Id, openpgp_window::Message),
    AutomaticBackup(backup_automatic::Message),
    Logfile(window::Id, crate::logfile_window::Message),
    SessionProcesses(window::Id, session_processes::Message),
    SessionProcessFeed(session_processes::Feed),
    SessionJournal(window::Id, journal_access::Message),
    JournalCapture(journal_capture_access::Message),
    JournalDock(window::Id, journal_dock::Message),
    JournalDockTick,
    JournalViewerTick,
    JournalAppearance(
        journal_appearance_access::Target,
        journal_appearance::controls::Event,
    ),
    JournalAppearanceSave,
    JournalSystemTheme(iced::theme::Mode),
    TerminalEffects(terminal_effect_access::Message),
    TerminalEffectFrame(std::time::Instant),
    SetTerminalEffect(
        window::Id,
        SessionId,
        Option<crate::terminal_effects::Effect>,
    ),
    TerminalEffectSpeed(window::Id, SessionId, u8),
    Settings(settings_window::Message),
    ShellIntegration(window::Id, shell_integration::Message),
    TerminalContext(window::Id, terminal_context::Message),
    TabContext(window::Id, tab_context::Message),
    TabWatchTick,
    CloseAllDialog(window::Id, close_all_dialog::Message),
    RecentlyClosed(recently_closed::Message),
    OpenRecent(open_recent::Message),
    TerminalCommand(terminal_commands::Message),
    TerminalLink(window::Id, terminal_link_dialog::Message),
    Key(window::Id, Key, Location, Modifiers, Option<String>),
    RepeatedKey(window::Id, Key, Location, Modifiers, Option<String>),
    CommitTabCycle(window::Id),
    NewWindow,
    OpenDiagram,
    Diagram(window::Id, diagram_window::Message),
    AsciiArt(window::Id, ascii_art::Message),
    OpenEditor,
    Editor(window::Id, editor_window::Message),
    TerminalFileLoaded(u64, Result<editor_window::Document, String>),
    LocalDock(window::Id, local_dock::Message),
    RemoteDock(window::Id, remote_dock::Message),
    RemoteDockTick,
    AiManager(ai_access::Message),
    Recording(recording_access::Message),
    NewTab(window::Id),
    NewWindowsShell(window::Id, LocalShell),
    ShowIbm(window::Id, IbmKind, Option<SessionId>),
    IbmAddress(window::Id, String),
    IbmDevice(window::Id, IbmKind),
    IbmDeviceName(window::Id, String),
    IbmPlaintext(window::Id, bool),
    ConnectIbm(window::Id),
    CancelIbm(window::Id),
    ShowSerial(window::Id, Option<SessionId>),
    Serial(window::Id, serial_dialog::Message),
    SerialReconnectReleased(window::Id, u64),
    SerialBreak(window::Id, SessionId),
    ConfirmSerialClose(window::Id, u64, bool),
    SelectTab(window::Id, u64),
    CloseTab(window::Id, u64),
    ToggleTransfer(window::Id),
    Transfer {
        from: window::Id,
        to: Option<window::Id>,
        tab: u64,
    },
    Split(window::Id, pane_grid::Axis),
    CancelSplitConnection(window::Id, SessionId),
    FocusPane(window::Id, pane_grid::Pane),
    ClosePane(window::Id, pane_grid::Pane),
    ZoomPane(window::Id, pane_grid::Pane),
    ResizeSplit(window::Id, pane_grid::ResizeEvent),
    DragPane(window::Id, pane_grid::DragEvent),
    TabDrag(tab_drag::Message),
    ToggleBroadcast(window::Id),
    ToggleCommandTimestamps(window::Id),
    ResizeCommandTimestamps(window::Id, Option<Size>),
    MultiExec(Option<window::Id>, multi_exec_access::Action),
    ToggleTypeAhead(SessionId),
    AudibleBell(bool),
    ShellTabTitles(window::Id, bool),
    PromptNavigation(window::Id, bool),
    PaneResized(SessionId, GridSize),
    SessionUpdate(SessionId),
    ReconnectHistoryReady(SessionId, bool),
    Selection(SessionId, Option<Selection>),
    SelectionFinished(window::Id, SessionId, Selection, std::sync::Weak<Frame>),
    OpenSearch(SessionId),
    Search(SessionId, terminal_search::Message),
    TerminalMouse(SessionId, Vec<u8>),
    Clipboard(clipboard_access::Message),
    Control(control_access::Message),
    SessionSecurity(session_security::Message),
    OpenConnections(window::Id),
    Connections(crate::connection_window::Message),
    ConnectionOverview(window::Id, profile_tools::connection_overview::Event),
    OsBadgeManager(window::Id, os_badge_manager::Event),
    ConnectionUsed(u64, Option<crate::connection_store::Store>),
    OpenVault(window::Id),
    Vault(window::Id, crate::vault_window::Message),
    Ssh(ssh_access::Message),
    Tunnel(tunnel_access::Message),
    Sftp(window::Id, sftp_window::Message),
    OpenScheduler(window::Id),
    Scheduler(window::Id, scheduler_window::Message),
    OpenSnippets(window::Id),
    Snippets(window::Id, crate::snippet_window::Message),
    SnippetRecovery(snippet_access::recovery::Event),
    SnippetStructureTick,
    SnippetComparison(window::Id, crate::snippet_window::compare::Message),
    SnippetRun(window::Id, snippet_access::RunMessage),
    SnippetTransfer(window::Id, u64, crate::snippet_transfer::Event),
    SnippetAnalysis(window::Id, snippet_access::AnalysisEvent),
    JobFinished(job_executor::Completion),
    RuntimeTick,
    Scroll(SessionId, i32),
    ScrollTo(SessionId, Option<kortty_terminal::ScrollbackAnchor>),
    Copy(window::Id),
    Paste(window::Id),
    PasteLoaded(window::Id, u64, u64, Vec<SessionId>, Option<String>),
    ConfirmPaste(window::Id, u64, bool),
    SaveLayout(window::Id),
    RestoreLayout(window::Id),
    LayoutLoaded(window::Id, Result<WorkspaceLayout, String>),
    Project(window::Id, project_access::Message),
    SessionRestore(session_restore::Message),
    Notice(window::Id, String),
    Quit,
    ExitReady,
    Noop,
}

#[derive(Default)]
pub struct Workspace {
    app_logging: crate::app_logging::Runtime,
    bell: bell::Bell,
    chrome: chrome::Chrome,
    palette: palette::Palette,
    quick_connect: Option<quick_connect::Dialog>,
    known_hosts: HashMap<window::Id, known_hosts::KnownHosts>,
    logfiles: logfile_access::Windows,
    session_processes: HashMap<window::Id, session_processes::Dialog>,
    journals: HashMap<window::Id, journal_access::Surface>,
    journal_edits: journal_access::metadata::Edits,
    journal_actions: journal_access::actions::Actions,
    journal_entry_deletes: journal_access::EntryDeleteDialogs,
    journal_markers: journal_access::marker_dialog::State,
    journal_replacements: journal_access::replace_dialog::Dialogs,
    journal_exports: journal_access::export_dialog::Exports,
    journal_screenshot_exports: journal_access::screenshot_export::Exports,
    journal_screenshot_copies: journal_access::screenshot_access::Copies,
    journal_screenshot_analyses: journal_access::screenshot_ai_access::Analyses,
    journal_note_translations: journal_note_translation::Translations,
    journal_screenshot_editors: journal_access::screenshot_editor::Editors,
    journal_captures: HashMap<u64, journal_capture_access::Entry>,
    retired_journal_captures: Vec<Arc<crate::session_journal::Capture>>,
    effect_dialogs: HashMap<window::Id, effects::Dialog>,
    effect_frame: Option<std::time::Instant>,
    settings: settings_window::Settings,
    shell_integration: shell_integration::Setup,
    terminal_context: Option<terminal_context::Context>,
    tab_context: tab_context::State,
    tab_attention: tab_watch::AttentionRequests,
    close_all_dialogs: close_all_dialog::Dialogs,
    recently_closed: recently_closed::History,
    open_recent: open_recent::History,
    pending_reconnects: HashMap<SessionId, reconnect::PendingReconnect>,
    link_confirmations: HashMap<window::Id, terminal_link_dialog::Confirmation>,
    environments: environment_access::Environments,
    saved_terminal_preferences: profile_tools::SavedTerminalPreferences,
    window_settings: tab_titles::WindowSettings,
    theme_cache: transparency::ThemeCache,
    transparency_save_pending: bool,
    journal_appearance_ui: journal_appearance_access::State,
    clipboard: crate::terminal_clipboard::TerminalClipboard,
    control: control_access::Control,
    security: session_security::Security,
    lifecycle: window_lifecycle::Lifecycle,
    dialog_placement: dialog_placement::State,
    snippet_session: Option<crate::snippet_window::session::Writer>,
    snippet_remembered: Option<crate::snippet_window::session::State>,
    snippet_recovery: snippet_access::recovery::State,
    snippet_structure_save_at: Option<std::time::Instant>,
    main_placement_closing: HashSet<window::Id>,
    exit_placement_pending: bool,
    local_dock: local_dock::Preference,
    remote_dialogs: remote_dock::Dialogs,
    remote_manager_closing: HashMap<SessionId, Receiver<()>>,
    remote_browser_closing: HashMap<SessionId, Receiver<()>>,
    session_restore: session_restore::Restore,
    project_reconnects: project_access::ProjectReconnects,
    windows: HashMap<window::Id, Window>,
    #[cfg(target_os = "macos")]
    native_focused_window: Option<window::Id>,
    tab_drag: tab_drag::DragState,
    multi_exec: multi_exec::Membership,
    diagrams: HashMap<window::Id, DiagramWindow>,
    ascii_art: HashMap<window::Id, ascii_art::Dialog>,
    editors: HashMap<window::Id, EditorWindow>,
    recording_windows: HashMap<window::Id, crate::recording_window::RecordingWindow>,
    video: recording_access::Video,
    ai_manager: ai_access::State,
    sftp: HashMap<window::Id, SftpWindow>,
    sftp_logins: HashMap<window::Id, ssh_access::PendingSsh>,
    connections: Option<crate::connection_window::ConnectionWindow>,
    connection_overview: Option<profile_tools::connection_overview::Dialog>,
    os_badge_manager: os_badge_manager::State,
    connections_id: Option<window::Id>,
    connection_source: Option<window::Id>,
    connection_usage: profile_tools::ConnectionUsage,
    vault: Option<crate::vault_window::VaultWindow>,
    vault_id: Option<window::Id>,
    ssh_keys: ssh_keys_ui::Tool,
    backup: backup_ui::Tool,
    openpgp: HashMap<window::Id, openpgp_window::Window>,
    openpgp_cache_refresh: Option<u64>,
    tool_close: tool_tabs::CloseState,
    backup_runner: backup_automatic::Runner,
    backup_preferences: crate::profile_backup::preferences::Preferences,
    backup_progress: crate::profile_backup::preferences::Progress,
    backup_status: Option<String>,
    backup_recipients: Vec<crate::snippets::recipients::Choice>,
    ssh_key_choices: Vec<crate::ssh_dialog::KeyChoice>,
    quit_waiting_profile: Option<window::Id>,
    scheduler: Option<SchedulerWindow>,
    scheduler_owners: HashMap<window::Id, window::Id>,
    scheduler_views: HashMap<window::Id, scheduler_window::SchedulerView>,
    scheduler_workers: usize,
    scheduler_queue: VecDeque<RunRequest<SshCommand>>,
    scheduler_draining: bool,
    snippets: HashMap<window::Id, snippet_access::Surface>,
    snippet_comparisons: HashMap<window::Id, snippet_access::ComparisonSurface>,
    snippet_comparison_workers: Vec<Receiver<()>>,
    sessions: HashMap<SessionId, PaneSession>,
    next_id: u64,
    pending_shutdown: Vec<Receiver<()>>,
    terminal_files: terminal_file_access::TerminalFiles,
    closing_tunnel_owners: HashMap<SessionId, tunnel_access::ClosingOwner>,
    quit_waiting_for: Option<window::Id>,
    quit_sftp_waiting: Option<window::Id>,
    exiting: bool,
}

struct Window {
    number: u64,
    size: Size,
    geometry: window_lifecycle::Geometry,
    transparency: transparency::WindowAlpha,
    local_files: local_dock::Browser,
    journal_dock: journal_dock::Dock,
    remote_modifiers: Modifiers,
    dashboard: dashboard::State,
    scale_factor: f32,
    tabs: Vec<TabEntry>,
    tab_mru: Vec<u64>,
    tab_cycle: window_navigation::TabCycle,
    terminal_only: window_navigation::TerminalOnly,
    timestamp_width_delta: Option<i32>,
    active: usize,
    local_profile: TerminalProfile,
    focused: bool,
    transfer_open: bool,
    paste_request: Option<u64>,
    pending_paste: Option<PendingPaste>,
    pending_ibm: Option<PendingIbm>,
    pending_serial: Option<PendingSerial>,
    pending_ssh: Option<ssh_access::PendingSsh>,
    pending_split_connection: Option<SessionId>,
    pending_tunnels: Option<SessionId>,
    pending_snippet_run: Option<snippet_access::RunPrompt>,
    pending_serial_close: Option<(u64, SerialClose)>,
    projects: Option<project_access::Panel>,
    clipboard_settings: bool,
    notice: Option<String>,
}

enum TabEntry {
    Terminal(Box<Tab>),
    Tool(tool_tabs::HostedTab),
}

impl TabEntry {
    fn id(&self) -> u64 {
        match self {
            Self::Terminal(tab) => tab.id,
            Self::Tool(tab) => tab.id,
        }
    }

    fn terminal(&self) -> Option<&Tab> {
        match self {
            Self::Terminal(tab) => Some(tab),
            Self::Tool(_) => None,
        }
    }

    fn terminal_mut(&mut self) -> Option<&mut Tab> {
        match self {
            Self::Terminal(tab) => Some(tab),
            Self::Tool(_) => None,
        }
    }
}

impl From<Tab> for TabEntry {
    fn from(tab: Tab) -> Self {
        Self::Terminal(Box::new(tab))
    }
}

struct Tab {
    id: u64,
    custom_title: Option<String>,
    group: Option<String>,
    connection_group_baseline: Option<String>,
    watch: tab_watch::TabWatch,
    reopen_origin: Option<recently_closed::Origin>,
    grid: pane_grid::State<SessionId>,
    remote_files: remote_dock::Dock,
    primary: pane_grid::Pane,
    focused: pane_grid::Pane,
    broadcast: bool,
    command_timestamps: bool,
    incognito: bool,
    tunnel_owner: Option<SessionId>,
}

struct PaneSession {
    session: Session,
    frame: Arc<Frame>,
    grid: GridSize,
    font: u8,
    font_baseline: u8,
    inherits_font_size: bool,
    selection: Option<Selection>,
    selected_output: Option<terminal_commands::SelectedOutput>,
    pending_split_connection: Option<split_connection::PendingSplitConnection>,
    search: Option<TerminalSearch>,
    typeahead_enabled: bool,
    strict_escapes: bool,
    local_security: Option<LocalSettings>,
    ssh: Option<SshTarget>,
    ibm_origin: Option<recently_closed::IbmOrigin>,
    credential_environment_source: Option<String>,
    forwarding: Option<tunnel_access::Forwarding>,
    effect: Option<effects::Active>,
}

impl PaneSession {
    fn font_size_override(&self) -> Option<u8> {
        (!self.inherits_font_size).then_some(self.font_baseline)
    }

    fn supports_typeahead(&self) -> bool {
        self.frame.serial.is_none()
            && self.frame.ibm.is_none()
            && self.frame.screen.profile == TerminalProfile::Xterm
            && self.frame.status == Status::Running
    }
}

struct PendingPaste {
    request: u64,
    tab: u64,
    targets: Vec<SessionId>,
    text: String,
    preview: String,
    warning: paste_protection::Warning,
    mode: paste_protection::WarningMode,
    raw_bytes: usize,
}

struct PendingIbm {
    kind: IbmKind,
    replace: Option<SessionId>,
    reconnect: Option<reconnect::transport::IbmReconnect>,
    font: Option<u8>,
    address: String,
    device_name: String,
    plaintext: bool,
}

struct PendingSerial {
    form: SerialDialog,
    replace: Option<SessionId>,
    reconnect: Option<reconnect::transport::SerialReconnect>,
    split: Option<split_connection::SplitTarget>,
    font: Option<u8>,
}

#[derive(Clone, Copy)]
enum SerialClose {
    Window,
    Tab(u64),
    TabBatch(u64),
    Pane {
        tab: u64,
        pane: pane_grid::Pane,
        session: SessionId,
    },
    Quit,
}

enum PaneInput {
    Local(Vec<u8>),
    Ibm(IbmInput),
}

#[derive(Clone, Copy)]
enum InputSource {
    Paste,
    Keyboard {
        enter: bool,
        predict: bool,
        shifted_enter: bool,
        physical_pane: Option<SessionId>,
    },
}

impl PendingIbm {
    fn device_name(&self) -> std::io::Result<Option<DeviceName5250>> {
        let name = self.device_name.trim();
        if name.is_empty() {
            Ok(None)
        } else {
            name.parse().map(Some)
        }
    }

    fn connection(&self) -> Option<(SocketAddr, Plaintext, Option<DeviceName5250>)> {
        if !self.plaintext {
            return None;
        }
        let address: SocketAddr = self.address.trim().parse().ok()?;
        let device_name = self.device_name().ok()?;
        (address.port() != 0).then_some((address, Plaintext::ExplicitlyAllowed, device_name))
    }
}

#[derive(Clone)]
struct Watch(SessionId, Receiver<()>);

impl Hash for Watch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

fn watch(watch: &Watch) -> BoxStream<'static, Message> {
    let id = watch.0;
    watch
        .1
        .clone()
        .map(move |()| Message::SessionUpdate(id))
        .boxed()
}

impl Window {
    fn remember_active_tab(&mut self) {
        self.tab_cycle.clear();
        if self.focused
            && let Some(tab) = self.tab_mut()
        {
            tab.watch.seen();
        }
        if let Some(tab) = self.active_tab_id() {
            self.tab_mru.retain(|&previous| previous != tab);
            self.tab_mru.insert(0, tab);
            self.tab_mru.truncate(window_navigation::HISTORY_LIMIT);
        }
    }

    fn cancel_paste(&mut self) {
        self.paste_request = None;
        self.pending_paste = None;
    }

    fn tab(&self) -> Option<&Tab> {
        self.active_entry()?.terminal()
    }

    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)?.terminal_mut()
    }

    fn active_entry(&self) -> Option<&TabEntry> {
        self.tabs.get(self.active)
    }

    fn active_tab_id(&self) -> Option<u64> {
        self.active_entry().map(TabEntry::id)
    }

    fn terminal_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter_map(TabEntry::terminal)
    }

    fn terminal_tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.tabs.iter_mut().filter_map(TabEntry::terminal_mut)
    }

    fn normalize_active(&mut self, removed: usize) {
        let removed_active = self.active == removed;
        if self.active > removed {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        if removed_active {
            self.remember_active_tab();
        }
        self.cancel_paste();
        self.pending_ibm = None;
        self.pending_serial = None;
        self.pending_ssh
            .take_if(|pending| pending.reopen_token().is_none());
        self.pending_tunnels = None;
        self.pending_serial_close
            .take_if(|(_, action)| !matches!(action, SerialClose::TabBatch(_)));
        self.transfer_open = false;
    }

    fn pane_location(&self, session: SessionId) -> Option<(usize, pane_grid::Pane)> {
        self.tabs.iter().enumerate().find_map(|(index, entry)| {
            let tab = entry.terminal()?;
            tab.grid
                .iter()
                .find_map(|(pane, id)| (*id == session).then_some((index, *pane)))
        })
    }
}

impl Tab {
    fn focused_session(&self) -> Option<SessionId> {
        self.grid.get(self.focused).copied()
    }

    fn close_pane(&mut self, pane: pane_grid::Pane) -> Option<SessionId> {
        let (session, sibling) = self.grid.close(pane)?;
        if self.primary == pane {
            self.primary = sibling;
        }
        if self.focused == pane {
            self.focused = sibling;
        }
        Some(session)
    }

    fn targets(&self) -> Vec<SessionId> {
        if self.broadcast {
            control_access::ordered_panes(self)
                .into_iter()
                .map(|(_, id)| id)
                .collect()
        } else {
            self.focused_session().into_iter().collect()
        }
    }
}

impl Workspace {
    pub(super) fn prepare_app_logging(
        &self,
        settings: &crate::app_logging::Settings,
    ) -> std::io::Result<Option<crate::app_logging::Candidate>> {
        if self.clipboard.policy.problem.is_some() {
            return Err(std::io::Error::other("Managed policy could not be applied"));
        }
        self.app_logging.prepare(
            settings,
            &self.clipboard.policy.app_logging,
            &profile::root()?,
        )
    }

    pub(super) fn commit_app_logging(&mut self, candidate: Option<crate::app_logging::Candidate>) {
        self.app_logging.commit(candidate);
    }

    pub(super) fn plan_app_logging(
        &self,
        settings: &crate::app_logging::Settings,
    ) -> std::io::Result<Option<crate::app_logging::Plan>> {
        if self.clipboard.policy.problem.is_some() {
            return Err(std::io::Error::other("Managed policy could not be applied"));
        }
        self.app_logging.plan(
            settings,
            &self.clipboard.policy.app_logging,
            &profile::root()?,
        )
    }

    pub(super) fn abort_app_logging(&mut self, candidate: Option<crate::app_logging::Candidate>) {
        self.app_logging.discard(candidate);
    }

    pub fn boot() -> (Self, Task<Message>) {
        let mut workspace = Self {
            video: recording_access::Video::load(),
            ai_manager: ai_access::State::load(),
            ..Self::default()
        };
        workspace.clipboard.policy = crate::clipboard_policy::ClipboardPolicy::load();
        workspace.refresh_backup_preferences();
        workspace.refresh_backup_recipients();
        workspace.chrome = chrome::Chrome::load();
        workspace.lifecycle = window_lifecycle::Lifecycle::initialize();
        workspace.dialog_placement = dialog_placement::State::initialize();
        match crate::snippets::profile_path()
            .map(|path| path.with_file_name(crate::snippet_window::session::FILE_NAME))
            .and_then(crate::snippet_window::session::Writer::start)
        {
            Ok(writer) => workspace.snippet_session = Some(writer),
            Err(error) => eprintln!("kortty: could not start snippet workspace writer: {error}"),
        }
        workspace.local_dock = local_dock::Preference::load();
        workspace.window_settings = tab_titles::WindowSettings::load();
        workspace.load_journal_markers();
        if workspace.clipboard.policy.problem.is_none()
            && let Ok(profile) = profile::root()
        {
            workspace.app_logging = crate::app_logging::Runtime::initialize(
                &workspace.window_settings.app_logging,
                &workspace.clipboard.policy.app_logging,
                &profile,
            );
        }
        workspace.environments = environment_access::Environments::load();
        workspace.saved_terminal_preferences = profile_tools::SavedTerminalPreferences::load();
        workspace.initialize_saved_clipboard_settings();
        workspace.security = session_security::Security::load();
        workspace.load_vault_status();
        workspace.refresh_ssh_key_choices();
        let (id, task) = workspace.boot_session_restore();
        workspace.initialize_scheduler(id, true);
        let recent = workspace.refresh_open_recent();
        let power = workspace.reconcile_sleep_prevention();
        let recovery = workspace.initialize_snippet_recovery();
        (
            workspace,
            Task::batch([
                task,
                recent,
                power,
                recovery,
                iced::system::theme().map(Message::JournalSystemTheme),
            ]),
        )
    }

    pub fn title(&self, id: window::Id) -> String {
        if self.effect_dialogs.contains_key(&id) {
            return terminal_effect_access::TITLE.into();
        }
        if let Some(title) = self
            .quick_connect_title(id)
            .or_else(|| self.known_hosts_title(id))
            .or_else(|| self.ssh_keys_title(id))
            .or_else(|| self.backup_title(id))
            .or_else(|| self.connection_import_title(id))
            .or_else(|| self.connection_export_title(id))
            .or_else(|| self.connection_overview_title(id))
            .or_else(|| self.os_badge_manager_title(id))
            .or_else(|| {
                self.is_openpgp_standalone(id)
                    .then_some(openpgp_window::TITLE)
            })
            .or_else(|| self.environment_title(id))
            .or_else(|| self.manual_title(id))
            .or_else(|| self.ai_manager_title(id))
            .or_else(|| self.chrome.title(id))
            .or_else(|| self.settings.title(id))
            .or_else(|| self.shell_integration_title(id))
            .or_else(|| self.link_confirmation_title(id))
            .or_else(|| self.rename_dialog_title(id))
            .or_else(|| self.tab_journal_note_title(id))
            .or_else(|| self.close_all_dialog_title(id))
            .or_else(|| self.remote_dialog_title(id))
        {
            return title.into();
        }
        if let Some(title) = self.logfiles.title(id) {
            return title;
        }
        if self.is_session_process_window(id) {
            return session_processes::TITLE.into();
        }
        if let Some(title) = self.journal_title(id) {
            return title.into_owned();
        }
        if let Some(recording) = self.recording_windows.get(&id) {
            return recording.title().into();
        }
        if self.diagrams.contains_key(&id) {
            return "kortty · Diagrams".into();
        }
        if self.ascii_art.contains_key(&id) {
            return ascii_art::TITLE.into();
        }
        if self.editors.contains_key(&id) {
            return "kortty · Editor".into();
        }
        if self.sftp.contains_key(&id) {
            return "kortty · SFTP".into();
        }
        if self.is_scheduler_window(id) {
            return scheduler_access::TITLE.into();
        }
        if self.is_snippet_window(id) {
            return snippet_access::TITLE.into();
        }
        if self.snippet_comparisons.contains_key(&id) {
            return crate::snippet_window::compare::TITLE.into();
        }
        if self.connections_id == Some(id) {
            return "Connection Manager".into();
        }
        if self.vault_id == Some(id) {
            return if self.vault_unlock_prompt_owner(id).is_some() {
                "Unlock Vault".into()
            } else {
                "kortty · Credential Vault".into()
            };
        }
        self.windows.get(&id).map_or_else(
            || "kortty".into(),
            |window| format!("kortty · Window {}", window.number),
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let events = event::listen_with(|event, status, id| match event {
            Event::Window(event) => Some(Message::Window(id, event)),
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                location,
                modifiers,
                text,
                repeat,
                ..
            }) if status == EventStatus::Ignored => {
                let text = text.map(|value| value.to_string());
                Some(if repeat {
                    Message::RepeatedKey(id, key, location, modifiers, text)
                } else {
                    Message::Key(id, key, location, modifiers, text)
                })
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if window_navigation::ends_tab_cycle(&key, modifiers) =>
            {
                Some(Message::CommitTabCycle(id))
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, modifiers, .. })
                if window_navigation::ends_tab_cycle_on_release(&key, modifiers) =>
            {
                Some(Message::CommitTabCycle(id))
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => Some(
                Message::RemoteDock(id, remote_dock::Message::Modifiers(modifiers)),
            ),
            _ => None,
        });
        Subscription::batch(
            [
                events,
                self.scheduler_subscription(),
                self.snippet_recovery_subscription(),
                self.snippet_structure_subscription(),
                self.backup_subscription(),
                self.recording_subscription(),
                self.tab_watch_subscription(),
                self.journal_dock_subscription(),
                self.journal_viewer_subscription(),
                iced::system::theme_changes().map(Message::JournalSystemTheme),
                self.effect_subscription(),
                self.session_process_subscription(),
                self.lifecycle.subscription(),
                self.session_restore.subscription(),
                self.tab_drag.subscription(),
                self.remote_dock_subscription(),
                self.control.subscription(),
                #[cfg(target_os = "linux")]
                self.dialog_placement.opening_subscription(),
            ]
            .into_iter()
            .chain(self.sessions.iter().map(|(id, pane)| {
                Subscription::run_with(Watch(*id, pane.session.updates()), watch)
            })),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let message = match message {
            Message::RepeatedKey(id, key, location, modifiers, text) => {
                if self.suppressed_quick_select_repeat(id, &key) {
                    return Task::none();
                }
                Message::Key(id, key, location, modifiers, text)
            }
            message => {
                self.interrupt_quick_select(&message);
                message
            }
        };
        #[cfg(target_os = "macos")]
        let previous_tool_focus = (
            self.native_focused_openpgp(),
            self.native_focused_tool_tab(),
        );
        let changes_layout = session_restore::changes_layout(&message)
            || matches!(&message, Message::MainPlacementClosing(..));
        let sync_remote = changes_layout
            || matches!(
                &message,
                Message::Window(_, window::Event::Opened { .. })
                    | Message::Transfer { .. }
                    | Message::TabDrag(_)
                    | Message::CloseTab(..)
                    | Message::Chrome(_)
                    | Message::Control(_)
                    | Message::TerminalOnlyInspected(..)
                    | Message::Palette(..)
                    | Message::Dashboard(..)
                    | Message::ReconnectHistoryReady(..)
            )
            || matches!(&message, Message::Key(_, key, _, modifiers, _) if window_navigation::tab_step(key, *modifiers).is_some()
                || self.window_settings.key_bindings.action_for(key, *modifiers).is_some());
        #[cfg(target_os = "macos")]
        let changes_native_menu = changes_layout
            || matches!(
                &message,
                Message::Window(
                    _,
                    window::Event::Opened { .. }
                        | window::Event::Focused
                        | window::Event::Unfocused
                        | window::Event::CloseRequested
                        | window::Event::Closed
                ) | Message::NativeMenu(_)
                    | Message::NativeRecentlyClosed(_)
                    | Message::NativeOpenRecent(_)
                    | Message::RecentlyClosed(_)
                    | Message::OpenRecent(_)
                    | Message::ConnectionUsed(..)
                    | Message::Connections(_)
                    | Message::CloseAllDialog(..)
                    | Message::ConfirmSerialClose(..)
                    | Message::Ssh(_)
                    | Message::ReconnectHistoryReady(..)
                    | Message::SerialReconnectReleased(..)
                    | Message::Serial(
                        _,
                        serial_dialog::Message::Reconnect(_) | serial_dialog::Message::Cancel
                    )
                    | Message::ConnectIbm(_)
                    | Message::CancelIbm(_)
                    | Message::Chrome(_)
                    | Message::WindowGeometryChecked(..)
                    | Message::TerminalOnlyInspected(..)
                    | Message::TerminalOnlyDeadline(..)
                    | Message::Key(_, Key::Named(Named::Escape | Named::F12 | Named::Tab), ..)
                    | Message::LocalDock(_, _)
                    | Message::RemoteDock(
                        _,
                        remote_dock::Message::Action(_, remote_dock::Action::Hide)
                            | remote_dock::Message::Dialog(_)
                    )
                    | Message::Settings(_)
                    | Message::QuickConnect(_, _)
                    | Message::KnownHosts(_, _)
                    | Message::SshKeys(_, _)
                    | Message::Vault(..)
                    | Message::ToggleBroadcast(_)
                    | Message::ToggleCommandTimestamps(_)
                    | Message::MultiExec(..)
                    | Message::ZoomPane(_, _)
                    | Message::ClosePane(_, _)
                    | Message::FocusPane(_, _)
                    | Message::CloseTab(_, _)
                    | Message::JournalCapture(_)
                    | Message::JournalDock(
                        _,
                        journal_dock::Message::Place(_) | journal_dock::Message::Toggle
                    )
                    | Message::TerminalEffects(_)
                    | Message::SetTerminalEffect(..)
                    | Message::TabContext(_, _)
                    | Message::Dashboard(
                        _,
                        dashboard::Message::Perform(_, dashboard::Action::Reconnect)
                    )
                    | Message::TerminalContext(_, terminal_context::Message::Invoke(..))
                    | Message::Palette(..)
                    | Message::Quit
            );
        let counts_changed = self.connections.is_some()
            && (changes_layout
                || matches!(
                    &message,
                    Message::CloseTab(..)
                        | Message::Window(_, window::Event::CloseRequested)
                        | Message::Transfer { .. }
                        | Message::TabDrag(_)
                        | Message::Project(..)
                        | Message::SessionRestore(_)
                        | Message::Control(_)
                ));
        let update_journals = !self.journal_captures.is_empty()
            && (changes_layout
                || matches!(
                    &message,
                    Message::ReconnectHistoryReady(..)
                        | Message::Ssh(_)
                        | Message::Serial(..)
                        | Message::ConnectIbm(_)
                        | Message::ClosePane(..)
                        | Message::JournalCapture(_)
                )
                || matches!(
                    &message,
                    Message::Control(control_access::Message::Request(request))
                        if request.method == kortty_control::Method::Close
                )
                || matches!(&message, Message::SessionUpdate(id) if self.sessions.get(id)
                    .is_some_and(|pane| pane.frame.status != Status::Running)));
        let sync_journal_docks = sync_remote
            || update_journals
            || matches!(
                &message,
                Message::JournalDock(
                    _,
                    journal_dock::Message::Place(_)
                        | journal_dock::Message::Toggle
                        | journal_dock::Message::Bound(..)
                ) | Message::Window(_, window::Event::CloseRequested | window::Event::Closed)
                    | Message::Control(_)
            );
        let task = self.update_message(message);
        let power = self.reconcile_sleep_prevention();
        let task = if power.units() > 0 {
            Task::batch([task, power])
        } else {
            task
        };
        self.refresh_quick_select();
        let task = if update_journals && !self.exiting && !self.scheduler_draining {
            Task::batch([task, self.reconcile_journal_sources()])
        } else {
            task
        };
        let task = if sync_remote {
            Task::batch([task, self.sync_remote_docks()])
        } else {
            task
        };
        let task = if sync_journal_docks {
            Task::batch([task, self.sync_journal_docks()])
        } else {
            task
        };
        let task = match self.reconcile_reopen_prompt() {
            Some(resume) => Task::batch([task, resume]),
            None => task,
        };
        let reconnects = self.advance_project_reconnects();
        let task = if reconnects.units() > 0 {
            Task::batch([task, reconnects])
        } else {
            task
        };
        if changes_layout && !self.exiting && !self.scheduler_draining {
            self.session_restore.changed(false);
        }
        if counts_changed {
            self.refresh_connection_counts();
        }
        #[cfg(target_os = "macos")]
        if changes_native_menu
            || previous_tool_focus
                != (
                    self.native_focused_openpgp(),
                    self.native_focused_tool_tab(),
                )
        {
            self.sync_native_menus();
        }
        task
    }

    fn update_message(&mut self, message: Message) -> Task<Message> {
        if self.exiting && !matches!(message, Message::ExitReady) {
            return Task::none();
        }
        if self.scheduler_draining
            && !matches!(
                message,
                Message::ExitReady
                    | Message::JobFinished(_)
                    | Message::Chrome(chrome::Message::Power(_))
                    | Message::RuntimeTick
                    | Message::SaveLastWindow
                    | Message::DialogPlacementClosing(..)
                    | Message::MainPlacementClosing(..)
                    | Message::ExitPlacementChecked(_)
                    | Message::SessionRestore(session_restore::Message::Worker(_))
            )
        {
            return Task::none();
        }
        match message {
            Message::Chrome(message) => return self.update_chrome(message),
            Message::Palette(id, message) => return self.update_palette(id, message),
            Message::Dashboard(id, message) => return self.update_dashboard(id, message),
            Message::SnippetRun(id, message) => return self.update_snippet_run(id, message),
            Message::SnippetTransfer(id, token, event) => {
                return self.snippet_transfer_event(id, token, event);
            }
            Message::SnippetAnalysis(id, event) => return self.snippet_analysis_event(id, event),
            Message::QuickConnect(id, message) => return self.update_quick_connect(id, message),
            Message::KnownHosts(id, message) => return self.update_known_hosts(id, message),
            Message::SshKeys(id, message) => return self.update_ssh_keys(id, message),
            Message::Backup(id, message) => return self.update_backup(id, message),
            Message::OpenPgp(id, message) => return self.update_openpgp(id, message),
            Message::AutomaticBackup(message) => return self.update_automatic_backup(message),
            Message::Logfile(id, message) => return self.update_logfile(id, message),
            Message::SessionProcesses(id, message) => {
                return self.update_session_processes(id, message);
            }
            Message::SessionProcessFeed(feed) => return self.update_session_process_feed(feed),
            Message::SessionJournal(id, message) => {
                return self.update_session_journal(id, message);
            }
            Message::JournalCapture(message) => return self.update_journal_capture(message),
            Message::JournalDock(owner, message) => {
                return self.update_journal_dock(owner, message);
            }
            Message::JournalDockTick => return self.journal_dock_tick(),
            Message::JournalViewerTick => return self.journal_viewer_tick(),
            Message::JournalAppearance(target, event) => {
                return self.update_journal_appearance(target, event);
            }
            Message::JournalAppearanceSave => return self.save_journal_appearance_when_ready(),
            Message::JournalSystemTheme(theme) => self.set_journal_system_theme(theme),
            Message::TerminalEffects(message) => return self.update_terminal_effects(message),
            Message::TerminalEffectFrame(now) => self.effect_frame = Some(now),
            Message::SetTerminalEffect(owner, session, effect) => {
                if self.effect_target_current(owner, session) {
                    self.select_terminal_effect(session, effect);
                }
            }
            Message::TerminalEffectSpeed(owner, session, speed) => {
                if self.effect_target_current(owner, session) {
                    self.set_terminal_effect_speed(session, speed);
                }
            }
            Message::Settings(message) => return self.update_settings(message),
            Message::ShellIntegration(id, message) => {
                return self.update_shell_integration(id, message);
            }
            Message::TerminalContext(id, message) => {
                return self.update_terminal_context(id, message);
            }
            Message::TabContext(id, message) => return self.update_tab_context(id, message),
            Message::CloseAllDialog(id, message) => {
                return self.update_close_all_dialog(id, message);
            }
            Message::RecentlyClosed(message) => return self.update_recently_closed(message),
            Message::TerminalCommand(message) => return self.update_terminal_command(message),
            Message::TerminalLink(id, message) => return self.update_terminal_link(id, message),
            Message::Control(message) => return self.update_control(message),
            Message::SessionSecurity(message) => return self.update_session_security(message),
            Message::Window(id, window::Event::CloseRequested) => {
                self.palette.dismiss(id);
                self.dismiss_terminal_context(id);
                self.dismiss_tab_context(id);
                if let Some((dialog, _)) = self.owner_dialog(id) {
                    return window::gain_focus(dialog);
                }
                return self.request_close(id, SerialClose::Window);
            }
            Message::Window(id, event @ window::Event::Opened { .. }) => {
                let dialog = self.dialog_placement.observe(id, &event);
                let geometry = self.observe_window_geometry(id, event);
                let scale = window::scale_factor(id)
                    .map(move |scale| Message::Window(id, window::Event::Rescaled(scale)));
                return if self.windows.contains_key(&id) {
                    #[cfg(target_os = "macos")]
                    chrome::install_native_menus();
                    Task::batch([
                        geometry,
                        dialog,
                        scale,
                        tab_drag::DragState::register(id),
                        self.start_local_dock(id),
                        self.detect_transparent_surface(id),
                    ])
                } else {
                    Task::batch([geometry, dialog, scale])
                };
            }
            Message::Window(id, event @ (window::Event::Resized(_) | window::Event::Moved(_))) => {
                if matches!(event, window::Event::Resized(_)) {
                    self.chrome.dismiss_toolbar_overflow(id);
                }
                let dialog = self.dialog_placement.observe(id, &event);
                return Task::batch([self.observe_window_geometry(id, event), dialog]);
            }
            Message::WindowGeometryChecked(id, revision, windowed_scale, mode) => {
                return self.confirm_window_geometry(id, revision, windowed_scale, mode);
            }
            Message::DialogPlacementChecked(id, revision, placement) => {
                return self.dialog_placement.checked(id, revision, placement);
            }
            #[cfg(target_os = "linux")]
            Message::DialogOpeningRedraw(id) => return self.dialog_placement.opening_redraw(id),
            Message::DialogPlacementClosing(id, placement) => {
                return self.finish_dialog_close(id, placement);
            }
            Message::MainPlacementClosing(id, user, inspected) => {
                if !self.main_placement_closing.remove(&id) || !self.windows.contains_key(&id) {
                    return Task::none();
                }
                if let Some((mode, placement)) = inspected {
                    self.capture_final_main_placement(id, mode, placement);
                }
                return self.close_window_with_placement(id, user, true);
            }
            Message::ExitPlacementChecked(inspected) => {
                if !std::mem::take(&mut self.exit_placement_pending) {
                    return Task::none();
                }
                for (id, mode, placement) in inspected {
                    self.capture_final_main_placement(id, mode, placement);
                    self.dialog_placement.capture(id, placement);
                }
                return self.exit_with_placement();
            }
            Message::Window(id, window::Event::Closed) => self.dialog_placement.closed(id),
            Message::SaveLastWindow => return self.lifecycle.save_if_due(),
            Message::WindowPlacementPoll(id, request) => {
                return self.poll_restored_placement(id, request);
            }
            Message::TerminalOnlyInspected(id, request, mode) => {
                return self.terminal_only_inspected(id, request, mode);
            }
            Message::TerminalOnlyDeadline(id, request, mode) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.terminal_only.timed_out(request, mode);
                }
            }
            Message::Transparency(message) => return self.update_transparency(message),
            Message::Window(id, window::Event::Rescaled(scale)) => {
                if let Some(diagram) = self.diagrams.get_mut(&id) {
                    diagram.set_scale_factor(scale);
                }
                if let Some(window) = self.windows.get_mut(&id) {
                    window.scale_factor = scale;
                }
                return self
                    .dialog_placement
                    .observe(id, &window::Event::Rescaled(scale));
            }
            Message::Window(id, window::Event::Focused) => {
                self.dialog_placement.focused(id);
                self.snippet_window_focused(id);
                #[cfg(target_os = "macos")]
                {
                    self.native_focused_window = Some(id);
                }
                for (window_id, window) in &mut self.windows {
                    window.focused = id == *window_id;
                    if window.focused {
                        if let Some(tab) = window.tab_mut() {
                            tab.watch.seen();
                        }
                    } else {
                        window.commit_tab_cycle();
                    }
                }
                let modal = self
                    .owner_dialog(id)
                    .map_or_else(Task::none, |(dialog, _)| window::gain_focus(dialog));
                return Task::batch([
                    modal,
                    self.refresh_transparent_window_mode(id),
                    self.journal_manager_focused(id),
                ]);
            }
            Message::Window(id, window::Event::Unfocused) => {
                if let Some(surface) = self.snippets.get_mut(&id) {
                    surface.window.dismiss_tab_context();
                }
                self.ascii_art_modifiers(id, Modifiers::empty());
                self.logfile_modifiers(id, Modifiers::empty());
                self.journal_modifiers(id, Modifiers::empty());
                if self.connections_id == Some(id)
                    && let Some(manager) = &mut self.connections
                {
                    manager.set_modifiers(Modifiers::empty());
                    manager.dismiss_tree_context();
                }
                #[cfg(target_os = "macos")]
                if self.native_focused_window == Some(id) {
                    self.native_focused_window = None;
                }
                self.chrome.dismiss(id);
                self.palette.dismiss(id);
                self.dismiss_terminal_context(id);
                self.dismiss_tab_context(id);
                self.dismiss_journal_context_for_window(id);
                self.commit_tab_cycle(id);
                if let Some(window) = self.windows.get_mut(&id) {
                    window.focused = false;
                }
                return self.refresh_transparent_window_mode(id);
            }
            Message::Window(id, window::Event::FileDropped(path)) => {
                if let Some(dialog) = self.vault_unlock_modal(id) {
                    return window::gain_focus(dialog);
                }
                return self.drop_ascii_art_image(id, path);
            }
            Message::Window(_, _) | Message::RepeatedKey(..) | Message::Noop => {}
            #[cfg(target_os = "macos")]
            Message::DockReopen(visible) => return self.dock_reopen(visible),
            #[cfg(target_os = "macos")]
            Message::NativeMenu(key) => return self.native_menu(key),
            #[cfg(target_os = "macos")]
            Message::NativeRecentlyClosed(entry) => return self.native_recently_closed(entry),
            #[cfg(target_os = "macos")]
            Message::NativeOpenRecent(entry) => return self.native_open_recent(entry),
            #[cfg(target_os = "macos")]
            Message::NativeOpenRecentShowing => return self.refresh_open_recent(),
            Message::NewWindow => return self.open_window(true).1,
            Message::OpenDiagram => return self.open_diagram(None),
            Message::AsciiArt(id, message) => return self.update_ascii_art(id, message),
            Message::Diagram(id, diagram_window::Message::Close) => return self.close_window(id),
            Message::Diagram(id, diagram_window::Message::CopySource) => {
                if let Some(diagram) = self.diagrams.get(&id) {
                    return self.write_clipboard(diagram.source());
                }
            }
            Message::Diagram(id, diagram_window::Message::CopyMermaid) => {
                if let Some(mermaid) = self.diagrams.get(&id).and_then(DiagramWindow::mermaid) {
                    return self.write_clipboard(mermaid);
                }
            }
            Message::Diagram(id, message) => {
                if let Some(diagram) = self.diagrams.get_mut(&id) {
                    return diagram
                        .update(message)
                        .map(move |message| Message::Diagram(id, message));
                }
            }
            Message::OpenEditor => return self.open_native_editor(None),
            Message::TerminalFileLoaded(ticket, result) => {
                return self.finish_terminal_file(ticket, result);
            }
            Message::LocalDock(id, message) => return self.update_local_dock(id, message),
            Message::RemoteDock(id, message) => {
                if let remote_dock::Message::Modifiers(modifiers) = &message {
                    self.ascii_art_modifiers(id, *modifiers);
                    self.logfile_modifiers(id, *modifiers);
                    self.journal_modifiers(id, *modifiers);
                    if self.connections_id == Some(id)
                        && let Some(manager) = &mut self.connections
                    {
                        manager.set_modifiers(*modifiers);
                    }
                }
                return self.update_remote_dock(id, message);
            }
            Message::RemoteDockTick => return self.remote_dock_tick(),
            Message::Recording(message) => return self.update_recording(message),
            Message::AiManager(message) => return self.update_ai_manager(message),
            Message::Editor(id, editor_window::Message::RequestClose) => {
                return self.close_window(id);
            }
            Message::Editor(_, editor_window::Message::RequestQuit) => return self.quit(),
            Message::Editor(id, message) => {
                let io_finished = matches!(
                    message,
                    editor_window::Message::Opened(_) | editor_window::Message::Saved(_)
                );
                let resume_quit = self.quit_waiting_for == Some(id);
                if let Some(editor) = self.editors.get_mut(&id) {
                    let canceled = matches!(message, editor_window::Message::DecideDiscard(false))
                        && editor.close_decision_available();
                    let task = editor
                        .update(message)
                        .map(move |message| Message::Editor(id, message));
                    if canceled && resume_quit {
                        self.quit_waiting_for = None;
                        window_lifecycle::reply_to_native_quit(false);
                    }
                    if editor.may_close() {
                        let close = self.close_window(id);
                        return if resume_quit {
                            Task::batch([task, close, self.quit()])
                        } else {
                            Task::batch([task, close])
                        };
                    }
                    if resume_quit && io_finished {
                        return Task::batch([task, self.quit()]);
                    }
                    return task;
                }
            }
            Message::NewTab(id) => {
                if let Some(window) = self.windows.get(&id) {
                    self.new_tab(id, window.local_profile);
                }
            }
            Message::NewWindowsShell(id, shell) => {
                if cfg!(windows)
                    && !shell.is_default()
                    && let Some(window) = self.windows.get(&id)
                {
                    let profile = window.local_profile;
                    match self.local_settings_for_shell(shell) {
                        Ok(settings) => {
                            if let Some(session) =
                                self.start_session_with_policy(id, profile, Some(settings))
                            {
                                self.add_tab(id, session);
                            }
                        }
                        Err(error) => {
                            self.notice(id, format!("Could not select the shell ({error})"))
                        }
                    }
                }
            }
            Message::ShowIbm(id, kind, replace) => {
                if !self.allow_in_process_transport(id, "IBM") {
                    return Task::none();
                }
                if let Some(window) = self.windows.get_mut(&id) {
                    window.cancel_paste();
                    window.pending_serial = None;
                    window.pending_ssh = None;
                    window.pending_ibm = Some(PendingIbm {
                        kind,
                        replace,
                        reconnect: None,
                        font: None,
                        address: String::new(),
                        device_name: String::new(),
                        plaintext: false,
                    });
                }
            }
            Message::IbmAddress(id, address) => {
                if address.len() <= 128
                    && let Some(dialog) = self
                        .windows
                        .get_mut(&id)
                        .and_then(|window| window.pending_ibm.as_mut())
                    && dialog.can_edit_route()
                {
                    dialog.address = address;
                    dialog.plaintext = false;
                }
            }
            Message::IbmDevice(id, kind) => {
                if let Some(dialog) = self
                    .windows
                    .get_mut(&id)
                    .and_then(|window| window.pending_ibm.as_mut())
                    && dialog.can_edit_route()
                    && dialog.kind.same_protocol(kind)
                    && dialog.kind != kind
                {
                    dialog.kind = kind;
                    dialog.plaintext = false;
                }
            }
            Message::IbmDeviceName(id, name) => {
                if name.len() <= 128
                    && let Some(dialog) = self
                        .windows
                        .get_mut(&id)
                        .and_then(|window| window.pending_ibm.as_mut())
                    && dialog.can_edit_route()
                    && dialog.kind.model_5250().is_some()
                {
                    dialog.device_name = name;
                    dialog.plaintext = false;
                }
            }
            Message::IbmPlaintext(id, allowed) => {
                if let Some(dialog) = self
                    .windows
                    .get_mut(&id)
                    .and_then(|window| window.pending_ibm.as_mut())
                {
                    dialog.plaintext = allowed;
                }
            }
            Message::ConnectIbm(id) => return self.connect_ibm(id),
            Message::CancelIbm(id) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.pending_ibm = None;
                }
                return self.finish_reopen_ibm(id, recently_closed::SshOutcome::Canceled);
            }
            Message::ShowSerial(id, replace) => return self.show_serial(id, replace),
            Message::Serial(id, serial_dialog::Message::Connect) => return self.connect_serial(id),
            Message::Serial(id, serial_dialog::Message::Reconnect(token)) => {
                return self.begin_serial_reconnect(id, token);
            }
            Message::SerialReconnectReleased(id, token) => {
                return self.resume_serial_reconnect(id, token);
            }
            Message::Serial(id, serial_dialog::Message::Cancel) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.pending_serial = None;
                    window.pending_ssh = None;
                }
            }
            Message::Serial(id, message) => {
                if let Some(pending) = self
                    .windows
                    .get_mut(&id)
                    .and_then(|window| window.pending_serial.as_mut())
                    && pending.accepts_form_input()
                {
                    return pending
                        .form
                        .update(message)
                        .map(move |message| Message::Serial(id, message));
                }
            }
            Message::SerialBreak(window, id) => self.send_serial_break(window, id),
            Message::ConfirmSerialClose(id, request, confirmed) => {
                let pending = self
                    .windows
                    .get_mut(&id)
                    .filter(|window| {
                        window
                            .pending_serial_close
                            .is_some_and(|(pending, _)| pending == request)
                    })
                    .and_then(|window| window.pending_serial_close.take());
                let Some((_, action)) = pending else {
                    return Task::none();
                };
                let close = self.finish_close_all_confirmation(id, request);
                if confirmed {
                    return Task::batch([close, self.apply_close(id, action)]);
                }
                if matches!(action, SerialClose::Quit) {
                    window_lifecycle::reply_to_native_quit(false);
                }
                if let SerialClose::TabBatch(batch) = action {
                    self.cancel_tab_batch_close(id, batch);
                }
                return close;
            }
            Message::SelectTab(id, tab_id) => {
                self.dismiss_tab_context(id);
                self.select_tab(id, tab_id, false);
            }
            Message::CommitTabCycle(id) => self.commit_tab_cycle(id),
            Message::CloseTab(id, tab_id) => {
                return self.request_close(id, SerialClose::Tab(tab_id));
            }
            Message::ToggleTransfer(id) => {
                if self.tab_transfer_window_available(id)
                    && let Some(window) = self.windows.get_mut(&id)
                    && window.active_entry().is_some()
                {
                    window.transfer_open = !window.transfer_open;
                }
            }
            Message::Transfer { from, to, tab } => {
                if self.tab_transfer_window_available(from)
                    && to.is_none_or(|id| self.tab_transfer_window_available(id))
                    && self.windows.get(&from).and_then(Window::active_tab_id) == Some(tab)
                {
                    return self.transfer(from, to, tab);
                }
            }
            Message::TabDrag(message) => return self.update_tab_drag(message),
            Message::Split(id, axis) => return self.split(id, axis),
            Message::CancelSplitConnection(id, session) => {
                self.cancel_split_connection(id, session)
            }
            Message::FocusPane(id, pane) => {
                if let Some(tab) = self.active_tab_mut(id)
                    && tab.grid.get(pane).is_some()
                {
                    tab.focused = pane;
                    tab.remote_files.release_keyboard();
                }
            }
            Message::ClosePane(id, pane) => {
                if let Some(tab) = self.windows.get(&id).and_then(Window::tab)
                    && let Some(&session) = tab.grid.get(pane)
                {
                    return self.request_close(
                        id,
                        SerialClose::Pane {
                            tab: tab.id,
                            pane,
                            session,
                        },
                    );
                }
            }
            Message::ZoomPane(id, pane) => {
                if let Some(tab) = self.active_tab_mut(id) {
                    if tab.grid.maximized() == Some(pane) {
                        tab.grid.restore();
                    } else if tab.grid.get(pane).is_some() {
                        tab.grid.maximize(pane);
                        tab.focused = pane;
                    }
                }
            }
            Message::ResizeSplit(id, resize) => {
                if let Some(tab) = self.active_tab_mut(id) {
                    tab.grid.resize(resize.split, resize.ratio);
                }
            }
            Message::DragPane(id, pane_grid::DragEvent::Dropped { pane, target }) => {
                if let Some(tab) = self.active_tab_mut(id) {
                    tab.grid.drop(pane, target);
                    tab.focused = pane;
                }
            }
            Message::DragPane(_, _) => {}
            Message::MultiExec(owner, action) => self.update_multi_exec(owner, action),
            Message::ToggleCommandTimestamps(id) => return self.toggle_command_timestamps(id),
            Message::ResizeCommandTimestamps(id, size) => {
                if let Some(window) = self.windows.get_mut(&id)
                    && let Some(delta) = window.timestamp_width_delta.take()
                    && let Some(size) = size
                    && delta != 0
                    && !window.terminal_only.needs_native_mode()
                {
                    return window::resize(
                        id,
                        Size::new((size.width + delta as f32).max(640.0), size.height),
                    );
                }
            }
            Message::ToggleBroadcast(id) => {
                if self.broadcast_menu_enabled(id)
                    && let Some(window) = self.windows.get_mut(&id)
                    && let Some(tab) = window.tab_mut()
                {
                    tab.broadcast = !tab.broadcast;
                    let sessions: Vec<_> = tab.grid.iter().map(|(_, id)| *id).collect();
                    window.cancel_paste();
                    for session in sessions {
                        self.sync_mirror_typeahead(session);
                    }
                }
            }
            Message::PaneResized(id, grid) => {
                if let Some(pane) = self.sessions.get_mut(&id)
                    && pane.grid != grid
                {
                    pane.grid = grid;
                    pane.selection = None;
                    pane.selected_output = None;
                    pane.session.resize(grid);
                }
            }
            Message::ToggleTypeAhead(id) => {
                if !self.mirror_blocks_typeahead(id)
                    && let Some(pane) = self.sessions.get_mut(&id)
                    && pane.supports_typeahead()
                {
                    pane.typeahead_enabled = !pane.typeahead_enabled;
                    pane.session.set_typeahead_enabled(pane.typeahead_enabled);
                }
            }
            Message::SessionUpdate(id) => {
                let mut bells = 0;
                #[cfg(target_os = "macos")]
                let mut refresh_session_menu = false;
                if let Some(pane) = self.sessions.get_mut(&id) {
                    bells = pane.session.take_bell_events();
                    let next = pane.session.frame();
                    if pane.frame.status != next.status {
                        let event = match next.status {
                            Status::Running => Some(crate::app_logging::Event::SessionStarted),
                            Status::Exited(_) | Status::Disconnected => {
                                Some(crate::app_logging::Event::SessionStopped)
                            }
                            Status::Error(_) => Some(crate::app_logging::Event::OperationFailed),
                            Status::Starting => None,
                        };
                        if let Some(event) = event {
                            self.app_logging.record(event);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    {
                        refresh_session_menu = (pane.frame.status == Status::Running)
                            != (next.status == Status::Running)
                            || (!self.journal_captures.is_empty()
                                && (pane.frame.screen.alternate_screen
                                    != next.screen.alternate_screen
                                    || (pane.frame.scroll_offset == 0)
                                        != (next.scroll_offset == 0)));
                    }
                    if pane.selection.is_some() && pane.frame.screen.cells != next.screen.cells {
                        pane.selection = None;
                    }
                    if pane
                        .selected_output
                        .as_ref()
                        .is_some_and(|output| output.stale(&next))
                    {
                        pane.selected_output = None;
                    }
                    if let Some(search) = &mut pane.search {
                        search.sync(&pane.session, &next);
                    }
                    if let Some(effect) = &mut pane.effect {
                        effect.update_flash(&pane.frame, &next);
                    }
                    pane.frame = next;
                }
                let reconnect = self.finish_reconnect_worker(id);
                self.finish_split_connection(id);
                self.control_finish_split(id);
                #[cfg(target_os = "macos")]
                if refresh_session_menu {
                    self.sync_native_menus();
                }
                let sidebar = self.remote_dock_session_updated(id);
                return Task::batch([
                    reconnect,
                    sidebar,
                    self.play_bell(id, bells),
                    self.clipboard_events(id),
                ]);
            }
            Message::TabWatchTick => return self.poll_tab_watches(std::time::Instant::now()),
            Message::ReconnectHistoryReady(id, ready) => {
                return self.finish_reconnect_history(id, ready);
            }
            Message::AudibleBell(enabled) => self.bell.enabled = enabled,
            Message::ShellTabTitles(id, enabled) => self.change_shell_title_visibility(id, enabled),
            Message::PromptNavigation(id, enabled) => self.change_prompt_navigation(id, enabled),
            Message::Selection(id, selection) => {
                if let Some(pane) = self.sessions.get_mut(&id) {
                    pane.selection = selection;
                    pane.selected_output = None;
                }
            }
            Message::SelectionFinished(owner, id, selection, frame) => {
                return self.finish_terminal_selection(owner, id, selection, &frame);
            }
            Message::OpenSearch(id) => {
                if let Some(pane) = self.sessions.get_mut(&id)
                    && pane.session.supports_search()
                {
                    return pane
                        .search
                        .get_or_insert_with(TerminalSearch::new)
                        .focus()
                        .map(move |message| Message::Search(id, message));
                }
            }
            Message::Search(id, message) => {
                if let Some(pane) = self.sessions.get_mut(&id)
                    && let Some(search) = &mut pane.search
                    && search.update(message, &pane.session)
                {
                    pane.search = None;
                }
            }
            Message::Scroll(id, lines) => {
                if let Some(pane) = self.sessions.get_mut(&id) {
                    pane.selection = None;
                    pane.session.scroll(lines);
                }
            }
            Message::ScrollTo(id, anchor) => {
                if let Some(pane) = self.sessions.get_mut(&id) {
                    pane.selection = None;
                    pane.session.scroll_to(anchor);
                }
            }
            Message::TerminalMouse(id, bytes) => {
                if let Some(pane) = self.sessions.get(&id) {
                    let _ = pane.session.send(bytes);
                }
            }
            Message::Key(id, key, location, modifiers, text) => {
                if self.native_terminal_effects() == Some(id)
                    && (key == Key::Named(Named::Escape)
                        || (modifiers.command()
                            && matches!(&key, Key::Character(value) if value.eq_ignore_ascii_case("w"))))
                {
                    return self.close_terminal_effects(id).unwrap_or_else(Task::none);
                }
                if key == Key::Named(Named::Escape) && self.tab_drag.cancel() {
                    return Task::none();
                }
                return self.key(id, key, location, modifiers, text.as_deref());
            }
            Message::Clipboard(message) => return self.update_clipboard(message),
            Message::OpenConnections(id) => return self.show_connections(id),
            Message::ConnectionOverview(id, event) => {
                return self.update_connection_overview(id, event);
            }
            Message::OsBadgeManager(id, event) => return self.update_os_badge_manager(id, event),
            Message::ConnectionUsed(revision, store) => {
                return self.connection_use_finished(revision, store);
            }
            Message::Connections(message) => return self.update_connections(message),
            Message::OpenVault(id) => return self.show_vault(id),
            Message::Vault(id, message) => return self.update_vault(id, message),
            Message::Ssh(message) => return self.update_ssh(message),
            Message::Tunnel(message) => return self.update_tunnel(message),
            Message::Sftp(id, message) => return self.update_sftp(id, message),
            Message::OpenScheduler(id) => return self.show_scheduler(id),
            Message::Scheduler(id, message) => return self.update_scheduler(id, message),
            Message::OpenSnippets(id) => return self.show_snippets(id),
            Message::Snippets(id, message) => return self.update_snippets(id, message),
            Message::SnippetRecovery(event) => return self.update_snippet_recovery(event),
            Message::SnippetStructureTick => {
                return self.poll_snippet_structures(std::time::Instant::now());
            }
            Message::SnippetComparison(id, message) => {
                return self.update_snippet_comparison(id, message);
            }
            Message::JobFinished(outcome) => return self.job_finished(outcome),
            Message::RuntimeTick => {
                self.prune_retired_journal_captures();
                self.app_logging.refresh_problem();
                self.poll_snippet_recovery(std::time::Instant::now());
                return self.runtime_tick();
            }
            Message::Copy(id) => {
                if let Some(session) = self
                    .windows
                    .get(&id)
                    .and_then(Window::tab)
                    .and_then(Tab::focused_session)
                {
                    return self.copy_terminal_selection(session);
                }
            }
            Message::Paste(id) => {
                if let Some(tab) = self.windows.get(&id).and_then(Window::tab) {
                    let tab_id = tab.id;
                    let targets: Vec<_> = tab.focused_session().into_iter().collect();
                    if self.current_targets(id, tab_id, &targets) {
                        let request = self.allocate();
                        if let Some(window) = self.windows.get_mut(&id) {
                            window.cancel_paste();
                            window.paste_request = Some(request);
                        }
                        return self.read_clipboard().map(move |text| {
                            Message::PasteLoaded(id, request, tab_id, targets.clone(), text)
                        });
                    }
                }
            }
            Message::PasteLoaded(id, request, tab, targets, text) => {
                let Some(window) = self.windows.get_mut(&id) else {
                    return Task::none();
                };
                if window.paste_request != Some(request) {
                    return Task::none();
                }
                window.paste_request = None;
                if let Some(raw) = text
                    && self.current_targets(id, tab, &targets)
                    && let [target] = targets.as_slice()
                {
                    if raw.len() > usize::from(paste_protection::MAX_PASTE_KIB) * 1024 {
                        self.notice(id, "Clipboard text exceeds 64 KiB".into());
                    } else {
                        let text = sanitize_paste(&raw);
                        if text.contains(['\n', '\t'])
                            && targets.iter().any(|id| {
                                self.sessions
                                    .get(id)
                                    .is_some_and(|pane| pane.frame.ibm.is_some())
                            })
                        {
                            self.notice(
                                id,
                                "IBM panes accept a single line of clipboard text without tabs"
                                    .into(),
                            );
                        } else if !text.is_empty() {
                            let mode = self.paste_warning_for(*target);
                            match paste_protection::decide(
                                &raw,
                                mode,
                                self.window_settings.paste_large_warning_kib,
                                self.all_paste_targets_bracketed(&targets),
                            ) {
                                paste_protection::Decision::Confirm(warning) => {
                                    if let Some(window) = self.windows.get_mut(&id) {
                                        window.pending_paste = Some(PendingPaste {
                                            request,
                                            tab,
                                            targets,
                                            text,
                                            preview: paste_protection::preview(&raw),
                                            warning,
                                            mode,
                                            raw_bytes: raw.len(),
                                        });
                                    }
                                }
                                decision => self.send_paste(
                                    id,
                                    &targets,
                                    &text,
                                    decision == paste_protection::Decision::RequireBrackets,
                                ),
                            }
                        }
                    }
                }
            }
            Message::ConfirmPaste(id, request, send) => {
                if let Some(paste) = self
                    .windows
                    .get_mut(&id)
                    .filter(|window| {
                        window
                            .pending_paste
                            .as_ref()
                            .is_some_and(|paste| paste.request == request)
                    })
                    .and_then(|window| window.pending_paste.take())
                    && send
                    && self.current_targets(id, paste.tab, &paste.targets)
                    && let [target] = paste.targets.as_slice()
                {
                    if self.paste_warning_for(*target) > paste.mode {
                        self.notice(
                            id,
                            "Paste protection changed. Repeat paste to review the text.".into(),
                        );
                    } else {
                        self.send_paste(id, &paste.targets, &paste.text, false);
                    }
                }
            }
            Message::SaveLayout(id) => {
                let Some(layout) = self.capture_layout() else {
                    return Task::none();
                };
                if layout.windows.is_empty() {
                    self.notice(id, "There are no regular windows to save.".into());
                    return Task::none();
                }
                return Task::future(async move {
                    let notice = match profile::save(&layout) {
                        Ok(()) => "Workspace layout saved".into(),
                        Err(error) => format!("Could not save workspace ({error})"),
                    };
                    Message::Notice(id, notice)
                });
            }
            Message::RestoreLayout(id) => {
                return Task::future(async move {
                    Message::LayoutLoaded(id, profile::load().map_err(|error| error.to_string()))
                });
            }
            Message::LayoutLoaded(id, layout) => {
                if self.windows.contains_key(&id) && !self.exiting {
                    match layout.and_then(|layout| self.restore_layout(layout)) {
                        Ok(task) => return task,
                        Err(error) => {
                            self.notice(id, format!("Could not restore workspace ({error})"))
                        }
                    }
                }
            }
            Message::Project(id, message) => return self.update_project(id, message),
            Message::OpenRecent(message) => return self.update_open_recent(message),
            Message::SessionRestore(message) => return self.update_session_restore(message),
            Message::Notice(id, notice) => self.notice(id, notice),
            Message::Quit => return self.quit(),
            Message::ExitReady => {
                if window_lifecycle::reply_to_native_quit(true) {
                    return Task::none();
                }
                return iced::exit();
            }
        }
        Task::none()
    }

    fn allocate(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn open_policy_window(&mut self) -> (window::Id, Task<Message>) {
        let (id, task) = self.open_window(self.clipboard.policy.problem.is_none());
        if let Some(problem) = self.clipboard.policy.problem.as_ref() {
            self.notice(
                id,
                format!("Managed policy could not be applied. Sessions are locked ({problem})"),
            );
        }
        (id, task)
    }

    fn open_diagram(&mut self, script: Option<(&str, bool)>) -> Task<Message> {
        let size = Size::new(1050.0, 660.0);
        let (id, opened) = self.open_placed_dialog(
            dialog_placement::Kind::Diagram,
            window::Settings {
                size,
                min_size: Some(Size::new(600.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let ai_allowed = self.ai_features_enabled();
        let (mut diagram, render_locally) = match script {
            Some((source, render)) => {
                (DiagramWindow::with_script(size, ai_allowed, source), render)
            }
            None => (DiagramWindow::new(size, ai_allowed), false),
        };
        diagram.configure_ai(ai_allowed, self.ai_configuration());
        let render = if render_locally {
            diagram
                .update(diagram_window::Message::Render)
                .map(move |message| Message::Diagram(id, message))
        } else {
            Task::none()
        };
        self.diagrams.insert(id, diagram);
        Task::batch([opened.map(|_| Message::Noop), render])
    }

    fn open_window(&mut self, with_tab: bool) -> (window::Id, Task<Message>) {
        let (placement, maximized, dashboard) =
            self.lifecycle.startup(self.window_settings.startup);
        let (id, opened) = self.open_window_at_placement(placement);
        if with_tab {
            self.new_tab(id, TerminalProfile::Xterm);
        }
        let opened = if maximized {
            opened.chain(window::maximize(id, true))
        } else {
            opened
        };
        let dashboard = dashboard && self.clipboard.policy.problem.is_none();
        (
            id,
            Task::batch([opened, self.set_dashboard_visible(id, dashboard)]),
        )
    }

    fn open_window_at_placement(
        &mut self,
        placement: WindowPlacement,
    ) -> (window::Id, Task<Message>) {
        let mut settings = window_lifecycle::main_window_settings(placement);
        let transparency =
            transparency::WindowAlpha::new(self.window_settings.terminal_transparency);
        transparency.configure(&mut settings);
        let size = settings.size;
        let (id, task) = window::open(settings);
        self.app_logging
            .record(crate::app_logging::Event::WindowOpened);
        let number = self.allocate();
        self.windows.insert(
            id,
            Window {
                number,
                size,
                geometry: window_lifecycle::Geometry::new(size, placement.position),
                transparency,
                local_files: local_dock::Browser::default(),
                journal_dock: journal_dock::Dock::new(self.window_settings.journal_dock),
                remote_modifiers: Modifiers::default(),
                dashboard: dashboard::State::default(),
                scale_factor: 1.0,
                tabs: Vec::new(),
                tab_mru: Vec::new(),
                tab_cycle: window_navigation::TabCycle::default(),
                terminal_only: window_navigation::TerminalOnly::default(),
                timestamp_width_delta: None,
                active: 0,
                local_profile: TerminalProfile::Xterm,
                focused: false,
                transfer_open: false,
                paste_request: None,
                pending_paste: None,
                pending_ibm: None,
                pending_serial: None,
                pending_ssh: None,
                pending_split_connection: None,
                pending_tunnels: None,
                pending_snippet_run: None,
                pending_serial_close: None,
                projects: None,
                clipboard_settings: false,
                notice: None,
            },
        );
        (
            id,
            task.then(move |id| window_lifecycle::restore_native_position(id, placement.position)),
        )
    }

    fn capture_layout(&self) -> Option<WorkspaceLayout> {
        self.capture_layout_ordered(None)
    }

    fn capture_layout_ordered(&self, first: Option<window::Id>) -> Option<WorkspaceLayout> {
        let mut windows: Vec<_> = self.windows.iter().collect();
        windows.sort_by_key(|(id, window)| (Some(**id) != first, window.number));
        let layouts = windows
            .into_iter()
            .filter(|(id, window)| {
                if window.terminal_tabs().next().is_none() {
                    first.is_none() || Some(**id) == first
                } else {
                    window.terminal_tabs().any(|tab| !tab.incognito)
                }
            })
            .map(|(_, window)| {
                let mut tab_terminal_effects = std::collections::BTreeMap::new();
                let tabs = window
                    .terminal_tabs()
                    .filter(|tab| !tab.incognito)
                    .enumerate()
                    .map(|(index, tab)| {
                        let mut panes = Vec::new();
                        let layout = self.capture_pane(tab, tab.grid.layout(), &mut panes)?;
                        let effects: profile::PaneEffects = panes
                            .into_iter()
                            .enumerate()
                            .filter_map(|(pane, effect)| effect.map(|effect| (pane, effect)))
                            .collect();
                        if !effects.is_empty() {
                            tab_terminal_effects.insert(index, effects);
                        }
                        Some(layout)
                    })
                    .collect::<Option<Vec<_>>>()?;
                let tab_titles = window
                    .terminal_tabs()
                    .filter(|tab| !tab.incognito)
                    .enumerate()
                    .filter_map(|(index, tab)| tab.custom_title.clone().map(|title| (index, title)))
                    .collect();
                let tab_groups: std::collections::BTreeMap<_, _> = window
                    .terminal_tabs()
                    .filter(|tab| !tab.incognito)
                    .enumerate()
                    .filter_map(|(index, tab)| tab.group.clone().map(|group| (index, group)))
                    .collect();
                let has_saved_connections = window
                    .terminal_tabs()
                    .filter(|tab| !tab.incognito)
                    .any(|tab| {
                        tab.grid
                            .get(tab.primary)
                            .and_then(|session| self.sessions.get(session))
                            .and_then(|pane| pane.ssh.as_ref())
                            .and_then(SshTarget::saved_id)
                            .is_some()
                    });
                let tab_primary_panes = window
                    .terminal_tabs()
                    .filter(|tab| !tab.incognito)
                    .enumerate()
                    .filter_map(|(index, tab)| {
                        tab.primary_pane_index()
                            .filter(|&primary| primary != 0)
                            .map(|primary| (index, primary))
                    })
                    .collect();
                let active = window
                    .tabs
                    .iter()
                    .take(window.active.saturating_add(1))
                    .filter_map(TabEntry::terminal)
                    .filter(|tab| !tab.incognito)
                    .count()
                    .saturating_sub(1);
                let placement = window.placement();
                Some(WindowLayout {
                    width: placement.width,
                    height: placement.height,
                    position: placement.position,
                    maximized: window.maximized(),
                    dashboard_visible: Some(window.dashboard.visible()),
                    active,
                    tabs,
                    tab_titles,
                    tab_groups: (!tab_groups.is_empty() || has_saved_connections)
                        .then_some(tab_groups),
                    tab_primary_panes,
                    tab_terminal_effects,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(WorkspaceLayout::new(layouts))
    }

    fn capture_pane(
        &self,
        tab: &Tab,
        node: &pane_grid::Node,
        effects: &mut Vec<Option<crate::terminal_effects::Selection>>,
    ) -> Option<PaneLayout> {
        match node {
            pane_grid::Node::Pane(pane) => {
                let session = self.sessions.get(tab.grid.get(*pane)?)?;
                effects.push(session.effect.map(|active| active.selection));
                if let Some(ibm) = &session.frame.ibm {
                    return Some(PaneLayout::Ibm {
                        font: session.font,
                        focused: *pane == tab.focused,
                        kind: ibm.kind,
                    });
                }
                let profile = session.frame.screen.profile;
                if let Some(target) = &session.ssh {
                    return Some(PaneLayout::Ssh {
                        font: session.font,
                        focused: *pane == tab.focused,
                        term: profile.term_name().into(),
                        target: target.clone(),
                    });
                }
                if let Some(connection) = &session.frame.serial {
                    return Some(PaneLayout::Serial {
                        font: session.font,
                        focused: *pane == tab.focused,
                        term: profile.term_name().into(),
                        connection: connection.as_ref().clone(),
                    });
                }
                Some(PaneLayout::Terminal {
                    font: session.font,
                    focused: *pane == tab.focused,
                    vt100: profile == TerminalProfile::Vt100,
                    term: (!matches!(profile, TerminalProfile::Vt100 | TerminalProfile::Xterm))
                        .then(|| profile.term_name().into()),
                    security: session
                        .local_security
                        .as_ref()
                        .filter(|settings| {
                            settings.isolation != IsolationLevel::None
                                || !settings.shell.is_default()
                        })
                        .cloned()
                        .map(|mut settings| {
                            if settings.isolation != IsolationLevel::Sandbox {
                                settings.directory.clear();
                            }
                            settings
                        }),
                })
            }
            pane_grid::Node::Split {
                axis, ratio, a, b, ..
            } => Some(PaneLayout::Split {
                vertical: *axis == pane_grid::Axis::Vertical,
                ratio: *ratio,
                first: Box::new(self.capture_pane(tab, a, effects)?),
                second: Box::new(self.capture_pane(tab, b, effects)?),
            }),
        }
    }

    fn restore_layout(&mut self, layout: WorkspaceLayout) -> Result<Task<Message>, String> {
        self.restore_layout_into(&layout, None)
            .map(|(_, task)| task)
    }

    fn restore_layout_into(
        &mut self,
        layout: &WorkspaceLayout,
        reuse: Option<window::Id>,
    ) -> Result<(window::Id, Task<Message>), String> {
        self.restore_layout_into_with_tools(layout, reuse, &[])
            .map(|(windows, task)| (windows[0], task))
    }

    fn restore_layout_into_with_tools(
        &mut self,
        layout: &WorkspaceLayout,
        mut reuse: Option<window::Id>,
        approved: &[tool_tabs::HostedTab],
    ) -> Result<(Vec<window::Id>, Task<Message>), String> {
        layout.validate().map_err(|error| error.to_string())?;
        if reuse.is_some_and(|id| self.hosted_tools_in(id) != approved) {
            return Err("The window's tool tabs changed. Open the project again.".into());
        }
        let mut created = Vec::new();
        let connections = OnceCell::new();
        // No window, including a reused empty window, changes until every pane is staged.
        let staged = layout
            .windows
            .iter()
            .map(|saved| {
                saved
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(index, pane)| {
                        let primary = saved.tab_primary_panes.get(&index).copied().unwrap_or(0);
                        let effects = saved.tab_terminal_effects.get(&index);
                        let mut tab = self.stage_restored_tab(
                            pane,
                            primary,
                            effects,
                            &connections,
                            &mut created,
                        )?;
                        tab.custom_title = saved.tab_titles.get(&index).cloned();
                        tab.group = saved.tab_groups.as_ref().map_or_else(
                            || tab.connection_group_baseline.clone(),
                            |groups| groups.get(&index).cloned(),
                        );
                        Ok(tab)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>();
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                for session in created {
                    self.stop_session(session);
                }
                return Err(error);
            }
        };
        let mut tasks = Vec::with_capacity(staged.len());
        let mut windows = Vec::with_capacity(staged.len());
        for (saved, tabs) in layout.windows.iter().zip(staged) {
            let destination = reuse.take();
            if let Some(id) = destination {
                for &tool in approved {
                    tasks.push(self.remove_hosted_tool(tool));
                }
                self.release_restored_window_tabs(id);
            }
            let (id, task) = self.open_restored_window(saved, destination);
            windows.push(id);
            tasks.push(task);
            let window = self.windows.get_mut(&id).expect("restored window exists");
            window.tabs = tabs.into_iter().map(TabEntry::from).collect();
            window.active = saved.active;
            window.arrange_tab_groups();
            window.remember_active_tab();
            if let Some(visible) = saved.dashboard_visible {
                tasks.push(self.set_dashboard_visible(id, visible));
            }
        }
        Ok((windows, Task::batch(tasks)))
    }

    fn release_restored_window_tabs(&mut self, id: window::Id) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        window.cancel_paste();
        window.active = 0;
        window.tab_mru.clear();
        window.tab_cycle.clear();
        for entry in std::mem::take(&mut window.tabs) {
            let TabEntry::Terminal(mut tab) = entry else {
                continue;
            };
            self.remove_tab_journal(tab.id);
            self.dispose_remote_dock(&mut tab);
            self.clipboard.forget_tab(tab.id);
            for (_, &session) in tab.grid.iter() {
                self.stop_session(session);
            }
        }
    }

    fn stage_restored_tab(
        &mut self,
        pane: &PaneLayout,
        primary: usize,
        effects: Option<&profile::PaneEffects>,
        connections: &OnceCell<Option<Store>>,
        created: &mut Vec<SessionId>,
    ) -> Result<Tab, String> {
        let first = created.len();
        let mut focused_session = None;
        let configuration = self.restore_pane(pane, created, &mut focused_session)?;
        let grid = pane_grid::State::with_configuration(configuration);
        let focused = grid
            .iter()
            .find_map(|(pane, session)| (Some(*session) == focused_session).then_some(*pane))
            .ok_or_else(|| "Saved tab has no focused pane".to_owned())?;
        let primary_session = created[first + primary];
        let primary = grid
            .iter()
            .find_map(|(pane, session)| (*session == primary_session).then_some(*pane))
            .ok_or_else(|| "Saved tab has no primary pane".to_owned())?;
        for (&index, &selection) in effects.into_iter().flatten() {
            let session = created[first + index];
            self.sessions
                .get_mut(&session)
                .expect("staged pane exists")
                .effect = Some(effects::Active::from_saved(selection));
            self.sync_pane_appearance(session);
        }
        let saved_connection = self.sessions[&primary_session]
            .ssh
            .as_ref()
            .and_then(|target| {
                let id = target.saved_id()?;
                let store = connections
                    .get_or_init(|| {
                        profile::connections_path()
                            .ok()
                            .and_then(|path| Store::open(path).ok())
                    })
                    .as_ref()?;
                let connection = store
                    .get(id)
                    .filter(|connection| target.matches(connection))?;
                Some((connection, store.group_of(id)))
            });
        let command_timestamps = saved_connection
            .and_then(|(connection, _)| connection.command_timestamps)
            .unwrap_or(self.window_settings.command_timestamps_enabled);
        let connection_group_baseline =
            saved_connection.and_then(|(_, group)| profile::normalize_tab_group(group?));
        Ok(Tab {
            id: self.allocate(),
            custom_title: None,
            group: None,
            connection_group_baseline,
            watch: Default::default(),
            reopen_origin: self
                .sessions
                .get(&primary_session)
                .and_then(recently_closed::Origin::capture),
            grid,
            remote_files: remote_dock::Dock::default(),
            primary,
            focused,
            broadcast: false,
            command_timestamps,
            incognito: false,
            tunnel_owner: Some(primary_session),
        })
    }

    fn restore_pane(
        &mut self,
        pane: &PaneLayout,
        created: &mut Vec<SessionId>,
        active: &mut Option<SessionId>,
    ) -> Result<pane_grid::Configuration<SessionId>, String> {
        let (id, focused) = match pane {
            PaneLayout::Terminal {
                font,
                focused,
                vt100,
                term,
                security,
            } => {
                if self.clipboard.policy.problem.is_some() {
                    return Err("Managed policy could not be applied. Sessions are locked.".into());
                }
                let profile = profile::local_profile(term.as_deref(), *vt100)
                    .ok_or_else(|| "Saved local terminal profile is unsupported".to_owned())?;
                let id = self
                    .store_local_session(profile, Some(*font), security.clone())
                    .map_err(|error| format!("Could not start a terminal worker ({error})"))?;
                (id, *focused)
            }
            PaneLayout::Ibm {
                font,
                focused,
                kind,
            } => {
                let id = self.store_session(
                    Session::disconnected_ibm(*kind),
                    GridSize::default(),
                    *font,
                );
                (id, *focused)
            }
            PaneLayout::Serial {
                font,
                focused,
                term,
                connection,
            } => {
                let profile = TerminalProfile::from_term_name(term)
                    .ok_or_else(|| "Saved serial terminal profile is unsupported".to_owned())?;
                let id = self
                    .store_disconnected_serial(connection.clone(), profile, *font)
                    .map_err(|error| format!("Could not restore the serial pane ({error})"))?;
                (id, *focused)
            }
            PaneLayout::Ssh {
                font,
                focused,
                term,
                target,
            } => {
                let profile = TerminalProfile::from_term_name(term)
                    .ok_or_else(|| "Saved SSH terminal profile is unsupported".to_owned())?;
                let id = self
                    .store_disconnected_ssh(target.clone(), profile, *font)
                    .map_err(|error| format!("Could not restore the SSH pane ({error})"))?;
                (id, *focused)
            }
            PaneLayout::Split {
                vertical,
                ratio,
                first,
                second,
            } => {
                return Ok(pane_grid::Configuration::Split {
                    axis: if *vertical {
                        pane_grid::Axis::Vertical
                    } else {
                        pane_grid::Axis::Horizontal
                    },
                    ratio: *ratio,
                    a: Box::new(self.restore_pane(first, created, active)?),
                    b: Box::new(self.restore_pane(second, created, active)?),
                });
            }
        };
        created.push(id);
        if focused {
            *active = Some(id);
        }
        Ok(pane_grid::Configuration::Pane(id))
    }

    fn start_session(&mut self, id: window::Id, profile: TerminalProfile) -> Option<SessionId> {
        self.start_session_with_policy(id, profile, None)
    }

    fn start_session_with_policy(
        &mut self,
        id: window::Id,
        profile: TerminalProfile,
        saved: Option<LocalSettings>,
    ) -> Option<SessionId> {
        if self.clipboard.policy.problem.is_some() {
            self.app_logging
                .record(crate::app_logging::Event::PolicyRejected);
            self.notice(
                id,
                "Managed policy could not be applied. Sessions are locked.".into(),
            );
            return None;
        }
        match self.store_local_session(profile, None, saved) {
            Ok(id) => Some(id),
            Err(error) => {
                self.notice(id, format!("Could not start a terminal worker ({error})"));
                None
            }
        }
    }

    fn store_local_session(
        &mut self,
        profile: TerminalProfile,
        font: Option<u8>,
        saved: Option<LocalSettings>,
    ) -> std::io::Result<SessionId> {
        self.store_local_session_in_grid(profile, font, saved, initial_grid(profile), false)
    }

    fn store_local_session_in_grid(
        &mut self,
        profile: TerminalProfile,
        font: Option<u8>,
        saved: Option<LocalSettings>,
        grid: GridSize,
        strict: bool,
    ) -> std::io::Result<SessionId> {
        let settings = self
            .security
            .local_settings(saved, self.clipboard.policy.minimum_isolation)?;
        let mut options = settings.launch_options()?;
        options.shell_integration = settings.shell_integration
            && settings.isolation != IsolationLevel::Sandbox
            && self.window_settings.prompt_navigation_enabled
            && shell_integration::supports_marks(profile);
        let session = Session::start_local_with_strict(
            grid,
            profile,
            options,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
            strict,
        )?;
        let id = self.store_session(
            session,
            grid,
            font.unwrap_or(self.window_settings.terminal_font),
        );
        let pane = self.sessions.get_mut(&id).expect("stored local session");
        pane.inherits_font_size = font.is_none();
        pane.strict_escapes = strict || settings.isolation == IsolationLevel::Sandbox;
        pane.local_security = Some(settings);
        Ok(id)
    }

    fn store_session(&mut self, session: Session, grid: GridSize, font: u8) -> SessionId {
        let session_id = SessionId(self.allocate());
        session.set_scrollback_compression_enabled(
            self.window_settings.scrollback_compression_enabled,
        );
        session.set_clipboard_paste_allowed(
            self.clipboard.read_allowed() && self.clipboard.policy.paste_events_allowed,
        );
        self.sessions.insert(
            session_id,
            PaneSession {
                frame: session.frame(),
                session,
                grid,
                font,
                font_baseline: font,
                inherits_font_size: false,
                selection: None,
                selected_output: None,
                pending_split_connection: None,
                search: None,
                typeahead_enabled: false,
                strict_escapes: false,
                local_security: None,
                ssh: None,
                ibm_origin: None,
                credential_environment_source: None,
                forwarding: None,
                effect: None,
            },
        );
        session_id
    }

    fn store_connected_session(
        &mut self,
        session: Session,
        grid: GridSize,
        replace: Option<SessionId>,
        font: Option<u8>,
    ) -> SessionId {
        let default = font.unwrap_or(self.window_settings.terminal_font);
        let (font, font_baseline, inherits_font_size, effect) = replace
            .and_then(|id| self.sessions.get(&id))
            .map_or((default, default, font.is_none(), None), |pane| {
                (
                    pane.font,
                    pane.font_baseline,
                    pane.inherits_font_size,
                    pane.effect,
                )
            });
        let id = self.store_session(session, grid, font);
        let pane = self.sessions.get_mut(&id).expect("stored pane");
        pane.font_baseline = font_baseline;
        pane.inherits_font_size = inherits_font_size;
        pane.effect = effect;
        if effect.is_some() {
            self.sync_pane_appearance(id);
        }
        id
    }

    fn new_tab(&mut self, id: window::Id, profile: TerminalProfile) {
        if !self.windows.contains_key(&id) {
            return;
        }
        let Some(session) = self.start_session(id, profile) else {
            return;
        };
        self.add_tab(id, session);
    }

    fn add_tab(&mut self, id: window::Id, session: SessionId) {
        self.add_tab_with_privacy(id, session, false);
    }

    fn add_tab_with_privacy(&mut self, id: window::Id, session: SessionId, incognito: bool) {
        self.add_configured_tab(id, session, incognito, None, None);
    }

    fn add_configured_tab(
        &mut self,
        id: window::Id,
        session: SessionId,
        incognito: bool,
        command_timestamps: Option<bool>,
        connection_group: Option<String>,
    ) {
        let tab_id = self.allocate();
        let origin = (!incognito)
            .then(|| {
                self.sessions
                    .get(&session)
                    .and_then(recently_closed::Origin::capture)
            })
            .flatten();
        let (grid, focused) = pane_grid::State::new(session);
        if let Some(window) = self.windows.get_mut(&id) {
            window.tabs.push(
                Tab {
                    id: tab_id,
                    custom_title: None,
                    group: connection_group.clone(),
                    connection_group_baseline: connection_group,
                    watch: Default::default(),
                    reopen_origin: origin,
                    grid,
                    remote_files: remote_dock::Dock::default(),
                    primary: focused,
                    focused,
                    broadcast: false,
                    command_timestamps: command_timestamps
                        .unwrap_or(self.window_settings.command_timestamps_enabled),
                    incognito,
                    tunnel_owner: Some(session),
                }
                .into(),
            );
            window.active = window.tabs.len() - 1;
            window.arrange_tab_groups();
            window.remember_active_tab();
            window.cancel_paste();
            window.pending_ibm = None;
            window.pending_serial = None;
            window
                .pending_ssh
                .take_if(|pending| pending.reopen_token().is_none());
            window
                .pending_serial_close
                .take_if(|(_, action)| !matches!(action, SerialClose::TabBatch(_)));
            window.transfer_open = false;
            self.session_restore.changed(false);
        }
    }

    fn show_serial(&mut self, window: window::Id, replace: Option<SessionId>) -> Task<Message> {
        if !self.allow_in_process_transport(window, "Serial connections") {
            return Task::none();
        }
        if !self.clipboard.policy.serial_allowed {
            self.notice(
                window,
                "Your organization's policy does not allow serial connections.".into(),
            );
            return Task::none();
        }
        let Some(state) = self.windows.get(&window) else {
            return Task::none();
        };
        let (connection, profile) = if let Some(id) = replace {
            let Some((_, _, previous)) = self.disconnected_serial_pane(window, id) else {
                return Task::none();
            };
            let Some(connection) = previous.frame.serial.as_deref() else {
                return Task::none();
            };
            (connection.clone(), previous.frame.screen.profile)
        } else {
            (SerialConnection::default(), state.local_profile)
        };
        self.show_serial_form(window, connection, profile, replace, None)
    }

    fn show_serial_form(
        &mut self,
        window: window::Id,
        connection: SerialConnection,
        profile: TerminalProfile,
        replace: Option<SessionId>,
        reconnect: Option<(reconnect::ReconnectTarget, bool)>,
    ) -> Task<Message> {
        let id = self.allocate();
        let Some(state) = self.windows.get_mut(&window) else {
            return Task::none();
        };
        let mut form = SerialDialog::new(id, connection, profile);
        let scan = form.refresh();
        state.cancel_paste();
        state.pending_ibm = None;
        state.pending_ssh = None;
        state.notice = None;
        state.pending_serial = Some(PendingSerial {
            form,
            replace,
            reconnect: reconnect.map(|(target, disconnect)| {
                reconnect::transport::SerialReconnect::new(target, id, disconnect)
            }),
            split: None,
            font: None,
        });
        scan.map(move |message| Message::Serial(window, message))
    }

    fn disconnected_serial_pane(
        &self,
        window: window::Id,
        id: SessionId,
    ) -> Option<(usize, pane_grid::Pane, &PaneSession)> {
        let (tab, pane) = self.windows.get(&window)?.pane_location(id)?;
        let previous = self.sessions.get(&id)?;
        let frame = previous.session.frame();
        (frame.serial.is_some()
            && matches!(
                frame.status,
                Status::Disconnected | Status::Exited(_) | Status::Error(_)
            ))
        .then_some((tab, pane, previous))
    }

    fn connect_serial(&mut self, window: window::Id) -> Task<Message> {
        if !self.allow_in_process_transport(window, "Serial connections") {
            return Task::none();
        }
        if !self.clipboard.policy.serial_allowed {
            return Task::none();
        }
        let Some(pending) = self
            .windows
            .get(&window)
            .and_then(|window| window.pending_serial.as_ref())
        else {
            return Task::none();
        };
        if pending.reconnect.is_some() {
            return Task::none();
        }
        let Ok((connection, profile)) = pending.form.validated() else {
            return Task::none();
        };
        let font = pending.font;
        let split = pending.split;
        if let Some(target) = split
            && let Err(reason) = self.split_connection_privacy(window, target, false)
        {
            if let Some(state) = self.windows.get_mut(&window) {
                state.pending_serial = None;
            }
            self.notice(window, reason.into());
            return Task::none();
        }
        let replacement = if let Some(old) = pending.replace {
            let Some((tab, pane, _)) = self.disconnected_serial_pane(window, old) else {
                return Task::none();
            };
            Some((tab, pane, old))
        } else {
            None
        };
        let grid = initial_grid(profile);
        let strict = replacement
            .and_then(|(_, _, old)| self.sessions.get(&old))
            .is_some_and(|pane| pane.strict_escapes);
        let session = match Session::start_serial_with_strict(
            connection,
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
            strict,
        ) {
            Ok(session) => session,
            Err(error) => {
                self.notice(
                    window,
                    format!("Could not start the serial worker ({error})"),
                );
                return Task::none();
            }
        };
        if let Some(state) = self.windows.get_mut(&window) {
            state.pending_serial = None;
        }
        let new_id =
            self.store_connected_session(session, grid, replacement.map(|(_, _, old)| old), font);
        if let Some(pane) = self.sessions.get_mut(&new_id) {
            pane.strict_escapes = strict;
        }
        if let Some((tab, pane, old)) = replacement {
            if let Some(slot) = self
                .windows
                .get_mut(&window)
                .and_then(|window| window.tabs.get_mut(tab))
                .and_then(TabEntry::terminal_mut)
                .and_then(|tab| tab.grid.get_mut(pane))
            {
                *slot = new_id;
                self.stop_session(old);
            } else {
                self.stop_session(new_id);
            }
        } else if let Some(target) = split {
            self.start_split_worker(window, target, new_id);
        } else {
            self.add_tab(window, new_id);
        }
        Task::none()
    }

    fn store_disconnected_serial(
        &mut self,
        connection: SerialConnection,
        profile: TerminalProfile,
        font: u8,
    ) -> std::io::Result<SessionId> {
        let grid = initial_grid(profile);
        let session = Session::disconnected_serial(
            connection,
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
        )?;
        Ok(self.store_session(session, grid, font))
    }

    fn send_serial_break(&mut self, window: window::Id, id: SessionId) {
        if self
            .windows
            .get(&window)
            .and_then(|window| window.pane_location(id))
            .is_some()
            && let Some(pane) = self.sessions.get(&id)
            && let Err(error) = pane.session.send_serial_break()
        {
            let reason = match error {
                InputError::Busy => "the input queue is full",
                InputError::Closed => "the serial port is closed",
                InputError::TooLarge => "the request exceeds the input limit",
            };
            self.notice(window, format!("Could not send Break ({reason})"));
        }
    }

    fn connect_ibm(&mut self, window: window::Id) -> Task<Message> {
        if !self.allow_in_process_transport(window, "IBM") {
            return Task::none();
        }
        let Some(dialog) = self
            .windows
            .get(&window)
            .and_then(|state| state.pending_ibm.as_ref())
        else {
            return Task::none();
        };
        let Some((address, plaintext, device_name)) = dialog.connection() else {
            return Task::none();
        };
        let kind = dialog.kind;
        let font = dialog.font;
        let reconnect = dialog.reconnect.as_ref().map(|reconnect| reconnect.target);
        if let Some(target) = reconnect
            && self.reconnect_target(window, target).is_none()
        {
            if let Some(state) = self.windows.get_mut(&window) {
                state.pending_ibm = None;
            }
            return Task::none();
        }
        let ibm_origin = recently_closed::IbmOrigin {
            kind,
            address: Some(address),
            device_name: dialog.device_name.trim().to_owned(),
        };
        let replacement = if let Some(old) = dialog.replace {
            let Some((tab_index, pane)) = self.windows[&window].pane_location(old) else {
                return Task::none();
            };
            if !self.sessions.get(&old).is_some_and(|pane| {
                pane.frame
                    .ibm
                    .as_ref()
                    .is_some_and(|ibm| ibm.kind.same_protocol(kind))
                    && matches!(
                        pane.frame.status,
                        Status::Disconnected | Status::Exited(_) | Status::Error(_)
                    )
            }) {
                return Task::none();
            };
            Some((tab_index, pane, old))
        } else {
            None
        };
        if let Some(state) = self.windows.get_mut(&window) {
            state.pending_ibm = None;
        }
        let session = match Session::start_ibm(address, kind, plaintext, device_name) {
            Ok(session) => session,
            Err(error) => {
                self.notice(window, format!("Could not start IBM worker ({error})"));
                return self.finish_reopen_ibm(window, recently_closed::SshOutcome::Unavailable);
            }
        };
        let session = self.store_connected_session(
            session,
            GridSize::default(),
            replacement.map(|(_, _, old)| old),
            font,
        );
        if let Some(pane) = self.sessions.get_mut(&session) {
            pane.ibm_origin = Some(ibm_origin.clone());
        }
        if let Some(target) = reconnect {
            return self.start_reconnect_worker(window, target, session, None, Default::default());
        } else if let Some((index, pane, old)) = replacement {
            if let Some(tab) = self
                .windows
                .get_mut(&window)
                .and_then(|state| state.tabs.get_mut(index))
                .and_then(TabEntry::terminal_mut)
                && let Some(slot) = tab.grid.get_mut(pane)
            {
                *slot = session;
                tab.broadcast = false;
                if tab.tunnel_owner == Some(old)
                    && let Some(recently_closed::Origin::Ibm(origin, _)) = &mut tab.reopen_origin
                    && origin.address.is_none()
                {
                    *origin = ibm_origin;
                    tab.tunnel_owner = Some(session);
                }
                self.stop_session(old);
            } else {
                self.stop_session(session);
            }
        } else {
            self.add_tab(window, session);
            if let Some(tab) = self.windows.get(&window).and_then(Window::tab) {
                return self.finish_reopen_ibm(window, recently_closed::SshOutcome::Opened(tab.id));
            }
        }
        Task::none()
    }

    fn active_tab_mut(&mut self, id: window::Id) -> Option<&mut Tab> {
        self.windows.get_mut(&id)?.tab_mut()
    }

    fn split(&mut self, id: window::Id, axis: pane_grid::Axis) -> Task<Message> {
        let Some(source) = self
            .windows
            .get(&id)
            .and_then(Window::tab)
            .and_then(Tab::focused_session)
        else {
            return Task::none();
        };
        self.split_same_server(id, source, axis)
    }

    fn close_pane(&mut self, id: window::Id, pane: pane_grid::Pane) -> Task<Message> {
        let Some(tab) = self.active_tab_mut(id) else {
            return Task::none();
        };
        if tab.grid.len() == 1 {
            let tab_id = tab.id;
            self.close_tab(id, tab_id);
        } else if let Some(session) = tab.close_pane(pane) {
            return self.close_pane_tunnels(id, session);
        }
        Task::none()
    }

    fn detach_tab(&mut self, id: window::Id, tab_id: u64) -> Option<TabEntry> {
        self.dismiss_tab_context(id);
        self.dismiss_journal_context_for_window(id);
        self.cancel_split_for_tab(id, tab_id);
        self.defer_interrupted_project_reconnects(id, Some(tab_id));
        self.cancel_reconnect_for_tab(id, tab_id);
        self.cancel_transport_reconnect_for_tab(id, tab_id);
        let window = self.windows.get_mut(&id)?;
        let index = window.tabs.iter().position(|tab| tab.id() == tab_id)?;
        let tab = window.tabs.remove(index);
        window.tab_mru.retain(|&previous| previous != tab_id);
        window.normalize_active(index);
        Some(tab)
    }

    fn tab_transfer_window_available(&self, id: window::Id) -> bool {
        self.palette_can_open(id)
            && !self.palette.is_open(id)
            && self.hosted_close_prompt(id).is_none()
    }

    fn transfer(&mut self, from: window::Id, to: Option<window::Id>, tab_id: u64) -> Task<Message> {
        self.transfer_at(from, to, tab_id, None)
    }

    fn transfer_at(
        &mut self,
        from: window::Id,
        to: Option<window::Id>,
        tab_id: u64,
        insert: Option<usize>,
    ) -> Task<Message> {
        if to.is_some_and(|id| (id == from && insert.is_none()) || !self.windows.contains_key(&id))
        {
            return Task::none();
        }
        if self.hosted_close_blocks_selection(from, tab_id)
            || to.is_some_and(|id| self.hosted_close_blocks_selection(id, tab_id))
        {
            return Task::none();
        }
        if self
            .hosted_tab_at(from, tab_id)
            .is_some_and(|tool| self.hosted_close_locks(tool.instance))
        {
            return Task::none();
        }
        let Some(tab) = self.detach_tab(from, tab_id) else {
            return Task::none();
        };
        let (to, open) = match to {
            Some(id) => (id, window::gain_focus(id)),
            None => self.open_window(false),
        };
        if let Some(window) = self.windows.get_mut(&to) {
            let position = insert.unwrap_or(window.tabs.len()).min(window.tabs.len());
            window.tabs.insert(position, tab);
            window.active = position;
            window.arrange_tab_groups();
            window.remember_active_tab();
            window.cancel_paste();
            if matches!(window.pending_serial_close, Some((_, SerialClose::Window))) {
                window.pending_serial_close = None;
            }
            self.session_restore.changed(false);
        }
        open
    }

    fn close_user_tab(&mut self, owner: window::Id, tab: u64) {
        let closed = self
            .windows
            .get(&owner)
            .and_then(|window| window.terminal_tabs().find(|item| item.id == tab))
            .and_then(|tab| self.remember_tab(tab));
        if let Some(closed) = closed {
            self.remember_closed_tabs(false, vec![closed]);
        }
        self.close_tab(owner, tab);
    }

    fn close_tab(&mut self, id: window::Id, tab: u64) {
        if self.hosted_tab_at(id, tab).is_some() {
            return;
        }
        if let Some(TabEntry::Terminal(mut tab)) = self.detach_tab(id, tab) {
            self.remove_tab_journal(tab.id);
            self.dispose_remote_dock(&mut tab);
            self.clipboard.forget_tab(tab.id);
            for (_, session) in tab.grid.iter() {
                self.stop_session(*session);
            }
            let last_tab = !tab.incognito
                && self
                    .windows
                    .values()
                    .all(|window| window.terminal_tabs().all(|tab| tab.incognito));
            self.session_restore.changed(last_tab);
        }
    }

    fn open_serial(&self, session: SessionId) -> bool {
        self.sessions.get(&session).is_some_and(|pane| {
            let frame = pane.session.frame();
            frame.serial.is_some() && matches!(frame.status, Status::Starting | Status::Running)
        })
    }

    fn tab_has_open_serial(&self, tab: &Tab) -> bool {
        tab.grid
            .iter()
            .any(|(_, session)| self.open_serial(*session))
    }

    fn request_close(&mut self, id: window::Id, action: SerialClose) -> Task<Message> {
        let confirm = self.windows.get(&id).is_some_and(|window| match action {
            SerialClose::Window => window
                .terminal_tabs()
                .any(|tab| self.tab_has_open_serial(tab)),
            SerialClose::Tab(tab) => window
                .terminal_tabs()
                .find(|state| state.id == tab)
                .is_some_and(|tab| self.tab_has_open_serial(tab)),
            SerialClose::TabBatch(request) => {
                self.tab_batch_all_count(id, request).is_some()
                    || self.tab_batch_has_serial(id, request)
            }
            SerialClose::Pane { session, .. } => self.open_serial(session),
            SerialClose::Quit => false,
        });
        if confirm {
            let request = self.allocate();
            if let Some(window) = self.windows.get_mut(&id) {
                window.cancel_paste();
                window.pending_serial_close = Some((request, action));
            }
            return Task::none();
        }
        self.apply_close(id, action)
    }

    fn apply_close(&mut self, id: window::Id, action: SerialClose) -> Task<Message> {
        match action {
            SerialClose::Window => return self.close_window_inner(id, true),
            SerialClose::Tab(tab) => {
                if let Some(tool) = self.hosted_tab_at(id, tab) {
                    return self.request_hosted_tool_close(tool.instance);
                }
                self.close_user_tab(id, tab);
            }
            SerialClose::TabBatch(request) => {
                if let Some(task) = self.guard_hosted_batch_close(id, request) {
                    return task;
                }
                return self.apply_tab_batch_close(id, request);
            }
            SerialClose::Pane { tab, pane, session } => {
                if self
                    .windows
                    .get(&id)
                    .and_then(Window::tab)
                    .is_some_and(|state| state.id == tab && state.grid.get(pane) == Some(&session))
                {
                    if self.windows[&id]
                        .tab()
                        .is_some_and(|tab| tab.grid.len() == 1)
                    {
                        self.close_user_tab(id, tab);
                        return Task::none();
                    }
                    return self.close_pane(id, pane);
                }
            }
            SerialClose::Quit => return self.quit_confirmed(true),
        }
        Task::none()
    }

    fn close_window(&mut self, id: window::Id) -> Task<Message> {
        self.close_window_inner(id, false)
    }

    fn close_window_inner(&mut self, id: window::Id, user: bool) -> Task<Message> {
        self.close_window_with_placement(id, user, false)
    }

    fn close_window_with_placement(
        &mut self,
        id: window::Id,
        user: bool,
        inspected: bool,
    ) -> Task<Message> {
        if self.main_placement_closing.contains(&id) {
            return Task::none();
        }
        if let Some(close) = self.close_connection_import(id) {
            return close;
        }
        if let Some(close) = self.close_connection_export(id) {
            return close;
        }
        if let Some(close) = self.close_os_badge_manager(id) {
            return close;
        }
        if let Some(close) = self.close_manual(id) {
            return close;
        }
        if let Some(close) = self.close_journal_edit(id) {
            return close;
        }
        if let Some(close) = self.close_ascii_art(id) {
            return close;
        }
        if let Some(close) = self.close_terminal_effects(id) {
            return close;
        }
        if let Some(dialog) = self
            .openpgp_dialog_for_owner(id)
            .or_else(|| self.hosted_ssh_keys_vault_for_owner(id))
            .or_else(|| self.vault_unlock_for_owner(id))
        {
            return window::gain_focus(dialog);
        }
        if self.is_openpgp_standalone(id) {
            return self.request_openpgp_close(id);
        }
        if let Some(task) = self.guard_native_ai_manager_close(id) {
            return task;
        }
        if let Some(task) = self.guard_native_logfile_close(id) {
            return task;
        }
        if let Some(task) = self.guard_native_journal_close(id) {
            return task;
        }
        if let Some(task) = self.guard_hosted_window_close(id, user) {
            return task;
        }
        self.palette.dismiss(id);
        self.dismiss_terminal_context(id);
        if let Some(close) = self.close_rename_dialog(id) {
            return close;
        }
        if let Some(close) = self.close_tab_journal_note(id) {
            return close;
        }
        if let Some(close) = self.cancel_close_all_dialog(id) {
            return close;
        }
        if let Some(close) = self.cancel_remote_dialog(id) {
            return close;
        }
        if self.link_confirmation_title(id).is_some() {
            return self.update_terminal_link(id, terminal_link_dialog::Message::Decide(false));
        }
        if let Some(close) = self.close_shell_integration(id) {
            return close;
        }
        if let Some(close) = self.close_quick_connect(id) {
            return close;
        }
        if let Some(close) = self.close_known_hosts(id) {
            return close;
        }
        if let Some(close) = self.close_ssh_keys(id) {
            return close;
        }
        if let Some(close) = self.close_session_process_window(id) {
            return close;
        }
        if let Some(close) = self.close_backup(id) {
            return close;
        }
        if let Some(close) = self.close_settings(id) {
            return close;
        }
        if let Some(close) = self.close_environment(id) {
            return close;
        }
        if !inspected && self.needs_final_main_placement(id) {
            self.main_placement_closing.insert(id);
            return window_lifecycle::inspect_placement(id)
                .collect()
                .map(move |mut results| Message::MainPlacementClosing(id, user, results.pop()));
        }
        let chrome_dialog = self.chrome.window_closed(id);
        let settings_dialog = self.settings.owner_closed(id);
        if self.is_snippet_window(id) {
            return self.request_snippet_close(id);
        }
        if let Some(close) = self.close_snippet_comparison(id) {
            return close;
        }
        if self.is_profile_tool(id) {
            return self.close_profile_tool(id, false);
        }
        if self.connection_overview_title(id).is_some() {
            return self.close_connection_overview(id);
        }
        if let Some(state) = self.sftp.get_mut(&id) {
            if state.request_close() {
                return self.finish_sftp_close(id);
            }
            return Task::none();
        }
        if let Some(close) = self.request_scheduler_close(id) {
            return close;
        }
        if let Some(editor) = self.editors.get_mut(&id)
            && !editor.may_close()
            && !editor.request_close()
        {
            return Task::none();
        }
        self.editors.remove(&id);
        self.close_terminal_file_editor(id);
        self.diagrams.remove(&id);
        self.recording_windows.remove(&id);
        self.logfiles.remove(id);
        let journals = self.remove_closed_journal_windows(id);
        let processes = self.close_session_process_owner(id);
        let close_all_dialog = self.close_all_dialog_owner_closed(id);
        let remote_dialog = self.remote_dialog_owner_closed(id);
        if self.windows.contains_key(&id) {
            self.preserve_closing_session_window(id);
        }
        self.flush_journal_dock_resize(id);
        if let Some(window) = self.windows.remove(&id) {
            self.app_logging
                .record(crate::app_logging::Event::WindowClosed);
            self.cancel_reopen_owner(id);
            let remember = user && !self.should_exit_after_window_close();
            let clear_when_empty = remember
                && window.terminal_tabs().any(|tab| !tab.incognito)
                && self
                    .windows
                    .values()
                    .all(|window| window.terminal_tabs().all(|tab| tab.incognito));
            if remember {
                let tabs = window
                    .terminal_tabs()
                    .filter_map(|tab| self.remember_tab(tab))
                    .collect();
                self.remember_closed_tabs(true, tabs);
            }
            if let Some(pending) = window.pending_split_connection {
                self.stop_session(pending);
            }
            self.control.close_settings(id);
            self.security.close(id);
            self.tab_drag.forget(id);
            self.lifecycle
                .capture(&window, self.window_settings.startup, true);
            self.lifecycle.flush();
            for entry in window.tabs {
                let TabEntry::Terminal(mut tab) = entry else {
                    continue;
                };
                self.remove_tab_journal(tab.id);
                self.dispose_remote_dock(&mut tab);
                self.clipboard.forget_tab(tab.id);
                for (_, session) in tab.grid.iter() {
                    self.stop_session(*session);
                }
            }
            self.session_restore.changed(clear_when_empty);
        }
        let close = Task::batch([
            self.close_placed_dialog(id),
            chrome_dialog.map_or_else(Task::none, |id| self.close_placed_dialog(id)),
            settings_dialog.map_or_else(Task::none, |id| self.close_placed_dialog(id)),
            self.quick_connect_owner_closed(id),
            self.close_known_hosts_owner(id),
            self.close_ssh_keys_owner(id),
            self.close_backup_owner(id),
            self.close_shell_integration_owner(id),
            self.close_tab_context_owner(id),
            close_all_dialog,
            remote_dialog,
            settings_dialog.map_or_else(Task::none, |owner| {
                self.close_shell_integration_owner(owner)
            }),
            processes,
            journals,
        ]);
        if self.should_exit_after_window_close() {
            Task::batch([close, self.quit()])
        } else {
            close
        }
    }

    fn no_windows(&self) -> bool {
        self.windows.is_empty()
            && !self.manual_open()
            && !self.connection_import_open()
            && !self.connection_export_open()
            && self.diagrams.is_empty()
            && self.ascii_art.is_empty()
            && self.editors.is_empty()
            && self.sftp.is_empty()
            && self.recording_windows.is_empty()
            && !self.ai_manager_open()
            && self.logfiles.is_empty()
            && self.journals.is_empty()
            && self.session_processes.is_empty()
            && self.scheduler_owners.is_empty()
            && self.snippets.is_empty()
            && self.snippet_comparisons.is_empty()
            && self.connections_id.is_none()
            && self.vault_id.is_none()
            && self.known_hosts.is_empty()
            && !self.ssh_keys.is_open()
            && !self.backup.is_open()
            && self.openpgp.is_empty()
            && !self.tab_context.has_dialogs()
            && self.close_all_dialogs.is_empty()
            && self.remote_dialogs.is_empty()
    }

    fn stop_session(&mut self, id: SessionId) {
        self.cancel_split_source(id);
        self.stop_tunnel_predecessors(id);
        if let Some(pane) = self.take_session(id) {
            self.app_logging
                .record(crate::app_logging::Event::SessionStopped);
            self.track_session_shutdown(pane.session.stop());
        }
    }

    fn take_session(&mut self, id: SessionId) -> Option<PaneSession> {
        self.cancel_snippet_transfers_for_session(id);
        self.forget_project_ssh(id);
        self.multi_exec.forget(id);
        self.remote_dock_session_removed(id);
        self.stop_terminal_file_session(id);
        self.cancel_reconnect_source(id);
        self.cancel_transport_reconnect_for_source(id);
        self.clipboard.forget_session(id);
        for window in self.windows.values_mut() {
            if window.pending_tunnels == Some(id) {
                window.pending_tunnels = None;
            }
            if window
                .pending_ssh
                .as_ref()
                .is_some_and(|pending| pending.depends_on(id))
            {
                window.pending_ssh = None;
            }
        }
        self.sessions.remove(&id)
    }

    fn track_session_shutdown(&mut self, done: Receiver<()>) {
        self.pending_shutdown.retain(|pending| !pending.is_closed());
        self.pending_shutdown.push(done);
    }

    fn quit(&mut self) -> Task<Message> {
        self.quit_confirmed(false)
    }

    fn quit_confirmed(&mut self, serial_confirmed: bool) -> Task<Message> {
        if self.exiting || self.scheduler_draining || self.exit_placement_pending {
            return Task::none();
        }
        if let Some(dialog) = self
            .connection_import_saving()
            .or_else(|| self.connection_export_saving())
            .or_else(|| self.os_badge_manager_saving())
        {
            window_lifecycle::reply_to_native_quit(false);
            return window::gain_focus(dialog);
        }
        if let Some(id) = self
            .windows
            .keys()
            .chain(self.journals.keys())
            .find_map(|&owner| {
                self.journal_edit_for_owner(owner)
                    .or_else(|| self.tab_journal_note_for_owner(owner))
            })
        {
            window_lifecycle::reply_to_native_quit(false);
            return window::gain_focus(id);
        }
        if let Some((_, id)) = self.settings.logging_preparing() {
            window_lifecycle::reply_to_native_quit(false);
            return window::gain_focus(id);
        }
        if let Some(task) = self.request_openpgp_quit() {
            return task;
        }
        for (id, editor) in &mut self.editors {
            if !editor.may_close() && !editor.request_close() {
                self.quit_waiting_for = Some(*id);
                return window::gain_focus(*id);
            }
        }
        self.quit_waiting_for = None;
        if let Some(task) = self.request_ssh_keys_quit() {
            return task;
        }
        if let Some(task) = self.request_backup_quit() {
            return task;
        }
        if let Some(task) = self.request_profile_quit() {
            return task;
        }
        if let Some(task) = self.request_toolbar_quit() {
            return task;
        }
        if let Some((&id, state)) = self
            .sftp
            .iter_mut()
            .find(|(_, state)| state.transfer_running())
        {
            state.request_close();
            self.quit_sftp_waiting = Some(id);
            return window::gain_focus(id);
        }
        self.quit_sftp_waiting = None;
        if !serial_confirmed {
            let serial_window = self
                .windows
                .iter()
                .filter(|(_, window)| {
                    window
                        .terminal_tabs()
                        .any(|tab| self.tab_has_open_serial(tab))
                })
                .min_by_key(|(_, window)| window.number)
                .map(|(id, _)| *id);
            if let Some(id) = serial_window {
                let request = self.allocate();
                if let Some(window) = self.windows.get_mut(&id) {
                    window.cancel_paste();
                    window.pending_serial_close = Some((request, SerialClose::Quit));
                }
                return window::gain_focus(id);
            }
        }
        if let Some(task) = self.guard_hosted_quit(serial_confirmed) {
            return task;
        }
        for window in self.windows.values_mut() {
            window.pending_ssh = None;
        }
        self.sftp_logins.clear();
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.begin_drain();
            if scheduler.has_active_runs() {
                self.scheduler_draining = true;
                self.cancel_journal_screenshot_analyses();
                self.cancel_obsolete_journal_note_translations();
                return Task::none();
            }
        }
        self.exit_after_workers()
    }

    fn exit_after_workers(&mut self) -> Task<Message> {
        if self.exit_placement_pending {
            return Task::none();
        }
        let inspections: Vec<_> = self
            .windows
            .keys()
            .copied()
            .filter(|id| self.needs_final_main_placement(*id))
            .chain(self.dialog_placement.native_windows())
            .map(|id| {
                window_lifecycle::inspect_placement(id)
                    .map(move |(mode, placement)| (id, mode, placement))
            })
            .collect();
        if !inspections.is_empty() {
            self.exit_placement_pending = true;
            return Task::batch(inspections)
                .collect()
                .map(Message::ExitPlacementChecked);
        }
        self.exit_with_placement()
    }

    fn exit_with_placement(&mut self) -> Task<Message> {
        self.flush_last_journal_dock_resize();
        self.flush_snippet_structure_preferences();
        self.save_ascii_art_on_quit();
        self.exiting = true;
        self.cancel_journal_screenshot_analyses();
        self.cancel_obsolete_journal_note_translations();
        self.finish_all_journals();
        self.stop_all_terminal_files();
        self.drain_automatic_backup();
        self.stop_all_remote_docks();
        self.flush_transparency();
        self.flush_journal_appearance();
        self.vault = None;
        self.finish_session_restore();
        let control = self.control.detach();
        if let Some(window) = self
            .windows
            .values()
            .max_by_key(|window| (window.focused, window.number))
        {
            self.lifecycle
                .capture(window, self.window_settings.startup, true);
        }
        let last_window = self.lifecycle.unsaved();
        if let Some(done) = self.dialog_placement.finish() {
            self.pending_shutdown.push(done);
        }
        if let Some(writer) = self.snippet_session.take() {
            self.pending_shutdown.push(writer.finish());
        }
        for surface in self.snippets.values_mut() {
            if let Some(transfer) = &surface.transfer {
                transfer.cancel();
            }
            surface.cancel_analysis();
            surface.window.discard_owned_recovery();
        }
        self.flush_snippet_recovery();
        if let Some(done) = self.snippet_recovery.finish() {
            self.pending_shutdown.push(done);
        }
        for (_, pane) in self.sessions.drain() {
            self.pending_shutdown.push(pane.session.stop());
        }
        self.pending_shutdown.extend(
            self.closing_tunnel_owners
                .drain()
                .map(|(_, owner)| owner.stop()),
        );
        for (_, mut state) in self.sftp.drain() {
            if let Some(done) = state.stop() {
                self.pending_shutdown.push(done);
            }
        }
        let pending = std::mem::take(&mut self.pending_shutdown);
        let logging = self.app_logging.stop();
        let sleep_request = self.take_sleep_request();
        Task::future(async move {
            if let Some(server) = control {
                server.stop();
            }
            for done in pending {
                let _ = done.recv().await;
            }
            if let Some(window) = last_window {
                let _ = profile::save_last_window(window);
            }
            for done in logging {
                let _ = done.recv().await;
            }
            drop(sleep_request);
            Message::ExitReady
        })
    }

    fn focused_session(&self, window: window::Id) -> Option<&PaneSession> {
        let id = self.windows.get(&window)?.tab()?.focused_session()?;
        self.sessions.get(&id)
    }

    fn toggle_command_timestamps(&mut self, id: window::Id) -> Task<Message> {
        if self.terminal_only_fullscreen_active(id) || !self.palette_can_open(id) {
            return Task::none();
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let Some(tab) = window.tab_mut() else {
            return Task::none();
        };
        tab.command_timestamps = !tab.command_timestamps;
        let width = i32::from(terminal_view::TIMESTAMP_GUTTER_WIDTH);
        let delta = if tab.command_timestamps {
            width
        } else {
            -width
        };
        if let Some(pending) = &mut window.timestamp_width_delta {
            *pending += delta;
            return Task::none();
        }
        window.timestamp_width_delta = Some(delta);
        window_lifecycle::window_mode(id).then(move |mode| {
            if mode.normal_geometry {
                window::size(id).map(move |size| Message::ResizeCommandTimestamps(id, Some(size)))
            } else {
                Task::done(Message::ResizeCommandTimestamps(id, None))
            }
        })
    }

    fn current_targets(&self, id: window::Id, tab_id: u64, targets: &[SessionId]) -> bool {
        if self.owner_dialog(id).is_some() || self.reopen_busy(id) {
            return false;
        }
        self.windows
            .get(&id)
            .filter(|window| {
                window.pending_ibm.is_none()
                    && window.pending_serial.is_none()
                    && window.pending_ssh.is_none()
                    && window.pending_split_connection.is_none()
                    && window.pending_serial_close.is_none()
                    && !window.clipboard_settings
                    && self.window_clipboard_prompt(window).is_none()
            })
            .and_then(Window::tab)
            .is_some_and(|tab| {
                tab.id == tab_id
                    && tab
                        .focused_session()
                        .is_some_and(|source| targets == [source])
            })
    }

    fn tab_has_ibm(&self, tab: &Tab) -> bool {
        tab.grid.iter().any(|(_, id)| {
            self.sessions
                .get(id)
                .is_some_and(|pane| pane.frame.ibm.is_some())
        })
    }

    fn all_paste_targets_bracketed(&self, targets: &[SessionId]) -> bool {
        targets.iter().all(|id| {
            self.sessions.get(id).is_some_and(|pane| {
                let frame = pane.session.frame();
                frame.status == Status::Running && frame.ibm.is_none() && frame.bracketed_paste
            })
        })
    }

    fn send_paste(
        &mut self,
        window: window::Id,
        targets: &[SessionId],
        text: &str,
        require_brackets: bool,
    ) {
        let bracket_notice = "Bracketed paste changed. Repeat paste to review the text.";
        if require_brackets && !self.all_paste_targets_bracketed(targets) {
            self.notice(window, bracket_notice.into());
            return;
        }
        if let [target] = targets
            && let Some(pane) = self.sessions.get(target)
        {
            let frame = pane.session.frame();
            if frame.status == Status::Running
                && frame.ibm.is_none()
                && (!require_brackets || frame.bracketed_paste)
            {
                let fallback = encode_stream_paste(&frame.screen, text, frame.bracketed_paste);
                if pane.session.try_clipboard_paste(text, &fallback) {
                    if let Some(pane) = self.sessions.get_mut(target) {
                        pane.selection = None;
                        pane.session.follow_output();
                    }
                    return;
                }
            }
        }
        let mut lost_brackets = false;
        self.send_to(window, targets, InputSource::Paste, |_, frame| {
            if require_brackets && !frame.bracketed_paste {
                lost_brackets = true;
                return None;
            }
            if let Some(ibm) = &frame.ibm {
                (frame.status == Status::Running && !ibm.nvt_mode)
                    .then(|| PaneInput::Ibm(IbmInput::Text(text.to_owned())))
            } else {
                Some(PaneInput::Local(encode_stream_paste(
                    &frame.screen,
                    text,
                    frame.bracketed_paste,
                )))
            }
        });
        if lost_brackets {
            self.notice(window, bracket_notice.into());
        }
    }

    fn send_to(
        &mut self,
        window: window::Id,
        targets: &[SessionId],
        source: InputSource,
        mut encode: impl FnMut(SessionId, &Frame) -> Option<PaneInput>,
    ) {
        let mut failures = Vec::new();
        for target in targets {
            if let Some(pane) = self.sessions.get_mut(target) {
                let frame = pane.session.frame();
                if let Some(input) = encode(*target, &frame) {
                    let result = match input {
                        PaneInput::Local(bytes) => match source {
                            InputSource::Paste => pane.session.send_paste(bytes),
                            InputSource::Keyboard {
                                enter,
                                shifted_enter,
                                physical_pane: Some(physical),
                                ..
                            } if physical != *target => {
                                let kind = if shifted_enter {
                                    Hp2392Input::ShiftEnter
                                } else {
                                    Hp2392Input::Key { enter }
                                };
                                pane.session.send_interactive(bytes, kind, true)
                            }
                            InputSource::Keyboard {
                                enter: true,
                                predict,
                                shifted_enter,
                                physical_pane: Some(physical),
                            } if physical == *target
                                && (!predict
                                    || (shifted_enter
                                        && frame.screen.profile == TerminalProfile::Hp2392)) =>
                            {
                                pane.session.send_physical_enter(bytes, shifted_enter)
                            }
                            InputSource::Keyboard {
                                shifted_enter: true,
                                ..
                            } if frame.screen.profile == TerminalProfile::Hp2392 => {
                                pane.session.send_shifted_hp_enter(bytes)
                            }
                            InputSource::Keyboard {
                                enter,
                                predict: true,
                                ..
                            } => pane.session.send_keyboard(bytes, enter),
                            InputSource::Keyboard { enter, .. } => {
                                pane.session.send_user_input(bytes, enter)
                            }
                        },
                        PaneInput::Ibm(input) => pane.session.send_ibm(input),
                    };
                    match result {
                        Ok(()) => {
                            pane.selection = None;
                            pane.session.follow_output();
                        }
                        Err(error) => failures.push(error),
                    }
                }
            }
        }
        if let Some(failure) = failures.first() {
            let reason = match failure {
                InputError::Busy => "terminal input queue is full",
                InputError::Closed => "terminal session is closed",
                InputError::TooLarge => "terminal input exceeds its size limit",
            };
            self.notice(
                window,
                format!("Input skipped in {} pane(s) ({reason})", failures.len()),
            );
        }
    }

    fn key(
        &mut self,
        id: window::Id,
        key: Key,
        location: Location,
        modifiers: Modifiers,
        input: Option<&str>,
    ) -> Task<Message> {
        if self.exiting {
            return Task::none();
        }
        if let Some(dialog) = self.vault_unlock_modal(id) {
            return window::gain_focus(dialog);
        }
        if let Some(task) = self.manual_key(id, &key, modifiers) {
            return task;
        }
        if key == Key::Named(Named::Escape) && self.chrome.title(id).is_some() {
            return self.close_window(id);
        }
        if let Some(task) = self.ascii_art_key(id, &key, modifiers) {
            return task;
        }
        if (self.connection_import_title(id).is_some()
            || self.connection_export_title(id).is_some())
            && (key == Key::Named(Named::Escape)
                || (modifiers.command()
                    && !modifiers.alt()
                    && matches!(&key, Key::Character(character) if character.eq_ignore_ascii_case("w"))))
        {
            return self
                .close_connection_import(id)
                .or_else(|| self.close_connection_export(id))
                .unwrap_or_else(Task::none);
        }
        if key == Key::Named(Named::Escape)
            && let Some(task) = self.escape_logfile_window(id)
        {
            return task;
        }
        if let Some(dialog) = self
            .openpgp_dialog_for_owner(id)
            .or_else(|| self.hosted_ssh_keys_vault_for_owner(id))
        {
            return window::gain_focus(dialog);
        }
        if window_navigation::ends_tab_cycle(&key, modifiers) {
            self.commit_tab_cycle(id);
        }
        if self.capture_settings_shortcut(id, &key, modifiers) {
            return Task::none();
        }
        if key == Key::Named(Named::Escape) && self.ssh_keys_title(id).is_some() {
            return self.update_ssh_keys(id, ssh_keys_ui::Message::Cancel);
        }
        if key == Key::Named(Named::Escape) && self.backup_title(id).is_some() {
            return self.update_backup(id, backup_ui::Message::Cancel);
        }
        if key == Key::Named(Named::Escape) && self.is_openpgp_standalone(id) {
            return self.update_openpgp(id, openpgp_window::Message::Escape);
        }
        if self.remote_dialog_title(id).is_some() {
            let action = match key {
                Key::Named(Named::Escape) => Some(remote_dock::DialogMessage::Cancel),
                Key::Named(Named::Enter) if modifiers.is_empty() => {
                    Some(remote_dock::DialogMessage::Proceed)
                }
                _ => None,
            };
            return action.map_or_else(Task::none, |action| {
                self.update_remote_dock(id, remote_dock::Message::Dialog(action))
            });
        }
        if let Some(task) = self.palette_key_before_layout(id, &key, modifiers, input) {
            return task;
        }
        if let Some(task) = self.tab_context_key(id, &key) {
            return task;
        }
        if self.close_all_dialog_title(id).is_some()
            && key == Key::Named(Named::Enter)
            && modifiers.is_empty()
        {
            return self.update_close_all_dialog(id, close_all_dialog::Message::Decide(true));
        }
        if self.shell_integration_title(id).is_some() && key == Key::Named(Named::Enter) {
            return self.update_shell_integration(id, shell_integration::Message::Copy);
        }
        if key == Key::Named(Named::Escape)
            && let Some(dismiss) = self.dismiss_known_hosts(id)
        {
            return dismiss;
        }
        if (self.quick_connect_title(id).is_some()
            || self.chrome.title(id).is_some()
            || self.settings.title(id).is_some()
            || self.shell_integration_title(id).is_some()
            || self.environment_title(id).is_some()
            || self.rename_dialog_title(id).is_some()
            || self.tab_journal_note_title(id).is_some()
            || self.close_all_dialog_title(id).is_some()
            || self.is_session_process_window(id))
            && key == Key::Named(Named::Escape)
        {
            return self.close_window(id);
        }
        if self.link_confirmation_title(id).is_some() {
            return if matches!(key, Key::Named(Named::Escape)) {
                self.update_terminal_link(id, terminal_link_dialog::Message::Decide(false))
            } else {
                Task::none()
            };
        }
        if modifiers.command()
            && !modifiers.alt()
            && matches!(&key, Key::Character(character) if character.eq_ignore_ascii_case("f"))
            && let Some(focus) = self.focus_settings_search(id)
        {
            return focus;
        }
        if let Some(task) = self.ai_manager_shortcut(id, &key, modifiers) {
            return task;
        }
        if let Some(task) = self.snippet_shortcut(id, &key, modifiers) {
            return task;
        }
        let app_shortcut = modifiers.command()
            && !modifiers.alt()
            && (cfg!(target_os = "macos") || modifiers.shift());
        if app_shortcut && let Key::Character(character) = &key {
            if character.eq_ignore_ascii_case("q") {
                return self.update(Message::Quit);
            }
            if character.eq_ignore_ascii_case("w")
                && (self.editors.contains_key(&id)
                    || self.diagrams.contains_key(&id)
                    || self.chrome.title(id).is_some()
                    || self.ascii_art.contains_key(&id)
                    || self.recording_windows.contains_key(&id)
                    || self.logfiles.contains(id)
                    || self.is_session_process_window(id)
                    || self.is_journal_window(id)
                    || self.tab_journal_note_title(id).is_some()
                    || self.environment_title(id).is_some()
                    || self.sftp.contains_key(&id)
                    || self.is_profile_tool(id)
                    || self.connection_overview_title(id).is_some()
                    || self.os_badge_manager_title(id).is_some()
                    || self.known_hosts_title(id).is_some()
                    || self.ssh_keys_title(id).is_some()
                    || self.backup_title(id).is_some()
                    || self.is_openpgp_standalone(id)
                    || self.shell_integration_title(id).is_some()
                    || self.is_snippet_window(id)
                    || self.snippet_comparisons.contains_key(&id)
                    || self.is_scheduler_window(id))
            {
                return self.close_window(id);
            }
        }
        if self.sftp.contains_key(&id) && key == Key::Named(Named::Escape) {
            if self.sftp_logins.contains_key(&id) {
                return self.update_ssh(ssh_access::Message::Cancel(id));
            }
            return self.update_sftp(id, sftp_window::Message::Dismiss);
        }
        if self.is_scheduler_window(id) && key == Key::Named(Named::Escape) {
            return self.update_scheduler(id, scheduler_window::Message::Dismiss);
        }
        if self.is_snippet_window(id) && key == Key::Named(Named::Escape) {
            return self.dismiss_snippets(id);
        }
        if self.snippet_comparisons.contains_key(&id) && key == Key::Named(Named::Escape) {
            return self.close_window(id);
        }
        if self.is_journal_window(id) && key == Key::Named(Named::Escape) {
            if self.dismiss_journal_menu(id) {
                return Task::none();
            }
            return self.close_window(id);
        }
        if self.is_profile_tool(id) && key == Key::Named(Named::Escape) {
            return self.close_profile_tool(id, false);
        }
        if self.connection_overview_title(id).is_some() && key == Key::Named(Named::Escape) {
            return self.update_connection_overview(
                id,
                profile_tools::connection_overview::Event::Dismiss,
            );
        }
        if self.os_badge_manager_title(id).is_some() && key == Key::Named(Named::Escape) {
            return self.update_os_badge_manager(id, os_badge_manager::Event::Dismiss);
        }
        if let Some((dialog, _)) = self.owner_dialog(id) {
            return window::gain_focus(dialog);
        }
        if !self.windows.contains_key(&id) {
            return Task::none();
        }
        if let Some(run) = &self.windows[&id].pending_snippet_run {
            return if key == Key::Named(Named::Escape) {
                self.update_snippet_run(id, run.cancel_message())
            } else {
                Task::none()
            };
        }
        if self.control.settings == Some(id) {
            if key == Key::Named(Named::Escape) {
                self.control.close_settings(id);
            }
            return Task::none();
        }
        if let Some((request, _)) = self.windows[&id].pending_serial_close {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::ConfirmSerialClose(id, request, false));
            }
            return Task::none();
        }
        if self.windows[&id].pending_serial.is_some() {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::Serial(id, serial_dialog::Message::Cancel));
            }
            return Task::none();
        }
        if self.reopen_waiting_for_serial(id) {
            if key == Key::Named(Named::Escape)
                && let Some(cancel) = self.reopen_wait_cancel(id)
            {
                return cancel;
            }
            return Task::none();
        }
        if let Some(pending) = self.windows[&id].pending_split_connection {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::CancelSplitConnection(id, pending));
            }
            return Task::none();
        }
        if self.windows[&id].pending_ssh.is_some() && self.project_ssh_target(id).is_none() {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::Ssh(ssh_access::Message::Cancel(id)));
            }
            return Task::none();
        }
        if self.windows[&id].pending_tunnels.is_some() {
            if key == Key::Named(Named::Escape) {
                return self.update_tunnel(tunnel_access::Message::Close(id));
            }
            return Task::none();
        }
        if let Some(review) = self.pending_tunnel_review(&self.windows[&id]) {
            if matches!(key, Key::Named(Named::Escape | Named::Enter | Named::Space)) {
                return self.update_tunnel(tunnel_access::Message::Review(
                    id,
                    review.clone(),
                    false,
                ));
            }
            return Task::none();
        }
        if self.windows[&id].pending_ibm.is_some() {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::CancelIbm(id));
            }
            return Task::none();
        }
        if let Some((session, prompt)) = self.window_clipboard_prompt(&self.windows[&id]) {
            if matches!(key, Key::Named(Named::Escape | Named::Enter)) {
                return self.update(Message::Clipboard(clipboard_access::Message::Decide(
                    session,
                    prompt.request,
                    crate::terminal_clipboard::Decision::Deny,
                )));
            }
            return Task::none();
        }
        if self.windows[&id].clipboard_settings {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::Clipboard(clipboard_access::Message::ShowSettings(
                    id, false,
                )));
            }
            return Task::none();
        }
        if self.security.editing(id) {
            if key == Key::Named(Named::Escape) {
                self.security.close(id);
            }
            return Task::none();
        }
        if let Some(tool) = self.hosted_close_prompt(id) {
            return if key == Key::Named(Named::Escape) {
                self.dismiss_hosted_tool(tool)
            } else {
                Task::none()
            };
        }
        if self.project_panel_open(id) {
            if key == Key::Named(Named::Escape) {
                return self.update_project(id, project_access::Message::Close);
            }
            return Task::none();
        }
        if let Some(paste) = &self.windows[&id].pending_paste {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::ConfirmPaste(id, paste.request, false));
            }
            return Task::none();
        }
        if let Some(task) = self.quick_select_key(id, &key, modifiers) {
            return task;
        }
        if key == Key::Named(Named::Escape) && self.dismiss_journal_dock_screenshot(id) {
            return Task::none();
        }
        let focused_ibm_kind = self
            .focused_session(id)
            .and_then(|pane| pane.frame.ibm.as_ref().map(|ibm| ibm.kind));
        let focused_ibm = focused_ibm_kind.is_some();
        if key == Key::Named(Named::Escape) && self.terminal_only_fullscreen_active(id) {
            return self.toggle_terminal_only_fullscreen(id);
        }
        if key == Key::Named(Named::F12)
            && (!focused_ibm || (modifiers.control() && !modifiers.alt() && !modifiers.shift()))
        {
            return self.toggle_window_fullscreen(id);
        }
        if let Some(backwards) = window_navigation::tab_step(&key, modifiers) {
            self.switch_tab_from_keyboard(id, backwards);
            return Task::none();
        }
        if let Some(task) = self.remote_dock_key(id, &key, modifiers) {
            return task;
        }
        if cfg!(target_os = "macos")
            && modifiers == Modifiers::LOGO
            && matches!(&key, Key::Character(character) if character.eq_ignore_ascii_case("k"))
            && self
                .focused_session(id)
                .is_some_and(|pane| pane.search.is_none())
            && self.focused_terminal_action_enabled(id, chrome::Action::ClearBuffer)
        {
            return self.invoke_focused_terminal_action(id, chrome::Action::ClearBuffer);
        }
        if let Some(action) = self
            .window_settings
            .key_bindings
            .action_for(&key, modifiers)
            && !(matches!(action, chrome::Action::Manual | chrome::Action::SaveProject)
                && focused_ibm_kind
                    .is_some_and(|kind| encode_ibm_key(kind, &key, modifiers, input).is_some()))
            && (!(action.is_split_navigation() || action.font_zoom().is_some())
                || self.chrome_enabled(id, action))
        {
            if action == chrome::Action::ReopenClosedTab {
                return self.update(Message::RecentlyClosed(
                    recently_closed::Message::ReopenLatest(id),
                ));
            }
            return self.update_chrome(chrome::Message::Invoke(id, action));
        }
        if let Some(TabEntry::Tool(tool)) = self.windows[&id].active_entry() {
            return if key == Key::Named(Named::Escape) {
                self.dismiss_hosted_tool(*tool)
            } else {
                Task::none()
            };
        }
        if cfg!(target_os = "macos") && modifiers.control() && modifiers.alt() {
            let action = match key {
                Key::Named(Named::ArrowLeft) => Some(chrome::Action::FocusPaneLeft),
                Key::Named(Named::ArrowRight) => Some(chrome::Action::FocusPaneRight),
                Key::Named(Named::ArrowUp) => Some(chrome::Action::FocusPaneAbove),
                Key::Named(Named::ArrowDown) => Some(chrome::Action::FocusPaneBelow),
                _ => None,
            };
            if let Some(action) = action
                && self.windows[&id]
                    .tab()
                    .is_some_and(|tab| tab.grid.len() >= 2)
            {
                return self.update_chrome(chrome::Message::Invoke(id, action));
            }
        }
        if modifiers == Modifiers::COMMAND | Modifiers::SHIFT {
            let direction = match key {
                Key::Named(Named::ArrowUp) => Some(PromptDirection::Previous),
                Key::Named(Named::ArrowDown) => Some(PromptDirection::Next),
                _ => None,
            };
            if let Some(direction) = direction
                && self
                    .focused_session(id)
                    .is_some_and(|pane| pane.session.navigate_prompt(direction))
            {
                return Task::none();
            }
        }
        if !focused_ibm
            && modifiers.shift()
            && !modifiers.control()
            && !modifiers.alt()
            && !modifiers.logo()
        {
            let lines = match key {
                Key::Named(Named::PageUp) => Some(20),
                Key::Named(Named::PageDown) => Some(-20),
                _ => None,
            };
            if let Some(lines) = lines
                && let Some(session) = self
                    .windows
                    .get(&id)
                    .and_then(Window::tab)
                    .and_then(Tab::focused_session)
            {
                return self.update(Message::Scroll(session, lines));
            }
        }
        if let Some(route) = self
            .windows
            .get(&id)
            .and_then(Window::tab)
            .and_then(|tab| self.keyboard_targets(tab))
        {
            let enter = key == Key::Named(Named::Enter);
            let source = InputSource::Keyboard {
                enter,
                predict: route.predict,
                shifted_enter: enter && modifiers.shift(),
                physical_pane: Some(route.source),
            };
            self.send_to(id, &route.panes, source, |target, frame| {
                if !route.accepts(target, frame) {
                    return None;
                }
                if let Some(ibm) = &frame.ibm {
                    (frame.status == Status::Running && !ibm.nvt_mode)
                        .then(|| encode_ibm_key(ibm.kind, &key, modifiers, input))
                        .flatten()
                        .map(PaneInput::Ibm)
                } else {
                    encode_stream_key(
                        &frame.screen,
                        &key,
                        location,
                        modifiers,
                        input,
                        frame.application_cursor_keys,
                    )
                    .map(PaneInput::Local)
                }
            });
        }
        Task::none()
    }

    fn notice(&mut self, id: window::Id, text: String) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.notice = Some(text);
        } else if let Some(window) = self.sftp.get_mut(&id) {
            window.set_status(text);
        }
    }

    fn openpgp_dialog_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.openpgp_for_source(owner)
            .or_else(|| {
                self.settings
                    .for_owner(owner)
                    .and_then(|settings| self.openpgp_for_source(settings))
            })
            .or_else(|| {
                self.windows
                    .get(&owner)?
                    .tabs
                    .iter()
                    .find_map(|tab| match tab {
                        TabEntry::Tool(tool) => self.openpgp_for_source(tool.instance),
                        TabEntry::Terminal(_) => None,
                    })
            })
            .or_else(|| self.native_snippet_modal_for_owner(owner))
    }

    fn owner_dialog(&self, owner: window::Id) -> Option<(window::Id, Message)> {
        if let Some(modal) = self.connection_manager_modal(owner) {
            return Some(modal);
        }
        if let Some(dialog) = self.vault_unlock_modal(owner) {
            return Some((
                dialog,
                Message::Vault(dialog, crate::vault_window::Message::FocusUnlock),
            ));
        }
        self.window_owner_dialog(owner)
    }

    fn window_owner_dialog(&self, owner: window::Id) -> Option<(window::Id, Message)> {
        if let Some(id) = self.connection_export_for_owner(owner) {
            return Some((id, chrome::connection_export::focus(id)));
        }
        self.journal_options_modal(owner)
            .or_else(|| self.journal_edit_for_owner(owner))
            .map(|id| (id, self.journal_edit_focus_message(id)))
            .or_else(|| self.terminal_effect_modal(owner))
            .or_else(|| {
                self.openpgp_dialog_for_owner(owner)
                    .map(|id| (id, Message::OpenPgp(id, openpgp_window::Message::Focus)))
            })
            .or_else(|| {
                self.hosted_ssh_keys_vault_for_owner(owner)
                    .map(|id| (id, Message::SshKeys(id, ssh_keys_ui::Message::Focus)))
            })
            .or_else(|| {
                self.quick_connect_for_owner(owner)
                    .map(|id| (id, Message::QuickConnect(id, quick_connect::Message::Focus)))
            })
            .or_else(|| {
                use chrome::connection_import;

                self.connection_import_for_owner(owner).map(|id| {
                    (
                        id,
                        connection_import::event(id, connection_import::Event::Focus),
                    )
                })
            })
            .or_else(|| {
                self.ascii_art_for_owner(owner)
                    .map(|id| (id, Message::AsciiArt(id, ascii_art::Message::Focus)))
            })
            .or_else(|| {
                self.known_hosts_for_owner(owner)
                    .map(|id| (id, Message::KnownHosts(id, known_hosts::Message::Focus)))
            })
            .or_else(|| {
                self.ssh_keys_for_owner(owner)
                    .map(|id| (id, Message::SshKeys(id, ssh_keys_ui::Message::Focus)))
            })
            .or_else(|| {
                self.backup_for_owner(owner)
                    .map(|id| (id, Message::Backup(id, backup_ui::Message::Focus)))
            })
            .or_else(|| {
                self.rename_dialog_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::TabContext(id, tab_context::Message::RenameFocus),
                    )
                })
            })
            .or_else(|| {
                self.tab_journal_note_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::TabContext(id, tab_context::Message::JournalNoteFocus),
                    )
                })
            })
            .or_else(|| {
                self.close_all_dialog_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::CloseAllDialog(id, close_all_dialog::Message::Focus),
                    )
                })
            })
            .or_else(|| {
                self.link_confirmation_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::TerminalLink(id, terminal_link_dialog::Message::Focus),
                    )
                })
            })
            .or_else(|| {
                self.remote_dialog_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::RemoteDock(
                            id,
                            remote_dock::Message::Dialog(remote_dock::DialogMessage::Focus),
                        ),
                    )
                })
            })
    }

    pub fn view(&self, id: window::Id) -> Element<'_, Message> {
        let element = self.view_window(id);
        let element = if let Some((_, focus)) = self.window_owner_dialog(id) {
            let cover = iced::widget::mouse_area(
                container(space::vertical())
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(focus);
            iced::widget::stack![element, iced::widget::opaque(cover)].into()
        } else {
            element
        };
        #[cfg(target_os = "macos")]
        let element = if let Some(window) = self
            .windows
            .get(&id)
            .filter(|window| window.transparency.requested)
        {
            column![
                transparency::chrome(
                    iced::widget::space::vertical()
                        .height(f32::from(window.transparency.titlebar_inset)),
                ),
                element,
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            element
        };
        let element = if self.journal_options_modal(id).is_none() {
            self.palette_view(id, element)
        } else {
            element
        };
        let element = if let Some(size) = self
            .windows
            .get(&id)
            .and_then(|window| window.terminal_only.size())
        {
            window_navigation::center(element, size)
        } else {
            element
        };
        let element = self.journal_context_view(id, element);
        let element = if self.clipboard.policy.internal {
            self.clipboard.buffer.isolate(element)
        } else {
            element
        };
        profile_tools::vault_modal::view(element, self.vault_unlock_modal(id))
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if let Some(view) = self.terminal_effect_dialog_view(id) {
            return view;
        }
        if let Some(dialog) = self
            .quick_connect_view(id)
            .or_else(|| self.known_hosts_view(id))
            .or_else(|| self.ssh_keys_view(id))
            .or_else(|| self.backup_view(id))
            .or_else(|| self.connection_import_view(id))
            .or_else(|| self.connection_export_view(id))
            .or_else(|| self.connection_overview_view(id))
            .or_else(|| self.os_badge_manager_view(id))
            .or_else(|| self.openpgp_view(id))
            .or_else(|| self.environment_view(id))
            .or_else(|| self.manual_view(id))
            .or_else(|| self.ai_manager_view(id))
            .or_else(|| self.chrome_dialog_view(id))
            .or_else(|| self.settings_view(id))
            .or_else(|| self.shell_integration_view(id))
            .or_else(|| self.link_confirmation_view(id))
            .or_else(|| self.rename_dialog_view(id))
            .or_else(|| self.tab_journal_note_view(id))
            .or_else(|| self.close_all_dialog_view(id))
            .or_else(|| self.remote_dialog_view(id))
            .or_else(|| self.session_process_view(id))
            .or_else(|| self.ascii_art_view(id))
            .or_else(|| self.logfile_view(id))
            .or_else(|| self.journal_view(id))
        {
            return dialog;
        }
        if let Some(recording) = self.recording_view(id) {
            return recording;
        }
        if let Some(tool) = self.profile_tool_view(id) {
            return tool;
        }
        if let Some(snippets) = self.snippet_view(id) {
            return snippets;
        }
        if let Some(state) = self.sftp.get(&id) {
            return self.view_sftp(id, state);
        }
        if let Some(scheduler) = self.scheduler_view(id) {
            return scheduler;
        }
        if let Some(editor) = self.editors.get(&id) {
            return editor
                .view(self.editor_content_theme())
                .map(move |message| Message::Editor(id, message));
        }
        if let Some(diagram) = self.diagrams.get(&id) {
            return diagram
                .view(self.editor_content_theme())
                .map(move |message| Message::Diagram(id, message));
        }
        let Some(window) = self.windows.get(&id) else {
            return container(text("kortty")).into();
        };
        if let Some(view) = self.reopen_wait_view(id) {
            return view;
        }
        if let Some(pending) = window.pending_split_connection {
            return container(
                column![
                    text("Opening split connection…").size(18),
                    button("Cancel").on_press(Message::CancelSplitConnection(id, pending)),
                ]
                .spacing(12),
            )
            .center(Length::Fill)
            .into();
        }
        if let Some(view) = self.snippet_run_view(id) {
            return view;
        }
        if let Some((request, action)) = window.pending_serial_close
            && !matches!(action, SerialClose::TabBatch(batch) if self.tab_batch_all_count(id, batch).is_some())
        {
            let prompt = match action {
                SerialClose::Quit => "Close open serial ports and quit?",
                SerialClose::Window => "Close this window and its open serial ports?",
                SerialClose::Tab(_) => "Close this tab and its open serial ports?",
                SerialClose::TabBatch(_) => "Close these tabs and their open serial ports?",
                SerialClose::Pane { .. } => "Close the serial port?",
            };
            return column![
                text(prompt).size(18),
                row![
                    button("Cancel").on_press(Message::ConfirmSerialClose(id, request, false)),
                    button(if matches!(action, SerialClose::Quit) {
                        "Quit"
                    } else {
                        "Close"
                    })
                    .on_press(Message::ConfirmSerialClose(id, request, true)),
                ]
                .spacing(8),
            ]
            .padding(16)
            .spacing(12)
            .into();
        }
        if let Some(pending) = &window.pending_serial {
            let mut content = column![
                pending
                    .view()
                    .map(move |message| Message::Serial(id, message))
            ]
            .height(Length::Fill);
            if let Some(notice) = &window.notice {
                content = content.push(text(notice).size(13));
            }
            return content.into();
        }
        if let Some(pending) = &window.pending_ssh
            && self.project_ssh_target(id).is_none()
        {
            let mut content = column![self.ssh_dialog_view(id, pending)].height(Length::Fill);
            if let Some(notice) = &window.notice {
                content = content.push(text(notice).size(13));
            }
            return content.into();
        }
        if let Some(session) = window.pending_tunnels {
            return self.tunnel_view(id, session);
        }
        if let Some(review) = self.pending_tunnel_review(window) {
            return self.tunnel_review_view(id, review);
        }
        if window.pending_ibm.is_none()
            && let Some((session, prompt)) = self.window_clipboard_prompt(window)
        {
            return self.clipboard_prompt_view(session, prompt);
        }
        if window.clipboard_settings {
            return self.clipboard_settings_view(id);
        }
        if self.control.settings == Some(id) {
            return self.view_control_settings(id);
        }
        if let Some(view) = self.session_security_view(id) {
            return view;
        }
        if self.hosted_close_prompt(id).is_none()
            && let Some(view) = self.project_view(id)
        {
            return view;
        }
        let transparency = self.terminal_transparency(id, window);
        let mut content = column![]
            .padding(7)
            .width(Length::Fill)
            .height(Length::Fill);
        if let Some(offer) = self.session_restore_offer(id) {
            content = content.push(transparency::chrome(offer).padding(iced::padding::bottom(5)));
        }
        if let Some(banner) = self.project_reconnect_banner(id) {
            content = content.push(transparency::chrome(banner).padding(iced::padding::bottom(5)));
        }
        if let Some(dialog) = &window.pending_ibm {
            let connect = dialog.connection().map(|_| Message::ConnectIbm(id));
            let models = if dialog.kind.model_5250().is_some() {
                [IbmKind::Tn5250, IbmKind::Tn5250Wide]
            } else {
                [IbmKind::Tn3270e, IbmKind::Tn3270eExtended]
            };
            let editable = dialog.can_edit_route();
            let device: Element<'_, Message> = if editable {
                pick_list(models, Some(dialog.kind), move |kind| {
                    Message::IbmDevice(id, kind)
                })
                .into()
            } else {
                text(dialog.kind.label()).into()
            };
            let mut fields = row![
                device,
                text_input("Numeric IP:port", &dialog.address)
                    .on_input_maybe(
                        editable.then_some(move |address| Message::IbmAddress(id, address))
                    )
                    .on_submit_maybe(connect.clone())
                    .width(185),
            ];
            if dialog.kind.model_5250().is_some() {
                fields = fields.push(
                    text_input("Device name (optional)", &dialog.device_name)
                        .on_input_maybe(
                            editable.then_some(move |name| Message::IbmDeviceName(id, name)),
                        )
                        .on_submit_maybe(connect.clone())
                        .width(170),
                );
            }
            let fields = fields
                .push(
                    checkbox(dialog.plaintext)
                        .label("Allow plaintext for this connection")
                        .on_toggle(move |allowed| Message::IbmPlaintext(id, allowed)),
                )
                .push(
                    button(if dialog.reconnect.is_some() {
                        "Reconnect"
                    } else {
                        "Connect"
                    })
                    .on_press_maybe(connect),
                )
                .push(button("Cancel").on_press(Message::CancelIbm(id)))
                .spacing(8)
                .align_y(Alignment::Center);
            let mut form = column![horizontal(fields)];
            if let Err(error) = dialog.device_name() {
                form = form.push(text(error.to_string()).size(12));
            }
            content = content.push(transparency::chrome(form).padding(iced::padding::bottom(5)));
        }
        let mut terminal = column![].width(Length::Fill).height(Length::Fill);
        if let Some(active_tab) = window.active_entry() {
            let tabs =
                window
                    .tabs
                    .iter()
                    .enumerate()
                    .fold(row![].spacing(4), |tabs, (index, entry)| {
                        let tab = match entry {
                            TabEntry::Terminal(tab) => tab,
                            TabEntry::Tool(tool) => {
                                let select = button(text(self.hosted_tool_title(tool)))
                                    .style(move |theme, status| {
                                        chrome::tab_button_style(
                                            theme,
                                            status,
                                            index == window.active,
                                        )
                                    })
                                    .on_press(Message::SelectTab(id, tool.id));
                                let close = button("×")
                                    .style(button::text)
                                    .on_press(Message::CloseTab(id, tool.id));
                                let header = tab_drag::tool_header(select, id, tool.id);
                                return tabs.push(row![header, close].align_y(Alignment::Center));
                            }
                        };
                        let (name, connection) = self.tab_names(tab);
                        let name = if tab.custom_title.is_some() {
                            name
                        } else {
                            format!("{name} {}", tab.id)
                        };
                        let name = if let Some(group) = &tab.group {
                            format!("[{group}] {name}")
                        } else {
                            name
                        };
                        let (markers, security) = self.tab_security(tab);
                        let label = if markers.is_empty() {
                            name
                        } else {
                            format!("{name} · {markers}")
                        };
                        let mut tooltip_text = match (connection, security) {
                            (Some(connection), Some(security)) => {
                                Some(format!("Connection · {connection}\n{security}"))
                            }
                            (Some(connection), None) => Some(format!("Connection · {connection}")),
                            (None, security) => security,
                        };
                        let mirror_marker = self.multi_exec_tab_marker(tab);
                        if let Some(marker) = &mirror_marker {
                            let description = tooltip_text.get_or_insert_with(String::new);
                            if !description.is_empty() {
                                description.push('\n');
                            }
                            description.push_str(marker);
                        }
                        let mut heading = row![].spacing(5).align_y(Alignment::Center);
                        if mirror_marker.is_some() {
                            heading = heading.push(text("⇄").size(13));
                        }
                        if let Some(reason) = tab.watch.attention_text() {
                            heading = heading.push(text("🔔").size(13));
                            let description = tooltip_text.get_or_insert_with(String::new);
                            if !description.is_empty() {
                                description.push('\n');
                            }
                            description.push_str(&reason);
                        }
                        if let Some(mark) = self.terminal_tab_color(tab) {
                            heading = heading.push(text("●").size(11).color(mark.color()));
                            let description = tooltip_text.get_or_insert_with(String::new);
                            if !description.is_empty() {
                                description.push('\n');
                            }
                            description.push_str(&mark.description());
                        }
                        let select = button(heading.push(text(label)))
                            .style(move |theme, status| {
                                chrome::tab_button_style(theme, status, index == window.active)
                            })
                            .on_press(Message::SelectTab(id, tab.id));
                        let select: Element<'_, Message> = if let Some(tooltip_text) = tooltip_text
                        {
                            tooltip(select, text(tooltip_text), tooltip::Position::Bottom).into()
                        } else {
                            select.into()
                        };
                        let close = button("×")
                            .style(button::text)
                            .on_press(Message::CloseTab(id, tab.id));
                        let header = self.tab_context_header(
                            id,
                            tab.id,
                            tab_drag::header(select, id, tab.id),
                        );
                        tabs.push(row![header, close].align_y(Alignment::Center))
                    });
            terminal = terminal.push(
                transparency::chrome(self.tab_drag.strip(id, horizontal(tabs)))
                    .padding(iced::padding::bottom(5)),
            );
            if window.transfer_open {
                let mut destinations: Vec<_> = self
                    .windows
                    .iter()
                    .filter(|(other, _)| {
                        **other != id && self.tab_transfer_window_available(**other)
                    })
                    .collect();
                destinations.sort_by_key(|(_, other)| other.number);
                let mut options =
                    row![button("Move to a new window").on_press(Message::Transfer {
                        from: id,
                        to: None,
                        tab: active_tab.id()
                    })]
                    .spacing(6);
                for (destination, other) in destinations {
                    options =
                        options.push(button(text(format!("Window {}", other.number))).on_press(
                            Message::Transfer {
                                from: id,
                                to: Some(*destination),
                                tab: active_tab.id(),
                            },
                        ));
                }
                terminal = terminal.push(
                    transparency::chrome(horizontal(options)).padding(iced::padding::bottom(5)),
                );
            }
            terminal = match active_tab {
                TabEntry::Terminal(tab) => {
                    let mut body = column![].width(Length::Fill).height(Length::Fill);
                    if let Some(bar) = self.journal_capture_bar(id, tab) {
                        body = body.push(transparency::chrome(bar).padding([3, 5]));
                    }
                    let color = if self.window_settings.connection_color_border_enabled {
                        self.terminal_tab_color(tab).map(|mark| mark.color())
                    } else {
                        None
                    };
                    let panes = self.panes(id, window, tab, transparency, color);
                    let body = body.push(self.remote_dock_view(id, window, tab, panes));
                    terminal.push(transparency::connection_frame(body.into(), color))
                }
                TabEntry::Tool(tool) => match self.hosted_tool_view(tool) {
                    Some(view) => terminal.push(view),
                    None => terminal,
                },
            };
        } else {
            terminal = terminal.push(
                container(text("Open a tab or move one here from another window"))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            );
        }
        let terminal = self.tab_drag.pane(id, terminal.into());
        let terminal = self.journal_dock_view(id, window, terminal);
        let terminal = self.local_dock_view(id, window, terminal);
        content = content.push(self.dashboard_view(id, window, terminal));
        if let Some(paste) = &window.pending_paste {
            content = content.push(
                column![
                    row![
                        text(format!(
                            "Paste {} bytes into {} pane(s)?",
                            paste.raw_bytes,
                            paste.targets.len()
                        )),
                        button("Cancel").on_press(Message::ConfirmPaste(id, paste.request, false)),
                        button("Paste")
                            .style(button::danger)
                            .on_press(Message::ConfirmPaste(id, paste.request, true)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text(paste.warning.explanation()).size(13),
                    scrollable(text(&paste.preview).font(iced::Font::MONOSPACE).size(13))
                        .height(Length::Fixed(84.0)),
                ]
                .spacing(5),
            );
        }
        if let Some(attention) = self.project_attention_bar(id) {
            content = content.push(transparency::chrome(attention).padding(iced::padding::top(5)));
        }
        if let Some(notice) = &window.notice {
            content = content
                .push(transparency::chrome(text(notice).size(13)).padding(iced::padding::top(5)));
        }
        let cursor = self
            .window_settings
            .appearance
            .status_cursor(window.focused);
        let multi_exec = self.multi_exec_status();
        if cursor.is_some() || multi_exec.is_some() || self.menu_bar_hidden(id) {
            let mut status = row![space::horizontal()]
                .spacing(12)
                .align_y(Alignment::Center);
            if let Some(multi_exec) = multi_exec {
                status = status.push(multi_exec);
            }
            if let Some(cursor) = cursor {
                status = status.push(cursor);
            }
            content = content.push(
                self.menu_bar_recovery(id, transparency::chrome(status).padding([2, 8]).into()),
            );
        }
        let drop_target = self.tab_drag.hover() == Some(id);
        container(
            Column::from_vec(vec![
                self.icon_toolbar(id),
                self.menu_bar(id, window),
                content.into(),
            ])
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .style(move |theme: &iced::Theme| {
            let style = container::Style::default();
            if drop_target {
                style.border(iced::Border {
                    color: theme.extended_palette().primary.strong.color,
                    width: if transparency > 0 { 7.0 } else { 2.0 },
                    ..iced::Border::default()
                })
            } else if transparency > 0 {
                style.border(iced::Border {
                    color: iced::theme::default(theme).background_color,
                    width: 7.0,
                    ..iced::Border::default()
                })
            } else {
                style
            }
        })
        .into()
    }

    fn terminal_tab_color(&self, tab: &Tab) -> Option<environment_access::Mark<'_>> {
        self.connection_color_mark(*tab.grid.get(tab.primary)?)
    }

    fn panes<'a>(
        &'a self,
        id: window::Id,
        window: &'a Window,
        tab: &'a Tab,
        transparency: u8,
        tab_color: Option<iced::Color>,
    ) -> Element<'a, Message> {
        let appearance = self.window_settings.terminal_appearance();
        let broadcast = self.tab_broadcast_active(tab);
        let pane_content = move |pane, session: &SessionId, zoomed| {
            let Some(session_state) = self.sessions.get(session) else {
                return pane_grid::Content::new(text("Session unavailable"));
            };
            let focus = pane == tab.focused;
            let multi_exec = self.multi_exec_member(*session);
            let mirrored = multi_exec || broadcast;
            let frame = &session_state.frame;
            let state = if let Some(ibm) = &frame.ibm {
                let state = match &frame.status {
                    Status::Disconnected => "Disconnected".into(),
                    Status::Starting => "Connecting".into(),
                    Status::Running if ibm.nvt_mode => "Host switched to NVT".into(),
                    Status::Running if ibm.keyboard_locked => "Keyboard locked".into(),
                    Status::Running if ibm.insert_mode => "Insert".into(),
                    Status::Running if ibm.kind.model_5250().is_some() && !ibm.invited => {
                        "Waiting for host READ".into()
                    }
                    Status::Running => "Ready".into(),
                    Status::Exited(_) => "Host disconnected".into(),
                    Status::Error(error) => error.clone(),
                };
                let mut label = format!("{} · {state}", ibm.kind.label());
                if ibm.message_waiting {
                    label.push_str(" · Message waiting");
                }
                if let Some(notice) = &ibm.notice {
                    label.push_str(" · ");
                    label.push_str(notice);
                }
                label
            } else if let Some(serial) = &frame.serial {
                let state = match &frame.status {
                    Status::Disconnected | Status::Exited(_) => "Disconnected".into(),
                    Status::Starting => "Opening".into(),
                    Status::Running if frame.scroll_offset != 0 => {
                        format!("Scrollback · {}", frame.scroll_offset)
                    }
                    Status::Running => "Connected".into(),
                    Status::Error(error) => error.clone(),
                };
                let mut label = format!(
                    "Serial · {} @ {} · {state}",
                    serial.device.display(),
                    serial.settings.baud_rate
                );
                if let Some(notice) = &frame.serial_notice {
                    label.push_str(" · ");
                    label.push_str(notice);
                }
                label
            } else if let Some(ssh) = &session_state.ssh {
                let status = match &frame.status {
                    Status::Disconnected => "Disconnected".into(),
                    Status::Starting => "Opening shell".into(),
                    Status::Running => "Connected".into(),
                    Status::Exited(code) => code.map_or_else(
                        || "Remote shell exited".into(),
                        |code| format!("Remote shell exited ({code})"),
                    ),
                    Status::Error(error) => error.clone(),
                };
                let mut label = format!("SSH · {} · {status}", ssh.label());
                if let Some(notice) = session_state.session.logfile_notice() {
                    label.push_str(" · ");
                    label.push_str(&notice);
                }
                label
            } else {
                match &frame.status {
                    Status::Disconnected => "Disconnected".into(),
                    Status::Starting => "Starting shell".into(),
                    Status::Running => {
                        if frame.scroll_offset == 0 {
                            "Local shell".into()
                        } else {
                            format!("Scrollback · {}", frame.scroll_offset)
                        }
                    }
                    Status::Exited(code) => code.map_or_else(
                        || "Shell exited".into(),
                        |code| format!("Shell exited ({code})"),
                    ),
                    Status::Error(error) => format!("Shell error ({error})"),
                }
            };
            let label = if focus { format!("● {state}") } else { state };
            let label = if mirrored {
                format!("{label} · ⇄")
            } else {
                label
            };
            let mut controls = row![].spacing(4);
            let session_id = *session;
            if session_state.session.supports_search() {
                controls = controls.push(
                    button("Find")
                        .style(button::text)
                        .on_press(Message::OpenSearch(session_id)),
                );
            }
            if !mirrored && session_state.supports_typeahead() {
                controls = controls.push(
                    button(if session_state.typeahead_enabled {
                        "Typeahead on"
                    } else {
                        "Typeahead off"
                    })
                    .style(button::text)
                    .on_press(Message::ToggleTypeAhead(session_id)),
                );
            }
            if frame.ibm.is_none() {
                controls = controls.push(
                    button(if session_state.strict_escapes {
                        "Strict on"
                    } else {
                        "Strict off"
                    })
                    .style(button::text)
                    .on_press(Message::Clipboard(
                        clipboard_access::Message::Strict(session_id),
                    )),
                );
            }
            if frame.serial.is_some() {
                match frame.status {
                    Status::Running => {
                        controls = controls.push(
                            button("Send Break")
                                .style(button::text)
                                .on_press(Message::SerialBreak(id, session_id)),
                        );
                    }
                    Status::Disconnected | Status::Exited(_) | Status::Error(_) => {
                        controls = controls.push(
                            button("Connect…")
                                .style(button::text)
                                .on_press(Message::ShowSerial(id, Some(session_id))),
                        );
                    }
                    Status::Starting => {}
                }
            }
            if let Some(ibm) = &frame.ibm
                && matches!(
                    frame.status,
                    Status::Disconnected | Status::Exited(_) | Status::Error(_)
                )
            {
                controls = controls.push(
                    button("Connect…")
                        .style(button::text)
                        .on_press(Message::ShowIbm(id, ibm.kind, Some(*session))),
                );
            }
            if session_state.ssh.is_some()
                && matches!(
                    frame.status,
                    Status::Disconnected | Status::Exited(_) | Status::Error(_)
                )
            {
                controls = controls.push(button("Connect…").style(button::text).on_press(
                    Message::Ssh(ssh_access::Message::Show(id, Some(session_id))),
                ));
            }
            if let Some(forwarding) = &session_state.forwarding {
                let count = forwarding.active_count();
                let label = if self.clipboard.policy.port_forwarding_allowed {
                    if count == 0 {
                        "Tunnels…".into()
                    } else {
                        format!("Tunnels ({count})…")
                    }
                } else {
                    "Tunnels disabled by policy".into()
                };
                controls =
                    controls.push(
                        button(text(label)).style(button::text).on_press_maybe(
                            self.clipboard.policy.port_forwarding_allowed.then_some(
                                Message::Tunnel(tunnel_access::Message::Show(id, session_id)),
                            ),
                        ),
                    );
            }
            controls = controls
                .push(
                    button(if zoomed { "Restore" } else { "Zoom" })
                        .style(button::text)
                        .on_press(Message::ZoomPane(id, pane)),
                )
                .push(
                    button("×")
                        .style(button::text)
                        .on_press(Message::ClosePane(id, pane)),
                );
            let bar = pane_grid::TitleBar::new(
                row![text(label).size(12), space::horizontal()].align_y(Alignment::Center),
            )
            .padding([3, 6])
            .style(transparency::opaque_chrome)
            .controls(pane_grid::Controls::new(controls));
            let themes = &self.window_settings.themes;
            let scheme = themes.appearance();
            let font_size =
                themes.font_size(scheme, session_state.font, session_state.font_baseline);
            let effect = session_state
                .effect
                .filter(|active| self.allowed_terminal_effect(active.selection.effect));
            let font = self
                .window_settings
                .terminal_font_face(scheme.filter(|_| effect.is_none()));
            let appearance = effect.map_or(appearance, |active| {
                active.selection.effect.appearance(appearance)
            });
            let terminal = TerminalView {
                session: *session,
                frame,
                font_size,
                font,
                appearance,
                scale_factor: window.scale_factor,
                background_transparency: transparency,
                focused: focus && window.focused,
                typeahead_enabled: session_state.typeahead_enabled && !mirrored,
                owner: id,
                plain_text_links: self.window_settings.terminal_link_detection_enabled,
                selection: session_state
                    .selection
                    .or_else(|| session_state.selected_output.as_ref()?.visible(frame)),
                search: session_state.search.as_ref().map(|search| SearchHighlight {
                    matches: search.matches(frame),
                    selected: search.selected_match(frame).map(|hit| hit.start),
                    first_row: frame
                        .screen
                        .scrollback_lines
                        .saturating_sub(frame.scroll_offset),
                }),
            };
            let scrollbar = (frame.ibm.is_none()
                && self
                    .window_settings
                    .terminal_scrollbar_visible(window.fullscreen()))
            .then(|| terminal_view::Scrollbar::new(&terminal));
            let view = terminal_view::view(terminal);
            let view = if let Some(effect) = effect {
                iced::widget::stack![
                    view,
                    effect.overlay(self.terminal_effect_frame(), font_size)
                ]
                .into()
            } else {
                view
            };
            let view = self.terminal_context_view(id, session_id, view);
            let view = terminal_view::quick_select::view(
                view,
                self.quick_select_labels(id, session_id),
                &frame.screen,
                font_size,
                font,
                appearance,
            );
            let view = terminal_view::with_timestamp_gutter(
                view,
                (tab.command_timestamps && frame.ibm.is_none()).then_some(terminal_view::Gutter {
                    frame,
                    font_size,
                    font,
                    appearance,
                }),
            );
            let view = if frame.ibm.is_some() {
                let size = terminal_view::content_size(&frame.screen, font_size, font);
                scrollable(container(view).width(size.width).height(size.height))
                    .direction(scrollable::Direction::Both {
                        vertical: scrollable::Scrollbar::default(),
                        horizontal: scrollable::Scrollbar::default(),
                    })
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                terminal_view::with_scrollback(view, scrollbar)
            };
            let view = if let Some(search) = &session_state.search {
                column![
                    transparency::chrome(horizontal(
                        search
                            .view()
                            .map(move |message| Message::Search(session_id, message))
                    ))
                    .padding(iced::padding::bottom(4)),
                    view
                ]
                .height(Length::Fill)
                .into()
            } else {
                view
            };
            pane_grid::Content::new(view).title_bar(bar)
        };
        let panes = pane_grid(&tab.grid, pane_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .spacing(transparency::SPLIT_SPACING)
            .min_size(transparency::SPLIT_MIN)
            .on_click(move |pane| Message::FocusPane(id, pane))
            .on_resize(5, move |event| Message::ResizeSplit(id, event))
            .on_drag(move |event| Message::DragPane(id, event))
            .into();
        let focus_ring = transparency::FocusRing::visible(
            &tab.grid,
            tab.focused,
            self.window_settings.split_pane_focus_ring_enabled,
            window.focused && self.palette_can_open(id) && !self.palette.is_open(id),
        );
        let connection_frames = if self.window_settings.connection_color_border_enabled {
            tab.grid
                .iter()
                .filter_map(|(pane, session)| {
                    let color = self.connection_color_mark(*session)?.color();
                    (Some(color) != tab_color).then_some((*pane, color))
                })
                .collect()
        } else {
            Vec::new()
        };
        transparency::decorate_panes(
            &tab.grid,
            window.transparency.requested,
            transparency,
            appearance.background,
            panes,
            focus_ring,
            connection_frames,
        )
    }
}

fn initial_grid(profile: TerminalProfile) -> GridSize {
    let (columns, rows) = profile.default_size();
    GridSize {
        columns,
        rows,
        ..GridSize::default()
    }
}

fn horizontal<'a>(contents: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    scrollable(contents)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(3).scroller_width(3),
        ))
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn removing_a_preceding_tab_preserves_selection() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        let state = workspace
            .windows
            .get_mut(&window)
            .expect("window registered");
        for id in 0..3 {
            let (grid, focused) = pane_grid::State::new(SessionId(id));
            state.tabs.push(
                Tab {
                    id,
                    grid,
                    remote_files: remote_dock::Dock::default(),
                    primary: focused,
                    focused,
                    broadcast: false,
                    command_timestamps: false,
                    incognito: false,
                    custom_title: None,
                    group: None,
                    connection_group_baseline: None,
                    watch: Default::default(),
                    reopen_origin: None,
                    tunnel_owner: Some(SessionId(id)),
                }
                .into(),
            );
        }
        state.active = 2;
        let removed = workspace.detach_tab(window, 0).expect("tab exists");
        assert_eq!(removed.id(), 0);
        assert_eq!(workspace.windows[&window].tab().map(|tab| tab.id), Some(2));
    }

    #[test]
    fn stale_clipboard_results_and_clicks_cannot_replace_a_confirmation() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        let session = SessionId(42);
        // Only the paste confirmation state is exercised. No clipboard or
        // session transport runs in this test.
        workspace.add_tab(window, session);
        let tab = workspace.windows[&window].tab().unwrap().id;
        let _ = workspace.update(Message::Paste(window));
        let previous = workspace.windows[&window].paste_request.unwrap();
        let _ = workspace.update(Message::PasteLoaded(
            window,
            previous,
            tab,
            vec![session],
            Some("previous\npayload".into()),
        ));

        let _ = workspace.update(Message::Paste(window));
        let current = workspace.windows[&window].paste_request.unwrap();
        let _ = workspace.update(Message::PasteLoaded(
            window,
            previous,
            tab,
            vec![session],
            Some("stale\nresult".into()),
        ));
        assert!(workspace.windows[&window].pending_paste.is_none());
        let _ = workspace.update(Message::PasteLoaded(
            window,
            current,
            tab,
            vec![session],
            Some("current\npayload".into()),
        ));
        let _ = workspace.update(Message::ConfirmPaste(window, previous, true));
        let pending = workspace.windows[&window].pending_paste.as_ref().unwrap();
        assert_eq!(pending.text, "current\npayload");
        let _ = workspace.update(Message::ConfirmPaste(window, current, false));
        assert!(workspace.windows[&window].pending_paste.is_none());
    }

    #[test]
    fn paste_protection_applies_the_managed_floor_before_sending_sanitized_text() {
        let mut workspace = Workspace::default();
        workspace.window_settings.paste_warning = paste_protection::WarningMode::Off;
        workspace.clipboard.policy.minimum_paste_warning = paste_protection::WarningMode::Always;
        let (window, _) = workspace.open_window(false);
        let session = SessionId(42);
        workspace.add_tab(window, session);
        let tab = workspace.windows[&window].tab().unwrap().id;
        let _ = workspace.update(Message::Paste(window));
        let request = workspace.windows[&window].paste_request.unwrap();
        let raw = "left\x1b[201~\x01right";
        let _ = workspace.update(Message::PasteLoaded(
            window,
            request,
            tab,
            vec![session],
            Some(raw.into()),
        ));
        let pending = workspace.windows[&window].pending_paste.as_ref().unwrap();
        assert_eq!(pending.text, "leftright");
        assert_eq!(pending.preview, "left<U+001B>[201~<U+0001>right");
        assert_eq!(pending.raw_bytes, raw.len());
    }

    #[test]
    fn terminal_zoom_preserves_the_open_font_baseline_across_reconnect() {
        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_font = 61;
        let (window, _) = workspace.open_window(false);
        let previous = workspace.store_connected_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            None,
            None,
        );
        assert_eq!(workspace.sessions[&previous].font, 61);
        workspace.sessions.get_mut(&previous).unwrap().font = profile::MAX_TERMINAL_FONT;
        workspace.window_settings.terminal_font = 17;
        let session = workspace.store_connected_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            Some(previous),
            None,
        );
        workspace.add_tab(window, session);
        let modifiers = if cfg!(target_os = "macos") {
            Modifiers::COMMAND
        } else {
            Modifiers::COMMAND | Modifiers::SHIFT
        };
        let _ = workspace.key(
            window,
            Key::Character("=".into()),
            Location::Standard,
            modifiers,
            None,
        );
        assert_eq!(
            workspace.sessions[&session].font,
            profile::MAX_TERMINAL_FONT
        );
        let _ = workspace.key(
            window,
            Key::Character("0".into()),
            Location::Standard,
            modifiers,
            None,
        );
        assert_eq!(workspace.sessions[&session].font, 61);
    }

    #[test]
    fn transfer_preserves_the_split_and_focused_session() {
        let mut workspace = Workspace::default();
        let (from, _) = workspace.open_window(false);
        let (to, _) = workspace.open_window(false);
        let (mut grid, first) = pane_grid::State::new(SessionId(10));
        let (focused, _) = grid
            .split(pane_grid::Axis::Vertical, first, SessionId(20))
            .expect("valid split");
        grid.split(pane_grid::Axis::Horizontal, focused, SessionId(30));
        workspace
            .windows
            .get_mut(&from)
            .expect("source window")
            .tabs
            .push(
                Tab {
                    id: 1,
                    grid,
                    remote_files: remote_dock::Dock::default(),
                    primary: focused,
                    focused,
                    broadcast: true,
                    command_timestamps: false,
                    incognito: false,
                    custom_title: None,
                    group: None,
                    connection_group_baseline: None,
                    watch: Default::default(),
                    reopen_origin: None,
                    tunnel_owner: Some(SessionId(10)),
                }
                .into(),
            );
        let before: HashSet<_> = workspace.windows[&from]
            .tab()
            .expect("tab")
            .targets()
            .into_iter()
            .collect();
        let previous_close = workspace.allocate();
        workspace.windows.get_mut(&to).unwrap().pending_serial_close =
            Some((previous_close, SerialClose::Window));
        let _ = workspace.transfer(from, Some(to), 1);
        assert!(workspace.windows[&from].tabs.is_empty());
        assert!(workspace.windows[&to].pending_serial_close.is_none());
        let _ = workspace.update(Message::ConfirmSerialClose(to, previous_close, true));
        let moved = workspace.windows[&to].tab().expect("moved tab");
        assert_eq!(moved.grid.len(), 3);
        assert_eq!(moved.focused_session(), Some(SessionId(20)));
        assert_eq!(moved.targets().into_iter().collect::<HashSet<_>>(), before);
    }

    #[test]
    fn restored_serial_layout_retains_settings_without_opening_the_port() {
        let connection = SerialConnection {
            // A device name for this test only. No device or port needs to exist.
            device: if cfg!(windows) {
                "COM42".into()
            } else {
                "/dev/kortty-serial-layout-test".into()
            },
            settings: crate::session::SerialSettings {
                baud_rate: 115200,
                assert_dtr: false,
                ..Default::default()
            },
            line_ending: crate::session::SerialLineEnding::CrLf,
            local_echo: true,
        };
        let layout = WorkspaceLayout::new(vec![WindowLayout {
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            width: 920.0,
            height: 620.0,
            position: None,
            maximized: false,
            dashboard_visible: Some(false),
            active: 0,
            tabs: vec![PaneLayout::Serial {
                font: 17,
                focused: true,
                term: TerminalProfile::Vt100.term_name().into(),
                connection: connection.clone(),
            }],
        }]);
        let encoded = serde_json::to_vec(&layout).expect("serialize serial layout");
        let decoded = serde_json::from_slice(&encoded).expect("deserialize serial layout");
        let mut workspace = Workspace::default();
        let _ = workspace
            .restore_layout(decoded)
            .expect("restore serial layout");
        let (&window, state) = workspace.windows.iter().next().expect("restored window");
        let session = state
            .tab()
            .and_then(Tab::focused_session)
            .expect("serial pane");
        let frame = workspace.sessions[&session].session.frame();
        assert!(matches!(frame.status, Status::Disconnected));
        assert_eq!(frame.serial.as_deref(), Some(&connection));
        assert_eq!(frame.screen.profile, TerminalProfile::Vt100);
        assert_eq!(workspace.capture_layout(), Some(layout));

        let _ = workspace.update(Message::ShowSerial(window, Some(session)));
        let pending = workspace.windows[&window]
            .pending_serial
            .as_ref()
            .expect("reconnect form");
        assert_eq!(pending.replace, Some(session));
        assert_eq!(
            pending.form.validated().unwrap(),
            (connection, TerminalProfile::Vt100)
        );
        let _ = workspace.update(Message::Serial(window, serial_dialog::Message::Cancel));
        assert!(workspace.windows[&window].pending_serial.is_none());
        assert!(matches!(
            workspace.sessions[&session].session.frame().status,
            Status::Disconnected
        ));
    }

    #[test]
    fn ibm_model_picker_preserves_the_address_and_resets_plaintext_consent() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        for (base, selected) in [
            (IbmKind::Tn5250, IbmKind::Tn5250Wide),
            (IbmKind::Tn3270e, IbmKind::Tn3270eExtended),
        ] {
            let _ = workspace.update(Message::ShowIbm(window, base, None));
            let _ = workspace.update(Message::IbmAddress(window, "127.0.0.1:23".into()));
            let _ = workspace.update(Message::IbmPlaintext(window, true));
            let _ = workspace.update(Message::IbmDevice(window, selected));

            let pending = workspace.windows[&window].pending_ibm.as_ref().unwrap();
            assert_eq!(pending.kind, selected);
            assert_eq!(pending.address, "127.0.0.1:23");
            assert!(pending.connection().is_none());
            let _ = workspace.update(Message::IbmPlaintext(window, true));
            assert!(
                workspace.windows[&window]
                    .pending_ibm
                    .as_ref()
                    .unwrap()
                    .connection()
                    .is_some()
            );
        }
    }

    #[test]
    fn ibm_device_name_requires_fresh_consent_and_reaches_the_5250_peer() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        let _ = workspace.update(Message::ShowIbm(window, IbmKind::Tn5250, None));
        let _ = workspace.update(Message::IbmAddress(window, address.to_string()));
        let _ = workspace.update(Message::IbmDeviceName(window, "12INVALID".into()));
        let _ = workspace.update(Message::IbmPlaintext(window, true));
        assert!(
            workspace.windows[&window]
                .pending_ibm
                .as_ref()
                .unwrap()
                .connection()
                .is_none()
        );
        let _ = workspace.update(Message::IbmDeviceName(window, " desk_01 ".into()));
        assert!(
            workspace.windows[&window]
                .pending_ibm
                .as_ref()
                .unwrap()
                .connection()
                .is_none()
        );
        let _ = workspace.update(Message::IbmPlaintext(window, true));

        let peer = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            peer.write_all(b"\xff\xfd\x27\xff\xfa\x27\x01\x03DEVNAME\xff\xf0")
                .unwrap();
            let expected = b"\xff\xfb\x27\xff\xfa\x27\x00\x03DEVNAME\x01DESK_01\xff\xf0";
            let mut reply = vec![0; expected.len()];
            peer.read_exact(&mut reply).unwrap();
            assert_eq!(reply, expected);
        });
        let _ = workspace.update(Message::ConnectIbm(window));
        assert!(workspace.windows[&window].pending_ibm.is_none());
        peer.join().unwrap();
    }

    #[test]
    fn ibm_tab_requires_fresh_consent_survives_transfer_and_restores_offline() {
        use std::io::{self, Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        const TIMEOUT: Duration = Duration::from_secs(5);
        fn exchange(peer: &mut TcpStream, bytes: &[u8], expected: &[u8]) {
            peer.write_all(bytes).unwrap();
            let mut received = vec![0; expected.len()];
            peer.read_exact(&mut received).unwrap();
            assert_eq!(received, expected);
        }
        fn wait_for(session: &Session, condition: impl Fn(&Frame) -> bool) {
            let deadline = Instant::now() + TIMEOUT;
            loop {
                let frame = session.frame();
                if condition(&frame) {
                    return;
                }
                assert!(
                    !matches!(frame.status, Status::Error(_)),
                    "{:?}",
                    frame.status
                );
                assert!(Instant::now() < deadline, "missing IBM session update");
                thread::sleep(Duration::from_millis(10));
            }
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let mut workspace = Workspace::default();
        let (from, _) = workspace.open_window(false);
        let (to, _) = workspace.open_window(false);
        let _ = workspace.update(Message::ShowIbm(from, IbmKind::Tn3270e, None));
        let _ = workspace.update(Message::IbmAddress(from, address.clone()));
        let _ = workspace.update(Message::ConnectIbm(from));
        assert!(workspace.sessions.is_empty());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let _ = workspace.update(Message::IbmPlaintext(from, true));
        let _ = workspace.update(Message::IbmAddress(from, format!(" {address}")));
        let _ = workspace.update(Message::ConnectIbm(from));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let (completed, replied) = mpsc::channel();
        let peer = thread::spawn(move || {
            let deadline = Instant::now() + TIMEOUT;
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("IBM loopback dial failed: {error}"),
                }
            };
            peer.set_nonblocking(false).unwrap();
            peer.set_read_timeout(Some(TIMEOUT)).unwrap();
            peer.set_write_timeout(Some(TIMEOUT)).unwrap();
            exchange(&mut peer, &[255, 253, 40], &[255, 251, 40]);
            exchange(
                &mut peer,
                &[255, 250, 40, 8, 2, 255, 240],
                &[&[255, 250, 40, 2, 7], b"IBM-3278-2".as_slice(), &[255, 240]].concat(),
            );
            exchange(
                &mut peer,
                &[
                    &[255, 250, 40, 2, 4],
                    b"IBM-3278-2".as_slice(),
                    &[1],
                    b"LU01",
                    &[255, 240],
                ]
                .concat(),
                &[255, 250, 40, 3, 7, 255, 240],
            );
            peer.write_all(&[255, 250, 40, 3, 4, 255, 240]).unwrap();
            exchange(
                &mut peer,
                &[0, 0, 0, 0, 0, 0xf5, 0xc2, 0x1d, 0x4c, 0x13, 255, 239],
                &[
                    0, 0, 0, 0, 0, 0x7d, 0x40, 0xc2, 0x11, 0x40, 0xc1, 0xe7, 255, 239,
                ],
            );
            completed.send(()).unwrap();
        });

        let _ = workspace.update(Message::IbmPlaintext(from, true));
        let _ = workspace.update(Message::ConnectIbm(from));
        let tab = workspace.windows[&from].tab().unwrap();
        let tab_id = tab.id;
        let session = tab.focused_session().unwrap();
        wait_for(&workspace.sessions[&session].session, |frame| {
            frame.screen.cells[1].style.hidden
        });
        let _ = workspace.update(Message::SessionUpdate(session));
        let _ = workspace.update(Message::ToggleBroadcast(from));
        assert!(!workspace.windows[&from].tab().unwrap().broadcast);
        let _ = workspace.key(
            from,
            Key::Character("X".into()),
            Location::Standard,
            Modifiers::empty(),
            Some("X"),
        );
        wait_for(&workspace.sessions[&session].session, |frame| {
            frame.screen.cursor.column == 2
        });
        let frame = workspace.sessions[&session].session.frame();
        assert!(frame.screen.cells[1].style.hidden);
        assert!(frame.screen.cells[1].text.is_empty());

        let _ = workspace.transfer(from, Some(to), tab_id);
        assert_eq!(
            workspace.windows[&to].tab().unwrap().focused_session(),
            Some(session)
        );
        let _ = workspace.key(
            to,
            Key::Named(Named::Enter),
            Location::Standard,
            Modifiers::empty(),
            None,
        );
        replied.recv_timeout(TIMEOUT).unwrap();
        peer.join().unwrap();

        let saved = workspace.capture_layout().unwrap();
        let encoded = serde_json::to_string(&saved).unwrap();
        assert!(!encoded.contains(&address));
        assert!(!encoded.contains("plaintext"));
        let _ = workspace.restore_layout(saved).expect("restore IBM layout");
        let restored = workspace
            .sessions
            .values()
            .find(|pane| pane.frame.status == Status::Disconnected)
            .unwrap();
        assert_eq!(restored.frame.ibm.as_ref().unwrap().kind, IbmKind::Tn3270e);
        assert!(restored.session.stop().is_closed());
        assert_eq!(
            restored.session.send_ibm(IbmInput::Text("X".into())),
            Err(InputError::Closed)
        );
    }

    #[test]
    fn a_dirty_editor_can_cancel_and_resume_app_quit() {
        use iced::widget::text_editor::{Action, Edit};

        let mut workspace = Workspace::default();
        let _ = workspace.open_window(false);
        let (editor, _) = window::open(window::Settings::default());
        workspace.editors.insert(editor, EditorWindow::new());
        let _ = workspace.update(Message::Editor(
            editor,
            editor_window::Message::Edit(Action::Edit(Edit::Insert('x'))),
        ));
        let _ = workspace.quit();
        assert!(!workspace.exiting);
        assert_eq!(workspace.quit_waiting_for, Some(editor));

        let _ = workspace.update(Message::Editor(
            editor,
            editor_window::Message::DecideDiscard(false),
        ));
        assert!(workspace.editors.contains_key(&editor));
        assert!(workspace.quit_waiting_for.is_none());
        assert!(!workspace.exiting);

        let _ = workspace.quit();
        let _ = workspace.update(Message::Editor(
            editor,
            editor_window::Message::DecideDiscard(true),
        ));
        assert!(!workspace.editors.contains_key(&editor));
        assert!(workspace.exiting);
    }

    pub(super) fn describe_test_pty(session: &Session, frame: &crate::session::Frame) -> String {
        let screen = &frame.screen;
        let lines: Vec<_> = (0..screen.rows)
            .rev()
            .filter_map(|row| {
                let line: String = screen
                    .row(row)?
                    .iter()
                    .flat_map(|cell| cell.text.chars())
                    .take(160)
                    .collect();
                (!line.trim().is_empty()).then(|| (row, line.trim_end().to_owned()))
            })
            .take(6)
            .collect();
        let last_read = session.take_output_activity().last.map(|at| at.elapsed());
        let pid = session.local_shell_pid();
        let state = format!(
            "status: {:?}, pid: {pid:?}, revision: {}, last read age: {last_read:?}, \
             grid: {}x{}, scrollback: {}, offset: {}, title: {:?}, recent rows: {lines:?}",
            frame.status,
            frame.search_revision,
            screen.columns,
            screen.rows,
            screen.scrollback_lines,
            frame.scroll_offset,
            screen.title
        );
        #[cfg(target_os = "macos")]
        if let Some(pid) = pid
            && let Ok(output) = std::process::Command::new("/bin/ps")
                .args(["-axo", "pid=,ppid=,pgid=,tty=,stat=,wchan=,comm="])
                .output()
        {
            let processes = String::from_utf8_lossy(&output.stdout);
            let related: Vec<_> = processes
                .lines()
                .filter(|line| {
                    line.split_whitespace()
                        .take(2)
                        .any(|field| field.parse::<u32>() == Ok(pid))
                })
                .map(str::trim)
                .collect();
            return format!("{state}, processes (pid ppid pgid tty stat wchan comm): {related:?}");
        }
        state
    }

    fn wait_for_terminal_line(session: &Session, marker: &str) {
        use std::thread;
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let frame = session.frame();
            let screen = &frame.screen;
            if (0..screen.rows).any(|row| {
                screen.row(row).is_some_and(|cells| {
                    cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        == marker
                })
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing shell output {marker} ({})",
                describe_test_pty(session, &frame),
            );
            thread::sleep(Duration::from_millis(15));
        }
    }

    #[test]
    fn command_timestamp_default_applies_only_to_new_tabs_and_moves_with_them() {
        let mut workspace = Workspace::default();
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        // Test-only pane IDs let the model exercise ownership without launching shells.
        workspace.add_tab(source, SessionId(80));
        let original = workspace.windows[&source].tab().unwrap().id;
        assert!(!workspace.windows[&source].tab().unwrap().command_timestamps);
        workspace.window_settings.command_timestamps_enabled = true;
        workspace.add_tab(source, SessionId(81));
        let next = workspace.windows[&source].tab().unwrap().id;
        assert!(workspace.windows[&source].tab().unwrap().command_timestamps);
        workspace.add_configured_tab(source, SessionId(82), false, Some(false), None);
        assert!(!workspace.windows[&source].tab().unwrap().command_timestamps);

        let _ = workspace.update(Message::SelectTab(source, original));
        assert!(!workspace.windows[&source].tab().unwrap().command_timestamps);
        let _ = workspace.update(Message::ToggleCommandTimestamps(source));
        let _ = workspace.update(Message::ToggleCommandTimestamps(source));
        assert_eq!(workspace.windows[&source].timestamp_width_delta, Some(0));
        let _ = workspace.update(Message::ResizeCommandTimestamps(source, None));
        assert!(workspace.windows[&source].timestamp_width_delta.is_none());

        let _ = workspace.transfer(source, Some(destination), next);
        assert!(
            workspace.windows[&destination]
                .tab()
                .unwrap()
                .command_timestamps
        );
        assert!(!workspace.windows[&source].tab().unwrap().command_timestamps);
    }

    #[cfg(unix)]
    #[test]
    fn multi_exec_routes_live_keys_across_windows_and_keeps_passwords_out_of_plain_panes() {
        use multi_exec_access::Action;

        let mut workspace = Workspace::default();
        let open = |workspace: &mut Workspace, prompt: &str, label: &str| {
            let (owner, _) = workspace.open_window(false);
            workspace.new_tab(owner, TerminalProfile::Xterm);
            let id = workspace.windows[&owner]
                .tab()
                .unwrap()
                .focused_session()
                .unwrap();
            let session = &workspace.sessions[&id].session;
            session.send(format!(
                "stty -echo; printf '%s' '{prompt}'; IFS= read -r val; printf '\\n{label}=%s\\n' \"$val\"; stty echo\r"
            ).into_bytes()).unwrap();
            wait_for_terminal_line(session, prompt);
            let _ = workspace.update(Message::MultiExec(Some(owner), Action::Pane));
            assert!(workspace.multi_exec_checked(owner, Action::Pane));
            (owner, id)
        };
        let (source, source_pane) = open(&mut workspace, "[sudo] password for test:", "SOURCE");
        let (_, plain) = open(&mut workspace, "Plain input>", "PLAIN");
        let (_, protected) = open(&mut workspace, "Kennwort:", "PROTECTED");

        assert!(
            workspace.sessions[&source_pane]
                .session
                .frame()
                .password_prompt
        );
        assert!(
            workspace.sessions[&protected]
                .session
                .frame()
                .password_prompt
        );
        assert!(!workspace.sessions[&plain].session.frame().password_prompt);
        let _ = workspace.key(
            source,
            Key::Character("q".into()),
            Location::Standard,
            Modifiers::empty(),
            Some("q"),
        );
        let _ = workspace.key(
            source,
            Key::Named(Named::Enter),
            Location::Standard,
            Modifiers::empty(),
            None,
        );
        wait_for_terminal_line(&workspace.sessions[&source_pane].session, "SOURCE=q");
        wait_for_terminal_line(&workspace.sessions[&protected].session, "PROTECTED=q");
        assert!(
            workspace.sessions[&source_pane]
                .session
                .take_output_activity()
                .mirrored
                .is_none()
        );
        assert!(
            workspace.sessions[&protected]
                .session
                .take_output_activity()
                .mirrored
                .is_some()
        );
        let plain = &workspace.sessions[&plain].session;
        plain.send(b"z\r".to_vec()).unwrap();
        wait_for_terminal_line(plain, "PLAIN=z");
        assert!(plain.take_output_activity().mirrored.is_none());
        let _ = workspace.update(Message::MultiExec(None, Action::Stop));
        assert!(workspace.multi_exec.members().is_empty());
        let ids: Vec<_> = workspace.sessions.keys().copied().collect();
        for id in ids {
            workspace.stop_session(id);
        }
        for worker in workspace.pending_shutdown {
            let _ = worker.recv_blocking();
        }
    }

    #[cfg(unix)]
    #[test]
    fn broadcast_keeps_a_second_pty_responsive_when_the_first_input_queue_is_full() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(true);
        let stalled = workspace.windows[&window]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        let _ = workspace.split(window, pane_grid::Axis::Vertical);
        workspace.wait_for_test_split(window);
        let ready = workspace.windows[&window]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        let first = &workspace.sessions[&stalled].session;
        first.send(b"stty -echo -icanon min 1 time 0; printf '%s%s\\n' 'kortty_blocked_' 'ready'; sleep 12\r".to_vec()).unwrap();
        wait_for_terminal_line(first, "kortty_blocked_ready");

        // The real PTY has a process that does not read. Saturate its writer
        // before broadcasting the command to both sessions.
        let mut saturated = false;
        for _ in 0..256 {
            match first.send(vec![b'x'; 32 * 1024]) {
                Ok(()) => {}
                Err(InputError::Busy) => {
                    saturated = true;
                    break;
                }
                Err(error) => panic!("unexpected input failure: {error:?}"),
            }
        }
        assert!(saturated, "the first PTY input queue did not fill");

        let _ = workspace.update(Message::ToggleBroadcast(window));
        let mut targets = workspace
            .keyboard_targets(workspace.windows[&window].tab().unwrap())
            .unwrap()
            .panes;
        targets.sort_by_key(|target| *target != stalled);
        assert_eq!(targets, [stalled, ready]);
        workspace.send_to(
            window,
            &targets,
            InputSource::Keyboard {
                enter: true,
                predict: false,
                shifted_enter: false,
                physical_pane: None,
            },
            |_, _| {
                Some(PaneInput::Local(
                    b"printf '%s%s\\n' 'kortty_broadcast_' 'reached'\r".to_vec(),
                ))
            },
        );
        assert!(
            workspace.windows[&window]
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("input queue is full"))
        );
        wait_for_terminal_line(
            &workspace.sessions[&ready].session,
            "kortty_broadcast_reached",
        );
    }

    #[test]
    fn transfers_a_live_pty_without_restarting_it() {
        let mut workspace = Workspace::default();
        let (from, _) = workspace.open_window(true);
        let (to, _) = workspace.open_window(false);
        let tab = workspace.windows[&from].tab().expect("local shell tab");
        let tab_id = tab.id;
        let id = tab.focused_session().expect("focused shell");
        let shell = &workspace.sessions[&id].session;
        #[cfg(unix)]
        let before_command =
            b"kortty_transfer=preserved; printf '\\033_Ga=T,i=17,f=24,s=1,v=1,q=2,C=1;/wAA\\033\\\\%s%s\\n' 'kortty_' 'before'\r";
        #[cfg(windows)]
        let before_command =
            b"$kortty_transfer='preserved'; [Console]::WriteLine('kortty_'+'before')\r";
        shell
            .send(before_command.to_vec())
            .expect("send command before transfer");
        wait_for_terminal_line(shell, "kortty_before");
        #[cfg(unix)]
        let image = {
            let image = Arc::clone(&shell.frame().screen.images[0].placement.image);
            assert_eq!(image.id, 17);
            assert_eq!(image.rgba.as_ref(), &[255, 0, 0, 255]);
            image
        };

        let _ = workspace.transfer(from, Some(to), tab_id);
        assert_eq!(
            workspace.windows[&to].tab().and_then(Tab::focused_session),
            Some(id)
        );
        let shell = &workspace.sessions[&id].session;
        #[cfg(unix)]
        let after_command = b"printf 'after=%s\\n' \"$kortty_transfer\"\r";
        #[cfg(windows)]
        let after_command = b"[Console]::WriteLine('after='+$kortty_transfer)\r";
        shell
            .send(after_command.to_vec())
            .expect("send command after transfer");
        wait_for_terminal_line(shell, "after=preserved");
        #[cfg(unix)]
        assert!(
            shell
                .frame()
                .screen
                .images
                .iter()
                .any(|placement| Arc::ptr_eq(&placement.placement.image, &image))
        );
    }
}
