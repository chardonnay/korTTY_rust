use std::fmt;

use iced::keyboard::{Key, Modifiers};
use iced::widget::{
    self, button, checkbox, column, container, pick_list, row, rule, scrollable, slider, space,
    text, text_input, tooltip,
};
use iced::{Alignment, Element, Length, Size, Task, window};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::clipboard_policy::ClipboardPolicy;
use crate::profile::{self, session::RestoreMode};
use crate::profile_backup::preferences::Preferences as BackupPreferences;
use crate::terminal_theme::{BoldTextMode, MONOSPACED, Scheme, installed_families, installed_font};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    Message as Desktop, Workspace,
    appearance::Design,
    chrome::TOOLBAR_ICON_SIZE,
    key_bindings::{Edit, Editor as KeyboardEditor},
    paste_protection::{MAX_PASTE_KIB, WarningMode},
    session_restore,
    tab_titles::{
        MAX_TERMINAL_SILENCE_SECONDS, MIN_TERMINAL_SILENCE_SECONDS, RemoteSidebarPosition,
        WindowSettings,
    },
};

const TITLE: &str = "Settings";
const MAX_SEARCH_BYTES: usize = 192;

mod appearance_settings;
mod backup;
mod color_settings;
mod export_settings;
mod logging_settings;
mod quick_select_settings;
mod startup_settings;
mod theme_settings;
mod video_settings;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Font,
    Colors,
    Themes,
    Appearance,
    Terminal,
    Messaging,
    Video,
    Backup,
    Logging,
    Export,
    Window,
    Keyboard,
}

impl Section {
    const ALL: [Self; 12] = [
        Self::Font,
        Self::Colors,
        Self::Themes,
        Self::Appearance,
        Self::Terminal,
        Self::Messaging,
        Self::Video,
        Self::Backup,
        Self::Logging,
        Self::Export,
        Self::Window,
        Self::Keyboard,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Font => "Font",
            Self::Colors => "Colors",
            Self::Themes => "Themes",
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Messaging => "Messaging",
            Self::Video => "Video",
            Self::Backup => "Backup",
            Self::Logging => "Logging",
            Self::Export => "Export",
            Self::Window => "Window",
            Self::Keyboard => "Keyboard",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Font => 0,
            Self::Colors => 1,
            Self::Themes => 2,
            Self::Appearance => 3,
            Self::Terminal => 4,
            Self::Messaging => 5,
            Self::Video => 6,
            Self::Backup => 7,
            Self::Logging => 8,
            Self::Export => 9,
            Self::Window => 10,
            Self::Keyboard => 11,
        }
    }

    pub(super) fn guide_chapter(self) -> String {
        format!("Settings · {}", self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Setting {
    FontFamily,
    FontSize,
    Colors,
    Themes,
    Appearance,
    PasteWarning,
    LargePasteWarning,
    ClipboardWrite,
    ClipboardRead,
    CommandMarks,
    CommandTimestamps,
    RemoteSidebar,
    TerminalLinkDetection,
    TerminalCopyOnSelect,
    QuickSelectAlphabet,
    QuickSelectPatterns,
    TerminalScrollbar,
    ScrollbackCompression,
    BoldTextMode,
    SilenceThreshold,
    RecordingAfterRestart,
    RecordingColors,
    BackupRetention,
    BackupCredential,
    AutomaticBackup,
    LogDirectory,
    LogRetention,
    JournalDirectory,
    JournalClickableLinks,
    JournalAiProfile,
    JournalVision,
    JournalAutomaticVision,
    ExportWatermark,
    ExportFooter,
    ToolbarIconSize,
    RememberGeometry,
    RememberDashboard,
    FixedGeometry,
    ToolWindowTabs,
    ConnectionColorBorder,
    SplitPaneFocusRing,
    ShellTitles,
    RecentTabs,
    Restore,
    Transparency,
    Shortcuts,
}

impl Setting {
    const ALL: [Self; 46] = [
        Self::FontFamily,
        Self::FontSize,
        Self::Colors,
        Self::Themes,
        Self::Appearance,
        Self::PasteWarning,
        Self::LargePasteWarning,
        Self::ClipboardWrite,
        Self::ClipboardRead,
        Self::CommandMarks,
        Self::CommandTimestamps,
        Self::RemoteSidebar,
        Self::TerminalLinkDetection,
        Self::TerminalCopyOnSelect,
        Self::QuickSelectAlphabet,
        Self::QuickSelectPatterns,
        Self::TerminalScrollbar,
        Self::ScrollbackCompression,
        Self::BoldTextMode,
        Self::SilenceThreshold,
        Self::RecordingAfterRestart,
        Self::RecordingColors,
        Self::BackupRetention,
        Self::BackupCredential,
        Self::AutomaticBackup,
        Self::LogDirectory,
        Self::LogRetention,
        Self::JournalDirectory,
        Self::JournalClickableLinks,
        Self::JournalAiProfile,
        Self::JournalVision,
        Self::JournalAutomaticVision,
        Self::ExportWatermark,
        Self::ExportFooter,
        Self::ToolbarIconSize,
        Self::RememberGeometry,
        Self::RememberDashboard,
        Self::FixedGeometry,
        Self::ToolWindowTabs,
        Self::ConnectionColorBorder,
        Self::SplitPaneFocusRing,
        Self::ShellTitles,
        Self::RecentTabs,
        Self::Restore,
        Self::Transparency,
        Self::Shortcuts,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::FontFamily => "Terminal font family",
            Self::FontSize => "Terminal font size",
            Self::Colors => "Terminal colors",
            Self::Themes => "Terminal themes",
            Self::Appearance => "App design and animations",
            Self::PasteWarning => "Warn about line breaks and control characters",
            Self::LargePasteWarning => "Warn about pastes larger than",
            Self::ClipboardWrite => {
                "Let programs in the terminal copy text to the clipboard (OSC 52 and OSC 5522)"
            }
            Self::ClipboardRead => {
                "Let programs in the terminal ask to read the clipboard (OSC 5522)"
            }
            Self::CommandMarks => {
                "Use the command marks of shells set up for shell integration (OSC 133)"
            }
            Self::CommandTimestamps => "Show command timestamps",
            Self::RemoteSidebar => "Remote files sidebar",
            Self::TerminalLinkDetection => "Detect web and e-mail addresses in terminal text",
            Self::TerminalCopyOnSelect => "Copy selection to clipboard automatically",
            Self::QuickSelectAlphabet => "Quick select label letters",
            Self::QuickSelectPatterns => "Quick select patterns",
            Self::TerminalScrollbar => "Show scrollbar in terminal",
            Self::ScrollbackCompression => "Compress the scrollback in the background while idle",
            Self::BoldTextMode => "Bold text",
            Self::SilenceThreshold => "Monitor for Silence reports after",
            Self::RecordingAfterRestart => "Enable terminal recording after app restart",
            Self::RecordingColors => "Capture terminal colors in recordings",
            Self::BackupRetention => "Maximum number of backups",
            Self::BackupCredential => "Backup encryption and credentials",
            Self::AutomaticBackup => "Automatic backup schedule and folder",
            Self::LogDirectory => "Application log directory",
            Self::LogRetention => "Keep application logs",
            Self::JournalDirectory => "Session journal storage folder",
            Self::JournalClickableLinks => "Make web addresses in journals clickable",
            Self::JournalAiProfile => "Enable local journal AI profile",
            Self::JournalVision => "Allow image input for this local model",
            Self::JournalAutomaticVision => "Automatically analyze screenshots with AI",
            Self::ExportWatermark => "Add a watermark to exported PDFs",
            Self::ExportFooter => "Show a footer in exported documents",
            Self::ToolbarIconSize => "Toolbar icon size",
            Self::RememberGeometry => "Remember window geometry",
            Self::RememberDashboard => "Remember dashboard state",
            Self::FixedGeometry => "Use fixed window geometry",
            Self::ToolWindowTabs => "Open tool windows as tabs",
            Self::ConnectionColorBorder => "Frame the terminal in its connection's tab color",
            Self::SplitPaneFocusRing => "Frame the split pane the keyboard is in",
            Self::ShellTitles => "Name terminal tabs after the title the shell sets",
            Self::RecentTabs => "Ctrl+Tab switches tabs in the order they were last used",
            Self::Restore => "At startup",
            Self::Transparency => "Terminal transparency",
            Self::Shortcuts => "Keyboard shortcuts",
        }
    }

    fn section(self) -> Section {
        match self {
            Self::FontFamily | Self::FontSize => Section::Font,
            Self::Colors => Section::Colors,
            Self::Themes => Section::Themes,
            Self::Appearance => Section::Appearance,
            Self::PasteWarning
            | Self::LargePasteWarning
            | Self::ClipboardWrite
            | Self::ClipboardRead
            | Self::CommandMarks
            | Self::CommandTimestamps
            | Self::RemoteSidebar
            | Self::TerminalLinkDetection
            | Self::TerminalCopyOnSelect
            | Self::QuickSelectAlphabet
            | Self::QuickSelectPatterns
            | Self::TerminalScrollbar
            | Self::ScrollbackCompression
            | Self::BoldTextMode => Section::Terminal,
            Self::SilenceThreshold => Section::Messaging,
            Self::RecordingAfterRestart | Self::RecordingColors => Section::Video,
            Self::BackupRetention | Self::BackupCredential | Self::AutomaticBackup => {
                Section::Backup
            }
            Self::LogDirectory
            | Self::LogRetention
            | Self::JournalDirectory
            | Self::JournalClickableLinks
            | Self::JournalAiProfile
            | Self::JournalVision
            | Self::JournalAutomaticVision => Section::Logging,
            Self::ExportWatermark | Self::ExportFooter => Section::Export,
            Self::ToolbarIconSize
            | Self::RememberGeometry
            | Self::RememberDashboard
            | Self::FixedGeometry
            | Self::ToolWindowTabs
            | Self::ConnectionColorBorder
            | Self::SplitPaneFocusRing
            | Self::ShellTitles
            | Self::RecentTabs
            | Self::Restore
            | Self::Transparency => Section::Window,
            Self::Shortcuts => Section::Keyboard,
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::FontFamily => "monospace typeface typography font face",
            Self::FontSize => "monospace text typography zoom appearance",
            Self::Colors => {
                "color profile palette foreground background cursor blink selection ANSI SGR TrueColor"
            }
            Self::Themes => "palette background foreground cursor colors appearance light dark",
            Self::Appearance => "windows dialogs menus toolbar theme light dark reduced motion",
            Self::PasteWarning => {
                "paste safety protection multiline bracketed escape bidi confirmation"
            }
            Self::LargePasteWarning => "paste size limit threshold KiB bytes confirmation",
            Self::ClipboardWrite => "paste write",
            Self::ClipboardRead => "paste access consent",
            Self::CommandMarks => "prompt navigation jumps setup bash zsh fish",
            Self::CommandTimestamps => "terminal time gutter command date",
            Self::RemoteSidebar => "sftp ssh folder dock left right hidden",
            Self::TerminalLinkDetection => "links clickable open URL email hyperlink OSC 8",
            Self::TerminalCopyOnSelect => {
                "copy on select automatic mouse drag terminal text clipboard PRIMARY"
            }
            Self::QuickSelectAlphabet => "links quick select labels alphabet shortcut keys",
            Self::QuickSelectPatterns => {
                "links quick select custom match regex regular expressions"
            }
            Self::TerminalScrollbar => "scroll back scrollback history vertical right",
            Self::ScrollbackCompression => "scrollback history memory compress idle background",
            Self::BoldTextMode => "terminal font weight bright ansi colors palette sgr xterm",
            Self::SilenceThreshold => {
                "silence threshold monitor notification quiet timeout seconds terminal output"
            }
            Self::RecordingAfterRestart => "video capture startup availability session replay",
            Self::RecordingColors => "video capture preserve style foreground background replay",
            Self::BackupRetention => "backup retention maximum archive rotate unlimited",
            Self::BackupCredential => {
                "backup encrypted ZIP AES-256 password vault OpenPGP GPG recipient fingerprint"
            }
            Self::AutomaticBackup => "backup off daily weekly monthly schedule folder directory",
            Self::LogDirectory => {
                "logging kortty.log application diagnostic support troubleshooting path folder"
            }
            Self::LogRetention => "logging retention days archive rotation compress gzip unlimited",
            Self::JournalDirectory => {
                "logging session journal storage directory path folder recording notes screenshots"
            }
            Self::JournalClickableLinks => {
                "session journal links hyperlink URL browser notes AI summaries"
            }
            Self::JournalAiProfile => {
                "session journal AI profile model server loopback address filter journal exports by topic"
            }
            Self::JournalVision => "session journal screenshot image vision consent local AI model",
            Self::JournalAutomaticVision => {
                "session journal automatically analyze screenshot capture description tags"
            }
            Self::ExportWatermark => "pdf branding text color mark document page",
            Self::ExportFooter => "pdf markdown html branding text document page",
            Self::ToolbarIconSize => "symbol size toolbar button pixels appearance",
            Self::RememberGeometry => "startup normal bounds size position maximized",
            Self::RememberDashboard => "startup show hide dashboard state navigation dock",
            Self::FixedGeometry => "startup width height fixed window position dimensions",
            Self::ToolWindowTabs => "management manager separate embedded presentation",
            Self::ConnectionColorBorder => "connection color border environment frame",
            Self::SplitPaneFocusRing => "active split pane focus ring border outline",
            Self::ShellTitles => "OSC 0 OSC 2 title",
            Self::RecentTabs => "MRU most recently used switch previous next cycle control tab",
            Self::Restore => "session restore previous automatic",
            Self::Transparency => "opacity background see-through",
            Self::Shortcuts => "key bindings accelerator record commands keymap",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id),
    OpenAt(window::Id, Section),
    Section(window::Id, Section),
    Guide(window::Id),
    Search(window::Id, String),
    Reveal(window::Id, Setting),
    FontFamily(window::Id, String),
    FontSize(window::Id, u8),
    FontSizeInput(window::Id, String),
    Color(window::Id, color_settings::Edit),
    Theme(window::Id, theme_settings::Edit),
    Design(window::Id, Design),
    DesignAnimations(window::Id, bool),
    PasteWarning(window::Id, WarningMode),
    LargePasteWarning(window::Id, u8),
    ToolbarIconSize(window::Id, u16),
    ToolbarIconSizeSaved(window::Id, Result<(), String>),
    Video(window::Id, video_settings::Edit),
    VideoSaved(
        window::Id,
        Result<crate::recording_window::preferences::Preferences, String>,
    ),
    Startup(window::Id, startup_settings::Edit),
    ToolWindowTabs(window::Id, bool),
    ConnectionColorBorder(window::Id, bool),
    SplitPaneFocusRing(window::Id, bool),
    ShellTitles(window::Id, bool),
    RecentTabs(window::Id, bool),
    CommandMarks(window::Id, bool),
    CommandTimestamps(window::Id, bool),
    RemoteSidebar(window::Id, RemoteSidebarPosition),
    TerminalLinkDetection(window::Id, bool),
    TerminalCopyOnSelect(window::Id, bool),
    QuickSelect(window::Id, quick_select_settings::Edit),
    TerminalScrollbar(window::Id, bool),
    ScrollbackCompression(window::Id, bool),
    BoldTextMode(window::Id, BoldTextMode),
    SilenceSeconds(window::Id, String),
    StepSilenceSeconds(window::Id, bool),
    ClipboardWrite(window::Id, bool),
    ClipboardRead(window::Id, bool),
    Restore(window::Id, RestoreMode),
    KeyBinding(window::Id, Edit),
    Backup(window::Id, backup::Edit),
    Logging(window::Id, logging_settings::Edit),
    Export(window::Id, export_settings::Edit),
    LoggingPrepared(
        window::Id,
        u64,
        crate::app_logging::Settings,
        Result<crate::app_logging::Delivery, String>,
    ),
    Save(window::Id),
    Cancel(window::Id),
}

#[derive(Clone)]
struct Draft {
    preferences: WindowSettings,
    restore: Option<RestoreMode>,
    backup: BackupPreferences,
}

impl Draft {
    fn merge_preferences(
        &self,
        initial: &Self,
        mut current: WindowSettings,
        policy: &ClipboardPolicy,
    ) -> WindowSettings {
        let edit = &self.preferences;
        let before = &initial.preferences;
        if edit.key_bindings != before.key_bindings {
            current.key_bindings = edit.key_bindings.clone();
        }
        if edit.terminal_font != before.terminal_font {
            current.terminal_font = edit.terminal_font;
        }
        if edit.terminal_font_family != before.terminal_font_family {
            current
                .terminal_font_family
                .clone_from(&edit.terminal_font_family);
        }
        if edit.themes != before.themes {
            current.themes = edit.themes.clone();
        }
        if edit.colors != before.colors {
            current.colors = edit.colors;
        }
        if edit.appearance != before.appearance {
            current.appearance = edit.appearance;
        }
        if edit.export_branding != before.export_branding {
            current.export_branding = edit.export_branding.clone();
        }
        if edit.startup != before.startup {
            current.startup.remember_geometry = edit.startup.remember_geometry;
            current.startup.remember_dashboard = edit.startup.remember_dashboard;
            current.startup.use_fixed_geometry = edit.startup.use_fixed_geometry;
            if edit.startup.use_fixed_geometry {
                current.startup.fixed = edit.startup.fixed;
            }
        }
        if edit.paste_warning != before.paste_warning {
            current.paste_warning = edit.paste_warning.max(policy.minimum_paste_warning);
        }
        if edit.paste_large_warning_kib != before.paste_large_warning_kib {
            current.paste_large_warning_kib = edit.paste_large_warning_kib;
        }
        if edit.open_tool_windows_as_tabs != before.open_tool_windows_as_tabs {
            current.open_tool_windows_as_tabs = edit.open_tool_windows_as_tabs;
        }
        if edit.connection_color_border_enabled != before.connection_color_border_enabled {
            current.connection_color_border_enabled = edit.connection_color_border_enabled;
        }
        if edit.split_pane_focus_ring_enabled != before.split_pane_focus_ring_enabled {
            current.split_pane_focus_ring_enabled = edit.split_pane_focus_ring_enabled;
        }
        if edit.tab_title_from_shell_enabled != before.tab_title_from_shell_enabled {
            current.tab_title_from_shell_enabled = edit.tab_title_from_shell_enabled;
        }
        if edit.tab_switch_most_recent_first != before.tab_switch_most_recent_first {
            current.tab_switch_most_recent_first = edit.tab_switch_most_recent_first;
        }
        if edit.prompt_navigation_enabled != before.prompt_navigation_enabled {
            current.prompt_navigation_enabled = edit.prompt_navigation_enabled;
        }
        if edit.command_timestamps_enabled != before.command_timestamps_enabled {
            current.command_timestamps_enabled = edit.command_timestamps_enabled;
        }
        if edit.remote_sidebar_position != before.remote_sidebar_position {
            current.remote_sidebar_position = edit.remote_sidebar_position;
        }
        if edit.terminal_link_detection_enabled != before.terminal_link_detection_enabled {
            current.terminal_link_detection_enabled = edit.terminal_link_detection_enabled;
        }
        if edit.terminal_copy_on_select_enabled != before.terminal_copy_on_select_enabled {
            current.terminal_copy_on_select_enabled = edit.terminal_copy_on_select_enabled;
        }
        if edit.show_terminal_scrollbar != before.show_terminal_scrollbar {
            current.show_terminal_scrollbar = edit.show_terminal_scrollbar;
        }
        if edit.scrollback_compression_enabled != before.scrollback_compression_enabled {
            current.scrollback_compression_enabled = edit.scrollback_compression_enabled;
        }
        if edit.terminal_bold_text_mode != before.terminal_bold_text_mode {
            current.terminal_bold_text_mode = edit.terminal_bold_text_mode;
        }
        if edit.terminal_silence_seconds != before.terminal_silence_seconds {
            current.terminal_silence_seconds = edit.terminal_silence_seconds;
        }
        if policy.read_allowed && edit.clipboard_read_enabled != before.clipboard_read_enabled {
            current.clipboard_read_enabled = edit.clipboard_read_enabled;
        }
        if policy.write_allowed && edit.clipboard_write_enabled != before.clipboard_write_enabled {
            current.clipboard_write_enabled = edit.clipboard_write_enabled;
        }
        current
    }
}

struct Dialog {
    id: window::Id,
    owner: window::Id,
    initial: Draft,
    draft: Draft,
    section: Section,
    search_input: widget::Id,
    tab_scroll: widget::Id,
    page_scroll: widget::Id,
    search: String,
    font_size_input: Option<String>,
    silence_seconds_input: Option<String>,
    toolbar_icon_size: Option<u16>,
    saving_toolbar_icon_size: bool,
    saving_logging: bool,
    saving_video: bool,
    logging_revision: u64,
    startup: startup_settings::Editor,
    revealed: Option<Setting>,
    problem: Option<String>,
    keyboard: KeyboardEditor,
    colors: color_settings::Editor,
    themes: theme_settings::Editor,
    backup: backup::Editor,
    logging: logging_settings::Editor,
    export: export_settings::Editor,
    quick_select: quick_select_settings::Editor,
    video: video_settings::Editor,
}

impl Dialog {
    fn saving(&self) -> bool {
        self.saving_toolbar_icon_size || self.saving_logging || self.saving_video
    }

    fn select_section(&mut self, section: Section) {
        self.section = section;
        if section != Section::Backup {
            self.backup.close_browser();
        }
        if section != Section::Logging {
            self.logging.close_browser();
        }
        self.revealed = None;
        self.keyboard.stop_recording();
    }

    fn reveal(&mut self, setting: Setting) -> Task<Desktop> {
        self.select_section(setting.section());
        self.revealed = Some(setting);
        self.search.clear();
        let page = match setting {
            Setting::QuickSelectAlphabet | Setting::QuickSelectPatterns => {
                self.focus_quick_select(setting)
            }
            Setting::JournalAutomaticVision => {
                self.logging.close_browser();
                widget::operation::snap_to(
                    self.page_scroll.clone(),
                    widget::operation::RelativeOffset::END,
                )
            }
            _ => Task::none(),
        };
        Task::batch([self.reveal_tab(), page])
    }

    fn reveal_tab(&self) -> Task<Desktop> {
        widget::operation::snap_to(
            self.tab_scroll.clone(),
            widget::operation::RelativeOffset {
                x: Some(self.section.index() as f32 / (Section::ALL.len() - 1) as f32),
                y: None,
            },
        )
    }

    fn reveal_page_start(&self) -> Task<Desktop> {
        Task::batch([
            self.reveal_tab(),
            widget::operation::snap_to(
                self.page_scroll.clone(),
                widget::operation::RelativeOffset::START,
            ),
        ])
    }

    fn backup_problem(&mut self, message: String) {
        self.select_section(Section::Backup);
        self.search.clear();
        self.problem = Some(message);
    }

    fn focus_quick_select(&self, setting: Setting) -> Task<Desktop> {
        Task::batch([
            widget::operation::snap_to(
                self.page_scroll.clone(),
                widget::operation::RelativeOffset::START,
            ),
            self.quick_select.focus(setting),
        ])
    }

    fn reveal_invalid_input(&mut self) -> Option<Task<Desktop>> {
        if let Some(setting) = self.quick_select.first_problem() {
            self.select_section(Section::Terminal);
            self.search.clear();
            self.revealed = Some(setting);
            self.problem = None;
            return Some(self.focus_quick_select(setting));
        }
        if self.silence_seconds_input.is_some() {
            self.select_section(Section::Messaging);
            self.search.clear();
            self.revealed = Some(Setting::SilenceThreshold);
            self.problem = None;
            return Some(Task::none());
        }
        None
    }
}

#[derive(Default)]
pub(super) struct Settings(Option<Dialog>);

impl Settings {
    pub(super) fn logging_preparing(&self) -> Option<(window::Id, window::Id)> {
        self.0
            .as_ref()
            .filter(|dialog| dialog.saving_logging)
            .map(|dialog| (dialog.owner, dialog.id))
    }

    pub(super) fn title(&self, id: window::Id) -> Option<&'static str> {
        self.0
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|_| TITLE)
    }

    pub(super) fn owner_closed(&mut self, owner: window::Id) -> Option<window::Id> {
        self.for_owner(owner)?;
        self.0.take().map(|dialog| dialog.id)
    }

    pub(super) fn for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.0
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    fn dialog_mut(&mut self, id: window::Id) -> Option<&mut Dialog> {
        self.0.as_mut().filter(|dialog| dialog.id == id)
    }
}

impl Workspace {
    pub(super) fn capture_settings_shortcut(
        &mut self,
        id: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> bool {
        self.settings.dialog_mut(id).is_some_and(|dialog| {
            !dialog.saving()
                && dialog.section == Section::Keyboard
                && dialog
                    .keyboard
                    .key(&mut dialog.draft.preferences.key_bindings, key, modifiers)
        })
    }

    pub(super) fn initialize_saved_clipboard_settings(&mut self) {
        self.apply_clipboard_read(self.window_settings.clipboard_read_enabled);
        self.apply_clipboard_write(self.window_settings.clipboard_write_enabled);
    }

    pub(super) fn close_settings(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        self.settings.title(id)?;
        if self.settings.0.as_ref().is_some_and(Dialog::saving) {
            return Some(Task::none());
        }
        if let Some(dialog) = self.openpgp_for_source(id) {
            return Some(window::gain_focus(dialog));
        }
        self.settings.0 = None;
        Some(Task::batch([
            self.close_placed_dialog(id),
            self.close_shell_integration_owner(id),
        ]))
    }

    pub(super) fn focus_settings_search(&self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.settings.0.as_ref().filter(|dialog| dialog.id == id)?;
        Some(widget::operation::focus(dialog.search_input.clone()))
    }

    pub(super) fn update_settings(&mut self, message: Message) -> Task<Desktop> {
        if !matches!(
            &message,
            Message::ToolbarIconSizeSaved(..)
                | Message::LoggingPrepared(..)
                | Message::VideoSaved(..)
        ) && let Some(dialog) = self.settings.0.as_ref().filter(|dialog| dialog.saving())
        {
            return if matches!(message, Message::Open(_) | Message::OpenAt(..)) {
                window::gain_focus(dialog.id)
            } else {
                Task::none()
            };
        }
        let (id, message) = match message {
            Message::Open(owner) => {
                return self.open_settings(owner, None);
            }
            Message::OpenAt(owner, section) => {
                return self.open_settings(owner, Some(section));
            }
            Message::Guide(id) => {
                return self
                    .settings
                    .0
                    .as_ref()
                    .filter(|dialog| dialog.id == id)
                    .map(|dialog| dialog.section.guide_chapter())
                    .map_or_else(Task::none, |chapter| self.show_manual_chapter(&chapter));
            }
            Message::Cancel(id) => return self.close_settings(id).unwrap_or_else(Task::none),
            Message::Save(id) => return self.save_settings(id),
            Message::ToolbarIconSizeSaved(id, result) => {
                return self.toolbar_icon_size_saved(id, result);
            }
            Message::Video(id, edit) => {
                let allowed = self.recording_allowed();
                if let Some(dialog) = self
                    .settings
                    .dialog_mut(id)
                    .filter(|dialog| dialog.section == Section::Video)
                {
                    dialog.video.update(edit, allowed);
                    dialog.problem = None;
                }
                return Task::none();
            }
            Message::VideoSaved(id, result) => return self.video_settings_saved(id, result),
            Message::Backup(id, edit) => return self.update_backup_settings(id, edit),
            Message::Logging(id, edit) => return self.update_logging_settings(id, edit),
            Message::LoggingPrepared(id, revision, settings, result) => {
                return self.logging_settings_prepared(id, revision, settings, result);
            }
            message @ (Message::Section(id, _)
            | Message::Search(id, _)
            | Message::Reveal(id, _)
            | Message::FontFamily(id, _)
            | Message::FontSize(id, _)
            | Message::FontSizeInput(id, _)
            | Message::Color(id, _)
            | Message::Theme(id, _)
            | Message::Export(id, _)
            | Message::Design(id, _)
            | Message::DesignAnimations(id, _)
            | Message::PasteWarning(id, _)
            | Message::LargePasteWarning(id, _)
            | Message::ToolbarIconSize(id, _)
            | Message::Startup(id, _)
            | Message::ToolWindowTabs(id, _)
            | Message::ConnectionColorBorder(id, _)
            | Message::SplitPaneFocusRing(id, _)
            | Message::ShellTitles(id, _)
            | Message::RecentTabs(id, _)
            | Message::CommandMarks(id, _)
            | Message::CommandTimestamps(id, _)
            | Message::RemoteSidebar(id, _)
            | Message::TerminalLinkDetection(id, _)
            | Message::TerminalCopyOnSelect(id, _)
            | Message::QuickSelect(id, _)
            | Message::TerminalScrollbar(id, _)
            | Message::ScrollbackCompression(id, _)
            | Message::BoldTextMode(id, _)
            | Message::SilenceSeconds(id, _)
            | Message::StepSilenceSeconds(id, _)
            | Message::ClipboardWrite(id, _)
            | Message::ClipboardRead(id, _)
            | Message::Restore(id, _)
            | Message::KeyBinding(id, _)) => (id, message),
        };
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        match message {
            Message::Section(_, section) => {
                dialog.select_section(section);
                return dialog.reveal_page_start();
            }
            Message::Search(_, mut query) => {
                query.truncate(query.floor_char_boundary(MAX_SEARCH_BYTES));
                dialog.search = query;
                dialog.revealed = None;
                dialog.keyboard.stop_recording();
            }
            Message::Reveal(_, setting) => return dialog.reveal(setting),
            Message::FontFamily(_, family) if dialog.section == Section::Font => {
                if installed_font(&family).is_some() {
                    dialog.draft.preferences.set_terminal_font_family(family);
                }
            }
            Message::FontSize(_, size) if dialog.section == Section::Font => {
                dialog.draft.preferences.terminal_font =
                    size.clamp(profile::MIN_TERMINAL_FONT, profile::MAX_TERMINAL_FONT);
                dialog.font_size_input = None;
                dialog.problem = None;
            }
            Message::FontSizeInput(_, mut value) if dialog.section == Section::Font => {
                let size = value.parse::<u8>().ok().filter(|size| {
                    (profile::MIN_TERMINAL_FONT..=profile::MAX_TERMINAL_FONT).contains(size)
                });
                if let Some(size) = size {
                    dialog.draft.preferences.terminal_font = size;
                    dialog.font_size_input = None;
                    dialog.problem = None;
                } else {
                    value.truncate(value.floor_char_boundary(8));
                    dialog.font_size_input = Some(value);
                }
            }
            Message::Color(_, edit) if dialog.section == Section::Colors => {
                let selected = dialog.draft.preferences.themes.selected();
                dialog.colors.update(&mut dialog.draft.preferences, edit);
                let themes = &dialog.draft.preferences.themes;
                if themes.apply_font && themes.selected() != selected && themes.selected().is_some()
                {
                    dialog.font_size_input = None;
                }
                dialog.problem = None;
            }
            Message::Theme(_, edit) if dialog.section == Section::Themes => {
                let selected = dialog.draft.preferences.themes.selected();
                dialog
                    .themes
                    .update(&mut dialog.draft.preferences.themes, edit);
                if dialog.draft.preferences.themes.selected() != selected {
                    dialog.colors.profile_changed();
                }
                dialog.problem = None;
            }
            Message::Export(_, edit) if dialog.section == Section::Export => {
                dialog
                    .export
                    .update(&mut dialog.draft.preferences.export_branding, edit);
                dialog.problem = None;
            }
            Message::Design(_, design) if dialog.section == Section::Appearance => {
                dialog.draft.preferences.appearance.design = design;
                dialog.problem = None;
            }
            Message::DesignAnimations(_, enabled) if dialog.section == Section::Appearance => {
                dialog.draft.preferences.appearance.animations = enabled;
                dialog.problem = None;
            }
            Message::PasteWarning(_, mode)
                if mode >= self.clipboard.policy.minimum_paste_warning =>
            {
                dialog.draft.preferences.paste_warning = mode;
            }
            Message::LargePasteWarning(_, value) => {
                dialog.draft.preferences.paste_large_warning_kib = value.min(MAX_PASTE_KIB);
            }
            Message::ToolbarIconSize(_, size) if dialog.section == Section::Window => {
                let size = size.clamp(*TOOLBAR_ICON_SIZE.start(), *TOOLBAR_ICON_SIZE.end());
                dialog.toolbar_icon_size =
                    (size != self.chrome.toolbar_icon_size()).then_some(size);
                dialog.problem = None;
            }
            Message::Startup(_, edit) if dialog.section == Section::Window => {
                dialog
                    .startup
                    .update(&mut dialog.draft.preferences.startup, edit);
                dialog.problem = None;
            }
            Message::ToolWindowTabs(_, enabled) => {
                dialog.draft.preferences.open_tool_windows_as_tabs = enabled;
            }
            Message::ConnectionColorBorder(_, enabled) => {
                dialog.draft.preferences.connection_color_border_enabled = enabled;
            }
            Message::SplitPaneFocusRing(_, enabled) => {
                dialog.draft.preferences.split_pane_focus_ring_enabled = enabled;
            }
            Message::ShellTitles(_, value) => {
                dialog.draft.preferences.tab_title_from_shell_enabled = value
            }
            Message::RecentTabs(_, value) => {
                dialog.draft.preferences.tab_switch_most_recent_first = value
            }
            Message::CommandMarks(_, value) => {
                dialog.draft.preferences.prompt_navigation_enabled = value
            }
            Message::CommandTimestamps(_, value) => {
                dialog.draft.preferences.command_timestamps_enabled = value;
            }
            Message::RemoteSidebar(_, position) => {
                dialog.draft.preferences.remote_sidebar_position = position;
            }
            Message::TerminalLinkDetection(_, value) => {
                dialog.draft.preferences.terminal_link_detection_enabled = value;
            }
            Message::TerminalCopyOnSelect(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.terminal_copy_on_select_enabled = value;
            }
            Message::QuickSelect(_, edit) if dialog.section == Section::Terminal => {
                dialog.quick_select.update(edit);
            }
            Message::TerminalScrollbar(_, value) => {
                dialog.draft.preferences.show_terminal_scrollbar = value;
            }
            Message::ScrollbackCompression(_, value) => {
                dialog.draft.preferences.scrollback_compression_enabled = value;
            }
            Message::BoldTextMode(_, mode) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.terminal_bold_text_mode = mode;
            }
            Message::SilenceSeconds(_, mut value) if dialog.section == Section::Messaging => {
                if let Some(seconds) = value.trim().parse::<u16>().ok().filter(|seconds| {
                    (MIN_TERMINAL_SILENCE_SECONDS..=MAX_TERMINAL_SILENCE_SECONDS).contains(seconds)
                }) {
                    dialog.draft.preferences.terminal_silence_seconds = seconds;
                    dialog.silence_seconds_input = None;
                } else {
                    value.truncate(value.floor_char_boundary(8));
                    dialog.silence_seconds_input = Some(value);
                }
            }
            Message::StepSilenceSeconds(_, up) if dialog.section == Section::Messaging => {
                let seconds = &mut dialog.draft.preferences.terminal_silence_seconds;
                *seconds = if up {
                    seconds.saturating_add(1).min(MAX_TERMINAL_SILENCE_SECONDS)
                } else {
                    seconds.saturating_sub(1).max(MIN_TERMINAL_SILENCE_SECONDS)
                };
                dialog.silence_seconds_input = None;
            }
            Message::ClipboardWrite(_, value) if self.clipboard.policy.write_allowed => {
                dialog.draft.preferences.clipboard_write_enabled = value;
            }
            Message::ClipboardRead(_, value) if self.clipboard.policy.read_allowed => {
                dialog.draft.preferences.clipboard_read_enabled = value;
            }
            Message::Restore(_, mode) if dialog.draft.restore.is_some() => {
                dialog.draft.restore = Some(mode)
            }
            Message::KeyBinding(_, edit) if dialog.section == Section::Keyboard => {
                dialog
                    .keyboard
                    .update(&mut dialog.draft.preferences.key_bindings, edit);
            }
            _ => {}
        }
        Task::none()
    }

    fn open_settings(&mut self, owner: window::Id, section: Option<Section>) -> Task<Desktop> {
        if !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(dialog) = &mut self.settings.0 {
            dialog.owner = owner;
            let focus = window::gain_focus(dialog.id);
            if let Some(section) = section {
                dialog.select_section(section);
                dialog.search.clear();
                return focus.chain(dialog.reveal_page_start());
            }
            return focus;
        }
        let draft = Draft {
            preferences: self.window_settings.clone(),
            restore: self.session_restore.preference(),
            backup: self.backup_preferences.clone(),
        };
        let quick_select = quick_select_settings::Editor::new(&draft.preferences.quick_select);
        let (id, opened) = self.open_placed_dialog(
            DialogKind::GlobalSettings,
            window::Settings {
                size: Size::new(1120.0, 960.0),
                min_size: Some(Size::new(620.0, 450.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.settings.0 = Some(Dialog {
            id,
            owner,
            initial: draft.clone(),
            draft,
            section: section.unwrap_or(Section::Font),
            search_input: widget::Id::unique(),
            tab_scroll: widget::Id::unique(),
            page_scroll: widget::Id::unique(),
            search: String::new(),
            font_size_input: None,
            silence_seconds_input: None,
            toolbar_icon_size: None,
            saving_toolbar_icon_size: false,
            saving_logging: false,
            saving_video: false,
            logging_revision: 0,
            startup: startup_settings::Editor::default(),
            revealed: None,
            problem: None,
            keyboard: KeyboardEditor::default(),
            colors: color_settings::Editor::default(),
            themes: theme_settings::Editor::default(),
            backup: backup::Editor::default(),
            logging: logging_settings::Editor::default(),
            export: export_settings::Editor::default(),
            quick_select,
            video: video_settings::Editor::new(self.video.preferences()),
        });
        opened.discard().chain(
            self.settings
                .0
                .as_ref()
                .map_or_else(Task::none, Dialog::reveal_tab),
        )
    }

    pub(super) fn open_journal_options(&mut self, owner: window::Id) -> Task<Desktop> {
        let open = self.update_settings(Message::OpenAt(owner, Section::Logging));
        if let Some(dialog) = self
            .settings
            .0
            .as_mut()
            .filter(|dialog| dialog.owner == owner && !dialog.saving())
        {
            return open.chain(dialog.reveal(Setting::JournalAutomaticVision));
        }
        open
    }

    fn save_settings(&mut self, id: window::Id) -> Task<Desktop> {
        if let Some(dialog) = self.openpgp_for_source(id) {
            return window::gain_focus(dialog);
        }
        let manager_profile_ready = self.manager_journal_profile_ready();
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        if let Some(reveal) = dialog.reveal_invalid_input() {
            return reveal;
        }
        if dialog.logging.browsing() || dialog.backup.browsing() {
            return self.save_settings_to(id, save_preferences);
        }
        if let Err(error) = dialog.logging.merge_journal(
            &dialog.initial.preferences.session_journal,
            &self.window_settings.session_journal,
            &self.clipboard.policy,
        ) {
            dialog.select_section(Section::Logging);
            dialog.search.clear();
            dialog.revealed = Some(Setting::JournalDirectory);
            dialog.problem = Some(error);
            return Task::none();
        }
        if let Err(error) = dialog.logging.merge_journal_ai(
            &dialog.initial.preferences.journal_ai,
            &self.window_settings.journal_ai,
            &self.clipboard.policy,
            manager_profile_ready,
        ) {
            dialog.select_section(Section::Logging);
            dialog.search.clear();
            dialog.revealed = Some(Setting::JournalAiProfile);
            dialog.problem = Some(error);
            return Task::none();
        }
        let settings = match dialog.logging.merge(
            &dialog.initial.preferences.app_logging,
            &self.window_settings.app_logging,
            self.clipboard.policy.app_logging.directory().is_some(),
            self.clipboard.policy.app_logging.retention_days().is_some(),
        ) {
            Ok(settings) => settings,
            Err(error) => {
                dialog.select_section(Section::Logging);
                dialog.search.clear();
                dialog.problem = Some(error);
                return Task::none();
            }
        };
        if settings == self.window_settings.app_logging {
            return self.save_settings_to(id, save_preferences);
        }
        let plan = match self.plan_app_logging(&settings) {
            Ok(Some(plan)) => plan,
            Ok(None) => return self.save_settings_to(id, save_preferences),
            Err(error) => {
                if let Some(dialog) = self.settings.dialog_mut(id) {
                    dialog.select_section(Section::Logging);
                    dialog.search.clear();
                    dialog.problem =
                        Some(format!("Could not prepare application logging ({error})."));
                }
                return Task::none();
            }
        };
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        dialog.saving_logging = true;
        dialog.logging_revision = dialog.logging_revision.wrapping_add(1);
        let revision = dialog.logging_revision;
        Task::perform(
            async move { plan.prepare().map_err(|error| error.to_string()) },
            move |result| {
                Desktop::Settings(Message::LoggingPrepared(
                    id,
                    revision,
                    settings.clone(),
                    result,
                ))
            },
        )
    }

    fn logging_settings_prepared(
        &mut self,
        id: window::Id,
        revision: u64,
        settings: crate::app_logging::Settings,
        result: Result<crate::app_logging::Delivery, String>,
    ) -> Task<Desktop> {
        let candidate = result.as_ref().ok().and_then(|delivery| delivery.take());
        let Some(dialog) = self
            .settings
            .dialog_mut(id)
            .filter(|dialog| dialog.saving_logging && dialog.logging_revision == revision)
        else {
            self.abort_app_logging(candidate);
            return Task::none();
        };
        dialog.saving_logging = false;
        if let Err(error) = result {
            dialog.select_section(Section::Logging);
            dialog.search.clear();
            dialog.problem = Some(format!("Could not open the log folder ({error})."));
            return Task::none();
        }
        let current = dialog.logging.merge(
            &dialog.initial.preferences.app_logging,
            &self.window_settings.app_logging,
            self.clipboard.policy.app_logging.directory().is_some(),
            self.clipboard.policy.app_logging.retention_days().is_some(),
        );
        if current.as_ref().ok() != Some(&settings) {
            self.abort_app_logging(candidate);
            return self.save_settings(id);
        }
        self.save_settings_prepared(id, save_preferences, candidate)
    }

    fn toolbar_icon_size_saved(
        &mut self,
        id: window::Id,
        result: Result<(), String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self
            .settings
            .dialog_mut(id)
            .filter(|dialog| dialog.saving_toolbar_icon_size)
        else {
            return Task::none();
        };
        dialog.saving_toolbar_icon_size = false;
        if let Err(error) = result {
            dialog.select_section(Section::Window);
            dialog.search.clear();
            dialog.problem = Some(format!("Could not save the toolbar icon size ({error})."));
            Task::none()
        } else {
            self.close_settings(id).unwrap_or_else(Task::none)
        }
    }

    fn video_settings_saved(
        &mut self,
        id: window::Id,
        result: Result<crate::recording_window::preferences::Preferences, String>,
    ) -> Task<Desktop> {
        let Some(result) = self.video.finish_global_preferences(id, result) else {
            return Task::none();
        };
        let Some(dialog) = self
            .settings
            .dialog_mut(id)
            .filter(|dialog| dialog.saving_video)
        else {
            return Task::none();
        };
        dialog.saving_video = false;
        if let Err(error) = result {
            dialog.select_section(Section::Video);
            dialog.search.clear();
            dialog.problem = Some(format!("Could not save recording settings ({error})."));
            return Task::none();
        }
        dialog.video = video_settings::Editor::new(self.video.preferences());
        self.finish_toolbar_settings(id)
    }

    fn finish_toolbar_settings(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        let Some(size) = dialog
            .toolbar_icon_size
            .filter(|&size| size != self.chrome.toolbar_icon_size())
        else {
            return self.close_settings(id).unwrap_or_else(Task::none);
        };
        if self.chrome.toolbar_save_pending() {
            dialog.select_section(Section::Window);
            dialog.search.clear();
            dialog.problem = Some("The toolbar is still being saved. Try again shortly.".into());
            return Task::none();
        }
        dialog.saving_toolbar_icon_size = true;
        self.chrome.save_toolbar_icon_size(dialog.owner, id, size)
    }

    fn save_settings_to(
        &mut self,
        id: window::Id,
        save: impl FnOnce(&WindowSettings) -> std::io::Result<()>,
    ) -> Task<Desktop> {
        self.save_settings_prepared(id, save, None)
    }

    fn save_settings_prepared(
        &mut self,
        id: window::Id,
        save: impl FnOnce(&WindowSettings) -> std::io::Result<()>,
        mut prepared: Option<crate::app_logging::Candidate>,
    ) -> Task<Desktop> {
        let task = self.apply_settings_to(id, save, &mut prepared);
        self.abort_app_logging(prepared);
        task
    }

    fn apply_settings_to(
        &mut self,
        id: window::Id,
        save: impl FnOnce(&WindowSettings) -> std::io::Result<()>,
        prepared: &mut Option<crate::app_logging::Candidate>,
    ) -> Task<Desktop> {
        if let Some(dialog) = self.openpgp_for_source(id) {
            return window::gain_focus(dialog);
        }
        let recording_allowed = self.recording_allowed();
        let manager_profile_ready = self.manager_journal_profile_ready();
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        if let Some(reveal) = dialog.reveal_invalid_input() {
            return reveal;
        }
        if dialog.themes.editing() {
            dialog.section = Section::Themes;
            dialog.problem = Some("Finish or cancel the theme edit before saving Settings.".into());
            return Task::none();
        }
        if !dialog.colors.valid() {
            dialog.section = Section::Colors;
            dialog.search.clear();
            dialog.revealed = None;
            dialog.problem = Some("Enter terminal colors as #RRGGBB.".into());
            return Task::none();
        }
        if !dialog
            .export
            .valid(&dialog.draft.preferences.export_branding)
        {
            dialog.select_section(Section::Export);
            dialog.search.clear();
            dialog.revealed = Some(Setting::ExportWatermark);
            dialog.problem = Some("Enter a watermark color as #RRGGBB.".into());
            return Task::none();
        }
        if dialog.font_size_input.is_some() {
            dialog.select_section(Section::Font);
            dialog.search.clear();
            return Task::none();
        }
        if !dialog.startup.valid() {
            dialog.select_section(Section::Window);
            dialog.search.clear();
            dialog.revealed = Some(Setting::FixedGeometry);
            return Task::none();
        }
        if dialog.backup.browsing() && self.clipboard.policy.backup.directory.is_none() {
            return Task::none();
        }
        if dialog.logging.browsing_unmanaged(
            self.clipboard.policy.app_logging.directory().is_some(),
            self.clipboard
                .policy
                .terminal_logging
                .journal_directory()
                .is_some(),
        ) {
            return Task::none();
        }
        let toolbar_icon_size = dialog
            .toolbar_icon_size
            .filter(|&size| size != self.chrome.toolbar_icon_size());
        if toolbar_icon_size.is_some() && self.chrome.toolbar_save_pending() {
            dialog.select_section(Section::Window);
            dialog.search.clear();
            dialog.problem = Some("The toolbar is still being saved. Try again shortly.".into());
            return Task::none();
        }
        let video = dialog
            .video
            .merge(self.video.preferences(), recording_allowed);
        if video.is_some() && self.video.save_pending() {
            dialog.select_section(Section::Video);
            dialog.search.clear();
            dialog.problem =
                Some("Recording settings are still being saved. Try again shortly.".into());
            return Task::none();
        }
        let (draft, initial) = (&dialog.draft, &dialog.initial);
        if let Some((_, actions)) = draft.preferences.key_bindings.conflicts().first() {
            dialog.section = Section::Keyboard;
            dialog.search.clear();
            dialog.revealed = None;
            dialog.problem = None;
            return dialog.keyboard.reveal(actions[0]);
        }
        let backup = match dialog.backup.merge(
            &draft.backup,
            &initial.backup,
            &self.backup_preferences,
            self.clipboard.policy.backup.interval.is_some(),
            self.clipboard.policy.backup.directory.is_some(),
        ) {
            Ok(backup) => backup,
            Err(error) => {
                dialog.backup_problem(error.into());
                return Task::none();
            }
        };
        let app_logging = match dialog.logging.merge(
            &initial.preferences.app_logging,
            &self.window_settings.app_logging,
            self.clipboard.policy.app_logging.directory().is_some(),
            self.clipboard.policy.app_logging.retention_days().is_some(),
        ) {
            Ok(settings) => settings,
            Err(error) => {
                dialog.select_section(Section::Logging);
                dialog.search.clear();
                dialog.problem = Some(error);
                return Task::none();
            }
        };
        let restore = draft.restore.filter(|mode| Some(*mode) != initial.restore);
        let session_journal = match dialog.logging.merge_journal(
            &initial.preferences.session_journal,
            &self.window_settings.session_journal,
            &self.clipboard.policy,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                dialog.select_section(Section::Logging);
                dialog.search.clear();
                dialog.revealed = Some(Setting::JournalDirectory);
                dialog.problem = Some(error);
                return Task::none();
            }
        };
        let journal_ai = match dialog.logging.merge_journal_ai(
            &initial.preferences.journal_ai,
            &self.window_settings.journal_ai,
            &self.clipboard.policy,
            manager_profile_ready,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                dialog.select_section(Section::Logging);
                dialog.search.clear();
                dialog.revealed = Some(Setting::JournalAiProfile);
                dialog.problem = Some(error);
                return Task::none();
            }
        };
        let mut next = draft.merge_preferences(
            initial,
            self.window_settings.clone(),
            &self.clipboard.policy,
        );
        next.app_logging = app_logging;
        next.session_journal = session_journal;
        next.journal_ai = journal_ai;
        dialog
            .quick_select
            .merge(&initial.preferences.quick_select, &mut next.quick_select);
        let logging_changed = next.app_logging != self.window_settings.app_logging;
        let journal_directory_changed =
            next.session_journal.directory != self.window_settings.session_journal.directory;
        let preferences_changed = next != self.window_settings;
        let appearance = next.terminal_appearance();
        let appearance_changed = appearance != self.window_settings.terminal_appearance();
        let prompt_changed =
            next.prompt_navigation_enabled != self.window_settings.prompt_navigation_enabled;
        let sidebar_changed =
            next.remote_sidebar_position != self.window_settings.remote_sidebar_position;
        let read = (self.clipboard.policy.read_allowed
            && draft.preferences.clipboard_read_enabled
                != initial.preferences.clipboard_read_enabled)
            .then_some(next.clipboard_read_enabled);
        let write = (self.clipboard.policy.write_allowed
            && draft.preferences.clipboard_write_enabled
                != initial.preferences.clipboard_write_enabled)
            .then_some(next.clipboard_write_enabled);
        let logging_candidate = if logging_changed && prepared.is_some() {
            prepared.take()
        } else if logging_changed {
            match self.prepare_app_logging(&next.app_logging) {
                Ok(candidate) => candidate,
                Err(error) => {
                    if let Some(dialog) = self.settings.dialog_mut(id) {
                        dialog.select_section(Section::Logging);
                        dialog.search.clear();
                        dialog.problem = Some(format!("Could not open the log folder ({error})."));
                    }
                    return Task::none();
                }
            }
        } else {
            None
        };
        if read == Some(false) {
            self.apply_clipboard_read(false);
        }
        if write == Some(false) {
            self.apply_clipboard_write(false);
        }
        if preferences_changed {
            if let Err(error) = save(&next) {
                self.abort_app_logging(logging_candidate);
                if let Some(dialog) = self.settings.dialog_mut(id) {
                    let detail = if read == Some(false) || write == Some(false) {
                        " Revoked clipboard permissions are off for this run."
                    } else {
                        ""
                    };
                    dialog.problem = Some(format!("Could not save settings ({error}).{detail}"));
                }
                return Task::none();
            }
            if next.tab_switch_most_recent_first
                != self.window_settings.tab_switch_most_recent_first
            {
                self.commit_all_tab_cycles();
            }
            if next.terminal_font != self.window_settings.terminal_font {
                for pane in self
                    .sessions
                    .values_mut()
                    .filter(|pane| pane.inherits_font_size)
                {
                    pane.font = next.terminal_font;
                    pane.font_baseline = next.terminal_font;
                }
            }
            if next.scrollback_compression_enabled
                != self.window_settings.scrollback_compression_enabled
            {
                for pane in self.sessions.values() {
                    pane.session
                        .set_scrollback_compression_enabled(next.scrollback_compression_enabled);
                }
            }
            let journal_ai_changed = next.journal_ai != self.window_settings.journal_ai;
            self.window_settings = next;
            if journal_ai_changed {
                self.renew_journal_screenshot_ai_consents();
                self.cancel_obsolete_journal_screenshot_analyses();
                self.cancel_obsolete_journal_note_translations();
            }
            self.commit_app_logging(logging_candidate);
            if prompt_changed {
                self.apply_prompt_navigation();
            }
            if appearance_changed {
                for &session in self.sessions.keys() {
                    self.sync_pane_appearance(session);
                }
            }
        }
        if read == Some(true) {
            self.apply_clipboard_read(true);
        }
        if write == Some(true) {
            self.apply_clipboard_write(true);
        }
        let restore_changed = restore.is_some();
        let restore = restore.map_or_else(Task::none, |mode| {
            self.update_session_restore(session_restore::Message::Mode(mode))
        });
        let sidebar = if sidebar_changed {
            self.remote_sidebar_preferences_changed()
        } else {
            Task::none()
        };
        let journals = if journal_directory_changed {
            self.refresh_inherited_journal_managers()
        } else {
            Task::none()
        };
        let complete = if backup != self.backup_preferences
            && let Err(error) = self.commit_backup_preferences(backup)
        {
            if let Some(dialog) = self.settings.dialog_mut(id) {
                let detail = if preferences_changed || restore_changed {
                    " Other settings were handled separately."
                } else {
                    ""
                };
                dialog.backup_problem(format!("Could not save backup settings ({error}).{detail}"));
            }
            Task::none()
        } else if let Some(video) = video {
            match self.save_global_video_preferences(id, video) {
                Ok(task) => {
                    if let Some(dialog) = self.settings.dialog_mut(id) {
                        dialog.saving_video = true;
                    }
                    task.map(move |result| Desktop::Settings(Message::VideoSaved(id, result)))
                }
                Err(error) => {
                    if let Some(dialog) = self.settings.dialog_mut(id) {
                        dialog.select_section(Section::Video);
                        dialog.search.clear();
                        dialog.problem =
                            Some(format!("Could not save recording settings ({error})."));
                    }
                    Task::none()
                }
            }
        } else {
            self.finish_toolbar_settings(id)
        };
        Task::batch([restore, sidebar, journals, complete])
    }

    pub(super) fn settings_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.settings.0.as_ref().filter(|dialog| dialog.id == id)?;
        if dialog.saving() {
            return Some(
                container(text(if dialog.saving_logging {
                    "Preparing application logging…"
                } else if dialog.saving_video {
                    "Saving recording settings…"
                } else {
                    "Saving toolbar icon size…"
                }))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
            );
        }
        let msg = Desktop::Settings;
        let transparency = self
            .windows
            .get(&dialog.owner)
            .and_then(|window| self.transparency_controls(window));
        let matches = search_matches(&dialog.search).filter(|setting| match setting {
            Setting::Restore => dialog.draft.restore.is_some(),
            Setting::Transparency => transparency.is_some(),
            _ => true,
        });
        let mut matches = matches.peekable();
        let input = row![
            text_input("Search settings…", &dialog.search)
                .id(dialog.search_input.clone())
                .on_input(move |query| msg(Message::Search(id, query)))
                .on_submit_maybe(
                    matches
                        .peek()
                        .map(|&setting| msg(Message::Reveal(id, setting)))
                ),
            button("×").style(button::secondary).on_press_maybe(
                (!dialog.search.is_empty()).then(|| msg(Message::Search(id, String::new())))
            ),
        ]
        .spacing(6);
        let mut search = column![input].width(Length::Fill).spacing(4);
        for setting in matches {
            search = search.push(
                button(text(format!(
                    "{} · {}",
                    setting.label(),
                    setting.section().label()
                )))
                .style(button::secondary)
                .width(Length::Fill)
                .on_press(msg(Message::Reveal(id, setting))),
            );
        }
        let search = container(search.max_width(660.0)).center_x(Length::Fill);

        let index = dialog.section.index();
        let navigation = row![
            button("◀").style(button::secondary).on_press_maybe(
                index
                    .checked_sub(1)
                    .map(|previous| msg(Message::Section(id, Section::ALL[previous])))
            ),
            text(format!(
                "{} ({}/{})",
                dialog.section.label(),
                index + 1,
                Section::ALL.len()
            ))
            .size(14),
            button("▶").style(button::secondary).on_press_maybe(
                Section::ALL
                    .get(index + 1)
                    .copied()
                    .map(|next| msg(Message::Section(id, next)))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let navigation = row![
            space::horizontal(),
            navigation,
            container(
                button(text("Manual for this page").size(13).style(text::primary))
                    .style(button::text)
                    .on_press(msg(Message::Guide(id)))
            )
            .align_x(Alignment::End)
            .width(Length::Fill),
        ]
        .align_y(Alignment::Center);
        let conflicts = dialog.draft.preferences.key_bindings.conflicts().len();
        let tabs = Section::ALL
            .into_iter()
            .fold(row![].spacing(4), |row, section| {
                row.push(
                    button(if section == Section::Keyboard && conflicts != 0 {
                        row![text(section.label()), text(conflicts).style(text::danger)].spacing(5)
                    } else {
                        row![text(section.label())]
                    })
                    .style(if dialog.section == section {
                        button::secondary
                    } else {
                        button::text
                    })
                    .on_press(msg(Message::Section(id, section))),
                )
            });
        let tabs = scrollable(tabs)
            .id(dialog.tab_scroll.clone())
            .horizontal()
            .width(Length::Fill);
        let page = match dialog.section {
            Section::Font => font_settings_page(dialog),
            Section::Colors => highlighted(
                dialog,
                Setting::Colors,
                dialog
                    .colors
                    .view(&dialog.draft.preferences)
                    .map(move |edit| msg(Message::Color(id, edit))),
            ),
            Section::Themes => highlighted(
                dialog,
                Setting::Themes,
                dialog
                    .themes
                    .view(&dialog.draft.preferences)
                    .map(move |edit| msg(Message::Theme(id, edit))),
            ),
            Section::Appearance => highlighted(
                dialog,
                Setting::Appearance,
                appearance_settings::view(dialog.draft.preferences.appearance, id).map(msg),
            ),
            Section::Terminal => self.terminal_settings_page(dialog),
            Section::Messaging => messaging_settings_page(dialog),
            Section::Video => video_settings::view(dialog, self.recording_allowed()),
            Section::Backup => self.backup_settings_page(dialog),
            Section::Logging => self.logging_settings_page(dialog),
            Section::Export => export_settings::view(dialog),
            Section::Window => self.window_settings_page(dialog, transparency),
            Section::Keyboard => highlighted(
                dialog,
                Setting::Shortcuts,
                dialog
                    .keyboard
                    .view(&dialog.draft.preferences.key_bindings, id),
            ),
        };
        let footer = row![space::horizontal()]
            .spacing(10)
            .align_y(Alignment::Center)
            .push(
                button("Cancel")
                    .style(button::secondary)
                    .on_press(msg(Message::Cancel(id))),
            )
            .push(
                button("Save").on_press_maybe(
                    ((!dialog.backup.browsing()
                        || self.clipboard.policy.backup.directory.is_some())
                        && !dialog.logging.browsing_unmanaged(
                            self.clipboard.policy.app_logging.directory().is_some(),
                            self.clipboard
                                .policy
                                .terminal_logging
                                .journal_directory()
                                .is_some(),
                        ))
                    .then(|| msg(Message::Save(id))),
                ),
            );
        let footer = column![
            dialog.problem.as_deref().map(|error| text(error).size(14)),
            footer
        ]
        .spacing(6);

        Some(
            column![
                container(text("Global Terminal Settings").size(19))
                    .width(Length::Fill)
                    .padding([12, 14])
                    .style(container::secondary),
                column![
                    search,
                    navigation,
                    column![rule::horizontal(1), tabs].spacing(4),
                    container(
                        scrollable(container(page).width(Length::Fill).padding(20))
                            .id(dialog.page_scroll.clone())
                    )
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .style(container::bordered_box),
                    footer,
                ]
                .spacing(10)
                .padding(12)
                .height(Length::Fill),
            ]
            .height(Length::Fill)
            .into(),
        )
    }

    fn terminal_settings_page<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let id = dialog.id;
        let settings = &dialog.draft.preferences;
        let policy = &self.clipboard.policy;
        let threshold = settings.paste_large_warning_kib;
        let paste_floor = policy.minimum_paste_warning;
        let paste_modes: Vec<_> = WarningMode::ALL
            .into_iter()
            .filter(|mode| *mode >= paste_floor)
            .collect();
        let paste = column![
            highlighted(
                dialog,
                Setting::PasteWarning,
                column![
                    text(Setting::PasteWarning.label()),
                    pick_list(
                        paste_modes,
                        Some(settings.paste_warning.max(paste_floor)),
                        move |mode| { Desktop::Settings(Message::PasteWarning(id, mode)) }
                    ),
                ]
                .spacing(5),
            ),
            highlighted(
                dialog,
                Setting::LargePasteWarning,
                row![
                    text(Setting::LargePasteWarning.label()),
                    slider(0..=MAX_PASTE_KIB, threshold, move |value| {
                        Desktop::Settings(Message::LargePasteWarning(id, value))
                    })
                    .width(180),
                    text(if threshold == 0 {
                        "Off".to_owned()
                    } else {
                        format!("{threshold} KiB")
                    }),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ),
            text(
                "The size warning is independent of the line-break and control-character warning."
            )
            .size(13),
            (paste_floor != WarningMode::Off)
                .then(|| text("Your organization sets the minimum paste warning level.").size(13)),
        ]
        .spacing(7);
        let clipboard = column![
            highlighted(
                dialog,
                Setting::ClipboardWrite,
                checkbox(settings.clipboard_write_enabled && policy.write_allowed)
                    .label(Setting::ClipboardWrite.label())
                    .on_toggle_maybe(policy.write_allowed.then_some(move |enabled| {
                        Desktop::Settings(Message::ClipboardWrite(id, enabled))
                    })),
            ),
            highlighted(
                dialog,
                Setting::ClipboardRead,
                checkbox(settings.clipboard_read_enabled && policy.read_allowed)
                    .label(Setting::ClipboardRead.label())
                    .on_toggle_maybe(policy.read_allowed.then_some(move |enabled| {
                        Desktop::Settings(Message::ClipboardRead(id, enabled))
                    })),
            ),
        ]
        .spacing(7);
        let mut page = column![
            text("Terminal Settings").size(18),
            text("Links").size(16),
            highlighted(
                dialog,
                Setting::TerminalLinkDetection,
                checkbox(settings.terminal_link_detection_enabled)
                    .label(Setting::TerminalLinkDetection.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::TerminalLinkDetection(id, enabled))
                    }),
            ),
            dialog.quick_select.view(dialog),
            rule::horizontal(1),
            highlighted(
                dialog,
                Setting::TerminalScrollbar,
                checkbox(settings.show_terminal_scrollbar)
                    .label(Setting::TerminalScrollbar.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::TerminalScrollbar(id, enabled))
                    }),
            ),
            highlighted(
                dialog,
                Setting::ScrollbackCompression,
                column![
                    checkbox(settings.scrollback_compression_enabled)
                        .label(Setting::ScrollbackCompression.label())
                        .on_toggle(move |enabled| {
                            Desktop::Settings(Message::ScrollbackCompression(id, enabled))
                        }),
                    text("Packs older scrollback lines while the terminal is idle. Applies to open panes when saved.").size(12),
                ].spacing(3),
            ),
            highlighted(
                dialog,
                Setting::BoldTextMode,
                column![
                    row![
                        text(Setting::BoldTextMode.label()),
                        pick_list(BoldTextMode::ALL, Some(settings.terminal_bold_text_mode), move |mode| {
                            Desktop::Settings(Message::BoldTextMode(id, mode))
                        }),
                    ].spacing(8).align_y(Alignment::Center),
                    text("Bright colors uses the bright ANSI palette for bold text in the eight basic colors. Applies to open terminals and styled recordings when saved.").size(12),
                ].spacing(4),
            ),
            highlighted(
                dialog,
                Setting::CommandTimestamps,
                column![
                    checkbox(settings.command_timestamps_enabled)
                        .label(Setting::CommandTimestamps.label())
                        .on_toggle(move |enabled| {
                            Desktop::Settings(Message::CommandTimestamps(id, enabled))
                        }),
                    text("Default for new terminal tabs.").size(12),
                ]
                .spacing(3),
            ),
            rule::horizontal(1),
            text("Paste Protection").size(16),
            paste,
            rule::horizontal(1),
            text("Clipboard Access").size(16),
            highlighted(
                dialog,
                Setting::TerminalCopyOnSelect,
                checkbox(settings.terminal_copy_on_select_enabled)
                    .label(Setting::TerminalCopyOnSelect.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::TerminalCopyOnSelect(id, enabled))),
            ),
            clipboard,
        ];
        if !policy.read_allowed || !policy.write_allowed {
            page = page.push(
                text("Unavailable clipboard permissions are managed by your organization.")
                    .size(13),
            );
        }
        if policy.internal {
            page = page.push(
                text("Terminal clipboard requests use the managed internal clipboard.").size(13),
            );
        }
        page.push(rule::horizontal(1))
            .push(text("Shell Integration").size(16))
            .push(highlighted(
                dialog,
                Setting::CommandMarks,
                checkbox(settings.prompt_navigation_enabled)
                    .label(Setting::CommandMarks.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::CommandMarks(id, enabled))
                    }),
            ))
            .push(
                button("Set Up Shell Integration…").on_press(Desktop::ShellIntegration(
                    id,
                    super::shell_integration::Message::Open(None),
                )),
            )
            .push(highlighted(
                dialog,
                Setting::RemoteSidebar,
                row![
                    text(Setting::RemoteSidebar.label()),
                    pick_list(
                        RemoteSidebarPosition::ALL,
                        Some(settings.remote_sidebar_position),
                        move |position| Desktop::Settings(Message::RemoteSidebar(id, position)),
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ))
            .spacing(12)
            .into()
    }

    fn window_settings_page<'a>(
        &self,
        dialog: &'a Dialog,
        transparency: Option<Element<'a, Desktop>>,
    ) -> Element<'a, Desktop> {
        let id = dialog.id;
        let icon_size = dialog
            .toolbar_icon_size
            .unwrap_or_else(|| self.chrome.toolbar_icon_size());
        let startup = dialog.draft.preferences.startup;
        let mut page = column![
            text("Window Settings").size(18),
            highlighted(
                dialog,
                Setting::ToolbarIconSize,
                row![
                    text(Setting::ToolbarIconSize.label()).width(122),
                    toolbar_size_scale(id, icon_size),
                    text(format!("{icon_size} px")).width(48),
                    self.toolbar_icon_preview(icon_size),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ),
            highlighted(
                dialog,
                Setting::RememberGeometry,
                checkbox(startup.remember_geometry)
                    .label(Setting::RememberGeometry.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::Startup(id, startup_settings::Edit::RememberGeometry(enabled)))),
            ),
            text("Restore the last size, position and maximized state when opening a window.").size(13),
            highlighted(
                dialog,
                Setting::RememberDashboard,
                checkbox(startup.remember_dashboard)
                    .label(Setting::RememberDashboard.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::Startup(id, startup_settings::Edit::RememberDashboard(enabled)))),
            ),
            text("Open the dashboard if it was visible when the last window closed.").size(13),
            rule::horizontal(1),
            text("Tabs").size(16),
            highlighted(
                dialog,
                Setting::ToolWindowTabs,
                checkbox(dialog.draft.preferences.open_tool_windows_as_tabs)
                    .label(Setting::ToolWindowTabs.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::ToolWindowTabs(id, enabled))
                    }),
            ),
            text("Takes effect the next time you open a supported tool. The tab opens in the window whose menu you used.")
                .size(13),
            highlighted(
                dialog,
                Setting::ConnectionColorBorder,
                column![
                    tooltip(
                        checkbox(dialog.draft.preferences.connection_color_border_enabled)
                            .label(Setting::ConnectionColorBorder.label())
                            .on_toggle(move |enabled| Desktop::Settings(Message::ConnectionColorBorder(id, enabled))),
                        text("The colored dot on the tab stays visible either way."),
                        tooltip::Position::Bottom,
                    ),
                    text("For colored connections, the frame takes 3 px per side. Switching it resizes open terminals.")
                        .size(13),
                ].spacing(3),
            ),
            highlighted(
                dialog,
                Setting::SplitPaneFocusRing,
                tooltip(
                    checkbox(dialog.draft.preferences.split_pane_focus_ring_enabled)
                        .label(Setting::SplitPaneFocusRing.label())
                        .on_toggle(move |enabled| Desktop::Settings(Message::SplitPaneFocusRing(id, enabled))),
                    text("Shows the pane with keyboard focus and dims the frame when focus moves outside the pane."),
                    tooltip::Position::Bottom,
                ),
            ),
            highlighted(
                dialog,
                Setting::ShellTitles,
                checkbox(dialog.draft.preferences.tab_title_from_shell_enabled)
                    .label(Setting::ShellTitles.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::ShellTitles(id, enabled))),
            ),
            text("Hover over a tab to see the connection it belongs to.").size(13),
            highlighted(
                dialog,
                Setting::RecentTabs,
                checkbox(dialog.draft.preferences.tab_switch_most_recent_first)
                    .label(Setting::RecentTabs.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::RecentTabs(id, enabled))),
            ),
            text("Uses Ctrl on macOS too. The order is kept while this window is open.").size(13),
            rule::horizontal(1),
            text("Session Restore").size(16),
        ];
        page = match dialog.draft.restore {
            Some(mode) => page.push(highlighted(
                dialog,
                Setting::Restore,
                row![
                    text(format!("{}:", Setting::Restore.label())).width(128),
                    pick_list(
                        RestoreMode::ALL.map(RestoreChoice),
                        Some(RestoreChoice(mode)),
                        move |choice| { Desktop::Settings(Message::Restore(id, choice.0)) }
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )),
            None => page.push(text("Session recovery is unavailable.").size(14)),
        };
        page = page.push(rule::horizontal(1)).push(highlighted(
            dialog,
            Setting::FixedGeometry,
            dialog
                .startup
                .view(startup)
                .map(move |edit| Desktop::Settings(Message::Startup(id, edit))),
        ));
        if let Some(controls) = transparency {
            page = page.push(rule::horizontal(1)).push(highlighted(
                dialog,
                Setting::Transparency,
                controls,
            ));
        }
        page.spacing(12).into()
    }
}

fn toolbar_size_scale(id: window::Id, size: u16) -> Element<'static, Desktop> {
    const MAJOR_TICK: usize = 8;
    let min = *TOOLBAR_ICON_SIZE.start();
    let ticks = TOOLBAR_ICON_SIZE.fold(row![], |ticks, mark| {
        let major = usize::from(mark - min).is_multiple_of(MAJOR_TICK);
        ticks.push(
            container(rule::vertical(1))
                .center_x(Length::Fill)
                .height(if major { 6 } else { 3 }),
        )
    });
    let labels = TOOLBAR_ICON_SIZE
        .step_by(MAJOR_TICK)
        .fold(row![], |mut labels, mark| {
            if mark != min {
                labels = labels.push(space::horizontal());
            }
            labels.push(text(mark.to_string()).size(11).font(iced::Font::MONOSPACE))
        });
    column![
        slider(TOOLBAR_ICON_SIZE, size, move |size| Desktop::Settings(
            Message::ToolbarIconSize(id, size)
        ))
        .step(1_u16),
        container(ticks).padding([0, 5]),
        labels,
    ]
    .width(260)
    .into()
}

fn messaging_settings_page(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let seconds = dialog.draft.preferences.terminal_silence_seconds;
    let saved = seconds.to_string();
    let value = dialog.silence_seconds_input.as_deref().unwrap_or(&saved);
    column![
        text("Notifications").size(18),
        highlighted(
            dialog,
            Setting::SilenceThreshold,
            column![
                row![
                    text(Setting::SilenceThreshold.label()),
                    button("−").on_press_maybe(
                        (seconds > MIN_TERMINAL_SILENCE_SECONDS)
                            .then_some(Desktop::Settings(Message::StepSilenceSeconds(id, false)))
                    ),
                    text_input("30", value)
                        .width(72)
                        .on_input(move |value| Desktop::Settings(Message::SilenceSeconds(
                            id, value
                        ))),
                    button("+").on_press_maybe(
                        (seconds < MAX_TERMINAL_SILENCE_SECONDS)
                            .then_some(Desktop::Settings(Message::StepSilenceSeconds(id, true)))
                    ),
                    text("seconds without output"),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .wrap(),
                dialog.silence_seconds_input.as_ref().map(|_| {
                    text("Enter a duration between 5 and 3600 seconds.")
                        .style(text::danger)
                        .size(13)
                }),
                text("Applies to tabs watched with Monitor for Silence in the tab’s context menu.")
                    .size(13),
            ]
            .spacing(5)
        ),
    ]
    .spacing(12)
    .into()
}

fn font_settings_page(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let preferences = &dialog.draft.preferences;
    let family = preferences
        .terminal_font_family
        .as_deref()
        .unwrap_or(MONOSPACED);
    let font = installed_font(family);
    let size = preferences.terminal_font;
    let formatted_size = size.to_string();
    let sample = Scheme::default();
    column![
        text("Font Settings").size(18),
        highlighted(
            dialog,
            Setting::FontFamily,
            row![
                text(Setting::FontFamily.label()).width(150),
                pick_list(
                    installed_families(),
                    Some(family.to_owned()),
                    move |value| { Desktop::Settings(Message::FontFamily(id, value)) }
                )
                .width(235),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ),
        font.is_none()
            .then(|| text("The selected font is unavailable. Using Monospaced.").size(13)),
        highlighted(
            dialog,
            Setting::FontSize,
            row![
                text(Setting::FontSize.label()).width(150),
                text_input(
                    "8–72",
                    dialog.font_size_input.as_deref().unwrap_or(&formatted_size)
                )
                .width(50)
                .on_input(move |value| Desktop::Settings(Message::FontSizeInput(id, value))),
                button("−").style(button::secondary).on_press_maybe(
                    (size > profile::MIN_TERMINAL_FONT)
                        .then(|| Desktop::Settings(Message::FontSize(id, size - 1))),
                ),
                button("+").style(button::secondary).on_press_maybe(
                    (size < profile::MAX_TERMINAL_FONT)
                        .then(|| Desktop::Settings(Message::FontSize(id, size + 1))),
                ),
                text("pt"),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ),
        dialog
            .font_size_input
            .as_ref()
            .map(|_| text("Enter a font size between 8 and 72.")
                .size(13)
                .style(text::danger)),
        text("Saving updates the size of panes that follow the global setting.").size(13),
        preferences
            .themes
            .apply_font
            .then(|| text("The selected theme can override font family and size.").size(13)),
        rule::horizontal(1),
        text("Preview").size(16),
        container(
            text("AaBbCcDdEe 0123456789")
                .font(font.unwrap_or(iced::Font::MONOSPACE))
                .size(f32::from(size))
                .color(sample.foreground.color())
        )
        .width(Length::Fill)
        .padding(12)
        .style(move |_| container::Style::default().background(sample.background.color())),
    ]
    .spacing(12)
    .into()
}

fn highlighted<'a>(
    dialog: &Dialog,
    setting: Setting,
    content: impl Into<Element<'a, Desktop>>,
) -> Element<'a, Desktop> {
    container(content)
        .width(Length::Fill)
        .padding(4)
        .style(if dialog.revealed == Some(setting) {
            container::secondary
        } else {
            container::transparent
        })
        .into()
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct RestoreChoice(RestoreMode);

impl fmt::Display for RestoreChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            RestoreMode::Ask => "Offer to restore the previous session",
            RestoreMode::Auto => "Restore the previous session automatically",
            RestoreMode::Off => "Do nothing",
        })
    }
}

fn search_matches(query: &str) -> impl Iterator<Item = Setting> {
    let query = normalized(query);
    Setting::ALL.into_iter().filter(move |setting| {
        let query = query.trim();
        if query.chars().take(2).count() < 2 {
            return false;
        }
        let target = normalized(&format!(
            "{} {} {}",
            setting.label(),
            setting.section().label(),
            setting.keywords()
        ));
        query.split_whitespace().all(|word| target.contains(word))
    })
}

fn normalized(value: &str) -> String {
    value
        .nfkd()
        .filter(|&ch| !is_combining_mark(ch))
        .flat_map(char::to_lowercase)
        .collect()
}

fn save_preferences(preferences: &WindowSettings) -> std::io::Result<()> {
    profile::window_settings_path().and_then(|path| preferences.save(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_design_save_is_atomic_and_keeps_terminal_and_editor_colors() {
        use super::super::chrome::{Action, Message as ChromeMessage};
        use crate::terminal_theme::{Id, Preset};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        workspace
            .window_settings
            .themes
            .select(Some(Id::BuiltIn(Preset::Dark)));
        let terminal = workspace.window_settings.terminal_appearance();
        let editor = workspace.editor_content_theme();
        let original = workspace.window_theme(owner);
        let _ = workspace.update_chrome(ChromeMessage::Invoke(owner, Action::AppearanceSettings));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Appearance);
        let id = dialog.id;
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.update_settings(Message::Design(id, Design::AmberCrt));
        let _ =
            workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write failure")));
        assert_eq!(workspace.window_theme(owner), original);
        assert_eq!(workspace.window_theme(other), original);
        assert!(workspace.settings.0.as_ref().unwrap().problem.is_some());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(workspace.window_settings.appearance.design, Design::Normal);

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Appearance));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let design = Design::AtlantaFxPrimerLight;
        let _ = workspace.update_settings(Message::Design(id, design));
        let _ = workspace.update_settings(Message::DesignAnimations(id, false));
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let folder = std::env::temp_dir().join(format!(
            "kortty-appearance-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("window-settings.json");
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let restored: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
        assert_eq!(restored.appearance.design, design);
        assert!(!restored.appearance.animations);
        assert_eq!(workspace.window_settings, restored);
        let palette = design.palette().unwrap();
        for window in [owner, other] {
            let theme = workspace.window_theme(window).palette();
            assert_eq!(theme.background, palette.background.color());
            assert_eq!(theme.primary, palette.accent.color());
        }
        assert_eq!(workspace.window_settings.terminal_appearance(), terminal);
        assert_eq!(workspace.editor_content_theme(), editor);
    }

    #[test]
    fn theme_editor_validates_drafts_and_save_updates_application_palette() {
        use crate::terminal_theme::{Rgb, Scheme};
        use theme_settings::{Channel, Edit as ThemeEdit};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        assert_eq!(workspace.window_theme(owner), iced::Theme::Dark);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Themes));
        let id = workspace.settings.0.as_ref().unwrap().id;
        assert!(workspace.settings_view(id).is_some());
        let mut edit = |message| workspace.update_settings(Message::Theme(id, message));
        let _ = edit(ThemeEdit::New);
        let _ = edit(ThemeEdit::Name("My theme".into()));
        let _ = edit(ThemeEdit::Color(Channel::Background, "invalid".into()));
        let _ = edit(ThemeEdit::Apply);
        let _ = workspace.save_settings_to(id, |_| panic!("an invalid theme reached persistence"));
        assert!(workspace.settings.title(id).is_some());
        assert!(workspace.window_settings.themes.appearance().is_none());
        let _ = workspace.update_settings(Message::Theme(
            id,
            ThemeEdit::Color(Channel::Background, "#F5F6F7".into()),
        ));
        let _ = workspace.update_settings(Message::Theme(
            id,
            ThemeEdit::Color(Channel::Foreground, "#202122".into()),
        ));
        let _ = workspace.update_settings(Message::Theme(id, ThemeEdit::Apply));
        let _ = workspace.update_settings(Message::Theme(id, ThemeEdit::ApplyFont(true)));
        assert!(workspace.window_settings.themes.appearance().is_none());

        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let folder =
            std::env::temp_dir().join(format!("kortty-themes-{:016x}", u64::from_ne_bytes(random)));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("window-settings.json");
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let persisted: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
        let scheme = Scheme {
            foreground: Rgb::from_hex(0x202122),
            background: Rgb::from_hex(0xF5F6F7),
            ..Scheme::default()
        };
        assert_eq!(persisted.themes.appearance(), Some(scheme));
        assert!(persisted.themes.apply_font);
        let palette = workspace.window_theme(owner).palette();
        assert_eq!(palette.background, scheme.background.color());
        assert_eq!(palette.text, scheme.foreground.color());
        assert!(workspace.settings.title(id).is_none());

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Themes));
        let next = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::Theme(next, ThemeEdit::Delete));
        let _ = workspace.update_settings(Message::Theme(next, ThemeEdit::ConfirmDelete));
        let _ = workspace.update_settings(Message::Cancel(next));
        assert_eq!(workspace.window_settings.themes, persisted.themes);
    }

    #[test]
    fn colors_validate_and_commit_profile_overrides_and_palette_in_one_save() {
        use crate::terminal_theme::{Id, Preset, Rgb};
        use color_settings::{Channel, Edit};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Colors));
        let id = workspace.settings.0.as_ref().unwrap().id;
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.update_settings(Message::Color(
            id,
            Edit::Rgb(Channel::Foreground, "invalid".into()),
        ));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid Colors reached persistence"));
        assert!(workspace.window_settings.colors.is_default());
        assert!(workspace.settings.0.as_ref().unwrap().problem.is_some());

        let profile = Id::BuiltIn(Preset::Dark);
        for edit in [
            Edit::Rgb(Channel::Foreground, "#102030".into()),
            Edit::Rgb(Channel::Selection, "#4080C0".into()),
            Edit::Rgb(Channel::Ansi(9), "#65AC32".into()),
            Edit::CursorBlink(false),
            Edit::Profile(Some(profile)),
            Edit::EnableSgr(false),
            Edit::Rgb(Channel::Ansi(9), "#123456".into()),
        ] {
            let _ = workspace.update_settings(Message::Color(id, edit));
        }
        let _ =
            workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write failure")));
        assert!(workspace.window_settings.colors.is_default());
        assert!(workspace.window_settings.themes.selected().is_none());

        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let folder =
            std::env::temp_dir().join(format!("kortty-colors-{:016x}", u64::from_ne_bytes(random)));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("window-settings.json");
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let mut restored: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
        assert_eq!(restored, workspace.window_settings);
        assert_eq!(restored.themes.selected(), Some(profile));
        let selected = restored.terminal_appearance();
        assert_eq!(
            selected.foreground,
            restored.themes.appearance().unwrap().foreground
        );
        assert_eq!(selected.selection, Some(Rgb::from_hex(0x4080C0)));
        assert_eq!(selected.palette.rgb(9), [0x65, 0xAC, 0x32]);
        assert!(!selected.cursor_blink && !selected.sgr_enabled);
        restored.themes.select(None);
        assert_eq!(
            restored.terminal_appearance().foreground,
            Rgb::from_hex(0x102030)
        );
        assert!(!restored.terminal_appearance().cursor_blink);
    }

    #[test]
    fn search_requires_two_characters_and_matches_words_across_sections_and_labels() {
        assert_eq!(search_matches("o").count(), 0);
        assert_eq!(
            search_matches("WÍNDOW restORE").collect::<Vec<_>>(),
            [Setting::Restore]
        );
        assert_eq!(
            search_matches("osc 133").collect::<Vec<_>>(),
            [Setting::CommandMarks]
        );
        assert_eq!(
            search_matches("sftp dock").collect::<Vec<_>>(),
            [Setting::RemoteSidebar]
        );
        assert_eq!(
            search_matches("window opacity").collect::<Vec<_>>(),
            [Setting::Transparency]
        );
        assert_eq!(
            search_matches("toolbar icon size").collect::<Vec<_>>(),
            [Setting::ToolbarIconSize]
        );
        assert_eq!(
            search_matches("tool windows as tabs").collect::<Vec<_>>(),
            [Setting::ToolWindowTabs]
        );
        assert_eq!(search_matches("window clipboard").count(), 0);
        assert_eq!(
            search_matches("connection color border").collect::<Vec<_>>(),
            [Setting::ConnectionColorBorder]
        );
        assert_eq!(
            search_matches("split pane focus ring").collect::<Vec<_>>(),
            [Setting::SplitPaneFocusRing]
        );
        assert_eq!(
            search_matches("font zoom").collect::<Vec<_>>(),
            [Setting::FontSize]
        );
        assert_eq!(
            search_matches("font face").collect::<Vec<_>>(),
            [Setting::FontFamily]
        );
        assert_eq!(
            search_matches("terminal bracketed").collect::<Vec<_>>(),
            [Setting::PasteWarning]
        );
        assert_eq!(
            search_matches("scrollback compress").collect::<Vec<_>>(),
            [Setting::ScrollbackCompression]
        );
        assert_eq!(
            search_matches("paste threshold").collect::<Vec<_>>(),
            [Setting::LargePasteWarning]
        );
    }

    #[test]
    fn draft_applies_only_edited_preferences_and_cannot_override_policy() {
        let initial = Draft {
            preferences: WindowSettings::default(),
            restore: None,
            backup: BackupPreferences::default(),
        };
        let mut draft = initial.clone();
        draft.preferences.tab_title_from_shell_enabled = false;
        draft.preferences.clipboard_read_enabled = true;
        draft.preferences.clipboard_write_enabled = true;
        draft.preferences.paste_warning = WarningMode::Off;
        draft.preferences.terminal_font = 36;
        draft.preferences.paste_large_warning_kib = 1;
        draft.preferences.command_timestamps_enabled = true;
        let current = WindowSettings {
            terminal_transparency: 42,
            open_tool_windows_as_tabs: true,
            prompt_navigation_enabled: false,
            ..initial.preferences.clone()
        };
        let policy = ClipboardPolicy {
            read_allowed: false,
            minimum_paste_warning: WarningMode::Always,
            ..ClipboardPolicy::default()
        };
        let next = draft.merge_preferences(&initial, current, &policy);
        assert!(!next.tab_title_from_shell_enabled);
        assert!(next.open_tool_windows_as_tabs);
        assert!(!next.prompt_navigation_enabled);
        assert!(next.command_timestamps_enabled);
        assert_eq!(next.terminal_transparency, 42);
        assert!(!next.clipboard_read_enabled);
        assert!(next.clipboard_write_enabled);
        assert_eq!(next.paste_warning, WarningMode::Always);
        assert_eq!(next.terminal_font, 36);
        assert_eq!(next.paste_large_warning_kib, 1);
    }

    #[test]
    fn connection_color_border_changes_only_after_settings_save() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Window));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ConnectionColorBorder(id, false));
        // Test double for a failed preferences write.
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert!(workspace.window_settings.connection_color_border_enabled);
        assert!(workspace.settings.title(id).is_some());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.connection_color_border_enabled);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ConnectionColorBorder(id, false));
        // Test double for successful persistence; the profile round-trip is covered separately.
        let _ = workspace.save_settings_to(id, |preferences| {
            assert!(!preferences.connection_color_border_enabled);
            Ok(())
        });
        assert!(!workspace.window_settings.connection_color_border_enabled);
        assert!(workspace.settings.title(id).is_none());
    }

    #[test]
    fn split_pane_focus_ring_changes_only_after_settings_save() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Window));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::SplitPaneFocusRing(id, false));
        // Test double for a failed preferences write.
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert!(workspace.window_settings.split_pane_focus_ring_enabled);
        assert!(workspace.settings.title(id).is_some());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.split_pane_focus_ring_enabled);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::SplitPaneFocusRing(id, false));
        // Test double for successful persistence; the profile round-trip is covered separately.
        let _ = workspace.save_settings_to(id, |preferences| {
            assert!(!preferences.split_pane_focus_ring_enabled);
            Ok(())
        });
        assert!(!workspace.window_settings.split_pane_focus_ring_enabled);
        assert!(workspace.settings.title(id).is_none());
    }

    #[test]
    fn tool_window_tabs_only_changes_after_successful_save() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Window));
        let id = workspace.settings.0.as_ref().unwrap().id;
        assert!(!workspace.window_settings.open_tool_windows_as_tabs);
        let _ = workspace.update_settings(Message::ToolWindowTabs(id, true));
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert!(!workspace.window_settings.open_tool_windows_as_tabs);
        assert!(workspace.settings.title(id).is_some());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(!workspace.window_settings.open_tool_windows_as_tabs);

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Window));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::ToolWindowTabs(id, true));
        let _ = workspace.save_settings_to(id, |preferences| {
            assert!(preferences.open_tool_windows_as_tabs);
            Ok(())
        });
        assert!(workspace.window_settings.open_tool_windows_as_tabs);
        assert!(workspace.settings.title(id).is_none());
    }

    #[test]
    fn scrollback_compression_settings_save_updates_open_and_new_streams() {
        use crate::session::{GridSize, Session};
        use kortty_terminal::TerminalProfile;

        let legacy: WindowSettings = serde_json::from_str("{}").unwrap();
        assert!(legacy.scrollback_compression_enabled);
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let start_stream = |workspace: &mut Workspace| {
            let grid = GridSize::default();
            let session = Session::ssh(
                None,
                grid,
                TerminalProfile::Xterm,
                true,
                workspace.window_settings.terminal_appearance(),
            )
            .unwrap();
            workspace.store_session(session, grid, workspace.window_settings.terminal_font)
        };
        let requested = |workspace: &Workspace, id| {
            workspace.sessions[&id]
                .session
                .scrollback_compression_requested()
        };
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let existing = start_stream(&mut workspace);
        assert!(requested(&workspace, existing));
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ScrollbackCompression(id, false));
        // Test double for a failed preference write. The live request stays unchanged.
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert!(requested(&workspace, existing));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.scrollback_compression_enabled);

        let directory = std::env::temp_dir().join(format!(
            "kortty-scrollback-settings-{}",
            crate::private_files::nonce().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("window-settings.json");
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ScrollbackCompression(id, false));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let restored: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!restored.scrollback_compression_enabled);
        assert!(!requested(&workspace, existing));
        let new = start_stream(&mut workspace);
        assert!(!requested(&workspace, new));

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ScrollbackCompression(id, true));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        std::fs::remove_dir_all(directory).unwrap();
        assert!(requested(&workspace, existing));
        assert!(requested(&workspace, new));
        for pane in workspace.sessions.values() {
            let _ = pane.session.stop().recv_blocking();
        }
    }

    #[test]
    fn terminal_copy_on_select_defaults_on_and_persists_only_after_save() -> std::io::Result<()> {
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.terminal_copy_on_select_enabled);
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let before = std::fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::TerminalCopyOnSelect(id, false));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.terminal_copy_on_select_enabled);
        assert_eq!(std::fs::read(&path)?, before);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::TerminalCopyOnSelect(id, false));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let saved: WindowSettings = serde_json::from_slice(&std::fs::read(path)?)?;
        assert!(!saved.terminal_copy_on_select_enabled);
        assert!(!workspace.window_settings.terminal_copy_on_select_enabled);
        assert!(workspace.settings.title(id).is_none());
        Ok(())
    }

    #[test]
    fn bold_text_mode_settings_apply_only_after_a_successful_save() -> std::io::Result<()> {
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert_eq!(
            legacy.terminal_appearance().bold_text_mode,
            BoldTextMode::BoldFont
        );
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let before = std::fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::BoldTextMode(id, BoldTextMode::BrightColor));
        // Test double for a failed write. Neither live preferences nor the file can change.
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert_eq!(
            workspace
                .window_settings
                .terminal_appearance()
                .bold_text_mode,
            BoldTextMode::BoldFont
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(std::fs::read(&path)?, before);

        let id = open(&mut workspace);
        let mode = BoldTextMode::BoldFontAndBrightColor;
        let _ = workspace.update_settings(Message::BoldTextMode(id, mode));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let bytes = std::fs::read(path)?;
        WindowSettings::validate_backup(&bytes)?;
        let saved: WindowSettings = serde_json::from_slice(&bytes)?;
        assert_eq!(saved.terminal_appearance().bold_text_mode, mode);
        assert_eq!(
            workspace
                .window_settings
                .terminal_appearance()
                .bold_text_mode,
            mode
        );
        assert!(workspace.settings.title(id).is_none());
        Ok(())
    }

    #[test]
    fn terminal_silence_threshold_is_bounded_and_saved_from_messaging() -> std::io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert_eq!(legacy.terminal_silence_seconds, 30);
        workspace.window_settings.save(&path)?;
        let before = std::fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Messaging));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::SilenceSeconds(id, "3601".into()));
        let _ = workspace.update_settings(Message::Section(id, Section::Font));
        let _ = workspace.save_settings_to(id, |_| {
            panic!("out of range silence threshold reached persistence")
        });
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Messaging);
        assert_eq!(dialog.revealed, Some(Setting::SilenceThreshold));
        assert_eq!(std::fs::read(&path)?, before);
        assert_eq!(
            search_matches("silence threshold").collect::<Vec<_>>(),
            [Setting::SilenceThreshold]
        );

        let _ = workspace.update_settings(Message::SilenceSeconds(id, "5".into()));
        let _ = workspace.update_settings(Message::StepSilenceSeconds(id, false));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let bytes = std::fs::read(path)?;
        WindowSettings::validate_backup(&bytes)?;
        let saved: WindowSettings = serde_json::from_slice(&bytes)?;
        assert_eq!(saved.terminal_silence_seconds, 5);
        assert_eq!(workspace.window_settings.terminal_silence_seconds, 5);
        assert!(workspace.settings.title(id).is_none());
        Ok(())
    }

    #[test]
    fn remote_sidebar_setting_commits_only_on_save_and_preserves_a_newer_width() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
        let id = workspace.settings.0.as_ref().unwrap().id;
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.update_settings(Message::RemoteSidebar(id, RemoteSidebarPosition::Left));
        assert_eq!(
            workspace.window_settings.remote_sidebar_position,
            RemoteSidebarPosition::Hidden
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.remote_sidebar_position,
            RemoteSidebarPosition::Hidden
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::RemoteSidebar(id, RemoteSidebarPosition::Right));
        workspace.window_settings.remote_sidebar_width = 420;
        let _ = workspace.save_settings_to(id, |preferences| {
            assert_eq!(
                preferences.remote_sidebar_position,
                RemoteSidebarPosition::Right
            );
            assert_eq!(preferences.remote_sidebar_width, 420);
            Ok(())
        });
        assert_eq!(
            workspace.window_settings.remote_sidebar_position,
            RemoteSidebarPosition::Right
        );
        assert_eq!(workspace.window_settings.remote_sidebar_width, 420);
        assert!(workspace.settings.title(id).is_none());
    }

    #[test]
    fn font_settings_preview_draft_validates_size_and_saves_the_selected_family() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let family = installed_families().get(1);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let id = workspace.settings.0.as_ref().unwrap().id;
        if let Some(family) = family {
            let _ = workspace.update_settings(Message::FontFamily(id, family.clone()));
        }
        let _ = workspace.update_settings(Message::FontSizeInput(id, "21".into()));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.terminal_font,
            super::super::DEFAULT_FONT
        );
        assert!(workspace.window_settings.terminal_font_family.is_none());

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let id = workspace.settings.0.as_ref().unwrap().id;
        if let Some(family) = family {
            let _ = workspace.update_settings(Message::FontFamily(id, family.clone()));
        }
        let _ = workspace.update_settings(Message::FontSizeInput(id, "21".into()));
        let _ = workspace.update_settings(Message::FontSizeInput(id, "100".into()));
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.save_settings_to(id, |_| panic!("invalid font size reached persistence"));
        assert!(workspace.settings.title(id).is_some());

        let _ = workspace.update_settings(Message::FontSizeInput(id, "22".into()));
        let _ = workspace.save_settings_to(id, |preferences| {
            let bytes = serde_json::to_vec(preferences).unwrap();
            let restored: WindowSettings = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(restored.terminal_font, 22);
            assert_eq!(restored.terminal_font_family.as_ref(), family);
            assert_eq!(
                restored.terminal_font_face(None),
                family
                    .and_then(|name| installed_font(name))
                    .unwrap_or(iced::Font::MONOSPACE)
            );
            Ok(())
        });
        assert!(workspace.settings.title(id).is_none());
    }

    #[test]
    fn saving_font_size_refreshes_inherited_panes_and_preserves_explicit_sizes() {
        use crate::session::{GridSize, IbmKind, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        // Offline sessions exercise the same font provenance as a connected pane.
        let offline = || Session::disconnected_ibm(IbmKind::Tn3270e);
        let inherited =
            workspace.store_connected_session(offline(), GridSize::default(), None, None);
        let explicit = workspace.store_connected_session(
            offline(),
            GridSize::default(),
            None,
            Some(workspace.window_settings.terminal_font),
        );
        let mut restored = Vec::new();
        workspace
            .restore_pane(
                &profile::PaneLayout::Ibm {
                    font: workspace.window_settings.terminal_font,
                    focused: false,
                    kind: IbmKind::Tn3270e,
                },
                &mut restored,
                &mut None,
            )
            .unwrap();
        let restored = restored[0];
        let previous_size = workspace.sessions[&explicit].font;
        let zoomed = previous_size + 2;
        workspace.sessions.get_mut(&inherited).unwrap().font = zoomed;

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::FontSize(id, 26));
        let _ =
            workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write failure")));
        assert_eq!(workspace.sessions[&inherited].font, zoomed);
        let _ = workspace.save_settings_to(id, |_| Ok(()));

        let pane = &workspace.sessions[&inherited];
        assert_eq!(
            (pane.font, pane.font_baseline, pane.font_size_override()),
            (26, 26, None)
        );
        for id in [explicit, restored] {
            let pane = &workspace.sessions[&id];
            assert_eq!(
                (pane.font, pane.font_baseline, pane.font_size_override()),
                (previous_size, previous_size, Some(previous_size))
            );
        }
        let reconnected = workspace.store_connected_session(
            offline(),
            GridSize::default(),
            Some(inherited),
            None,
        );
        assert_eq!(workspace.sessions[&reconnected].font_size_override(), None);
    }

    #[test]
    fn cancel_discards_settings_and_old_window_messages_cannot_apply_to_a_new_draft() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Window));
        let old = workspace.settings.0.as_ref().unwrap().id;
        assert!(workspace.settings_view(old).is_some());
        let _ = workspace.update_settings(Message::ShellTitles(old, false));
        let _ = workspace.update_settings(Message::Section(old, Section::Font));
        let _ = workspace.update_settings(Message::FontSize(old, 255));
        let _ = workspace.update_settings(Message::LargePasteWarning(old, 255));
        let edited = &workspace.settings.0.as_ref().unwrap().draft.preferences;
        assert_eq!(edited.terminal_font, profile::MAX_TERMINAL_FONT);
        assert_eq!(edited.paste_large_warning_kib, MAX_PASTE_KIB);
        assert!(workspace.window_settings.tab_title_from_shell_enabled);
        let _ = workspace.update_settings(Message::Cancel(old));
        assert!(workspace.settings.title(old).is_none());

        let _ = workspace.update_settings(Message::Open(owner));
        let new = workspace.settings.0.as_ref().unwrap().id;
        assert_ne!(old, new);
        let _ = workspace.update_settings(Message::Save(old));
        let draft = workspace.settings.0.as_ref().unwrap();
        assert_eq!(draft.id, new);
        assert_eq!(draft.section, Section::Font);
        assert_eq!(
            draft.draft.preferences.terminal_font,
            super::super::DEFAULT_FONT
        );
        assert!(draft.draft.preferences.tab_title_from_shell_enabled);
        let (other, _) = workspace.open_window(false);
        assert_eq!(workspace.settings.owner_closed(other), None);
        assert_eq!(workspace.settings.owner_closed(owner), Some(new));
        assert!(workspace.settings.title(new).is_none());
    }

    #[test]
    fn managed_permissions_are_applied_when_loading_saved_clipboard_settings() {
        let mut workspace = Workspace::default();
        workspace.window_settings.clipboard_read_enabled = true;
        workspace.window_settings.clipboard_write_enabled = true;
        workspace.clipboard.policy.read_allowed = false;
        workspace.initialize_saved_clipboard_settings();
        assert!(!workspace.clipboard.read_allowed());
        assert!(workspace.clipboard.write_allowed());
    }

    #[test]
    fn keyboard_conflict_blocks_save_then_rebinds_actions_in_already_open_windows() {
        use super::super::chrome::Action;
        use iced::keyboard::{Location, key::Named};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        // Test-only pane identifiers provide a split without starting PTYs.
        workspace.add_tab(owner, crate::session::SessionId(42));
        let tab = workspace.active_tab_mut(owner).unwrap();
        tab.grid
            .split(
                iced::widget::pane_grid::Axis::Vertical,
                tab.focused,
                crate::session::SessionId(43),
            )
            .unwrap();
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Keyboard));
        let settings = workspace.settings.0.as_ref().unwrap().id;
        let edit = |edit| Message::KeyBinding(settings, edit);
        let (quick_connect_key, modifiers) = if cfg!(target_os = "macos") {
            (Key::Character("t".into()), Modifiers::COMMAND)
        } else {
            (Key::Named(Named::F2), Modifiers::COMMAND | Modifiers::SHIFT)
        };
        let _ = workspace.update_settings(edit(Edit::Select(Action::Broadcast)));
        let _ = workspace.update_settings(edit(Edit::Record));
        let _ = workspace.key(
            settings,
            quick_connect_key.clone(),
            Location::Standard,
            modifiers,
            None,
        );
        let _ = workspace.key(
            settings,
            Key::Named(Named::Escape),
            Location::Standard,
            Modifiers::empty(),
            None,
        );
        assert_eq!(workspace.settings.title(settings), Some(TITLE));
        assert!(!workspace.capture_settings_shortcut(
            settings,
            &Key::Named(Named::F8),
            Modifiers::empty()
        ));

        let _ = workspace.update_settings(Message::Section(settings, Section::Font));
        let _ = workspace.save_settings_to(settings, |_| panic!("conflict reached persistence"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().section,
            Section::Keyboard
        );
        let _ = workspace.update_settings(edit(Edit::Select(Action::QuickConnect)));
        let _ = workspace.update_settings(edit(Edit::Record));
        let f7 = Key::Named(Named::F7);
        let _ = workspace.key(
            settings,
            f7.clone(),
            Location::Standard,
            Modifiers::empty(),
            None,
        );

        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let folder = std::env::temp_dir().join(format!(
            "kortty-keyboard-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("window-settings.json");
        let _ = workspace.save_settings_to(settings, |preferences| preferences.save(&path));
        let persisted: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, workspace.window_settings);
        assert!(workspace.settings.title(settings).is_none());
        let _ = workspace.key(
            owner,
            quick_connect_key.clone(),
            Location::Standard,
            modifiers,
            None,
        );
        assert!(workspace.windows[&owner].tab().unwrap().broadcast);
        let _ = workspace.key(
            other,
            f7.clone(),
            Location::Standard,
            Modifiers::empty(),
            None,
        );
        let quick_connect = workspace.quick_connect_for_owner(other).unwrap();
        let _ = workspace.close_window(quick_connect);

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Keyboard));
        let settings = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::KeyBinding(settings, Edit::ResetAll));
        let _ = workspace.save_settings_to(settings, |preferences| preferences.save(&path));
        let _ = workspace.key(other, f7, Location::Standard, Modifiers::empty(), None);
        assert!(workspace.quick_connect_for_owner(other).is_none());
        let _ = workspace.key(
            other,
            quick_connect_key,
            Location::Standard,
            modifiers,
            None,
        );
        assert!(workspace.quick_connect_for_owner(other).is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
