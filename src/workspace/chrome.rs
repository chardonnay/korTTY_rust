//! The Java application's grouped icon toolbar and window menu structure.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use iced::theme::Base as _;
use iced::widget::{
    button, column, container, mouse_area, pane_grid, row, scrollable, space, text,
};
use iced::{Alignment, Color, Element, Length, Task, window};
use serde::{Deserialize, Serialize};

use crate::terminal_theme::{Rgb, Scheme};

use super::{
    IbmKind, Message as WorkspaceMessage, Window, Workspace, control_access, local_dock,
    multi_exec_access, project_access, recording_access, session_restore, session_security,
    settings_window, ssh_access, tab_titles::RemoteSidebarPosition,
    terminal_context::FontAdjustment,
};

mod about;
pub(super) mod connection_export;
pub(super) mod connection_import;
mod icons;
mod jobs_menu;
mod manual;
mod open_recent_menu;
mod pane_navigation;
pub(crate) mod popup;
mod power;
mod quick_select;
mod toolbar_drag;
mod toolbar_item_context;
mod toolbar_settings;
mod toolbar_strip;
mod toolbar_view;
use icons::Icon;
pub(crate) use icons::Icon as ToolIcon;
pub use toolbar_settings::Message as ToolbarMessage;
pub(super) use toolbar_settings::validate_backup as validate_toolbar_backup;
use toolbar_settings::{Catalog, Group, MAX_ICON, MIN_ICON, Row as ToolbarRow, Toolbar};

pub(super) const TOOLBAR_ICON_SIZE: RangeInclusive<u16> = MIN_ICON..=MAX_ICON;

const TOOLBAR: Color = Color::from_rgb8(32, 39, 46);
const TOOLBAR_HOVER: Color = Color::from_rgb8(48, 59, 69);
const MENU_BG: Color = Color::from_rgb8(46, 46, 46);
const MENU_HOVER: Color = Color::from_rgb8(65, 65, 65);
const FOREGROUND: Color = Color::from_rgb8(228, 232, 237);
const MUTED: Color = Color::from_rgb8(155, 160, 165);
const MENU_BAR_SHOWN: &str = "Menu bar shown";

#[derive(Clone, Copy)]
struct ChromeColors {
    toolbar: Color,
    toolbar_hover: Color,
    menu: Color,
    menu_hover: Color,
    foreground: Color,
    muted: Color,
    icon: Option<Rgb>,
}

impl ChromeColors {
    const DEFAULT: Self = Self {
        toolbar: TOOLBAR,
        toolbar_hover: TOOLBAR_HOVER,
        menu: MENU_BG,
        menu_hover: MENU_HOVER,
        foreground: FOREGROUND,
        muted: MUTED,
        icon: None,
    };

    fn from_scheme(scheme: Scheme) -> Self {
        Self::themed(
            scheme.background.color(),
            scheme.foreground.color(),
            Some(scheme.foreground),
        )
    }

    fn from_design(palette: super::appearance::Palette) -> Self {
        Self {
            muted: palette.muted.color(),
            ..Self::themed(
                palette.background.color(),
                palette.foreground.color(),
                Some(palette.foreground),
            )
        }
    }

    fn themed(background: Color, foreground: Color, icon: Option<Rgb>) -> Self {
        let luminance = 0.299 * background.r + 0.587 * background.g + 0.114 * background.b;
        let blend = if luminance < 0.5 {
            Color::WHITE
        } else {
            Color::BLACK
        };
        let brightness = if background.r.max(background.g).max(background.b) > 0.5 {
            0.92
        } else {
            1.3
        };
        let adjust = |channel: f32| (channel * brightness).min(1.0);
        Self {
            toolbar: background,
            toolbar_hover: Color::from_rgb(
                adjust(background.r),
                adjust(background.g),
                adjust(background.b),
            ),
            menu: mix(background, blend, 0.08),
            menu_hover: mix(background, blend, 0.15),
            foreground,
            muted: mix(foreground, background, 0.45),
            icon,
        }
    }
}

fn mix(left: Color, right: Color, ratio: f32) -> Color {
    Color::from_rgb(
        left.r + (right.r - left.r) * ratio,
        left.g + (right.g - left.g) * ratio,
        left.b + (right.b - left.b) * ratio,
    )
}

#[derive(Default)]
pub(super) struct Chrome {
    menu: Option<OpenMenu>,
    quick_select: quick_select::Controller,
    dialog: Option<about::Dialog>,
    toolbar: Toolbar,
    toolbar_overflow: Option<OpenToolbarOverflow>,
    hidden_menu_bars: HashSet<window::Id>,
    manual: manual::State,
    power: power::State,
    connection_import: Option<connection_import::Dialog>,
    connection_export: Option<connection_export::Dialog>,
    open_recent: std::cell::RefCell<open_recent_menu::Rows>,
}

#[derive(Clone, Copy)]
struct OpenMenu {
    window: window::Id,
    category: Category,
    branch: Option<Branch>,
    selected: Option<Choice>,
    jobs_context: bool,
}

#[derive(Clone, Copy)]
struct OpenToolbarOverflow {
    window: window::Id,
    group: Group,
    first_hidden: usize,
    selected: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    File,
    Edit,
    Connections,
    Configuration,
    Tools,
    Ai,
    Teamwork,
    Plugins,
    View,
    Help,
    Jobs,
    Toolbar,
    StatusRecovery,
}

#[derive(Clone, Copy, Debug)]
pub enum Navigation {
    Previous,
    Next,
    Left,
    Right,
    Activate,
    Escape,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u16)]
pub enum Action {
    QuickConnect,
    LocalShell,
    IncognitoShell,
    NewWindow,
    CloseTab,
    CloseWindow,
    OpenProject,
    SaveProject,
    ManageProjects,
    SaveLayout,
    RestoreLayout,
    RestoreSession,
    Quit,
    CopySelection,
    Paste,
    Find,
    ManageConnections,
    Ssh,
    IncognitoSsh,
    Sftp,
    Serial,
    Tn3270,
    Tn5250,
    Settings,
    WindowSettings,
    Clipboard,
    Vault,
    Isolation,
    ControlApi,
    Snippets,
    Scheduler,
    ToggleRecording,
    Recordings,
    Editor,
    Diagrams,
    FilesLeft,
    FilesRight,
    SplitRight,
    SplitDown,
    ClosePane,
    ZoomPane,
    MoveTab,
    Broadcast,
    AudibleBell,
    TabTitles,
    PromptJumps,
    About,
    TerminalSettings,
    FontSettings,
    CustomizeToolbar,
    ToggleActionToolbar,
    ToggleConnectionToolbar,
    KnownHosts,
    LogfileManager,
    SessionProcesses,
    KeyboardSettings,
    ThemeSettings,
    ColorSettings,
    Fullscreen,
    SplitPane,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneAbove,
    FocusPaneBelow,
    NextPane,
    PreviousPane,
    CommandPalette,
    SnippetPalette,
    NextTab,
    PreviousTab,
    Dashboard,
    TerminalOnlyFullscreen,
    ClearBuffer,
    PreviousPrompt,
    NextPrompt,
    SelectLastOutput,
    CopyLastOutput,
    RenameTab,
    CloseOtherTabs,
    CloseTabsToRight,
    CloseAllTabs,
    ReopenClosedTab,
    RemoteSidebarLeft,
    RemoteSidebarRight,
    RemoteSidebarToggle,
    SshKeys,
    CreateBackup,
    ImportBackup,
    OpenPgpRecipients,
    NewSnippetManagerWindow,
    SessionJournals,
    ToggleSessionJournal,
    SessionJournalScreenshot,
    TerminalEffectsManager,
    EffectNone,
    EffectAmber,
    EffectCommodore,
    EffectRadar,
    EffectRain,
    EffectHud,
    EffectMother,
    EffectNeon,
    EffectPoltergeist,
    EffectSynthwave,
    EffectTypewriter,
    EffectVhs,
    JournalDockLeft,
    JournalDockRight,
    ToggleJournalDock,
    MultiExecIncludePane,
    MultiExecIncludeTab,
    MultiExecIncludeWindow,
    MultiExecStop,
    ShowCommandTimestamps,
    ShowMenuBar,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    AsciiArt,
    SftpHere,
    SendSerialBreak,
    AppearanceSettings,
    PowerShell,
    CommandPrompt,
    HideTerminalScrollbarsFullscreen,
    LoggingSettings,
    QuickSelect,
    UnlockVault,
    Manual,
    PreventSystemSleep,
    ImportConnections,
    AiManager,
    ExportConnections,
}

#[derive(Clone, Copy)]
enum Entry {
    Action(Action),
    Submenu(Branch),
    Unavailable(&'static str),
    BackgroundTransparency,
    Separator,
}

use Action::*;
use Entry::{Action as Item, Separator, Submenu};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Branch {
    Security,
    FileBrowser,
    Panes,
    RemoteSidebar,
    RecentlyClosed,
    OpenRecent,
    TerminalEffects,
    LiveJournal,
    MultiExec,
}

impl Branch {
    #[cfg(target_os = "macos")]
    const ALL: [Self; 9] = [
        Self::Security,
        Self::FileBrowser,
        Self::Panes,
        Self::RemoteSidebar,
        Self::RecentlyClosed,
        Self::OpenRecent,
        Self::TerminalEffects,
        Self::LiveJournal,
        Self::MultiExec,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Security => "Security",
            Self::FileBrowser => "File Browser",
            Self::Panes => "Panes",
            Self::RemoteSidebar => "Remote Files Sidebar",
            Self::RecentlyClosed => "Recently Closed",
            Self::OpenRecent => "Open Recent",
            Self::TerminalEffects => "Terminal Effect",
            Self::LiveJournal => "Live Journal",
            Self::MultiExec => "Multi-exec",
        }
    }

    fn menu_path(self) -> &'static str {
        match self {
            Self::Security => "Configuration › Security",
            Self::FileBrowser => "View › File Browser",
            Self::Panes => "View › Panes",
            Self::RemoteSidebar => "View › Remote Files Sidebar",
            Self::RecentlyClosed => "File › Recently Closed",
            Self::OpenRecent => "File › Open Recent",
            Self::TerminalEffects => "View › Terminal Effect",
            Self::LiveJournal => "View › Live Journal",
            Self::MultiExec => "View › Multi-exec",
        }
    }

    #[cfg(target_os = "macos")]
    fn category(self) -> Category {
        match self {
            Self::Security => Category::Configuration,
            Self::FileBrowser
            | Self::Panes
            | Self::RemoteSidebar
            | Self::TerminalEffects
            | Self::LiveJournal
            | Self::MultiExec => Category::View,
            Self::RecentlyClosed | Self::OpenRecent => Category::File,
        }
    }

    fn entries(self) -> &'static [Entry] {
        match self {
            Self::Security => &[
                Item(UnlockVault),
                Separator,
                Item(Vault),
                Item(OpenPgpRecipients),
                Item(SshKeys),
                Item(KnownHosts),
                Separator,
                Item(Isolation),
            ],
            Self::FileBrowser => &[Item(FilesLeft), Item(FilesRight)],
            Self::RemoteSidebar => &[
                Item(RemoteSidebarLeft),
                Item(RemoteSidebarRight),
                Separator,
                Item(RemoteSidebarToggle),
            ],
            Self::RecentlyClosed | Self::OpenRecent => &[],
            Self::LiveJournal => &[
                Item(JournalDockLeft),
                Item(JournalDockRight),
                Separator,
                Item(ToggleJournalDock),
            ],
            Self::TerminalEffects => &[
                Item(EffectNone),
                Separator,
                Item(EffectAmber),
                Item(EffectCommodore),
                Item(EffectRadar),
                Item(EffectRain),
                Item(EffectHud),
                Item(EffectMother),
                Item(EffectNeon),
                Item(EffectPoltergeist),
                Item(EffectSynthwave),
                Item(EffectTypewriter),
                Item(EffectVhs),
            ],
            Self::Panes => &[
                Item(SplitPane),
                Item(SplitRight),
                Item(SplitDown),
                Item(ClosePane),
                Separator,
                Item(FocusPaneLeft),
                Item(FocusPaneRight),
                Item(FocusPaneAbove),
                Item(FocusPaneBelow),
                Separator,
                Item(NextPane),
                Item(PreviousPane),
                Separator,
                Item(ZoomPane),
                Item(Broadcast),
            ],
            Self::MultiExec => &[
                Item(MultiExecIncludePane),
                Item(MultiExecIncludeTab),
                Item(MultiExecIncludeWindow),
                Separator,
                Item(MultiExecStop),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Choice {
    Action(Action),
    Branch(Branch),
    RecentlyClosed(u64),
    ClearRecentHistory,
    OpenRecent(u64),
    CancelJob(u64),
}

impl OpenMenu {
    fn root(window: window::Id, category: Category) -> Self {
        Self {
            window,
            category,
            branch: None,
            selected: None,
            jobs_context: false,
        }
    }

    fn return_to_parent(&mut self) {
        self.selected = self.branch.take().map(Choice::Branch);
    }
}

impl Category {
    const ALL: [Self; 11] = [
        Self::File,
        Self::Edit,
        Self::Connections,
        Self::Configuration,
        Self::Tools,
        Self::Ai,
        Self::Teamwork,
        Self::Plugins,
        Self::View,
        Self::Help,
        Self::Jobs,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Edit => "Edit",
            Self::Connections => "Connections",
            Self::Configuration => "Configuration",
            Self::Tools => "Tools",
            Self::Ai => "AI",
            Self::Teamwork => "Teamwork",
            Self::Plugins => "Plugins",
            Self::View => "View",
            Self::Help => "Help",
            Self::Jobs => "Jobs: none",
            Self::Toolbar => "Toolbar",
            Self::StatusRecovery => "Status Bar",
        }
    }

    fn entries(self) -> &'static [Entry] {
        match self {
            Self::File => &[
                Item(QuickConnect),
                Item(IncognitoShell),
                Item(RenameTab),
                Item(CloseTab),
                Item(CloseOtherTabs),
                Item(CloseTabsToRight),
                Item(CloseAllTabs),
                Item(ReopenClosedTab),
                Submenu(Branch::RecentlyClosed),
                Separator,
                Item(NewWindow),
                Item(CloseWindow),
                Separator,
                Item(OpenProject),
                Submenu(Branch::OpenRecent),
                Item(SaveProject),
                Item(ManageProjects),
                Item(SaveLayout),
                Item(RestoreLayout),
                Item(RestoreSession),
                Separator,
                Item(CreateBackup),
                Item(ImportBackup),
                Separator,
                Item(Quit),
            ],
            Self::Edit => &[
                Item(CopySelection),
                Item(Paste),
                Separator,
                Item(Find),
                Item(QuickSelect),
                Separator,
                Item(PreviousPrompt),
                Item(NextPrompt),
                Item(SelectLastOutput),
                Item(CopyLastOutput),
            ],
            Self::Connections => &[
                Item(QuickConnect),
                Item(ManageConnections),
                Separator,
                Item(ImportConnections),
                Item(ExportConnections),
                Separator,
                Item(Sftp),
                Item(SftpHere),
                Separator,
                Item(SendSerialBreak),
                Separator,
                Item(LocalShell),
                Item(Ssh),
                Item(IncognitoSsh),
                Item(Serial),
                Item(Tn3270),
                Item(Tn5250),
            ],
            Self::Configuration => &[
                Submenu(Branch::Security),
                Separator,
                Item(Settings),
                Item(WindowSettings),
                Item(Clipboard),
                Separator,
                Item(ControlApi),
                Separator,
                Item(PreventSystemSleep),
            ],
            Self::Tools => &[
                Item(Snippets),
                Item(NewSnippetManagerWindow),
                Separator,
                Item(Scheduler),
                Separator,
                Item(Recordings),
                Item(ToggleRecording),
                Separator,
                Item(LogfileManager),
                Separator,
                Item(SessionJournals),
                Item(ToggleSessionJournal),
                Item(SessionJournalScreenshot),
                Separator,
                Item(SessionProcesses),
                Separator,
                Item(AsciiArt),
                Separator,
                Item(Editor),
                Item(Diagrams),
            ],
            Self::View => &[
                Item(CommandPalette),
                Item(SnippetPalette),
                Separator,
                Item(Dashboard),
                Item(ShowCommandTimestamps),
                Item(ShowMenuBar),
                Separator,
                Item(ToggleActionToolbar),
                Item(ToggleConnectionToolbar),
                Item(CustomizeToolbar),
                Separator,
                Submenu(Branch::FileBrowser),
                Submenu(Branch::LiveJournal),
                Submenu(Branch::RemoteSidebar),
                Separator,
                Item(ZoomIn),
                Item(ZoomOut),
                Item(ResetZoom),
                Entry::BackgroundTransparency,
                Separator,
                Submenu(Branch::Panes),
                Submenu(Branch::MultiExec),
                Item(MoveTab),
                Separator,
                Item(AudibleBell),
                Item(TabTitles),
                Item(PromptJumps),
                Submenu(Branch::TerminalEffects),
                Separator,
                Item(Fullscreen),
                Item(TerminalOnlyFullscreen),
                Item(HideTerminalScrollbarsFullscreen),
            ],
            Self::Help => &[Item(Manual), Separator, Item(About)],
            Self::Jobs => &[Item(Scheduler)],
            Self::Toolbar => &[
                Item(CustomizeToolbar),
                Separator,
                Item(ToggleActionToolbar),
                Item(ToggleConnectionToolbar),
                Item(ShowMenuBar),
            ],
            Self::StatusRecovery => &[
                Item(ShowMenuBar),
                Item(ToggleActionToolbar),
                Item(ToggleConnectionToolbar),
            ],
            Self::Ai => &[
                Item(AiManager),
                Entry::Unavailable("Saved Chats…"),
                Entry::Unavailable("AI Agent…"),
                Entry::Unavailable("AI Planning…"),
                Entry::Unavailable("AI Swarm…"),
            ],
            Self::Teamwork => &[Entry::Unavailable("Teamwork Settings…")],
            Self::Plugins => &[
                Item(TerminalEffectsManager),
                Entry::Unavailable("AI Agent Design…"),
            ],
        }
    }

    fn available(self) -> bool {
        !self.entries().is_empty()
    }
}

impl Action {
    pub(super) fn font_zoom(self) -> Option<FontAdjustment> {
        match self {
            ZoomIn => Some(FontAdjustment::Increase),
            ZoomOut => Some(FontAdjustment::Decrease),
            ResetZoom => Some(FontAdjustment::Reset),
            _ => None,
        }
    }

    fn effect_choice(self) -> Option<Option<crate::terminal_effects::Effect>> {
        use crate::terminal_effects::Effect;
        Some(Some(match self {
            EffectNone => return Some(None),
            EffectAmber => Effect::AmberCrt90,
            EffectCommodore => Effect::CommodoreHeritage,
            EffectRadar => Effect::DeepSpaceRadar,
            EffectRain => Effect::DigitalRain,
            EffectHud => Effect::HologramHud,
            EffectMother => Effect::Mother,
            EffectNeon => Effect::NeonCity,
            EffectPoltergeist => Effect::Poltergeist,
            EffectSynthwave => Effect::SynthwaveHorizon,
            EffectTypewriter => Effect::TypewriterNoir,
            EffectVhs => Effect::Vhs1987,
            _ => return None,
        }))
    }

    pub(super) fn is_split_navigation(self) -> bool {
        matches!(
            self,
            ZoomPane
                | FocusPaneLeft
                | FocusPaneRight
                | FocusPaneAbove
                | FocusPaneBelow
                | NextPane
                | PreviousPane
        )
    }

    fn multi_exec_action(self) -> Option<multi_exec_access::Action> {
        use multi_exec_access::Action;

        match self {
            MultiExecIncludePane => Some(Action::Pane),
            MultiExecIncludeTab => Some(Action::Tab),
            MultiExecIncludeWindow => Some(Action::Window),
            MultiExecStop => Some(Action::Stop),
            _ => None,
        }
    }

    pub(super) fn shortcut_catalog() -> impl Iterator<Item = (Self, &'static str, &'static str)> {
        Category::ALL
            .into_iter()
            .flat_map(|category| {
                category.entries().iter().flat_map(move |entry| {
                    let (entries, section) = match *entry {
                        Submenu(branch) => (branch.entries(), branch.menu_path()),
                        _ => (std::slice::from_ref(entry), category.title()),
                    };
                    entries.iter().filter_map(move |entry| match *entry {
                        Item(action) => Some((action, action.label(category), section)),
                        _ => None,
                    })
                })
            })
            .chain([
                (NextTab, NextTab.title(), "Tabs"),
                (PreviousTab, PreviousTab.title(), "Tabs"),
                (ClearBuffer, ClearBuffer.title(), "Terminal"),
            ])
            .chain(
                [PowerShell, CommandPrompt]
                    .into_iter()
                    .filter(|_| cfg!(windows))
                    .map(|action| (action, action.title(), "Terminal")),
            )
    }

    fn title(self) -> &'static str {
        match self {
            QuickConnect => "New Tab",
            LocalShell => "New Local Shell",
            PowerShell => "New PowerShell",
            CommandPrompt => "New Command Prompt",
            IncognitoShell => "New Incognito Shell",
            NewWindow => "New Window",
            CloseTab => "Close Tab",
            CloseWindow => "Close Window",
            RenameTab => "Rename Tab…",
            CloseOtherTabs => "Close Other Tabs",
            CloseTabsToRight => "Close Tabs to the Right",
            CloseAllTabs => "Close All Tabs",
            ReopenClosedTab => "Reopen Closed Tab",
            OpenProject => "Open Project…",
            SaveProject => "Save Project…",
            ManageProjects => "Manage Projects…",
            SaveLayout => "Save Layout…",
            RestoreLayout => "Restore Layout in New Windows",
            RestoreSession => "Restore Previous Session",
            CreateBackup => "Create Backup…",
            ImportBackup => "Import Backup…",
            Quit => "Quit",
            CopySelection => "Copy",
            Paste => "Paste",
            Find => "Find…",
            QuickSelect => "Quick Select",
            ManageConnections => "Manage Connections…",
            ImportConnections => "Import…",
            ExportConnections => "Export Connections…",
            Ssh => "SSH…",
            IncognitoSsh => "Incognito SSH…",
            Sftp => "SFTP-Client…",
            SftpHere => "Open SFTP Here",
            SendSerialBreak => "Send Break",
            Serial => "Serial…",
            Tn3270 => "TN3270E…",
            Tn5250 => "TN5250…",
            Settings => "Global Settings…",
            WindowSettings => "Window Settings…",
            Clipboard => "Clipboard…",
            UnlockVault => "Unlock Vault…",
            Vault => "Credentials…",
            OpenPgpRecipients => "OpenPGP recipients…",
            SshKeys => "SSH-Keys…",
            KnownHosts => "Known Hosts…",
            Isolation => "Session Security…",
            ControlApi => "Control API…",
            Snippets => "Snippet Manager…",
            NewSnippetManagerWindow => "New Snippet Manager Window",
            Scheduler => "JobScheduler…",
            ToggleRecording => "Start/Stop Terminal Recording",
            Recordings => "Video Manager…",
            AiManager => "AI Manager…",
            LogfileManager => "Logfile Manager…",
            SessionProcesses => "Session Processes…",
            AsciiArt => "ASCII Art…",
            SessionJournals => "Session Journals…",
            ToggleSessionJournal => "Start/Stop Session Journal",
            SessionJournalScreenshot => "Add Journal Screenshot",
            TerminalEffectsManager => "Terminal Effects…",
            EffectNone => "None",
            EffectAmber => crate::terminal_effects::Effect::AmberCrt90.name(),
            EffectCommodore => crate::terminal_effects::Effect::CommodoreHeritage.name(),
            EffectRadar => crate::terminal_effects::Effect::DeepSpaceRadar.name(),
            EffectRain => crate::terminal_effects::Effect::DigitalRain.name(),
            EffectHud => crate::terminal_effects::Effect::HologramHud.name(),
            EffectMother => crate::terminal_effects::Effect::Mother.name(),
            EffectNeon => crate::terminal_effects::Effect::NeonCity.name(),
            EffectPoltergeist => crate::terminal_effects::Effect::Poltergeist.name(),
            EffectSynthwave => crate::terminal_effects::Effect::SynthwaveHorizon.name(),
            EffectTypewriter => crate::terminal_effects::Effect::TypewriterNoir.name(),
            EffectVhs => crate::terminal_effects::Effect::Vhs1987.name(),
            JournalDockLeft => "Dock Left",
            JournalDockRight => "Dock Right",
            ToggleJournalDock => "Show/Hide",
            MultiExecIncludePane => "Include This Pane",
            MultiExecIncludeTab => "Include All Panes of This Tab",
            MultiExecIncludeWindow => "Include All Terminals of This Window",
            MultiExecStop => "Stop Multi-exec",
            ShowCommandTimestamps => "Show Command Timestamps",
            ShowMenuBar => "Show Menu Bar",
            Editor => "Editor…",
            Diagrams => "Diagrams…",
            FilesLeft => "Show on Left",
            FilesRight => "Show on Right",
            RemoteSidebarLeft => "Dock Left",
            RemoteSidebarRight => "Dock Right",
            RemoteSidebarToggle => "Show/Hide Remote Files Sidebar",
            SplitPane => "Split Pane",
            SplitRight => "Split Right",
            SplitDown => "Split Down",
            ClosePane => "Close Pane",
            FocusPaneLeft => "Focus Pane Left",
            FocusPaneRight => "Focus Pane Right",
            FocusPaneAbove => "Focus Pane Above",
            FocusPaneBelow => "Focus Pane Below",
            NextPane => "Next Pane",
            PreviousPane => "Previous Pane",
            CommandPalette => "Command Palette…",
            SnippetPalette => "Snippet Palette…",
            NextTab => "Next Tab",
            PreviousTab => "Previous Tab",
            ZoomPane => "Zoom Pane",
            ZoomIn => "Zoom In",
            ZoomOut => "Zoom Out",
            ResetZoom => "Reset Zoom",
            MoveTab => "Move Tab…",
            Broadcast => "Broadcast to All Panes of This Tab",
            AudibleBell => "Audible Bell",
            TabTitles => "Shell Tab Titles",
            PromptJumps => "Prompt Jumps",
            Manual => "Manual",
            PreventSystemSleep => power::label(),
            About => about::TITLE,
            TerminalSettings => "Terminal Settings…",
            FontSettings => "Font",
            KeyboardSettings => "Keyboard",
            ThemeSettings => "Themes",
            ColorSettings => "Colors",
            AppearanceSettings => "Appearance",
            LoggingSettings => "Logging",
            Fullscreen => "Fullscreen",
            TerminalOnlyFullscreen => "Terminal-only Fullscreen",
            HideTerminalScrollbarsFullscreen => "Hide terminal scrollbars in fullscreen",
            Dashboard => "Dashboard",
            ClearBuffer => "Clear Buffer",
            PreviousPrompt => "Previous Prompt",
            NextPrompt => "Next Prompt",
            SelectLastOutput => "Select Last Output",
            CopyLastOutput => "Copy Last Output",
            CustomizeToolbar => "Customize toolbar…",
            ToggleActionToolbar => "Action toolbar",
            ToggleConnectionToolbar => "Connection toolbar",
        }
    }

    fn label(self, category: Category) -> &'static str {
        match (self, category) {
            (QuickConnect, Category::Connections) => "Quick Connect…",
            (ShowMenuBar, Category::Toolbar) => "Show/hide menu bar",
            _ => self.title(),
        }
    }

    fn remote_sidebar_placement(
        self,
        current: RemoteSidebarPosition,
    ) -> Option<RemoteSidebarPosition> {
        use RemoteSidebarPosition::{Hidden, Left, Right};
        let side = match self {
            RemoteSidebarLeft => Left,
            RemoteSidebarRight => Right,
            RemoteSidebarToggle if current == Hidden => Right,
            RemoteSidebarToggle => Hidden,
            _ => return None,
        };
        Some(if current == side { Hidden } else { side })
    }

    fn remote_sidebar_checked(self, position: RemoteSidebarPosition) -> bool {
        matches!(
            (self, position),
            (RemoteSidebarLeft, RemoteSidebarPosition::Left)
                | (RemoteSidebarRight, RemoteSidebarPosition::Right)
        )
    }

    #[cfg(target_os = "macos")]
    fn from_native(id: u16) -> Option<Self> {
        Self::shortcut_catalog().find_map(|(action, _, _)| (action as u16 == id).then_some(action))
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    About(window::Id, about::Event),
    ConnectionImport(window::Id, connection_import::Event),
    ConnectionExport(window::Id, connection_export::Event),
    Manual(window::Id, manual::Event),
    Power(power::Event),
    Toolbar(ToolbarMessage),
    OpenToolbarOverflow(window::Id, Group, usize),
    NavigateToolbarOverflow(window::Id, Group, Navigation),
    InvokeToolbarOverflow(window::Id, Group, usize),
    OpenMenu(window::Id, Category),
    OpenJobsContext(window::Id),
    OpenBranch(window::Id, Branch),
    PointAtRootAction(window::Id, Category, Action),
    Dismiss(window::Id),
    Navigate(window::Id, Navigation),
    Invoke(window::Id, Action),
    RestoreStatus(window::Id, Action),
    ReopenRecent(window::Id, u64),
    ClearRecentHistory(window::Id),
    OpenRecent(window::Id, u64),
    CancelJob(window::Id, u64),
    #[cfg(target_os = "macos")]
    CancelNativeJob(u64),
    #[cfg(target_os = "macos")]
    OpenRecentInNewWindow(window::Id, u64),
}

impl Chrome {
    pub(super) fn load() -> Self {
        Self {
            toolbar: Toolbar::load(),
            ..Self::default()
        }
    }

    pub(super) fn refresh_toolbar_connections(&mut self, store: &crate::connection_store::Store) {
        self.toolbar.refresh_connections(store);
    }

    pub(super) fn toolbar_icon_size(&self) -> u16 {
        self.toolbar.committed_icon_size()
    }

    pub(super) fn toolbar_save_pending(&self) -> bool {
        self.toolbar.pending()
    }

    pub(super) fn save_toolbar_icon_size(
        &mut self,
        owner: window::Id,
        dialog: window::Id,
        size: u16,
    ) -> Task<WorkspaceMessage> {
        self.toolbar.save_icon_size(owner, dialog, size)
    }

    pub(super) fn rename_toolbar_folder(
        &mut self,
        owner: window::Id,
        rename: &crate::connection_window::FolderRename,
    ) -> Task<WorkspaceMessage> {
        self.toolbar.rename_folder(owner, rename)
    }

    pub(super) fn title(&self, id: window::Id) -> Option<&'static str> {
        self.dialog
            .as_ref()
            .is_some_and(|dialog| dialog.id == id)
            .then_some(about::TITLE)
    }

    pub(super) fn dismiss(&mut self, id: window::Id) {
        if self.menu.as_ref().is_some_and(|menu| menu.window == id) {
            self.menu = None;
        }
        self.dismiss_toolbar_overflow(id);
        self.toolbar.dismiss_item_menu(id);
    }

    pub(super) fn dismiss_toolbar_overflow(&mut self, id: window::Id) {
        if self
            .toolbar_overflow
            .is_some_and(|overflow| overflow.window == id)
        {
            self.toolbar_overflow = None;
        }
    }

    pub(super) fn window_closed(&mut self, id: window::Id) -> Option<window::Id> {
        self.dismiss(id);
        self.hidden_menu_bars.remove(&id);
        self.toolbar.close(id);
        let dialog = self
            .dialog
            .take_if(|dialog| dialog.id == id || dialog.owner == id)?;
        (id == dialog.owner).then_some(dialog.id)
    }
}

impl Workspace {
    pub(super) fn toolbar_icon_preview(&self, size: u16) -> Element<'static, WorkspaceMessage> {
        container(Icon::Gear.widget(true, size, self.chrome_colors().icon))
            .center_x(80)
            .center_y(80)
            .into()
    }

    fn chrome_colors(&self) -> ChromeColors {
        if let Some(palette) = self.window_settings.appearance.design.palette() {
            ChromeColors::from_design(palette)
        } else {
            self.window_settings
                .themes
                .appearance()
                .map_or(ChromeColors::DEFAULT, ChromeColors::from_scheme)
        }
    }

    pub(super) fn update_chrome(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::About(id, event) => return self.update_about(id, event),
            Message::ConnectionImport(id, event) => {
                return self.update_connection_import(id, event);
            }
            Message::ConnectionExport(id, event) => {
                return self.update_connection_export(id, event);
            }
            Message::Manual(id, event) => return self.update_manual(id, event),
            Message::Power(event) => return self.update_sleep_prevention(event),
            Message::Toolbar(message) => return self.update_toolbar(message),
            Message::OpenToolbarOverflow(id, group, first_hidden) => {
                self.open_toolbar_overflow(id, group, first_hidden);
            }
            Message::NavigateToolbarOverflow(id, group, navigation) => {
                return self.navigate_toolbar_overflow(id, group, navigation);
            }
            Message::InvokeToolbarOverflow(id, group, index) => {
                return self.invoke_toolbar_overflow(id, group, index);
            }
            Message::OpenMenu(id, category)
                if self.windows.contains_key(&id)
                    && category.available()
                    && self.menu_surface_available(id, category) =>
            {
                self.dismiss_terminal_context(id);
                self.chrome.toolbar.dismiss_item_menu(id);
                self.chrome.toolbar_overflow = None;
                self.chrome.menu = Some(OpenMenu::root(id, category));
                if category == Category::File {
                    return self.refresh_open_recent();
                }
            }
            Message::OpenJobsContext(id)
                if self.windows.contains_key(&id)
                    && self.menu_surface_available(id, Category::Jobs) =>
            {
                let task = self.update_chrome(Message::OpenMenu(id, Category::Jobs));
                if let Some(menu) = &mut self.chrome.menu {
                    menu.jobs_context = true;
                }
                return task;
            }
            Message::OpenBranch(id, branch) => self.open_menu_branch(id, branch),
            Message::PointAtRootAction(id, category, action) => {
                if let Some(menu) = &mut self.chrome.menu
                    && menu.window == id
                    && menu.category == category
                    && menu.branch.is_some()
                {
                    menu.branch = None;
                    menu.selected = Some(Choice::Action(action));
                }
            }
            Message::Dismiss(id) => self.chrome.dismiss(id),
            Message::Navigate(id, navigation) => return self.navigate_chrome(id, navigation),
            Message::Invoke(id, action) => return self.invoke_chrome(id, action),
            Message::RestoreStatus(id, action) => return self.restore_status_chrome(id, action),
            Message::ReopenRecent(id, entry) => return self.invoke_recent_history(id, Some(entry)),
            Message::ClearRecentHistory(id) => return self.invoke_recent_history(id, None),
            Message::OpenRecent(id, entry) => return self.invoke_open_recent(id, entry),
            Message::CancelJob(owner, id) => return self.cancel_job_from_menu(owner, id),
            #[cfg(target_os = "macos")]
            Message::CancelNativeJob(id) => return self.cancel_menu_job(id),
            #[cfg(target_os = "macos")]
            Message::OpenRecentInNewWindow(id, entry) => {
                return self.open_recent_in_new_window(id, entry);
            }
            Message::OpenMenu(_, _) | Message::OpenJobsContext(_) => {}
        }
        Task::none()
    }

    fn menu_surface_available(&self, id: window::Id, category: Category) -> bool {
        match category {
            Category::Toolbar => true,
            Category::StatusRecovery => self.menu_bar_hidden(id) && self.palette_can_open(id),
            Category::Jobs => !self.menu_bar_hidden(id) && self.job_menu_title().is_some(),
            _ => !self.menu_bar_hidden(id),
        }
    }

    pub(super) fn menu_bar_hidden(&self, id: window::Id) -> bool {
        self.chrome.hidden_menu_bars.contains(&id)
    }

    pub(super) fn show_menu_bar(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.menu_bar_hidden(id) {
            return self.invoke_chrome(id, ShowMenuBar);
        }
        Task::none()
    }

    fn restore_status_chrome(&mut self, id: window::Id, action: Action) -> Task<WorkspaceMessage> {
        if !self.menu_surface_available(id, Category::StatusRecovery)
            || !self
                .chrome
                .menu
                .is_some_and(|menu| menu.window == id && menu.category == Category::StatusRecovery)
            || !self.chrome_enabled(id, action)
        {
            return Task::none();
        }
        self.chrome.dismiss(id);
        let layout = self.chrome.toolbar.layout(id);
        let row = match action {
            ShowMenuBar => return self.show_menu_bar(id),
            ToggleActionToolbar if !layout.actions_visible() => ToolbarRow::Actions,
            ToggleConnectionToolbar if !layout.visible(Group::Connections) => {
                ToolbarRow::Group(Group::Connections)
            }
            _ => return Task::none(),
        };
        self.update_toolbar(ToolbarMessage::Visible(id, row, true))
    }

    fn open_menu_branch(&mut self, id: window::Id, branch: Branch) {
        let Some(mut menu) = self.chrome.menu.filter(|menu| menu.window == id) else {
            return;
        };
        if menu.branch == Some(branch)
            || !menu
                .category
                .entries()
                .iter()
                .any(|entry| matches!(entry, Submenu(child) if *child == branch))
            || !self.menu_branch_enabled(id, branch)
        {
            return;
        }
        menu.branch = Some(branch);
        menu.selected = None;
        self.chrome.menu = Some(menu);
    }

    fn menu_branch_enabled(&self, id: window::Id, branch: Branch) -> bool {
        matches!(
            branch,
            Branch::RecentlyClosed | Branch::OpenRecent | Branch::MultiExec
        ) || branch
            .entries()
            .iter()
            .any(|entry| matches!(entry, Item(action) if self.chrome_enabled(id, *action)))
    }

    fn remote_sidebar_menu_offered(&self, owner: Option<window::Id>) -> bool {
        self.window_settings.remote_sidebar_position != RemoteSidebarPosition::Hidden
            || owner
                .and_then(|id| self.windows.get(&id))
                .and_then(Window::tab)
                .is_some_and(|tab| {
                    tab.grid
                        .iter()
                        .any(|(_, id)| self.remote_pane(*id).is_some())
                })
    }

    fn navigate_chrome(
        &mut self,
        id: window::Id,
        navigation: Navigation,
    ) -> Task<WorkspaceMessage> {
        let Some(mut menu) = self.chrome.menu.filter(|menu| menu.window == id) else {
            return Task::none();
        };
        match navigation {
            Navigation::Activate => match menu.selected {
                Some(Choice::Action(action)) if menu.category == Category::StatusRecovery => {
                    return self.restore_status_chrome(id, action);
                }
                Some(Choice::Action(action)) => return self.invoke_chrome(id, action),
                Some(Choice::Branch(branch)) => self.open_menu_branch(id, branch),
                Some(Choice::RecentlyClosed(entry)) => {
                    return self.invoke_recent_history(id, Some(entry));
                }
                Some(Choice::ClearRecentHistory) => return self.invoke_recent_history(id, None),
                Some(Choice::OpenRecent(entry)) => return self.invoke_open_recent(id, entry),
                Some(Choice::CancelJob(run)) => return self.cancel_job_from_menu(id, run),
                None => {}
            },
            Navigation::Escape | Navigation::Left if menu.branch.is_some() => {
                menu.return_to_parent();
                self.chrome.menu = Some(menu);
            }
            Navigation::Escape => self.chrome.dismiss(id),
            Navigation::Right if menu.branch.is_some() => {}
            Navigation::Left | Navigation::Right => {
                if let (Navigation::Right, Some(Choice::Branch(branch))) =
                    (navigation, menu.selected)
                {
                    self.open_menu_branch(id, branch);
                    return Task::none();
                }
                if matches!(menu.category, Category::Toolbar | Category::StatusRecovery) {
                    return Task::none();
                }
                let categories: Vec<_> = Category::ALL
                    .into_iter()
                    .filter(|category| {
                        category.available() && self.menu_surface_available(id, *category)
                    })
                    .collect();
                let next = next_item(
                    &categories,
                    Some(menu.category),
                    matches!(navigation, Navigation::Right),
                );
                if let Some(category) = next {
                    self.chrome.menu = Some(OpenMenu::root(id, category));
                    if category == Category::File {
                        return self.refresh_open_recent();
                    }
                }
            }
            Navigation::Previous | Navigation::Next => {
                let choices = self.menu_choices(menu);
                menu.selected = next_item(
                    &choices,
                    menu.selected,
                    matches!(navigation, Navigation::Next),
                );
                self.chrome.menu = Some(menu);
            }
        }
        Task::none()
    }

    fn menu_choices(&self, menu: OpenMenu) -> Vec<Choice> {
        if menu.category == Category::Jobs {
            return self.job_menu_choices(menu);
        }
        if menu.branch == Some(Branch::OpenRecent) {
            return self.open_recent_choices(menu.window);
        }
        if menu.branch == Some(Branch::RecentlyClosed) {
            let can_reopen = self.has_recently_closed() && self.palette_can_open(menu.window);
            return self
                .recently_closed_menu_rows()
                .into_iter()
                .filter(|_| can_reopen)
                .map(|(entry, _)| Choice::RecentlyClosed(entry))
                .chain(
                    self.has_recently_closed_entries()
                        .then_some(Choice::ClearRecentHistory),
                )
                .collect();
        }
        menu.branch
            .map_or_else(|| menu.category.entries(), Branch::entries)
            .iter()
            .filter_map(|entry| match entry {
                Item(action) if self.chrome_enabled(menu.window, *action) => {
                    Some(Choice::Action(*action))
                }
                Submenu(branch) if self.menu_branch_enabled(menu.window, *branch) => {
                    Some(Choice::Branch(*branch))
                }
                _ => None,
            })
            .collect()
    }

    fn invoke_recent_history(
        &mut self,
        owner: window::Id,
        entry: Option<u64>,
    ) -> Task<WorkspaceMessage> {
        if !self.windows.contains_key(&owner)
            || !self.chrome.menu.is_some_and(|menu| {
                menu.window == owner && menu.branch == Some(Branch::RecentlyClosed)
            })
        {
            return Task::none();
        }
        match entry {
            Some(entry) if self.palette_can_open(owner) && self.recent_history_has_row(entry) => {
                self.chrome.dismiss(owner);
                self.reopen_closed_tab(Some(owner), Some(entry))
            }
            None if self.has_recently_closed_entries() => {
                self.chrome.dismiss(owner);
                self.clear_recently_closed()
            }
            _ => Task::none(),
        }
    }

    fn recent_history_has_row(&self, entry: u64) -> bool {
        self.has_recently_closed()
            && self
                .recently_closed_menu_rows()
                .iter()
                .any(|(id, _)| *id == entry)
    }

    pub(super) fn invoke_chrome(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<WorkspaceMessage> {
        self.chrome.dismiss(id);
        self.dismiss_terminal_context(id);
        if !self.chrome_enabled(id, action) {
            return Task::none();
        }
        if let Some(position) =
            action.remote_sidebar_placement(self.window_settings.remote_sidebar_position)
        {
            return self.set_remote_sidebar_position(position, Some(id));
        }
        if let Some(adjustment) = action.font_zoom() {
            self.adjust_tab_font(id, adjustment);
            return Task::none();
        }
        match action {
            ImportConnections => return self.show_connection_import(id),
            ExportConnections => return self.show_connection_export(id),
            PreventSystemSleep => return self.toggle_sleep_prevention(Some(id)),
            UnlockVault => return self.show_vault_unlock(id),
            CustomizeToolbar => {
                return self.update_toolbar(ToolbarMessage::Open(id, Catalog::Actions));
            }
            ToggleActionToolbar | ToggleConnectionToolbar => {
                let layout = self.chrome.toolbar.layout(id);
                let (row, visible) = if action == ToggleActionToolbar {
                    (ToolbarRow::Actions, layout.actions_visible())
                } else {
                    (
                        ToolbarRow::Group(Group::Connections),
                        layout.visible(Group::Connections),
                    )
                };
                return self.update_toolbar(ToolbarMessage::Visible(id, row, !visible));
            }
            ShowMenuBar => {
                if self.chrome.hidden_menu_bars.remove(&id) {
                    self.notice(id, MENU_BAR_SHOWN.into());
                } else {
                    self.chrome.hidden_menu_bars.insert(id);
                    if let Some(window) = self.windows.get_mut(&id)
                        && window.notice.as_deref() == Some(MENU_BAR_SHOWN)
                    {
                        window.notice = None;
                    }
                }
                #[cfg(target_os = "macos")]
                self.sync_native_menus();
                return Task::none();
            }
            QuickConnect => return self.show_quick_connect(id),
            QuickSelect => return self.start_quick_select(id),
            Manual => return self.show_manual(),
            About => return self.show_about(id),
            OpenPgpRecipients => return self.show_openpgp(id),
            NewSnippetManagerWindow => return self.open_snippet_window(id),
            LogfileManager => return self.open_logfiles(id),
            SessionProcesses => return self.open_session_processes(id),
            AsciiArt => return self.open_ascii_art(id),
            Fullscreen => return self.toggle_window_fullscreen(id),
            TerminalOnlyFullscreen => return self.toggle_terminal_only_fullscreen(id),
            HideTerminalScrollbarsFullscreen => {
                self.window_settings.hide_terminal_scrollbars_in_fullscreen =
                    !self.window_settings.hide_terminal_scrollbars_in_fullscreen;
                self.save_window_settings(Some(id));
                #[cfg(target_os = "macos")]
                self.sync_native_menus();
            }
            ReopenClosedTab => return self.reopen_closed_tab(Some(id), None),
            CloseAllTabs => return self.request_close_all_tabs(id),
            RenameTab | CloseOtherTabs | CloseTabsToRight => {
                if let Some(tab) = self.windows.get(&id).and_then(Window::active_tab_id) {
                    return if action == RenameTab {
                        self.show_rename_tab(id, tab)
                    } else {
                        self.request_tab_batch_close(id, tab, action == CloseTabsToRight)
                    };
                }
                return Task::none();
            }
            NextTab | PreviousTab => {
                self.switch_tab_once(id, action == PreviousTab);
                return Task::none();
            }
            ClearBuffer | PreviousPrompt | NextPrompt | SelectLastOutput | CopyLastOutput
            | SftpHere | SendSerialBreak => {
                return self.invoke_focused_terminal_action(id, action);
            }
            CommandPalette | SnippetPalette => {
                return self.toggle_palette(id, action == SnippetPalette);
            }
            _ => {}
        }
        if let Some(movement) = pane_navigation::Movement::from_action(action) {
            return self.navigate_pane(id, movement);
        }
        self.chrome_action(id, action)
            .map_or_else(Task::none, |message| self.update(message))
    }

    pub(super) fn chrome_enabled(&self, id: window::Id, action: Action) -> bool {
        if !matches!(action, Quit | MultiExecStop)
            && (self.owner_dialog(id).is_some()
                || self.hosted_close_prompt(id).is_some()
                || self.windows.get(&id).is_some_and(|window| {
                    window.pending_snippet_run.is_some()
                        || window.pending_split_connection.is_some()
                }))
        {
            return false;
        }
        match action {
            ImportConnections | ExportConnections => {
                self.windows.contains_key(&id) && self.clipboard.policy.problem.is_none()
            }
            PreventSystemSleep => {
                self.windows.contains_key(&id) && self.sleep_prevention_available()
            }
            UnlockVault => self.windows.contains_key(&id) && self.vault_unlock_available(),
            CustomizeToolbar | ToggleActionToolbar | ToggleConnectionToolbar => {
                self.windows.contains_key(&id) && self.chrome.toolbar.can_edit(id)
            }
            QuickConnect
            | Manual
            | About
            | OpenPgpRecipients
            | NewSnippetManagerWindow
            | LogfileManager
            | SessionProcesses
            | AsciiArt
            | Fullscreen
            | TerminalOnlyFullscreen
            | HideTerminalScrollbarsFullscreen => self.windows.contains_key(&id),
            ToggleSessionJournal => self.journal_toggle_enabled(id),
            QuickSelect => self.quick_select_available(id),
            ShowMenuBar => self.palette_can_open(id),
            ZoomIn | ZoomOut | ResetZoom => {
                self.focused_session(id).is_some() && self.palette_can_open(id)
            }
            SessionJournalScreenshot => self.journal_screenshot_enabled(id),
            TerminalEffectsManager => {
                self.windows.contains_key(&id) && self.terminal_effects_permitted()
            }
            action if action.effect_choice().is_some() => {
                self.effect_menu_target(id).is_some()
                    && self.terminal_effects_enabled()
                    && action
                        .effect_choice()
                        .flatten()
                        .is_none_or(|effect| self.allowed_terminal_effect(effect))
            }
            RenameTab => self.windows.get(&id).is_some_and(|w| w.tab().is_some()),
            CloseAllTabs => self.windows.get(&id).is_some_and(|w| !w.tabs.is_empty()),
            CloseOtherTabs => self.windows.get(&id).is_some_and(|w| w.tabs.len() > 1),
            CloseTabsToRight => self
                .windows
                .get(&id)
                .is_some_and(|w| w.active + 1 < w.tabs.len()),
            ReopenClosedTab => self.palette_can_open(id) && self.has_recently_closed(),
            NextTab | PreviousTab => self
                .windows
                .get(&id)
                .is_some_and(|window| window.tabs.len() >= 2),
            ClearBuffer | PreviousPrompt | NextPrompt | SelectLastOutput | CopyLastOutput => {
                self.focused_terminal_action_enabled(id, action)
            }
            SftpHere | SendSerialBreak => {
                self.palette_can_open(id) && self.focused_terminal_action_enabled(id, action)
            }
            CommandPalette | SnippetPalette => self.palette_can_open(id),
            RemoteSidebarLeft | RemoteSidebarRight | RemoteSidebarToggle => {
                self.windows.contains_key(&id) && self.remote_sidebar_menu_offered(Some(id))
            }
            FocusPaneLeft | FocusPaneRight | FocusPaneAbove | FocusPaneBelow | NextPane
            | PreviousPane => self
                .windows
                .get(&id)
                .and_then(Window::tab)
                .is_some_and(|tab| tab.grid.len() >= 2),
            _ => self.chrome_action(id, action).is_some(),
        }
    }

    fn chrome_action(&self, id: window::Id, action: Action) -> Option<WorkspaceMessage> {
        use WorkspaceMessage as W;
        if let Some(multi_exec) = action.multi_exec_action() {
            let owner = (action != MultiExecStop).then_some(id);
            return (owner.is_none_or(|id| self.palette_can_open(id))
                && self.multi_exec_available(owner, multi_exec))
            .then_some(W::MultiExec(owner, multi_exec));
        }
        let window = self.windows.get(&id)?;
        if let Some(effect) = action.effect_choice() {
            return Some(W::SetTerminalEffect(
                id,
                self.effect_menu_target(id)?,
                effect,
            ));
        }
        let tab = window.tab();
        let permitted = self.clipboard.policy.problem.is_none();
        let message = match action {
            JournalDockLeft | JournalDockRight | ToggleJournalDock
                if self.journal_allowed() && !self.terminal_only_fullscreen_active(id) =>
            {
                use super::journal_dock::{Message, Placement};
                W::JournalDock(
                    id,
                    match action {
                        JournalDockLeft => Message::Place(Placement::Left),
                        JournalDockRight => Message::Place(Placement::Right),
                        _ => Message::Toggle,
                    },
                )
            }
            TerminalEffectsManager if self.terminal_effects_permitted() => {
                W::TerminalEffects(super::terminal_effect_access::Message::Open(id))
            }
            QuickConnect
            | About
            | LogfileManager
            | SessionProcesses
            | AsciiArt
            | Fullscreen
            | NewSnippetManagerWindow => return None,
            NewWindow => W::NewWindow,
            Dashboard if self.palette_can_open(id) => {
                W::Dashboard(id, super::dashboard::Message::Toggle)
            }
            Quit => W::Quit,
            LocalShell if permitted => W::NewTab(id),
            PowerShell if cfg!(windows) && permitted => {
                W::NewWindowsShell(id, kortty_pty::LocalShell::PowerShell)
            }
            CommandPrompt if cfg!(windows) && permitted => {
                W::NewWindowsShell(id, kortty_pty::LocalShell::CommandPrompt)
            }
            IncognitoShell if permitted && self.clipboard.policy.incognito_allowed => {
                W::SessionSecurity(session_security::Message::IncognitoLocal(id))
            }
            CloseTab => W::CloseTab(id, window.active_tab_id()?),
            CloseWindow => W::Window(id, window::Event::CloseRequested),
            OpenProject => W::Project(id, project_access::Message::ShowOpenFile),
            SaveProject
                if self
                    .windows
                    .values()
                    .any(|window| window.terminal_tabs().any(|tab| !tab.incognito)) =>
            {
                W::Project(id, project_access::Message::ShowSaveFile)
            }
            ManageProjects => W::Project(id, project_access::Message::Show),
            SaveLayout => W::SaveLayout(id),
            RestoreLayout => W::RestoreLayout(id),
            RestoreSession if self.session_restore.can_restore() => {
                W::SessionRestore(session_restore::Message::Restore(id))
            }
            CreateBackup if permitted => W::Backup(id, super::backup_ui::Message::OpenExport),
            ImportBackup if permitted => W::Backup(id, super::backup_ui::Message::OpenImport),
            CopySelection => {
                tab?.focused_session()?;
                W::Copy(id)
            }
            Paste => {
                tab?.focused_session()?;
                W::Paste(id)
            }
            Find => W::OpenSearch(tab?.focused_session()?),
            ManageConnections => W::OpenConnections(id),
            Ssh if permitted => W::Ssh(ssh_access::Message::Show(id, None)),
            IncognitoSsh if permitted && self.clipboard.policy.incognito_allowed => {
                W::SessionSecurity(session_security::Message::IncognitoSsh(id))
            }
            Sftp if permitted => W::Ssh(ssh_access::Message::ShowSftp(id)),
            Serial if self.clipboard.policy.serial_allowed => W::ShowSerial(id, None),
            Tn3270 if permitted => W::ShowIbm(id, IbmKind::Tn3270e, None),
            Tn5250 if permitted => W::ShowIbm(id, IbmKind::Tn5250, None),
            Settings => W::Settings(settings_window::Message::Open(id)),
            WindowSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Window,
            )),
            FontSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Font,
            )),
            KeyboardSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Keyboard,
            )),
            ThemeSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Themes,
            )),
            ColorSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Colors,
            )),
            AppearanceSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Appearance,
            )),
            LoggingSettings => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Logging,
            )),
            TerminalSettings | Clipboard => W::Settings(settings_window::Message::OpenAt(
                id,
                settings_window::Section::Terminal,
            )),
            Vault => W::OpenVault(id),
            SshKeys => W::SshKeys(id, super::ssh_keys_ui::Message::Open),
            KnownHosts => W::KnownHosts(id, super::known_hosts::Message::Open),
            SessionJournals if self.journal_allowed() => {
                W::SessionJournal(id, super::journal_access::Message::Open)
            }
            ToggleSessionJournal if self.journal_toggle_enabled(id) => {
                W::JournalCapture(super::journal_capture_access::Message::Toggle(id, tab?.id))
            }
            SessionJournalScreenshot if self.journal_screenshot_enabled(id) => W::JournalCapture(
                super::journal_capture_access::Message::Screenshot(id, tab?.id),
            ),
            Isolation => W::SessionSecurity(session_security::Message::Open(id)),
            ControlApi => W::Control(control_access::Message::Settings(id, true)),
            Snippets => W::OpenSnippets(id),
            Scheduler => W::OpenScheduler(id),
            ToggleRecording if self.clipboard.policy.recording_allowed && !tab?.incognito => {
                W::Recording(recording_access::Message::Toggle(id))
            }
            Recordings => W::Recording(recording_access::Message::Open(id)),
            AiManager => W::AiManager(super::ai_access::Message::Open(id)),
            Editor => W::OpenEditor,
            Diagrams => W::OpenDiagram,
            FilesLeft => W::LocalDock(id, local_dock::Message::Toggle(local_dock::Side::Left)),
            FilesRight => W::LocalDock(id, local_dock::Message::Toggle(local_dock::Side::Right)),
            SplitPane | SplitRight | SplitDown
                if self.same_server_split_allowed(id, tab?.focused_session()?) =>
            {
                let axis = match action {
                    SplitRight => pane_grid::Axis::Vertical,
                    SplitDown => pane_grid::Axis::Horizontal,
                    _ => {
                        pane_navigation::split_axis(self.focused_session(id).map(|pane| pane.grid))
                    }
                };
                W::Split(id, axis)
            }
            ClosePane if tab?.grid.len() >= 2 => W::ClosePane(id, tab?.focused),
            ZoomPane if tab?.grid.len() >= 2 || tab?.grid.maximized().is_some() => {
                W::ZoomPane(id, tab?.focused)
            }
            MoveTab if window.active_entry().is_some() && self.palette_can_open(id) => {
                W::ToggleTransfer(id)
            }
            ShowCommandTimestamps if tab.is_some() && self.palette_can_open(id) => {
                W::ToggleCommandTimestamps(id)
            }
            Broadcast if self.broadcast_menu_enabled(id) => W::ToggleBroadcast(id),
            AudibleBell => W::AudibleBell(!self.bell.enabled),
            TabTitles => W::ShellTabTitles(id, !self.window_settings.tab_title_from_shell_enabled),
            PromptJumps => W::PromptNavigation(id, !self.window_settings.prompt_navigation_enabled),
            _ => return None,
        };
        Some(message)
    }

    pub(super) fn chrome_checked(&self, id: window::Id, window: &Window, action: Action) -> bool {
        if let Some(multi_exec) = action.multi_exec_action() {
            return self.multi_exec_checked(id, multi_exec);
        }
        if let Some(effect) = action.effect_choice() {
            return self.effect_menu_target(id).is_some_and(|session| {
                self.pane_effect(session)
                    .map(|active| active.selection.effect)
                    == effect
            });
        }
        match action {
            PreventSystemSleep => self.sleep_prevention_checked(),
            JournalDockLeft => {
                self.journal_dock_placement(id) == super::journal_dock::Placement::Left
            }
            JournalDockRight => {
                self.journal_dock_placement(id) == super::journal_dock::Placement::Right
            }
            ToggleActionToolbar => self.chrome.toolbar.layout(id).actions_visible(),
            ToggleConnectionToolbar => self.chrome.toolbar.layout(id).visible(Group::Connections),
            ShowMenuBar => !self.menu_bar_hidden(id),
            AudibleBell => self.bell.enabled,
            TabTitles => self.window_settings.tab_title_from_shell_enabled,
            PromptJumps => self.window_settings.prompt_navigation_enabled,
            Broadcast => window.tab().is_some_and(|tab| tab.broadcast),
            ShowCommandTimestamps => window.tab().is_some_and(|tab| tab.command_timestamps),
            Dashboard => window.dashboard.visible(),
            TerminalOnlyFullscreen => self.terminal_only_fullscreen_active(id),
            HideTerminalScrollbarsFullscreen => {
                self.window_settings.hide_terminal_scrollbars_in_fullscreen
            }
            ZoomPane => window
                .tab()
                .is_some_and(|tab| tab.grid.maximized().is_some()),
            FilesLeft => self.local_dock.side() == Some(local_dock::Side::Left),
            FilesRight => self.local_dock.side() == Some(local_dock::Side::Right),
            RemoteSidebarLeft | RemoteSidebarRight => {
                action.remote_sidebar_checked(self.window_settings.remote_sidebar_position)
            }
            _ => false,
        }
    }

    pub(super) fn menu_bar(
        &self,
        id: window::Id,
        window: &Window,
    ) -> Element<'_, WorkspaceMessage> {
        if self.menu_bar_hidden(id) {
            return space::vertical().height(0).into();
        }
        let colors = self.chrome_colors();
        let menu = self.chrome.menu.filter(|menu| {
            menu.window == id
                && !matches!(menu.category, Category::Toolbar | Category::StatusRecovery)
        });
        let buttons = Category::ALL
            .into_iter()
            .fold(row![].spacing(1), |row, category| {
                let selected = menu.is_some_and(|menu| menu.category == category);
                let label = if category == Category::Jobs {
                    let Some(label) = self.job_menu_title() else {
                        return row;
                    };
                    label
                } else {
                    category.title().to_owned()
                };
                let trigger = button(text(label).size(13))
                    .padding([6, 9])
                    .style(move |_, status| {
                        button_style(status, colors.menu_hover, selected, colors)
                    })
                    .on_press_maybe(
                        category
                            .available()
                            .then_some(wrap(Message::OpenMenu(id, category))),
                    );
                let trigger: Element<'_, WorkspaceMessage> = if category == Category::Jobs {
                    mouse_area(trigger)
                        .on_right_press(wrap(Message::OpenJobsContext(id)))
                        .into()
                } else {
                    trigger.into()
                };
                if let Some(menu) = menu.filter(|menu| menu.category == category) {
                    row.push(popup::anchor(
                        trigger,
                        self.menu_items(window, menu, None),
                        wrap(Message::Dismiss(id)),
                        move |action| wrap(Message::Navigate(id, action)),
                    ))
                } else if menu.is_some() && category.available() {
                    row.push(mouse_area(trigger).on_enter(wrap(Message::OpenMenu(id, category))))
                } else {
                    row.push(trigger)
                }
            });
        container(buttons.wrap())
            .padding([2, 8])
            .width(Length::Fill)
            .style(move |_| container::Style::default().background(colors.menu))
            .into()
    }

    pub(super) fn menu_bar_recovery<'a>(
        &'a self,
        id: window::Id,
        status: Element<'a, WorkspaceMessage>,
    ) -> Element<'a, WorkspaceMessage> {
        let Some(window) = self.windows.get(&id).filter(|_| self.menu_bar_hidden(id)) else {
            return status;
        };
        let hint = self.window_settings.key_bindings.menu_hint(ShowMenuBar).map_or_else(
            || "Menu bar hidden. Right-click the terminal or status bar to show it again.".into(),
            |shortcut| format!("Menu bar hidden. Press {shortcut} or right-click the terminal or status bar to show it again."),
        );
        let colors = self.chrome_colors();
        let trigger = mouse_area(
            container(
                row![text(hint).size(13).color(colors.foreground), status]
                    .spacing(8)
                    .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([3, 8])
            .style(move |_| container::Style::default().background(colors.menu)),
        )
        .on_right_press(wrap(Message::OpenMenu(id, Category::StatusRecovery)))
        .into();
        if let Some(menu) = self
            .chrome
            .menu
            .filter(|menu| menu.window == id && menu.category == Category::StatusRecovery)
        {
            popup::anchor(
                trigger,
                self.menu_items(window, menu, None),
                wrap(Message::Dismiss(id)),
                move |navigation| wrap(Message::Navigate(id, navigation)),
            )
        } else {
            trigger
        }
    }

    fn menu_items(
        &self,
        window: &Window,
        menu: OpenMenu,
        level: Option<Branch>,
    ) -> Element<'_, WorkspaceMessage> {
        let OpenMenu {
            window: id,
            category,
            branch,
            selected,
            ..
        } = menu;
        let colors = self.chrome_colors();
        let mut rows = if category == Category::Jobs {
            self.job_menu_rows(menu, colors)
        } else if level == Some(Branch::OpenRecent) {
            self.open_recent_menu_rows(menu, colors)
        } else if level == Some(Branch::RecentlyClosed) {
            self.recent_history_menu_rows(menu, colors)
        } else {
            level
                .map_or_else(|| category.entries(), Branch::entries)
                .iter()
                .fold(column![].spacing(2), |rows, entry| match entry {
                    Separator => rows.push(divider(colors.menu_hover)),
                    Entry::BackgroundTransparency => {
                        if let Some(controls) = self.menu_transparency_controls(window) {
                            rows.push(divider(colors.menu_hover)).push(controls)
                        } else {
                            rows
                        }
                    }
                    Entry::Unavailable(label) => rows.push(
                        button(row![space::horizontal().width(15), text(*label).size(13)])
                            .width(Length::Fill)
                            .padding([5, 6])
                            .style(move |_, status| {
                                button_style(status, colors.menu_hover, false, colors)
                            }),
                    ),
                    Submenu(Branch::RemoteSidebar)
                        if !self.remote_sidebar_menu_offered(Some(id)) =>
                    {
                        rows
                    }
                    Submenu(child) => {
                        let active = branch == Some(*child);
                        let pointed = active || selected == Some(Choice::Branch(*child));
                        let enabled = self.menu_branch_enabled(id, *child);
                        let open = wrap(Message::OpenBranch(id, *child));
                        let trigger = button(
                            row![
                                space::horizontal().width(15),
                                text(child.title()).size(13),
                                space::horizontal(),
                                text("›").size(15)
                            ]
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding([5, 6])
                        .style(move |_, status| {
                            button_style(status, colors.menu_hover, pointed, colors)
                        })
                        .on_press_maybe(enabled.then_some(open.clone()));
                        let trigger = if enabled {
                            mouse_area(trigger).on_enter(open).into()
                        } else {
                            trigger.into()
                        };
                        if active {
                            rows.push(popup::submenu(
                                trigger,
                                self.menu_items(window, menu, Some(*child)),
                            ))
                        } else {
                            rows.push(trigger)
                        }
                    }
                    Item(action) => {
                        let restore = category == Category::StatusRecovery;
                        let checked = !restore && self.chrome_checked(id, window, *action);
                        let shortcut = (!restore)
                            .then(|| self.window_settings.key_bindings.menu_hint(*action))
                            .flatten();
                        let invoke = if restore {
                            Message::RestoreStatus(id, *action)
                        } else {
                            Message::Invoke(id, *action)
                        };
                        let item = button(
                            row![
                                text(if checked { "✓" } else { "" }).width(15),
                                text(action.label(category)).size(13),
                                space::horizontal(),
                                shortcut
                                    .map(|shortcut| text(shortcut).size(11).color(colors.muted)),
                            ]
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding([5, 6])
                        .style(move |_, status| {
                            button_style(
                                status,
                                colors.menu_hover,
                                level == branch && selected == Some(Choice::Action(*action)),
                                colors,
                            )
                        })
                        .on_press_maybe(self.chrome_enabled(id, *action).then_some(wrap(invoke)));
                        if level.is_none() && branch.is_some() {
                            rows.push(
                                mouse_area(item).on_enter(wrap(Message::PointAtRootAction(
                                    id, category, *action,
                                ))),
                            )
                        } else {
                            rows.push(item)
                        }
                    }
                })
        };
        if level == Some(Branch::TerminalEffects)
            && let Some(speed) = self
                .effect_menu_target(id)
                .and_then(|session| self.effect_speed_view(id, session))
        {
            rows = rows.push(divider(colors.menu_hover)).push(speed);
        }
        menu_popup(rows, colors)
    }

    fn recent_history_menu_rows(
        &self,
        menu: OpenMenu,
        colors: ChromeColors,
    ) -> iced::widget::Column<'_, WorkspaceMessage> {
        let row = |title: String, choice: Option<Choice>, message: Option<Message>| {
            let selected = message.is_some() && menu.selected == choice;
            button(
                row![space::horizontal().width(15), text(title).size(13)]
                    .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 6])
            .style(move |_, status| button_style(status, colors.menu_hover, selected, colors))
            .on_press_maybe(message.map(wrap))
        };
        let entries = self.recently_closed_menu_rows();
        let can_reopen = self.has_recently_closed() && self.palette_can_open(menu.window);
        let mut rows = column![].spacing(2);
        if entries.is_empty() {
            rows = rows.push(row("No closed tabs".into(), None, None));
        }
        for (entry, title) in entries {
            rows = rows.push(row(
                title,
                Some(Choice::RecentlyClosed(entry)),
                can_reopen.then_some(Message::ReopenRecent(menu.window, entry)),
            ));
        }
        if self.has_recently_closed_entries() {
            rows = rows.push(divider(colors.menu_hover)).push(row(
                "Clear List".into(),
                Some(Choice::ClearRecentHistory),
                Some(Message::ClearRecentHistory(menu.window)),
            ));
        }
        rows
    }

    #[cfg(target_os = "macos")]
    fn native_active_window(&self) -> Option<(window::Id, &Window)> {
        self.windows
            .iter()
            .max_by_key(|(_, window)| (window.focused, window.number))
            .map(|(&id, window)| (id, window))
    }

    #[cfg(target_os = "macos")]
    fn native_focused_main_owner(&self) -> Option<(window::Id, &Window)> {
        let id = self.native_focused_window?;
        let window = self.windows.get(&id)?;
        (window.focused && self.chrome.dialog.is_none() && !self.native_menu_blocked(window))
            .then_some((id, window))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn native_focused_openpgp(&self) -> Option<window::Id> {
        self.native_focused_window
            .filter(|&id| self.is_openpgp_standalone(id))
    }

    #[cfg(target_os = "macos")]
    fn native_focused_snippet_comparison(&self) -> Option<window::Id> {
        self.native_focused_window
            .filter(|id| self.snippet_comparisons.contains_key(id))
    }

    #[cfg(target_os = "macos")]
    fn native_focused_vault_unlock(&self) -> Option<window::Id> {
        self.current_vault_unlock()
            .filter(|&id| self.native_focused_window == Some(id))
    }

    #[cfg(target_os = "macos")]
    fn native_focused_manual(&self) -> Option<window::Id> {
        self.native_focused_window
            .filter(|&id| self.manual_title(id).is_some())
    }

    #[cfg(target_os = "macos")]
    fn native_focused_connection_dialog(&self) -> Option<window::Id> {
        self.native_focused_window.filter(|&id| {
            self.connection_import_title(id).is_some()
                || self.connection_export_title(id).is_some()
                || self.connection_overview_title(id).is_some()
                || self.os_badge_manager_title(id).is_some()
        })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn native_focused_tool_tab(&self) -> bool {
        self.native_focused_window
            .and_then(|id| self.windows.get(&id))
            .is_some_and(|window| matches!(window.active_entry(), Some(super::TabEntry::Tool(_))))
    }

    #[cfg(target_os = "macos")]
    fn native_modal_blocks_fallback(&self, active: Option<(window::Id, &Window)>) -> bool {
        // A modal can own focus while no main window is focused. Its menu must
        // not target an unrelated main window via the newest-window fallback.
        self.current_vault_unlock().is_some()
            || self.chrome.dialog.is_some()
            || self.native_focused_connection_dialog().is_some()
            || active.is_none_or(|(_, window)| !window.focused)
                && (!self.close_all_dialogs.is_empty()
                    || self.chrome.connection_import.is_some()
                    || self.chrome.connection_export.is_some()
                    || self.has_standalone_openpgp()
                    || self.native_terminal_effects().is_some()
                    || !self.ascii_art.is_empty()
                    || self
                        .native_focused_window
                        .is_some_and(|id| self.ai_manager_title(id).is_some())
                    || self.windows.keys().any(|&owner| {
                        self.backup_for_owner(owner).is_some()
                            || self.tab_journal_note_for_owner(owner).is_some()
                    }))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn sync_native_menus(&self) {
        let active = self.native_active_window();
        let modal_blocks = self.native_modal_blocks_fallback(active);
        let comparison_focused = self.native_focused_snippet_comparison().is_some();
        let snippet_focused = self
            .native_focused_window
            .and_then(|id| self.focused_snippet_surface(id));
        let modal_closable = self.native_focused_openpgp().is_some()
            || self.native_focused_manual().is_some()
            || self
                .native_focused_window
                .is_some_and(|id| self.chrome.title(id).is_some())
            || self
                .native_focused_window
                .is_some_and(|id| self.ai_manager_title(id).is_some())
            || self.native_focused_vault_unlock().is_some()
            || comparison_focused
            || snippet_focused.is_some()
            || self.native_focused_connection_dialog().is_some()
            || self
                .native_focused_window
                .is_some_and(|id| self.ascii_art.contains_key(&id))
            || self
                .native_terminal_effects()
                .is_some_and(|id| self.native_focused_window == Some(id));
        kortty_macos::sync_application_menu_command_key(
            CloseWindow as u16,
            if modal_closable && snippet_focused.is_none() {
                "w"
            } else {
                ""
            },
        );
        kortty_macos::sync_application_menu_command_key(
            CloseTab as u16,
            if snippet_focused.is_some() || self.native_focused_tool_tab() {
                "w"
            } else {
                ""
            },
        );
        let sidebar_offered = self.remote_sidebar_menu_offered(active.map(|(id, _)| id));
        let sidebar_position = self.window_settings.remote_sidebar_position;
        kortty_macos::sync_application_submenu_visibility(
            NATIVE_REMOTE_SIDEBAR_VISIBILITY,
            sidebar_offered,
        );
        kortty_macos::sync_recently_closed_menu(
            &self.recently_closed_menu_rows(),
            !modal_blocks
                && self.has_recently_closed()
                && active.is_none_or(|(id, _)| self.palette_can_open(id)),
            !modal_blocks && self.has_recently_closed_entries(),
            NATIVE_CLEAR_RECENT_HISTORY,
        );
        self.sync_native_open_recent(!modal_blocks && self.native_open_recent_available(active));
        self.sync_native_jobs_menu();
        kortty_macos::sync_application_menu_items(|key| {
            let action = Action::from_native(key)?;
            if action == PreventSystemSleep {
                return Some((
                    !modal_blocks
                        && self.chrome.dialog.is_none()
                        && self.sleep_prevention_available()
                        && active.is_none_or(|(id, window)| {
                            !self.native_menu_blocked(window) && self.chrome_enabled(id, action)
                        }),
                    self.sleep_prevention_checked(),
                ));
            }
            if comparison_focused && action == CloseTab {
                return Some((false, false));
            }
            if snippet_focused.is_some() && action == CloseTab {
                return Some((true, false));
            }
            if action == MultiExecStop {
                return Some((
                    self.multi_exec_available(None, multi_exec_access::Action::Stop),
                    false,
                ));
            }
            if (action == SessionJournals && !self.journal_allowed())
                || (action == TerminalEffectsManager && !self.terminal_effects_permitted())
                || (action == UnlockVault && !self.vault_unlock_available())
                || (matches!(action, ImportConnections | ExportConnections)
                    && self.clipboard.policy.problem.is_some())
            {
                return Some((false, false));
            }
            if action == Quit
                || (action == NewWindow
                    && self.chrome.dialog.is_none()
                    && self.native_terminal_effects().is_none()
                    && self.current_vault_unlock().is_none())
                || (action == CloseWindow && modal_closable)
            {
                return Some((true, false));
            }
            if modal_blocks {
                return Some((false, false));
            }
            let active = if native_menu_requires_focused_owner(action) {
                self.native_focused_main_owner()
            } else {
                active
            };
            Some(active.map_or_else(
                || {
                    if action.remote_sidebar_placement(sidebar_position).is_some() {
                        return (
                            sidebar_offered,
                            action.remote_sidebar_checked(sidebar_position),
                        );
                    }
                    let enabled = if action == ReopenClosedTab {
                        self.has_recently_closed()
                    } else {
                        native_can_open_window(action)
                    };
                    (enabled, false)
                },
                |(id, window)| {
                    (
                        self.chrome_enabled(id, action),
                        self.chrome_checked(id, window, action),
                    )
                },
            ))
        });
    }

    #[cfg(target_os = "macos")]
    pub(super) fn native_menu(&mut self, key: u16) -> Task<WorkspaceMessage> {
        let active = self.native_active_window();
        let modal_blocks = self.native_modal_blocks_fallback(active);
        if key == NATIVE_CLEAR_RECENT_HISTORY {
            return if !modal_blocks && self.has_recently_closed_entries() {
                self.clear_recently_closed()
            } else {
                Task::none()
            };
        }
        let Some(action) = Action::from_native(key) else {
            return Task::none();
        };
        if action == Quit {
            return self.update(WorkspaceMessage::Quit);
        }
        if action == MultiExecStop {
            return self.update(WorkspaceMessage::MultiExec(
                None,
                multi_exec_access::Action::Stop,
            ));
        }
        if action == SessionJournals && !self.journal_allowed() {
            return Task::none();
        }
        if (action == TerminalEffectsManager && !self.terminal_effects_permitted())
            || (action == UnlockVault && !self.vault_unlock_available())
            || (matches!(action, ImportConnections | ExportConnections)
                && self.clipboard.policy.problem.is_some())
        {
            return Task::none();
        }
        if let Some(id) = self.native_terminal_effects() {
            return if action == CloseWindow && self.native_focused_window == Some(id) {
                self.close_terminal_effects(id).unwrap_or_else(Task::none)
            } else {
                window::gain_focus(id)
            };
        }
        if let Some(id) = self.current_vault_unlock() {
            return if action == CloseWindow && self.native_focused_vault_unlock() == Some(id) {
                self.close_profile_tool(id, false)
            } else {
                window::gain_focus(id)
            };
        }
        if let Some(dialog) = &self.chrome.dialog {
            return if action == CloseWindow && self.native_focused_window == Some(dialog.id) {
                self.close_window(dialog.id)
            } else {
                Task::none()
            };
        }
        if action == NewWindow {
            return self.update(WorkspaceMessage::NewWindow);
        }
        if action == CloseWindow
            && let Some(id) = self
                .native_focused_window
                .filter(|&id| self.ai_manager_title(id).is_some())
        {
            return self.close_window(id);
        }
        if action == CloseWindow
            && let Some(id) = self.native_focused_manual()
        {
            return self.close_manual(id).unwrap_or_else(Task::none);
        }
        if action == CloseWindow
            && let Some(id) = self.native_focused_connection_dialog()
        {
            return self.close_window(id);
        }
        if action == CloseWindow
            && let Some(id) = self.native_focused_openpgp()
        {
            return self.request_openpgp_close(id);
        }
        if let Some(id) = self.native_focused_snippet_comparison() {
            match action {
                CloseWindow => return self.close_window(id),
                CloseTab => return Task::none(),
                _ => {}
            }
        }
        if action == CloseTab
            && let Some(id) = self
                .native_focused_window
                .and_then(|id| self.focused_snippet_surface(id))
        {
            return self.update_snippets(id, crate::snippet_window::Message::CloseActiveTab);
        }
        if action == CloseWindow
            && let Some(id) = self
                .native_focused_window
                .filter(|id| self.ascii_art.contains_key(id))
        {
            return self.close_window(id);
        }
        if modal_blocks {
            return Task::none();
        }
        if action == Manual && active.is_none() {
            return self.show_manual();
        }
        if action == PreventSystemSleep && active.is_none() {
            return self.toggle_sleep_prevention(None);
        }
        let active = if native_menu_requires_focused_owner(action) {
            self.native_focused_main_owner()
        } else {
            active
        };
        if let Some((id, window)) = active {
            if self.native_menu_blocked(window) {
                return Task::none();
            }
            return self.invoke_chrome(id, action);
        }
        if action == ReopenClosedTab {
            return self.reopen_closed_tab(None, None);
        }
        if let Some(position) =
            action.remote_sidebar_placement(self.window_settings.remote_sidebar_position)
        {
            return if self.remote_sidebar_menu_offered(None) {
                self.set_remote_sidebar_position(position, None)
            } else {
                Task::none()
            };
        }
        if !native_can_open_window(action) {
            return Task::none();
        }
        let (id, opened) = self.open_window(false);
        opened.chain(Task::done(wrap(Message::Invoke(id, action))))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn native_recently_closed(&mut self, entry: u64) -> Task<WorkspaceMessage> {
        if self.chrome.dialog.is_some() || !self.recent_history_has_row(entry) {
            return Task::none();
        }
        let owner = self.native_active_window();
        if self.native_modal_blocks_fallback(owner)
            || owner.is_some_and(|(id, _)| !self.palette_can_open(id))
        {
            return Task::none();
        }
        self.reopen_closed_tab(owner.map(|(id, _)| id), Some(entry))
    }

    #[cfg(target_os = "macos")]
    fn native_menu_blocked(&self, window: &Window) -> bool {
        window.pending_serial_close.is_some()
            || window.pending_ssh.is_some()
            || window.pending_serial.is_some()
            || window.pending_ibm.is_some()
            || window.pending_tunnels.is_some()
            || window.pending_snippet_run.is_some()
            || window.pending_split_connection.is_some()
            || window.pending_paste.is_some()
            || self.window_clipboard_prompt(window).is_some()
    }
}

#[cfg(target_os = "macos")]
const NATIVE_CLEAR_RECENT_HISTORY: u16 = u16::MAX;

#[cfg(target_os = "macos")]
const NATIVE_REMOTE_SIDEBAR_VISIBILITY: u16 = RemoteSidebarToggle as u16;

#[cfg(target_os = "macos")]
fn native_menu_requires_focused_owner(action: Action) -> bool {
    matches!(
        action,
        ShowCommandTimestamps
            | ShowMenuBar
            | QuickSelect
            | SftpHere
            | SendSerialBreak
            | HideTerminalScrollbarsFullscreen
    ) || action.multi_exec_action().is_some()
        || action.font_zoom().is_some()
}

#[cfg(target_os = "macos")]
fn native_can_open_window(action: Action) -> bool {
    action.effect_choice().is_none()
        && !native_menu_requires_focused_owner(action)
        && !matches!(
            action,
            CopySelection
                | Paste
                | Find
                | CloseTab
                | CloseWindow
                | RenameTab
                | CloseOtherTabs
                | CloseTabsToRight
                | CloseAllTabs
                | SaveProject
                | ClosePane
                | ZoomPane
                | SplitPane
                | SplitRight
                | SplitDown
                | FocusPaneLeft
                | FocusPaneRight
                | FocusPaneAbove
                | FocusPaneBelow
                | NextPane
                | PreviousPane
                | Broadcast
                | MoveTab
                | Fullscreen
                | RemoteSidebarLeft
                | RemoteSidebarRight
                | RemoteSidebarToggle
                | ToggleSessionJournal
                | SessionJournalScreenshot
                | JournalDockLeft
                | JournalDockRight
                | ToggleJournalDock
        )
}

#[cfg(target_os = "macos")]
pub(super) fn install_native_menus() {
    use kortty_macos::{ApplicationMenu, ApplicationMenuEntry};
    fn leaf(entry: Entry, category: Category) -> Option<ApplicationMenuEntry<'static>> {
        Some(match entry {
            Item(action) => ApplicationMenuEntry::Action {
                title: action.label(category),
                id: action as u16,
            },
            Separator => ApplicationMenuEntry::Separator,
            Entry::Unavailable(title) => ApplicationMenuEntry::Disabled { title },
            Entry::BackgroundTransparency | Submenu(_) => return None,
        })
    }
    let branches = Branch::ALL.map(|branch| {
        branch
            .entries()
            .iter()
            .filter_map(|entry| leaf(*entry, branch.category()))
            .collect::<Vec<_>>()
    });
    let entries: Vec<Vec<_>> = Category::ALL
        .into_iter()
        .map(|category| {
            if category == Category::Jobs {
                return vec![ApplicationMenuEntry::JobsStatus];
            }
            category
                .entries()
                .iter()
                .filter_map(|entry| match *entry {
                    Submenu(Branch::RecentlyClosed) => Some(ApplicationMenuEntry::RecentlyClosed {
                        title: Branch::RecentlyClosed.title(),
                    }),
                    Submenu(Branch::OpenRecent) => Some(ApplicationMenuEntry::OpenRecent {
                        title: Branch::OpenRecent.title(),
                    }),
                    Submenu(Branch::RemoteSidebar) => {
                        Some(ApplicationMenuEntry::ConditionalSubmenu {
                            title: Branch::RemoteSidebar.title(),
                            entries: &branches[Branch::RemoteSidebar as usize],
                            visibility_key: NATIVE_REMOTE_SIDEBAR_VISIBILITY,
                        })
                    }
                    Submenu(branch) => Some(ApplicationMenuEntry::Submenu {
                        title: branch.title(),
                        entries: &branches[branch as usize],
                    }),
                    entry => leaf(entry, category),
                })
                .collect()
        })
        .collect();
    let menus: Vec<_> = Category::ALL
        .into_iter()
        .zip(&entries)
        .map(|(category, entries)| ApplicationMenu {
            title: category.title(),
            entries,
        })
        .collect();
    let _ = kortty_macos::install_application_menus(&menus);
}

fn next_item<T: Copy + Eq>(items: &[T], current: Option<T>, forward: bool) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let index = current.and_then(|item| items.iter().position(|value| *value == item));
    let next = match (index, forward) {
        (Some(index), true) => (index + 1) % items.len(),
        (Some(index), false) => (index + items.len() - 1) % items.len(),
        (None, true) => 0,
        (None, false) => items.len() - 1,
    };
    items.get(next).copied()
}

fn wrap(message: Message) -> WorkspaceMessage {
    WorkspaceMessage::Chrome(message)
}

fn menu_popup(
    rows: iced::widget::Column<'_, WorkspaceMessage>,
    colors: ChromeColors,
) -> Element<'_, WorkspaceMessage> {
    container(scrollable(rows).height(Length::Shrink))
        .padding(4)
        .width(306)
        .style(move |_| container::Style {
            background: Some(colors.menu.into()),
            text_color: Some(colors.foreground),
            border: iced::Border {
                color: colors.menu_hover,
                width: 1.0,
                radius: 3.0.into(),
            },
            // Tiny Skia paints shadows beyond the popup bounds used to compute damage.
            #[cfg(not(target_os = "macos"))]
            shadow: iced::Shadow {
                color: Color::from_rgba8(0, 0, 0, 0.45),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 8.0,
            },
            ..container::Style::default()
        })
        .into()
}

fn divider<'a>(color: Color) -> Element<'a, WorkspaceMessage> {
    container(space::horizontal())
        .height(1)
        .width(Length::Fill)
        .style(move |_| container::Style::default().background(color))
        .into()
}

fn button_style(
    status: button::Status,
    hover: Color,
    selected: bool,
    colors: ChromeColors,
) -> button::Style {
    let background =
        selected || matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: background.then_some(hover.into()),
        text_color: if status == button::Status::Disabled {
            colors.muted
        } else {
            colors.foreground
        },
        border: iced::Border {
            radius: 5.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

pub(super) fn tab_button_style(
    theme: &iced::Theme,
    status: button::Status,
    selected: bool,
) -> button::Style {
    let themed = theme.name().starts_with(super::transparency::THEME_PREFIX);
    let colors = if themed {
        let palette = theme.palette();
        ChromeColors::themed(palette.background, palette.text, None)
    } else {
        ChromeColors::DEFAULT
    };
    let active = if themed {
        colors.toolbar
    } else {
        Color::from_rgb8(30, 30, 30)
    };
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                colors.menu_hover
            } else if selected {
                active
            } else {
                colors.menu
            }
            .into(),
        ),
        text_color: if selected {
            colors.foreground
        } else if themed {
            mix(colors.foreground, colors.toolbar, 0.20)
        } else {
            Color::from_rgb8(194, 194, 194)
        },
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

#[cfg(test)]
mod menu_tests {
    use iced::Size;

    use super::*;

    #[test]
    fn windows_shell_toolbar_actions_respect_platform_and_managed_policy() {
        use kortty_pty::LocalShell as Preset;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        for (action, shell) in [
            (PowerShell, Preset::PowerShell),
            (CommandPrompt, Preset::CommandPrompt),
        ] {
            let message = workspace.chrome_action(owner, action);
            if cfg!(windows) {
                assert!(matches!(
                    message,
                    Some(WorkspaceMessage::NewWindowsShell(id, preset))
                        if id == owner && preset == shell
                ));
            } else {
                assert!(message.is_none());
            }
        }
        workspace.clipboard.policy.problem = Some("test managed policy failure".into());
        assert!(workspace.chrome_action(owner, PowerShell).is_none());
        assert!(workspace.chrome_action(owner, CommandPrompt).is_none());
    }

    #[test]
    fn toolbar_overflow_dispatch_uses_its_window_and_skips_unavailable_entries() {
        use super::toolbar_settings::{Edit, Item as ToolbarItem};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update_chrome(Message::OpenToolbarOverflow(owner, Group::Tools, 5));
        let _ = workspace.update_chrome(Message::NavigateToolbarOverflow(
            other,
            Group::Tools,
            Navigation::Next,
        ));
        assert_eq!(workspace.chrome.toolbar_overflow.unwrap().selected, None);
        let _ = workspace.update_chrome(Message::NavigateToolbarOverflow(
            owner,
            Group::Tools,
            Navigation::Next,
        ));
        assert_eq!(workspace.chrome.toolbar_overflow.unwrap().selected, Some(5));
        let _ = workspace.update(WorkspaceMessage::Window(
            owner,
            window::Event::Resized(Size::new(520.0, 420.0)),
        ));
        assert!(workspace.chrome.toolbar_overflow.is_none());

        let _ = workspace.update_toolbar(ToolbarMessage::Open(owner, Catalog::Settings));
        let _ = workspace.update_toolbar(ToolbarMessage::Edit(
            owner,
            Edit::Add(Group::Settings, ToolbarItem::Separator),
        ));
        let _ = workspace.update_toolbar(ToolbarMessage::Edit(
            owner,
            Edit::Add(Group::Settings, ToolbarItem::Action(ShowMenuBar)),
        ));
        let _ = workspace.update_toolbar(ToolbarMessage::Finish(owner));
        // Test double for the persistence completion. No profile file is written.
        let _ = workspace.update_toolbar(ToolbarMessage::Saved(Ok(())));
        let hidden = workspace
            .chrome
            .toolbar
            .layout(owner)
            .items(Group::Settings)
            .len()
            - 1;
        let _ = workspace.update_chrome(Message::OpenToolbarOverflow(
            owner,
            Group::Settings,
            hidden - 1,
        ));
        let _ = workspace.update_chrome(Message::InvokeToolbarOverflow(
            other,
            Group::Settings,
            hidden,
        ));
        assert!(!workspace.menu_bar_hidden(other));
        let _ = workspace.update_chrome(Message::NavigateToolbarOverflow(
            owner,
            Group::Settings,
            Navigation::Next,
        ));
        let _ = workspace.update_chrome(Message::NavigateToolbarOverflow(
            owner,
            Group::Settings,
            Navigation::Activate,
        ));
        assert!(workspace.menu_bar_hidden(owner));
        assert!(!workspace.menu_bar_hidden(other));
        assert!(workspace.chrome.toolbar_overflow.is_none());
    }

    #[test]
    fn connections_menu_offers_attached_commands_only_for_the_focused_transport() {
        use crate::session::{GridSize, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, session);

        assert!(Category::Connections.entries().windows(4).any(|items| {
            matches!(
                items,
                [Item(Sftp), Item(SftpHere), Separator, Item(SendSerialBreak)]
            )
        }));
        assert!(workspace.chrome_enabled(owner, Sftp));
        assert!(!workspace.chrome_enabled(owner, SftpHere));
        assert!(!workspace.chrome_enabled(owner, SendSerialBreak));
        #[cfg(target_os = "macos")]
        for action in [SftpHere, SendSerialBreak] {
            assert!(native_menu_requires_focused_owner(action));
            assert!(!native_can_open_window(action));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn connection_menu_sends_break_through_the_running_serial_pane() {
        use std::time::{Duration, Instant};

        use kortty_terminal::TerminalProfile;
        use rustix::io::{FdFlags, fcntl_setfd};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

        use crate::session::{Frame, GridSize, SerialConnection, Session, Status};

        let peer = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        fcntl_setfd(&peer, FdFlags::CLOEXEC).unwrap();
        grantpt(&peer).unwrap();
        unlockpt(&peer).unwrap();
        let device = ptsname(&peer, Vec::new()).unwrap().into_string().unwrap();
        let grid = GridSize::default();
        let session = Session::start_serial(
            SerialConnection {
                device: device.into(),
                ..SerialConnection::default()
            },
            grid,
            TerminalProfile::Xterm,
            true,
            None,
        )
        .unwrap();
        fn wait_for(session: &Session, ready: impl Fn(&Frame) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(4);
            while !ready(&session.frame()) {
                assert!(Instant::now() < deadline, "serial action timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        wait_for(&session, |frame| frame.status == Status::Running);
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(owner, session);
        assert!(workspace.chrome_enabled(owner, SendSerialBreak));
        assert!(!workspace.chrome_enabled(owner, SftpHere));
        let _ = workspace.update_chrome(Message::Invoke(owner, SendSerialBreak));
        let terminal = &workspace.sessions[&session].session;
        wait_for(terminal, |frame| frame.serial_notice.is_some());
        let result = terminal.frame();
        let _ = terminal.stop().recv_blocking();
        assert_eq!(result.serial_notice.as_deref(), Some("Break sent."));
    }

    #[test]
    fn shutdown_waits_for_the_toolbar_write_and_recovers_after_an_error() {
        for close_last_window in [false, true] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.update_toolbar(ToolbarMessage::Visible(
                owner,
                ToolbarRow::Actions,
                false,
            ));
            assert!(workspace.chrome.toolbar.pending());
            if close_last_window {
                let _ = workspace.close_window(owner);
                assert!(workspace.should_exit_after_window_close());
            } else {
                let _ = workspace.quit();
                assert!(workspace.windows[&owner].notice.is_some());
            }
            assert!(!workspace.exiting);

            // Test double for the asynchronous writer's error completion.
            let _ = workspace.update_toolbar(ToolbarMessage::Saved(Err("test write error".into())));
            assert!(!workspace.chrome.toolbar.pending());
            if !close_last_window {
                assert!(!workspace.exiting);
                let _ = workspace.quit();
            }
            assert!(workspace.exiting);
        }
    }

    #[test]
    fn view_zoom_applies_to_the_active_tab_and_resets_each_pane_to_its_open_size() {
        use crate::session::{GridSize, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        assert!(!workspace.chrome_enabled(owner, ZoomIn));
        let mut create = |font| {
            workspace.store_session(
                Session::disconnected_ibm(IbmKind::Tn3270e),
                GridSize::default(),
                font,
            )
        };
        let left = create(14);
        let right = create(21);
        let background = create(17);
        workspace.add_tab(owner, left);
        workspace.add_tab(other, background);
        let tab = workspace
            .windows
            .get_mut(&owner)
            .unwrap()
            .tab_mut()
            .unwrap();
        let (split, _) = tab
            .grid
            .split(pane_grid::Axis::Vertical, tab.focused, right)
            .unwrap();
        tab.focused = split;
        let menu = OpenMenu::root(owner, Category::View);
        for action in [ZoomIn, ZoomOut, ResetZoom] {
            assert!(
                workspace
                    .menu_choices(menu)
                    .contains(&Choice::Action(action))
            );
        }

        let _ = workspace.update_chrome(Message::Invoke(owner, ZoomIn));
        assert_eq!(workspace.sessions[&left].font, 15);
        assert_eq!(workspace.sessions[&right].font, 22);
        assert_eq!(workspace.sessions[&background].font, 17);
        assert_eq!(workspace.windows[&owner].tab().unwrap().focused, split);

        let _ = workspace.update_chrome(Message::Invoke(owner, ZoomOut));
        let _ = workspace.update_chrome(Message::Invoke(owner, ZoomOut));
        assert_eq!(workspace.sessions[&left].font, 13);
        assert_eq!(workspace.sessions[&right].font, 20);
        let _ = workspace.update_chrome(Message::Invoke(owner, ResetZoom));
        assert_eq!(workspace.sessions[&left].font, 14);
        assert_eq!(workspace.sessions[&right].font, 21);
    }

    #[cfg(all(
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        target_endian = "little"
    ))]
    #[test]
    fn view_transparency_slider_updates_live_and_keeps_the_menu_open() {
        use iced::advanced::widget::Tree;
        use iced::advanced::{Layout, Shell, clipboard, layout};
        use iced::{Event, Font, Point, Rectangle, Renderer, Vector, mouse};

        use super::super::transparency::Message as Transparency;

        fn slider_bounds(layout: Layout<'_>) -> Option<Rectangle> {
            let bounds = layout.bounds();
            if bounds.size() == Size::new(200.0, 16.0) {
                Some(bounds)
            } else {
                layout.children().find_map(slider_bounds)
            }
        }

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_chrome(Message::OpenMenu(owner, Category::View));
        let messages = {
            let mut menu = workspace.menu_bar(owner, &workspace.windows[&owner]);
            let mut tree = Tree::new(menu.as_widget());
            let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let viewport = Rectangle::with_size(Size::new(1024.0, 900.0));
            let node = menu.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            let mut overlay = menu
                .as_widget_mut()
                .overlay(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &viewport,
                    Vector::ZERO,
                )
                .unwrap();
            let popup = overlay.as_overlay_mut();
            let popup_node = popup.layout(&renderer, viewport.size());
            let popup_layout = Layout::new(&popup_node);
            let bounds = slider_bounds(popup_layout).expect("View menu transparency slider");
            let at = |ratio| Point::new(bounds.x + ratio * bounds.width, bounds.y + 8.0);
            let mut messages = Vec::new();
            for (event, cursor) in [
                (mouse::Event::ButtonPressed(mouse::Button::Left), at(0.35)),
                (mouse::Event::CursorMoved { position: at(0.60) }, at(0.60)),
                (mouse::Event::ButtonReleased(mouse::Button::Left), at(0.60)),
            ] {
                popup.update(
                    &Event::Mouse(event),
                    popup_layout,
                    mouse::Cursor::Available(cursor),
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                );
            }
            messages
        };
        assert!(matches!(
            messages.as_slice(),
            [
                WorkspaceMessage::Transparency(Transparency::Set(35)),
                WorkspaceMessage::Transparency(Transparency::Set(60)),
                WorkspaceMessage::Transparency(Transparency::Save),
            ]
        ));

        // Test double for a pending save. Dispatch only live changes to avoid profile writes.
        workspace.transparency_save_pending = true;
        for (message, expected) in messages.into_iter().zip([35, 60]) {
            let _ = workspace.update(message);
            assert_eq!(workspace.window_settings.terminal_transparency, expected);
            assert!(
                workspace.chrome.menu.is_some_and(|menu| {
                    menu.window == owner && menu.category == Category::View
                })
            );
        }
    }

    #[test]
    fn menu_bar_visibility_is_per_window_and_recovery_only_shows() {
        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let _ = workspace.invoke_chrome(first, ShowMenuBar);
        assert!(workspace.menu_bar_hidden(first));
        assert!(!workspace.menu_bar_hidden(second));
        assert!(!workspace.chrome_checked(first, &workspace.windows[&first], ShowMenuBar));
        assert!(workspace.chrome.toolbar.layout(first).actions_visible());
        let _ = workspace.update_chrome(Message::OpenMenu(first, Category::View));
        assert!(workspace.chrome.menu.is_none());

        let recovery = Message::OpenMenu(first, Category::StatusRecovery);
        let _ = workspace.update_chrome(recovery.clone());
        let _ = workspace.update_chrome(Message::RestoreStatus(first, ToggleActionToolbar));
        assert!(workspace.chrome.toolbar.layout(first).actions_visible());
        assert!(!workspace.chrome.toolbar.pending());
        assert!(workspace.menu_bar_hidden(first));

        let _ = workspace.update_chrome(recovery);
        let _ = workspace.navigate_chrome(first, Navigation::Next);
        let _ = workspace.navigate_chrome(first, Navigation::Activate);
        assert!(!workspace.menu_bar_hidden(first));
        let _ = workspace.update_chrome(Message::RestoreStatus(first, ShowMenuBar));
        assert!(!workspace.menu_bar_hidden(first));

        let _ = workspace.invoke_chrome(first, ShowMenuBar);
        assert!(workspace.menu_bar_hidden(first));
        let _ = workspace.chrome.window_closed(first);
        assert!(!workspace.menu_bar_hidden(first));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_menu_can_restore_a_hidden_bar_only_in_the_focused_main_window() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.invoke_chrome(owner, ShowMenuBar);
        let _ = workspace.native_menu(ShowMenuBar as u16);
        assert!(workspace.menu_bar_hidden(owner));
        assert!(!native_can_open_window(ShowMenuBar));

        let _ = workspace.update(WorkspaceMessage::Window(owner, window::Event::Focused));
        let _ = workspace.native_menu(ShowMenuBar as u16);
        assert!(!workspace.menu_bar_hidden(owner));
    }

    #[test]
    fn multi_exec_menu_registers_the_four_reference_commands_without_default_shortcuts() {
        use super::super::key_bindings::command;

        assert!(Category::View.entries().windows(2).any(|entries| matches!(
            entries,
            [Submenu(Branch::Panes), Submenu(Branch::MultiExec)]
        )));
        assert!(matches!(
            Branch::MultiExec.entries(),
            [
                Item(MultiExecIncludePane),
                Item(MultiExecIncludeTab),
                Item(MultiExecIncludeWindow),
                Separator,
                Item(MultiExecStop)
            ]
        ));
        for (action, label) in [
            (MultiExecIncludePane, "Include This Pane"),
            (MultiExecIncludeTab, "Include All Panes of This Tab"),
            (
                MultiExecIncludeWindow,
                "Include All Terminals of This Window",
            ),
            (MultiExecStop, "Stop Multi-exec"),
        ] {
            let command = command(action).unwrap();
            assert_eq!(command.title, label);
            assert_eq!(command.menu, "View › Multi-exec");
            assert!(command.default_chord().is_none());
            #[cfg(target_os = "macos")]
            {
                assert_eq!(Action::from_native(action as u16), Some(action));
                assert!(!native_can_open_window(action));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_multi_exec_include_has_no_unfocused_window_fallback() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        assert_eq!(workspace.native_active_window().unwrap().0, owner);
        assert!(workspace.native_focused_main_owner().is_none());

        let _ = workspace.update(WorkspaceMessage::Window(owner, window::Event::Focused));
        assert_eq!(workspace.native_focused_main_owner().unwrap().0, owner);

        let _ = workspace.show_about(owner);
        assert!(workspace.native_focused_main_owner().is_none());
    }

    #[test]
    fn move_tab_chooser_moves_hosted_tools_and_rechecks_both_windows() {
        use super::super::{clipboard_access, close_all_dialog, tool_tabs};

        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        // Test-only ID for the destination's confirmation, with no terminal worker.
        workspace.add_tab(destination, crate::session::SessionId(42));
        let _ = workspace.show_openpgp(source);
        let tool = workspace
            .hosted_tool_in(source, tool_tabs::Kind::OpenPgp)
            .unwrap();
        let tab = workspace.windows[&source].active_tab_id().unwrap();
        let transfer = |to| WorkspaceMessage::Transfer {
            from: source,
            to,
            tab,
        };

        let _ = workspace.invoke_chrome(source, MoveTab);
        assert!(workspace.windows[&source].transfer_open);
        let _ = workspace.request_close_all_tabs(destination);
        let confirm = workspace.close_all_dialog_for_owner(destination).unwrap();
        let _ = workspace.update(transfer(Some(destination)));
        assert_eq!(workspace.hosted_tool_owner(tool), Some(source));
        let _ =
            workspace.update_close_all_dialog(confirm, close_all_dialog::Message::Decide(false));

        let clipboard = |visible| {
            WorkspaceMessage::Clipboard(clipboard_access::Message::ShowSettings(source, visible))
        };
        let _ = workspace.update(clipboard(true));
        let _ = workspace.update(transfer(Some(destination)));
        let _ = workspace.update(transfer(None));
        assert_eq!(workspace.hosted_tool_owner(tool), Some(source));
        assert_eq!(workspace.windows.len(), 2);
        let _ = workspace.update(clipboard(false));

        let _ = workspace.update(transfer(Some(destination)));
        assert_eq!(workspace.hosted_tool_owner(tool), Some(destination));
        assert_eq!(workspace.windows[&destination].active_tab_id(), Some(tab));
        let _ = workspace.invoke_chrome(destination, MoveTab);
        let _ = workspace.update(WorkspaceMessage::Transfer {
            from: destination,
            to: None,
            tab,
        });
        let new_owner = workspace.hosted_tool_owner(tool).unwrap();
        assert_ne!(new_owner, source);
        assert_ne!(new_owner, destination);
        assert_eq!(workspace.windows[&new_owner].active_tab_id(), Some(tab));
    }

    #[test]
    fn remote_sidebar_submenu_matches_reference_actions_and_defaults() {
        use super::super::key_bindings::command;
        use RemoteSidebarPosition::{Hidden, Left, Right};

        assert!(
            Category::View
                .entries()
                .iter()
                .any(|entry| matches!(entry, Submenu(Branch::RemoteSidebar)))
        );
        assert!(matches!(
            Branch::RemoteSidebar.entries(),
            [
                Item(RemoteSidebarLeft),
                Item(RemoteSidebarRight),
                Separator,
                Item(RemoteSidebarToggle)
            ]
        ));
        for action in [RemoteSidebarLeft, RemoteSidebarRight, RemoteSidebarToggle] {
            let command = command(action).unwrap();
            assert_eq!(command.menu, "View › Remote Files Sidebar");
            assert!(command.default_chord().is_none());
        }
        assert_eq!(
            RemoteSidebarLeft.remote_sidebar_placement(Hidden),
            Some(Left)
        );
        assert_eq!(
            RemoteSidebarLeft.remote_sidebar_placement(Left),
            Some(Hidden)
        );
        assert_eq!(
            RemoteSidebarRight.remote_sidebar_placement(Left),
            Some(Right)
        );
        assert_eq!(
            RemoteSidebarRight.remote_sidebar_placement(Right),
            Some(Hidden)
        );
        assert_eq!(
            RemoteSidebarToggle.remote_sidebar_placement(Hidden),
            Some(Right)
        );
        assert_eq!(
            RemoteSidebarToggle.remote_sidebar_placement(Left),
            Some(Hidden)
        );
    }

    #[test]
    fn remote_sidebar_dock_stays_available_when_visible_and_no_ssh_is_connected() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let menu = OpenMenu::root(owner, Category::View);
        assert!(!workspace.remote_sidebar_menu_offered(Some(owner)));
        assert!(
            !workspace
                .menu_choices(menu)
                .contains(&Choice::Branch(Branch::RemoteSidebar))
        );

        workspace.window_settings.remote_sidebar_position = RemoteSidebarPosition::Right;
        assert!(workspace.remote_sidebar_menu_offered(None));
        assert!(
            workspace
                .menu_choices(menu)
                .contains(&Choice::Branch(Branch::RemoteSidebar))
        );
        assert!(workspace.chrome_checked(owner, &workspace.windows[&owner], RemoteSidebarRight));
        assert!(!workspace.chrome_checked(owner, &workspace.windows[&owner], RemoteSidebarLeft));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_menu_does_not_fall_back_to_another_window_while_close_all_has_focus() {
        use crate::session::SessionId;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (newer, _) = workspace.open_window(false);
        workspace.add_tab(owner, SessionId(42));
        workspace.add_tab(newer, SessionId(43));
        let _ = workspace.request_close_all_tabs(owner);
        let dialog = workspace.close_all_dialog_for_owner(owner).unwrap();
        let _ = workspace.update(WorkspaceMessage::Window(dialog, window::Event::Focused));
        assert_eq!(workspace.native_active_window().unwrap().0, newer);
        assert!(workspace.native_modal_blocks_fallback(workspace.native_active_window()));

        let _ = workspace.native_menu(CloseTab as u16);
        assert_eq!(workspace.windows[&newer].tabs.len(), 1);
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);

        let _ = workspace.update(WorkspaceMessage::Window(newer, window::Event::Focused));
        assert!(!workspace.native_modal_blocks_fallback(workspace.native_active_window()));
        let _ = workspace.native_menu(CloseTab as u16);
        assert!(workspace.windows[&newer].tabs.is_empty());
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_menu_does_not_fall_back_to_another_window_while_backup_has_focus() {
        use crate::session::SessionId;
        use crate::workspace::backup_ui::Message as BackupMessage;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (newer, _) = workspace.open_window(false);
        workspace.add_tab(owner, SessionId(42));
        workspace.add_tab(newer, SessionId(43));
        let _ = workspace.update(WorkspaceMessage::Backup(owner, BackupMessage::OpenImport));
        let dialog = workspace.backup_for_owner(owner).unwrap();
        let _ = workspace.update(WorkspaceMessage::Window(dialog, window::Event::Focused));
        assert_eq!(workspace.native_active_window().unwrap().0, newer);

        let _ = workspace.native_menu(CloseTab as u16);
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert_eq!(workspace.windows[&newer].tabs.len(), 1);

        let _ = workspace.update(WorkspaceMessage::Window(newer, window::Event::Focused));
        let _ = workspace.native_menu(CloseTab as u16);
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert!(workspace.windows[&newer].tabs.is_empty());
        assert_eq!(workspace.backup_for_owner(owner), Some(dialog));
    }

    #[test]
    fn file_menu_orders_tab_closures_before_recently_closed() {
        let file = Category::File.entries();
        assert!(matches!(
            file,
            [
                Item(QuickConnect),
                Item(IncognitoShell),
                Item(RenameTab),
                Item(CloseTab),
                Item(CloseOtherTabs),
                Item(CloseTabsToRight),
                Item(CloseAllTabs),
                Item(ReopenClosedTab),
                Submenu(Branch::RecentlyClosed),
                Separator,
                ..
            ]
        ));
    }

    #[test]
    fn recently_closed_uses_current_ids_and_ignores_a_row_from_before_clear() {
        use crate::session::SessionId;
        use crate::session_security::LocalSettings;
        use crate::workspace::recently_closed::Origin;
        use kortty_terminal::TerminalProfile;

        let mut workspace = Workspace::default();
        let (id, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update_chrome(Message::OpenMenu(id, Category::File));
        workspace.open_menu_branch(id, Branch::RecentlyClosed);
        assert!(!workspace.chrome_enabled(id, ReopenClosedTab));
        assert!(
            workspace
                .menu_choices(workspace.chrome.menu.unwrap())
                .is_empty()
        );

        // UI fixture only: the menu needs launch metadata, without starting a
        // terminal just to populate the history.
        workspace.add_tab(id, SessionId(42));
        let record = |workspace: &mut Workspace, title: &str| {
            let tab = workspace.windows.get_mut(&id).unwrap().tab_mut().unwrap();
            tab.custom_title = Some(title.into());
            tab.reopen_origin = Some(Origin::Local(
                TerminalProfile::Xterm,
                Some(14),
                LocalSettings::default(),
            ));
            let tab = workspace.windows[&id].tab().unwrap();
            let closed = workspace.remember_tab(tab).unwrap();
            workspace.remember_closed_tabs(false, vec![closed]);
            workspace.recently_closed_menu_rows()[0].0
        };
        let old = record(&mut workspace, "first_tab");
        assert!(workspace.chrome_enabled(id, ReopenClosedTab));
        let _ = workspace.navigate_chrome(id, Navigation::Next);
        assert_eq!(
            workspace.chrome.menu.unwrap().selected,
            Some(Choice::RecentlyClosed(old))
        );
        let _ = workspace.update_chrome(Message::ClearRecentHistory(other));
        assert!(workspace.has_recently_closed());
        let _ = workspace.update_chrome(Message::ClearRecentHistory(id));
        assert!(!workspace.has_recently_closed());
        let current = record(&mut workspace, "second_tab");
        assert_ne!(current, old);

        let _ = workspace.update_chrome(Message::OpenMenu(id, Category::File));
        workspace.open_menu_branch(id, Branch::RecentlyClosed);
        let _ = workspace.update_chrome(Message::ReopenRecent(id, old));
        assert_eq!(
            workspace.chrome.menu.unwrap().branch,
            Some(Branch::RecentlyClosed)
        );
        assert_eq!(
            workspace.recently_closed_menu_rows(),
            [(current, "second_tab".to_string())]
        );
        assert_eq!(workspace.windows[&id].tabs.len(), 1);
        assert!(workspace.sessions.is_empty());
    }

    #[test]
    fn keyboard_can_enter_and_leave_the_security_submenu() {
        let mut workspace = Workspace::default();
        let (id, _) = workspace.open_window(false);
        let _ = workspace.update_chrome(Message::OpenMenu(id, Category::Configuration));
        let _ = workspace.navigate_chrome(id, Navigation::Next);
        assert_eq!(
            workspace.chrome.menu.unwrap().selected,
            Some(Choice::Branch(Branch::Security))
        );
        let _ = workspace.navigate_chrome(id, Navigation::Right);
        let _ = workspace.navigate_chrome(id, Navigation::Next);
        let menu = workspace.chrome.menu.unwrap();
        assert_eq!(menu.branch, Some(Branch::Security));
        assert_eq!(menu.selected, Some(Choice::Action(Vault)));

        let _ = workspace.navigate_chrome(id, Navigation::Escape);
        let menu = workspace.chrome.menu.unwrap();
        assert_eq!(menu.branch, None);
        assert_eq!(menu.selected, Some(Choice::Branch(Branch::Security)));
        let _ = workspace.navigate_chrome(id, Navigation::Escape);
        assert!(workspace.chrome.menu.is_none());
    }

    #[test]
    fn unlock_vault_menu_is_application_modal_and_only_unlocks_an_existing_vault() {
        use crate::vault_window::{Completion, Message as VaultMessage, VaultWindow};
        use kortty_vault::Vault as EncryptedVault;

        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "kortty-unlock-vault-menu-{:016x}",
            u64::from_ne_bytes(nonce)
        )));
        let mut directory = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            directory.mode(0o700);
        }
        directory.create(&scratch.0).unwrap();
        let path = scratch.0.join("vault.bin");
        let password = b"app-test-master-password";

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let invoke = WorkspaceMessage::Chrome(Message::Invoke(owner, UnlockVault));
        workspace.vault = Some(VaultWindow::open(path.clone()).unwrap());
        assert!(!workspace.chrome_enabled(owner, UnlockVault));
        assert_eq!(workspace.update(invoke.clone()).units(), 0);
        assert!(!path.exists());

        drop(EncryptedVault::create(&path, password).unwrap());
        workspace.vault = Some(VaultWindow::open(path.clone()).unwrap());
        assert!(workspace.chrome_enabled(owner, UnlockVault));
        let _ = workspace.update(WorkspaceMessage::Chrome(Message::Invoke(
            window::Id::unique(),
            UnlockVault,
        )));
        assert!(workspace.vault_id.is_none());
        let _ = workspace.update(invoke.clone());
        let canceled = workspace.vault_id.unwrap();
        assert_eq!(workspace.title(canceled), "Unlock Vault");
        assert!(workspace.profile_tool_view(canceled).is_some());
        assert_eq!(workspace.owner_dialog(owner).unwrap().0, canceled);
        assert_eq!(workspace.owner_dialog(other).unwrap().0, canceled);
        assert!(workspace.owner_dialog(canceled).is_none());
        assert!(!workspace.chrome_enabled(owner, Vault));
        assert!(!workspace.chrome_enabled(other, UnlockVault));
        let _ = workspace.update(WorkspaceMessage::Window(
            other,
            window::Event::CloseRequested,
        ));
        assert!(workspace.windows.contains_key(&other));
        let _ = workspace.update(WorkspaceMessage::Window(
            canceled,
            window::Event::CloseRequested,
        ));
        assert!(workspace.vault_id.is_none());
        assert!(workspace.chrome_enabled(owner, UnlockVault));

        let _ = workspace.update(invoke.clone());
        let prompt = workspace.vault_id.unwrap();
        assert_ne!(prompt, canceled);
        assert_eq!(
            workspace
                .update(WorkspaceMessage::Vault(canceled, VaultMessage::Close))
                .units(),
            0
        );
        let _ = workspace.update(WorkspaceMessage::Vault(
            prompt,
            VaultMessage::Completed(Completion::unlock_fixture(&path, b"wrong password", false)),
        ));
        assert_eq!(workspace.vault_id, Some(prompt));
        let _ = workspace.update(WorkspaceMessage::Vault(
            prompt,
            VaultMessage::Completed(Completion::unlock_fixture(&path, password, true)),
        ));
        assert!(workspace.vault_id.is_none());
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.owner_dialog(other).is_none());
        assert!(!workspace.chrome_enabled(owner, UnlockVault));
        assert_eq!(workspace.update(invoke).units(), 0);

        let _ = workspace.update(WorkspaceMessage::Chrome(Message::Invoke(owner, Vault)));
        let credentials = workspace.vault_id.unwrap();
        assert_eq!(workspace.title(credentials), "kortty · Credential Vault");
        let _ = workspace.update(WorkspaceMessage::Vault(credentials, VaultMessage::Lock));
        assert!(workspace.chrome_enabled(owner, UnlockVault));
    }

    #[test]
    fn openpgp_recipients_security_menu_opens_a_separate_modal_window() {
        use crate::snippet_window::SnippetWindow;
        use crate::snippets::SnippetId;

        assert!(matches!(
            Branch::Security.entries(),
            [
                Item(UnlockVault),
                Separator,
                Item(Vault),
                Item(OpenPgpRecipients),
                Item(SshKeys),
                Item(KnownHosts),
                ..
            ]
        ));

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let snippet_window = window::Id::unique();
        let library = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/.scratch")
            .join(format!(
                "openpgp-menu-{}-{snippet_window:?}",
                std::process::id()
            ))
            .join("snippets.json");
        // The test opens an empty library without creating files or running the load task.
        let mut snippets = SnippetWindow::open(library).unwrap();
        let probe = SnippetId::try_from("0".repeat(32)).unwrap();
        assert!(snippets.reveal_saved(probe));
        workspace.snippets.insert(
            snippet_window,
            super::super::snippet_access::Surface::new(snippets, owner),
        );

        let closed_owner = window::Id::unique();
        let _ = workspace.update_chrome(Message::Invoke(closed_owner, OpenPgpRecipients));
        assert!(workspace.openpgp.is_empty());
        assert!(
            workspace
                .snippets
                .get_mut(&snippet_window)
                .unwrap()
                .window
                .reveal_saved(probe)
        );

        let _ = workspace.update_chrome(Message::OpenMenu(owner, Category::Configuration));
        workspace.open_menu_branch(owner, Branch::Security);
        let _ = workspace.navigate_chrome(owner, Navigation::Next);
        let _ = workspace.navigate_chrome(owner, Navigation::Next);
        assert_eq!(
            workspace.chrome.menu.unwrap().selected,
            Some(Choice::Action(OpenPgpRecipients))
        );
        let _ = workspace.navigate_chrome(owner, Navigation::Activate);
        let recipients = workspace.openpgp_for_source(owner).unwrap();
        assert_ne!(recipients, snippet_window);
        assert_eq!(workspace.owner_dialog(owner).unwrap().0, recipients);
        assert!(
            workspace
                .snippets
                .get_mut(&snippet_window)
                .unwrap()
                .window
                .reveal_saved(probe)
        );
        let _ = workspace.show_openpgp(snippet_window);
        assert_eq!(workspace.openpgp_for_source(owner), Some(recipients));
        assert!(workspace.chrome.menu.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn openpgp_recipients_native_menu_creates_an_owner_after_last_window_closed() {
        let mut workspace = Workspace::default();
        assert!(workspace.windows.is_empty());
        assert_eq!(
            Action::from_native(OpenPgpRecipients as u16),
            Some(OpenPgpRecipients)
        );
        let _ = workspace.native_menu(OpenPgpRecipients as u16);
        assert_eq!(workspace.windows.len(), 1);
        // The native task invokes the command after the new owner opens.
        assert!(workspace.openpgp.is_empty());
        let owner = *workspace.windows.keys().next().unwrap();
        let _ = workspace.update_chrome(Message::Invoke(owner, OpenPgpRecipients));
        assert!(workspace.openpgp_for_source(owner).is_some());
    }
}
