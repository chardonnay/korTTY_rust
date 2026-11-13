use std::fmt;
use std::ops::RangeInclusive;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    self, button, checkbox, column, container, opaque, pick_list, row, rule, scrollable, slider,
    space, stack, text, text_input, tooltip,
};
use iced::{Alignment, Element, Length, Size, Task, window};
use kortty_ssh::KeepAlive;
use kortty_terminal::TextEncoding;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::{
    MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS, MIN_TERMINAL_COLUMNS, MIN_TERMINAL_ROWS,
};
use crate::profile::{self, session::RestoreMode};
use crate::profile_backup::preferences::Preferences as BackupPreferences;
use crate::sftp_window::ConflictDefault;
#[cfg(test)]
use crate::terminal_agent::design::Preset as AgentDesign;
use crate::terminal_theme::{BoldTextMode, MONOSPACED, Scheme, installed_families, installed_font};
use crate::workspace::dialog_placement::{Kind as DialogKind, escape::on_escape};
use kortty_terminal::ScrollbackConfig;

use super::{
    Message as Desktop, Workspace,
    appearance::Design,
    chrome::TOOLBAR_ICON_SIZE,
    highlight,
    key_bindings::{Edit, Editor as KeyboardEditor},
    paste_protection::{MAX_PASTE_KIB, WarningMode},
    session_restore,
    sftp_access::SftpConflictPolicy,
    tab_titles::{
        MAX_TERMINAL_SILENCE_SECONDS, MIN_TERMINAL_SILENCE_SECONDS, RemoteSidebarPosition,
        WindowSettings,
    },
};

const TITLE: &str = "Settings";
const MAX_SEARCH_BYTES: usize = 192;
const SEARCH_MAX_WIDTH: f32 = 660.0;
const SEARCH_RESULTS_MAX_HEIGHT: f32 = 380.0;
const SEARCH_ROW_HEIGHT: f32 = 32.0;

mod agent_design_settings;
mod appearance_settings;
mod backup;
mod color_settings;
mod editor_settings;
mod export_settings;
mod highlight_settings;
mod logging_settings;
mod quick_select_settings;
mod search_input;
mod sftp_settings;
mod snippet_editor_settings;
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
    Editor,
    SnippetEditor,
    AgentDesign,
    Updates,
    Sftp,
}

impl Section {
    pub(in crate::workspace) const ALL: [Self; 17] = [
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
        Self::Editor,
        Self::SnippetEditor,
        Self::AgentDesign,
        Self::Updates,
        Self::Sftp,
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
            Self::Editor => "Editor",
            Self::SnippetEditor => "Snippet Editor",
            Self::AgentDesign => "AI Agent Design",
            Self::Updates => "Updates",
            Self::Sftp => "SFTP",
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
            Self::Editor => 12,
            Self::SnippetEditor => 13,
            Self::AgentDesign => 14,
            Self::Updates => 15,
            Self::Sftp => 16,
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
    PasteLineDelay,
    ClipboardWrite,
    ClipboardRead,
    CommandMarks,
    CommandTimestamps,
    RemoteSidebar,
    SftpParallel,
    SftpResume,
    SftpConflictDefault,
    SftpArchive,
    SftpIdle,
    InitialTerminalSize,
    SerialDefaultEncoding,
    TerminalLinkDetection,
    TerminalCopyOnSelect,
    QuickSelectAlphabet,
    QuickSelectPatterns,
    Highlighting,
    TerminalScrollbar,
    ScrollbackLines,
    ScrollbackMemory,
    ScrollbackCompression,
    SshKeepAlive,
    SshKeepAliveInterval,
    CodingAgentDetection,
    CodingAgentAppBadge,
    CodingAgentNotifications,
    TerminalNotifications,
    CodingAgentDockAutomatic,
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
    ExportSnippetCode,
    ToolbarIconSize,
    StartupMasterPassword,
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
    EditorCursorStyle,
    EditorCursorColor,
    SnippetFontFamily,
    SnippetFontSize,
    SnippetForeground,
    SnippetBackground,
    SnippetCursorStyle,
    SnippetCursorColor,
    AgentDesign,
    ApplicationUpdates,
    ApplicationUpdateInterval,
}

impl Setting {
    const ALL: [Self; 77] = [
        Self::FontFamily,
        Self::FontSize,
        Self::Colors,
        Self::Themes,
        Self::Appearance,
        Self::PasteWarning,
        Self::LargePasteWarning,
        Self::PasteLineDelay,
        Self::ClipboardWrite,
        Self::ClipboardRead,
        Self::CommandMarks,
        Self::CommandTimestamps,
        Self::RemoteSidebar,
        Self::SftpParallel,
        Self::SftpResume,
        Self::SftpConflictDefault,
        Self::SftpArchive,
        Self::SftpIdle,
        Self::InitialTerminalSize,
        Self::SerialDefaultEncoding,
        Self::TerminalLinkDetection,
        Self::TerminalCopyOnSelect,
        Self::QuickSelectAlphabet,
        Self::QuickSelectPatterns,
        Self::Highlighting,
        Self::TerminalScrollbar,
        Self::ScrollbackLines,
        Self::ScrollbackMemory,
        Self::ScrollbackCompression,
        Self::SshKeepAlive,
        Self::SshKeepAliveInterval,
        Self::CodingAgentDetection,
        Self::CodingAgentAppBadge,
        Self::CodingAgentNotifications,
        Self::TerminalNotifications,
        Self::CodingAgentDockAutomatic,
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
        Self::ExportSnippetCode,
        Self::ToolbarIconSize,
        Self::StartupMasterPassword,
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
        Self::EditorCursorStyle,
        Self::EditorCursorColor,
        Self::SnippetFontFamily,
        Self::SnippetFontSize,
        Self::SnippetForeground,
        Self::SnippetBackground,
        Self::SnippetCursorStyle,
        Self::SnippetCursorColor,
        Self::AgentDesign,
        Self::ApplicationUpdates,
        Self::ApplicationUpdateInterval,
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
            Self::PasteLineDelay => "Pause between pasted lines",
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
            Self::SftpParallel => "Parallel file transfers",
            Self::SftpResume => "Resume partial transfers",
            Self::SftpConflictDefault => "Default transfer conflict action",
            Self::SftpArchive => "Archive defaults",
            Self::SftpIdle => "Close SFTP after inactivity",
            Self::InitialTerminalSize => "Initial terminal size",
            Self::SerialDefaultEncoding => "Default serial character encoding",
            Self::TerminalLinkDetection => "Detect web and e-mail addresses in terminal text",
            Self::TerminalCopyOnSelect => "Copy selection to clipboard automatically",
            Self::QuickSelectAlphabet => "Quick select label letters",
            Self::QuickSelectPatterns => "Quick select patterns",
            Self::Highlighting => "Keyword highlighting",
            Self::TerminalScrollbar => "Show scrollbar in terminal",
            Self::ScrollbackLines => "Maximum scrollback lines",
            Self::ScrollbackMemory => "Scrollback memory limit",
            Self::ScrollbackCompression => "Compress the scrollback in the background while idle",
            Self::SshKeepAlive => "Enable SSH keepalive",
            Self::SshKeepAliveInterval => "SSH keepalive interval",
            Self::CodingAgentDetection => "Detect coding agents in local shell tabs",
            Self::CodingAgentAppBadge => {
                "Show the number of agents waiting for a decision on the app icon"
            }
            Self::CodingAgentNotifications => {
                "Notify when a coding agent needs a decision or finishes in an unfocused pane"
            }
            Self::TerminalNotifications => "Terminal desktop notifications",
            Self::CodingAgentDockAutomatic => "Automatically show and hide the Coding Agents panel",
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
            Self::ExportWatermark => "Add a watermark to exported documents",
            Self::ExportFooter => "Show a footer in exported documents",
            Self::ExportSnippetCode => "Include full script in analysis reports by default",
            Self::ToolbarIconSize => "Toolbar icon size",
            Self::StartupMasterPassword => "Require master password at startup",
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
            Self::EditorCursorStyle => "Cursor style",
            Self::EditorCursorColor => "Cursor color",
            Self::SnippetFontFamily => "Font family",
            Self::SnippetFontSize => "Font size",
            Self::SnippetForeground => "Foreground color",
            Self::SnippetBackground => "Background color",
            Self::SnippetCursorStyle => "Cursor style",
            Self::SnippetCursorColor => "Cursor color",
            Self::AgentDesign => "Default AI agent design",
            Self::ApplicationUpdates => "Check automatically for Rust updates",
            Self::ApplicationUpdateInterval => "Update check interval",
        }
    }

    fn section(self) -> Section {
        match self {
            Self::FontFamily | Self::FontSize => Section::Font,
            Self::Colors => Section::Colors,
            Self::Themes => Section::Themes,
            Self::Appearance => Section::Appearance,
            Self::AgentDesign => Section::AgentDesign,
            Self::ApplicationUpdates | Self::ApplicationUpdateInterval => Section::Updates,
            Self::SftpParallel
            | Self::SftpResume
            | Self::SftpConflictDefault
            | Self::SftpArchive
            | Self::SftpIdle => Section::Sftp,
            Self::PasteWarning
            | Self::LargePasteWarning
            | Self::PasteLineDelay
            | Self::ClipboardWrite
            | Self::ClipboardRead
            | Self::CommandMarks
            | Self::CommandTimestamps
            | Self::RemoteSidebar
            | Self::InitialTerminalSize
            | Self::SerialDefaultEncoding
            | Self::TerminalLinkDetection
            | Self::TerminalCopyOnSelect
            | Self::QuickSelectAlphabet
            | Self::QuickSelectPatterns
            | Self::Highlighting
            | Self::TerminalScrollbar
            | Self::ScrollbackLines
            | Self::ScrollbackMemory
            | Self::ScrollbackCompression
            | Self::SshKeepAlive
            | Self::SshKeepAliveInterval
            | Self::CodingAgentDetection
            | Self::CodingAgentAppBadge
            | Self::CodingAgentDockAutomatic
            | Self::BoldTextMode => Section::Terminal,
            Self::CodingAgentNotifications
            | Self::TerminalNotifications
            | Self::SilenceThreshold => Section::Messaging,
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
            Self::ExportWatermark | Self::ExportFooter | Self::ExportSnippetCode => Section::Export,
            Self::ToolbarIconSize
            | Self::StartupMasterPassword
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
            Self::EditorCursorStyle | Self::EditorCursorColor => Section::Editor,
            Self::SnippetFontFamily
            | Self::SnippetFontSize
            | Self::SnippetForeground
            | Self::SnippetBackground
            | Self::SnippetCursorStyle
            | Self::SnippetCursorColor => Section::SnippetEditor,
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
            Self::PasteLineDelay => "clipboard paste line delay pace pacing milliseconds",
            Self::ClipboardWrite => "paste write",
            Self::ClipboardRead => "paste access consent",
            Self::CommandMarks => "prompt navigation jumps setup bash zsh fish",
            Self::CommandTimestamps => "terminal time gutter command date",
            Self::RemoteSidebar => "sftp ssh folder dock left right hidden",
            Self::SftpParallel => {
                "sftp simultaneous parallel uploads downloads transfer queue channels"
            }
            Self::SftpResume => "sftp resume interrupted partial cancel upload download",
            Self::SftpConflictDefault => {
                "sftp default conflict ask skip overwrite replace existing files"
            }
            Self::SftpArchive => "sftp archive remote destination path zip compression level",
            Self::SftpIdle => "sftp idle auto close inactive timeout minutes",
            Self::InitialTerminalSize => {
                "initial terminal grid dimensions columns rows pty shell ssh"
            }
            Self::SerialDefaultEncoding => {
                "serial device port character set charset utf-8 latin1 latin9 iso-8859 windows-1252"
            }
            Self::TerminalLinkDetection => "links clickable open URL email hyperlink OSC 8",
            Self::TerminalCopyOnSelect => {
                "copy on select automatic mouse drag terminal text clipboard PRIMARY"
            }
            Self::QuickSelectAlphabet => "links quick select labels alphabet shortcut keys",
            Self::QuickSelectPatterns => {
                "links quick select custom match regex regular expressions"
            }
            Self::Highlighting => {
                "highlight rule set default full screen alternate screen trigger notifications snippets"
            }
            Self::TerminalScrollbar => "scroll back scrollback history vertical right",
            Self::ScrollbackLines => "scrollback history maximum lines rows",
            Self::ScrollbackMemory => "scrollback history memory bytes MiB",
            Self::ScrollbackCompression => "scrollback history memory compress idle background",
            Self::SshKeepAlive => "ssh keep-alive heartbeat idle connection network",
            Self::SshKeepAliveInterval => "ssh keep-alive heartbeat seconds interval",
            Self::CodingAgentDetection => {
                "coding agents Claude Code Codex Gemini local status monitor tab badge"
            }
            Self::CodingAgentAppBadge => {
                "coding agents blocked count application badge window title taskbar Dock"
            }
            Self::CodingAgentNotifications => {
                "coding agents desktop notifications alerts waiting blocked finished completion"
            }
            Self::TerminalNotifications => {
                "desktop notifications bell BEL OSC 9 777 programs long commands duration AI approval"
            }
            Self::CodingAgentDockAutomatic => {
                "coding agents sidebar dock panel automatic visibility"
            }
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
            Self::ExportWatermark => {
                "pdf html snippet analysis branding text color mark document page"
            }
            Self::ExportFooter => "pdf markdown html branding text document page",
            Self::ExportSnippetCode => {
                "snippet analysis source script code report appendix pdf html markdown"
            }
            Self::ToolbarIconSize => "symbol size toolbar button pixels appearance",
            Self::StartupMasterPassword => "vault unlock startup login authentication security",
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
            Self::EditorCursorStyle => "file editor caret shape block line underscore",
            Self::EditorCursorColor => "file editor caret colour color hex rgb",
            Self::SnippetFontFamily => "monospace typeface typography font face",
            Self::SnippetFontSize => "zoom typography text size",
            Self::SnippetForeground => "text color hex rgb",
            Self::SnippetBackground => "editor background color hex rgb",
            Self::SnippetCursorStyle => "caret shape block line underscore",
            Self::SnippetCursorColor => "caret colour color hex rgb",
            Self::Shortcuts => "key bindings accelerator record commands keymap",
            Self::AgentDesign => "AI agent presentation activity status floating classic standard",
            Self::ApplicationUpdates => {
                "software application release background GitHub stable check"
            }
            Self::ApplicationUpdateInterval => {
                "update schedule frequency every days automatic background"
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id),
    OpenAt(window::Id, Section),
    Focus(window::Id),
    Section(window::Id, Section),
    Guide(window::Id),
    Search(window::Id, String),
    SearchStep(window::Id, isize),
    SearchChoose(window::Id),
    SearchClear(window::Id),
    Escape(window::Id),
    Reveal(window::Id, Setting),
    FontFamily(window::Id, String),
    FontSize(window::Id, u8),
    FontSizeInput(window::Id, String),
    Color(window::Id, color_settings::Edit),
    EditorCaret(window::Id, editor_settings::Edit),
    SnippetAppearance(window::Id, snippet_editor_settings::Edit),
    Theme(window::Id, theme_settings::Edit),
    Design(window::Id, Design),
    DesignAnimations(window::Id, bool),
    AgentDesignCatalog(window::Id, agent_design_settings::Edit),
    ApplicationUpdates(window::Id, bool),
    ApplicationUpdateInterval(window::Id, u8),
    PasteWarning(window::Id, WarningMode),
    LargePasteWarning(window::Id, u8),
    PasteLineDelay(window::Id, u16),
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
    SftpParallel(window::Id, u8),
    SftpResume(window::Id, bool),
    SftpKeepPartial(window::Id, bool),
    SftpXml(window::Id, sftp_settings::java::Action),
    SftpConflictDefault(window::Id, ConflictDefault),
    SftpArchiveDirectory(window::Id, String),
    SftpArchiveLevel(window::Id, u8),
    SftpIdleEnabled(window::Id, bool),
    SftpIdleMinutes(window::Id, u16),
    TerminalColumns(window::Id, String),
    TerminalRows(window::Id, String),
    SerialDefaultEncoding(window::Id, TextEncoding),
    TerminalLinkDetection(window::Id, bool),
    TerminalCopyOnSelect(window::Id, bool),
    QuickSelect(window::Id, quick_select_settings::Edit),
    TerminalScrollbar(window::Id, bool),
    ScrollbackCompression(window::Id, bool),
    ScrollbackLines(window::Id, u32),
    ScrollbackMemory(window::Id, u8),
    SshKeepAlive(window::Id, bool),
    SshKeepAliveInterval(window::Id, u16),
    CodingAgentDetection(window::Id, bool),
    CodingAgentAppBadge(window::Id, bool),
    CodingAgentNotifications(window::Id, bool),
    TerminalNotifications(window::Id, super::terminal_notifications::Edit),
    CodingAgentDockAutomatic(window::Id, bool),
    BoldTextMode(window::Id, BoldTextMode),
    SilenceSeconds(window::Id, String),
    StepSilenceSeconds(window::Id, bool),
    ClipboardWrite(window::Id, bool),
    ClipboardRead(window::Id, bool),
    Restore(window::Id, RestoreMode),
    RestoreOutput(window::Id, bool),
    RestoreOutputLines(window::Id, u16),
    KeyBinding(window::Id, Edit),
    Backup(window::Id, backup::Edit),
    Logging(window::Id, logging_settings::Edit),
    Export(window::Id, export_settings::Edit),
    Highlighting(window::Id, highlight_settings::Edit),
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
        if edit.ai_agent_design != before.ai_agent_design {
            current.ai_agent_design = edit.ai_agent_design;
        }
        if edit.ai_agent_designs != before.ai_agent_designs {
            current.ai_agent_designs = edit.ai_agent_designs.clone();
        }
        if policy.problem.is_none() && policy.updates_allowed {
            if edit.application_updates.enabled != before.application_updates.enabled {
                current.application_updates.enabled = edit.application_updates.enabled;
            }
            if edit.application_updates.interval_days != before.application_updates.interval_days {
                current.application_updates.interval_days = edit.application_updates.interval_days;
            }
        }
        if edit.editor_caret != before.editor_caret {
            current.editor_caret = edit.editor_caret;
        }
        if policy.problem.is_none() && policy.session_restore_output.is_none() {
            if edit.restore_output.enabled != before.restore_output.enabled {
                current.restore_output.enabled = edit.restore_output.enabled;
            }
            if edit.restore_output.lines != before.restore_output.lines
                && session_restore::OutputPreferences::LINES.contains(&edit.restore_output.lines)
            {
                current.restore_output.lines = edit.restore_output.lines;
            }
        }
        if edit.snippet_appearance != before.snippet_appearance {
            current
                .snippet_appearance
                .clone_from(&edit.snippet_appearance);
        }
        if edit.export_branding != before.export_branding {
            current.export_branding = edit.export_branding.clone();
        }
        if edit.snippet_analysis_export_include_code != before.snippet_analysis_export_include_code
        {
            current.snippet_analysis_export_include_code =
                edit.snippet_analysis_export_include_code;
        }
        if edit.startup != before.startup {
            if policy.problem.is_none()
                && !policy.require_master_password
                && edit.startup.require_master_password != before.startup.require_master_password
            {
                current.startup.require_master_password = edit.startup.require_master_password;
            }
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
        if edit.paste_line_delay_ms != before.paste_line_delay_ms {
            current.paste_line_delay_ms = edit.paste_line_delay_ms;
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
        if policy.problem.is_none() && policy.file_transfer_allowed {
            let cap = crate::sftp_window::parallel::managed_cap(policy.sftp_max_parallel);
            if cap > 1 && edit.sftp_parallel_transfers != before.sftp_parallel_transfers {
                current.sftp_parallel_transfers = edit.sftp_parallel_transfers.clamp(1, cap);
            }
            if edit.sftp_resume_partial_transfers != before.sftp_resume_partial_transfers {
                current.sftp_resume_partial_transfers = edit.sftp_resume_partial_transfers;
            }
            if edit.sftp_keep_partial_on_cancel != before.sftp_keep_partial_on_cancel {
                current.sftp_keep_partial_on_cancel = edit.sftp_keep_partial_on_cancel;
            }
        }
        if SftpConflictPolicy::from_policy(policy).editable()
            && edit.sftp_conflict_default != before.sftp_conflict_default
        {
            current.sftp_conflict_default = edit.sftp_conflict_default;
        }
        if edit.sftp_idle_minutes != before.sftp_idle_minutes {
            current.sftp_idle_minutes = edit.sftp_idle_minutes;
        }
        if edit.sftp_archive.remote_directory != before.sftp_archive.remote_directory {
            current.sftp_archive.remote_directory = edit.sftp_archive.remote_directory.clone();
        }
        if edit.sftp_archive.level != before.sftp_archive.level {
            current.sftp_archive.level = edit.sftp_archive.level;
        }
        if edit.terminal_link_detection_enabled != before.terminal_link_detection_enabled {
            current.terminal_link_detection_enabled = edit.terminal_link_detection_enabled;
        }
        if edit.serial_default_encoding != before.serial_default_encoding {
            current.serial_default_encoding = edit.serial_default_encoding;
        }
        if edit.terminal_copy_on_select_enabled != before.terminal_copy_on_select_enabled {
            current.terminal_copy_on_select_enabled = edit.terminal_copy_on_select_enabled;
        }
        if edit.show_terminal_scrollbar != before.show_terminal_scrollbar {
            current.show_terminal_scrollbar = edit.show_terminal_scrollbar;
        }
        if edit.terminal_columns != before.terminal_columns {
            current.terminal_columns = edit.terminal_columns;
        }
        if edit.terminal_rows != before.terminal_rows {
            current.terminal_rows = edit.terminal_rows;
        }
        if edit.scrollback_compression_enabled != before.scrollback_compression_enabled {
            current.scrollback_compression_enabled = edit.scrollback_compression_enabled;
        }
        if edit.scrollback_lines != before.scrollback_lines {
            current.scrollback_lines = edit.scrollback_lines;
        }
        if edit.scrollback_memory_mib != before.scrollback_memory_mib {
            current.scrollback_memory_mib = edit.scrollback_memory_mib;
        }
        if edit.ssh_keepalive_enabled != before.ssh_keepalive_enabled {
            current.ssh_keepalive_enabled = edit.ssh_keepalive_enabled;
        }
        if edit.ssh_keepalive_interval != before.ssh_keepalive_interval {
            current.ssh_keepalive_interval = edit.ssh_keepalive_interval;
        }
        if edit.coding_agent_detection_enabled != before.coding_agent_detection_enabled {
            current.coding_agent_detection_enabled = edit.coding_agent_detection_enabled;
        }
        if edit.coding_agent_app_badge_enabled != before.coding_agent_app_badge_enabled {
            current.coding_agent_app_badge_enabled = edit.coding_agent_app_badge_enabled;
        }
        if policy.desktop_notifications_allowed
            && policy.problem.is_none()
            && edit.coding_agent_notifications_enabled != before.coding_agent_notifications_enabled
        {
            current.coding_agent_notifications_enabled = edit.coding_agent_notifications_enabled;
        }
        if edit.coding_agent_dock_automatic != before.coding_agent_dock_automatic {
            current.coding_agent_dock_automatic = edit.coding_agent_dock_automatic;
        }
        if edit.terminal_bold_text_mode != before.terminal_bold_text_mode {
            current.terminal_bold_text_mode = edit.terminal_bold_text_mode;
        }
        if edit.terminal_silence_seconds != before.terminal_silence_seconds {
            current.terminal_silence_seconds = edit.terminal_silence_seconds;
        }
        if policy.desktop_notifications_allowed
            && policy.problem.is_none()
            && edit.terminal_notifications != before.terminal_notifications
        {
            current.terminal_notifications = edit.terminal_notifications.clone();
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
    installed_sftp_conflict_policy: SftpConflictPolicy,
    sftp_xml: sftp_settings::java::Exchange,
    initial_highlighting: highlight::Preferences,
    highlighting: highlight::Preferences,
    section: Section,
    search_input: widget::Id,
    search_scroll: widget::Id,
    tab_scroll: widget::Id,
    page_scroll: widget::Id,
    revealed_row: widget::Id,
    revealed_input: widget::Id,
    search: String,
    search_selection: usize,
    font_size_input: Option<String>,
    editor_cursor_color_input: Option<String>,
    silence_seconds_input: Option<String>,
    terminal_columns_input: Option<String>,
    terminal_rows_input: Option<String>,
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
    snippet_appearance: snippet_editor_settings::Editor,
    themes: theme_settings::Editor,
    agent_design: agent_design_settings::Editor,
    backup: backup::Editor,
    logging: logging_settings::Editor,
    export: export_settings::Editor,
    quick_select: quick_select_settings::Editor,
    video: video_settings::Editor,
}

impl Dialog {
    fn saving(&self) -> bool {
        self.saving_toolbar_icon_size
            || self.saving_logging
            || self.saving_video
            || self.sftp_xml.busy()
            || self.themes.exchanging().is_some()
    }

    fn search_results(&self, has_transparency: bool) -> impl Iterator<Item = Setting> {
        search_matches(&self.search).filter(move |setting| match setting {
            Setting::Restore => self.draft.restore.is_some(),
            Setting::Transparency => has_transparency,
            _ => true,
        })
    }

    fn select_section(&mut self, section: Section) {
        self.section = section;
        if section == Section::Sftp {
            self.installed_sftp_conflict_policy = SftpConflictPolicy::load();
        }
        if section != Section::Backup {
            self.backup.close_browser();
        }
        if section != Section::Logging {
            self.logging.close_browser();
        }
        if section != Section::Themes {
            self.themes.leave();
        }
        self.revealed = None;
        self.keyboard.stop_recording();
    }

    fn reveal(&mut self, setting: Setting) -> Task<Desktop> {
        self.select_section(setting.section());
        self.revealed = Some(setting);
        self.search.clear();
        if setting == Setting::JournalAutomaticVision {
            self.logging.close_browser();
        }
        let focus = match setting {
            Setting::QuickSelectAlphabet | Setting::QuickSelectPatterns => {
                self.quick_select.focus(setting)
            }
            _ => Task::none(),
        };
        let scroll =
            search_input::scroll_to_row(self.page_scroll.clone(), self.revealed_row.clone());
        Task::batch([self.reveal_tab(), scroll.chain(focus)])
    }

    fn reveal_result(&mut self, setting: Setting) -> Task<Desktop> {
        self.reveal(setting)
            .chain(crate::settings_search::focus_if_present(
                self.revealed_input.clone(),
            ))
    }

    fn input_target(&self, setting: Setting, editable: bool) -> Option<&widget::Id> {
        (editable && self.revealed == Some(setting)).then_some(&self.revealed_input)
    }

    fn reveal_input<'a>(
        &self,
        setting: Setting,
        editable: bool,
        input: widget::TextInput<'a, Desktop>,
    ) -> widget::TextInput<'a, Desktop> {
        crate::settings_search::mark_input(input, self.input_target(setting, editable))
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
        if self.terminal_columns_input.is_some() || self.terminal_rows_input.is_some() {
            self.problem = None;
            return Some(self.reveal(Setting::InitialTerminalSize));
        }
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
        if self.editor_cursor_color_input.is_some() {
            self.problem = None;
            return Some(self.reveal(Setting::EditorCursorColor));
        }
        if let Some(setting) = self.snippet_appearance.first_problem() {
            self.problem = None;
            return Some(self.reveal(setting));
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
        let dialog = self.settings.0.as_ref().filter(|dialog| dialog.id == id)?;
        if dialog.saving() {
            return Some(Task::none());
        }
        if let Some(dialog) = self.openpgp_for_source(id) {
            return Some(window::gain_focus(dialog));
        }
        if let Some(dialog) = self.highlight_rules_for_settings(id) {
            return Some(window::gain_focus(dialog));
        }
        let owner = dialog.owner;
        self.settings.0 = None;
        let close = self.close_placed_dialog_and_focus(id, owner);
        Some(self.close_shell_integration_owner_then(id, close))
    }

    pub(super) fn focus_settings_search(&self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.settings.0.as_ref().filter(|dialog| dialog.id == id)?;
        Some(
            widget::operation::focus(dialog.search_input.clone())
                .chain(widget::operation::select_all(dialog.search_input.clone())),
        )
    }

    pub(super) fn update_settings(&mut self, message: Message) -> Task<Desktop> {
        if !matches!(
            &message,
            Message::Focus(_)
                | Message::ToolbarIconSizeSaved(..)
                | Message::LoggingPrepared(..)
                | Message::VideoSaved(..)
                | Message::Theme(
                    _,
                    theme_settings::Edit::Java(theme_settings::exchange::Action::Finished(..))
                )
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
            Message::Focus(id) => {
                if let Some(dialog) = self
                    .settings
                    .dialog_mut(id)
                    .filter(|dialog| dialog.section == Section::Sftp)
                {
                    dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                }
                return self.settings.title(id).map_or_else(Task::none, |_| {
                    window::gain_focus(
                        self.openpgp_for_source(id)
                            .or_else(|| self.highlight_rules_for_settings(id))
                            .unwrap_or(id),
                    )
                });
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
            Message::Escape(id) => {
                return self.dismiss_settings(id);
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
            Message::Highlighting(id, edit) => return self.update_highlight_settings(id, edit),
            Message::LoggingPrepared(id, revision, settings, result) => {
                return self.logging_settings_prepared(id, revision, settings, result);
            }
            message @ (Message::Section(id, _)
            | Message::Search(id, _)
            | Message::SearchStep(id, _)
            | Message::SearchChoose(id)
            | Message::SearchClear(id)
            | Message::Reveal(id, _)
            | Message::FontFamily(id, _)
            | Message::FontSize(id, _)
            | Message::FontSizeInput(id, _)
            | Message::Color(id, _)
            | Message::SnippetAppearance(id, _)
            | Message::EditorCaret(id, _)
            | Message::Theme(id, _)
            | Message::Export(id, _)
            | Message::Design(id, _)
            | Message::DesignAnimations(id, _)
            | Message::AgentDesignCatalog(id, _)
            | Message::ApplicationUpdates(id, _)
            | Message::ApplicationUpdateInterval(id, _)
            | Message::PasteWarning(id, _)
            | Message::LargePasteWarning(id, _)
            | Message::PasteLineDelay(id, _)
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
            | Message::SftpParallel(id, _)
            | Message::SftpResume(id, _)
            | Message::SftpKeepPartial(id, _)
            | Message::SftpXml(id, _)
            | Message::SftpConflictDefault(id, _)
            | Message::SftpArchiveDirectory(id, _)
            | Message::SftpArchiveLevel(id, _)
            | Message::SftpIdleEnabled(id, _)
            | Message::SftpIdleMinutes(id, _)
            | Message::TerminalColumns(id, _)
            | Message::TerminalRows(id, _)
            | Message::SerialDefaultEncoding(id, _)
            | Message::TerminalLinkDetection(id, _)
            | Message::TerminalCopyOnSelect(id, _)
            | Message::QuickSelect(id, _)
            | Message::TerminalScrollbar(id, _)
            | Message::ScrollbackLines(id, _)
            | Message::ScrollbackMemory(id, _)
            | Message::ScrollbackCompression(id, _)
            | Message::SshKeepAlive(id, _)
            | Message::SshKeepAliveInterval(id, _)
            | Message::CodingAgentDetection(id, _)
            | Message::CodingAgentAppBadge(id, _)
            | Message::CodingAgentNotifications(id, _)
            | Message::TerminalNotifications(id, _)
            | Message::CodingAgentDockAutomatic(id, _)
            | Message::BoldTextMode(id, _)
            | Message::SilenceSeconds(id, _)
            | Message::StepSilenceSeconds(id, _)
            | Message::ClipboardWrite(id, _)
            | Message::ClipboardRead(id, _)
            | Message::Restore(id, _)
            | Message::RestoreOutput(id, _)
            | Message::RestoreOutputLines(id, _)
            | Message::KeyBinding(id, _)) => (id, message),
        };
        let has_transparency =
            matches!(message, Message::SearchStep(..) | Message::SearchChoose(_))
                && self
                    .settings
                    .0
                    .as_ref()
                    .filter(|dialog| dialog.id == id)
                    .and_then(|dialog| self.windows.get(&dialog.owner))
                    .and_then(|window| self.transparency_controls(window))
                    .is_some();
        if let Some(child) = self.highlight_rules_for_settings(id) {
            return window::gain_focus(child);
        }
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
                dialog.search_selection = 0;
                dialog.revealed = None;
                dialog.keyboard.stop_recording();
                return widget::operation::snap_to(
                    dialog.search_scroll.clone(),
                    widget::operation::RelativeOffset::START,
                );
            }
            Message::SearchStep(_, direction) => {
                let last = dialog
                    .search_results(has_transparency)
                    .count()
                    .saturating_sub(1);
                dialog.search_selection = dialog
                    .search_selection
                    .saturating_add_signed(direction)
                    .min(last);
                return widget::operation::snap_to(
                    dialog.search_scroll.clone(),
                    widget::operation::RelativeOffset {
                        x: None,
                        y: Some(dialog.search_selection as f32 / last.max(1) as f32),
                    },
                );
            }
            Message::SearchChoose(_) => {
                let choice = dialog
                    .search_results(has_transparency)
                    .nth(dialog.search_selection);
                return choice.map_or_else(Task::none, |setting| dialog.reveal_result(setting));
            }
            Message::SearchClear(_) => {
                dialog.search.clear();
                dialog.search_selection = 0;
                dialog.revealed = None;
                return widget::operation::focus(dialog.search_input.clone());
            }
            Message::Reveal(_, setting) => return dialog.reveal_result(setting),
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
                let task = dialog
                    .themes
                    .update(&mut dialog.draft.preferences.themes, edit);
                if dialog.draft.preferences.themes.selected() != selected {
                    dialog.colors.profile_changed();
                }
                dialog.problem = None;
                return task.map(move |edit| Desktop::Settings(Message::Theme(id, edit)));
            }
            Message::Export(_, edit) if dialog.section == Section::Export => {
                dialog.export.update(&mut dialog.draft.preferences, edit);
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
            Message::EditorCaret(_, edit) if dialog.section == Section::Editor => {
                editor_settings::update(dialog, edit);
                dialog.problem = None;
            }
            Message::SnippetAppearance(_, edit) if dialog.section == Section::SnippetEditor => {
                dialog
                    .snippet_appearance
                    .update(&mut dialog.draft.preferences.snippet_appearance, edit);
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
            Message::PasteLineDelay(_, value) => {
                dialog.draft.preferences.paste_line_delay_ms =
                    value.min(crate::connection_store::MAX_PASTE_LINE_DELAY_MS);
            }
            Message::ToolbarIconSize(_, size) if dialog.section == Section::Window => {
                let size = size.clamp(*TOOLBAR_ICON_SIZE.start(), *TOOLBAR_ICON_SIZE.end());
                dialog.toolbar_icon_size =
                    (size != self.chrome.toolbar_icon_size()).then_some(size);
                dialog.problem = None;
            }
            Message::Startup(_, edit) if dialog.section == Section::Window => {
                if matches!(edit, startup_settings::Edit::MasterPassword(_))
                    && (self.clipboard.policy.problem.is_some()
                        || self.clipboard.policy.require_master_password)
                {
                    return Task::none();
                }
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
            Message::SftpParallel(_, parallel) if dialog.section == Section::Sftp => {
                dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                let policy = dialog
                    .installed_sftp_conflict_policy
                    .restrict(&self.clipboard.policy);
                if policy.transfer_allowed
                    && policy.parallel_cap > 1
                    && (1..=policy.parallel_cap).contains(&parallel)
                {
                    dialog.draft.preferences.sftp_parallel_transfers = parallel;
                }
            }
            Message::SftpResume(_, enabled) if dialog.section == Section::Sftp => {
                dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                if dialog
                    .installed_sftp_conflict_policy
                    .restrict(&self.clipboard.policy)
                    .transfer_allowed
                {
                    dialog.draft.preferences.sftp_resume_partial_transfers = enabled;
                }
            }
            Message::SftpKeepPartial(_, enabled) if dialog.section == Section::Sftp => {
                dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                if dialog.draft.preferences.sftp_resume_partial_transfers
                    && dialog
                        .installed_sftp_conflict_policy
                        .restrict(&self.clipboard.policy)
                        .transfer_allowed
                {
                    dialog.draft.preferences.sftp_keep_partial_on_cancel = enabled;
                }
            }
            Message::SftpXml(_, action) => {
                dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                let policy = dialog
                    .installed_sftp_conflict_policy
                    .restrict(&self.clipboard.policy);
                let (imported, task) = dialog.sftp_xml.update(
                    action,
                    (&self.window_settings).into(),
                    dialog.section == Section::Sftp && policy.transfer_allowed,
                );
                if let Some(imported) = imported {
                    let preferences = &mut dialog.draft.preferences;
                    if policy.parallel_cap > 1 {
                        preferences.sftp_parallel_transfers =
                            imported.parallel.min(policy.parallel_cap);
                    }
                    preferences.sftp_resume_partial_transfers = imported.resume;
                    preferences.sftp_keep_partial_on_cancel = imported.keep;
                    if let Some(directory) = imported.archive_directory {
                        preferences.sftp_archive.remote_directory = directory;
                    }
                    if let Some(level) = imported.archive_level {
                        preferences.sftp_archive.level = level;
                    }
                }
                return task.map(move |action| Desktop::Settings(Message::SftpXml(id, action)));
            }
            Message::SftpConflictDefault(_, preference) if dialog.section == Section::Sftp => {
                dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
                if dialog
                    .installed_sftp_conflict_policy
                    .restrict(&self.clipboard.policy)
                    .editable()
                {
                    dialog.draft.preferences.sftp_conflict_default = preference;
                }
            }
            Message::SftpIdleEnabled(_, enabled) if dialog.section == Section::Sftp => {
                dialog.draft.preferences.sftp_idle_minutes =
                    enabled.then_some(dialog.draft.preferences.sftp_idle_minutes.unwrap_or(10));
            }
            Message::SftpArchiveDirectory(_, path)
                if dialog.section == Section::Sftp && path.len() <= 4096 =>
            {
                dialog.draft.preferences.sftp_archive.remote_directory = path;
            }
            Message::SftpArchiveLevel(_, level)
                if dialog.section == Section::Sftp && level <= 9 =>
            {
                dialog.draft.preferences.sftp_archive.level = level;
            }
            Message::SftpIdleMinutes(_, minutes)
                if dialog.section == Section::Sftp
                    && dialog.draft.preferences.sftp_idle_minutes.is_some()
                    && (1..=profile::RemoteProjectTool::MAX_IDLE_MINUTES).contains(&minutes) =>
            {
                dialog.draft.preferences.sftp_idle_minutes = Some(minutes);
            }
            Message::TerminalColumns(_, value) if dialog.section == Section::Terminal => {
                update_grid_dimension(
                    &mut dialog.draft.preferences.terminal_columns,
                    &mut dialog.terminal_columns_input,
                    value,
                    MIN_TERMINAL_COLUMNS..=MAX_TERMINAL_COLUMNS,
                );
            }
            Message::TerminalRows(_, value) if dialog.section == Section::Terminal => {
                update_grid_dimension(
                    &mut dialog.draft.preferences.terminal_rows,
                    &mut dialog.terminal_rows_input,
                    value,
                    MIN_TERMINAL_ROWS..=MAX_TERMINAL_ROWS,
                );
            }
            Message::SerialDefaultEncoding(_, encoding) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.serial_default_encoding = encoding;
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
            Message::ScrollbackLines(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.scrollback_lines =
                    value.clamp(ScrollbackConfig::MIN_LINES, ScrollbackConfig::MAX_LINES);
            }
            Message::ScrollbackMemory(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.scrollback_memory_mib = value.clamp(
                    ScrollbackConfig::MIN_MEMORY_MIB,
                    ScrollbackConfig::MAX_MEMORY_MIB,
                );
            }
            Message::SshKeepAlive(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.ssh_keepalive_enabled = value;
            }
            Message::SshKeepAliveInterval(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.ssh_keepalive_interval =
                    value.clamp(KeepAlive::MIN_SECONDS, KeepAlive::MAX_SECONDS);
            }
            Message::CodingAgentDetection(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.coding_agent_detection_enabled = value;
            }
            Message::CodingAgentAppBadge(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.coding_agent_app_badge_enabled = value;
            }
            Message::CodingAgentNotifications(_, value)
                if dialog.section == Section::Messaging
                    && self.clipboard.policy.desktop_notifications_allowed
                    && self.clipboard.policy.problem.is_none() =>
            {
                dialog.draft.preferences.coding_agent_notifications_enabled = value;
            }
            Message::TerminalNotifications(_, edit)
                if dialog.section == Section::Messaging
                    && self.clipboard.policy.desktop_notifications_allowed
                    && self.clipboard.policy.problem.is_none()
                    && (dialog.draft.preferences.prompt_navigation_enabled
                        || !matches!(
                            edit,
                            super::terminal_notifications::Edit::Seconds(_)
                                | super::terminal_notifications::Edit::Commands(_)
                        )) =>
            {
                dialog.draft.preferences.terminal_notifications.edit(edit);
            }
            Message::CodingAgentDockAutomatic(_, value) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.coding_agent_dock_automatic = value;
            }
            Message::BoldTextMode(_, mode) if dialog.section == Section::Terminal => {
                dialog.draft.preferences.terminal_bold_text_mode = mode;
            }
            Message::AgentDesignCatalog(_, edit) if dialog.section == Section::AgentDesign => {
                let preferences = &mut dialog.draft.preferences;
                dialog.agent_design.update(
                    &mut preferences.ai_agent_designs,
                    &mut preferences.ai_agent_design,
                    edit,
                );
            }
            Message::ApplicationUpdates(_, enabled)
                if dialog.section == Section::Updates
                    && self.clipboard.policy.problem.is_none()
                    && self.clipboard.policy.updates_allowed =>
            {
                dialog.draft.preferences.application_updates.enabled = enabled;
            }
            Message::ApplicationUpdateInterval(_, days)
                if dialog.section == Section::Updates
                    && self.clipboard.policy.problem.is_none()
                    && self.clipboard.policy.updates_allowed
                    && dialog.draft.preferences.application_updates.enabled
                    && crate::application_update::Preferences::INTERVAL_DAYS.contains(&days) =>
            {
                dialog.draft.preferences.application_updates.interval_days = days;
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
            Message::Restore(_, mode)
                if dialog.draft.restore.is_some()
                    && self.clipboard.policy.session_restore_mode.is_none() =>
            {
                dialog.draft.restore = Some(mode)
            }
            Message::RestoreOutput(_, enabled)
                if dialog.section == Section::Window
                    && self.clipboard.policy.problem.is_none()
                    && self.clipboard.policy.session_restore_output.is_none() =>
            {
                dialog.draft.preferences.restore_output.enabled = enabled;
            }
            Message::RestoreOutputLines(_, lines)
                if dialog.section == Section::Window
                    && dialog.draft.preferences.restore_output.enabled
                    && self.clipboard.policy.problem.is_none()
                    && self.clipboard.policy.session_restore_output.is_none()
                    && session_restore::OutputPreferences::LINES.contains(&lines) =>
            {
                dialog.draft.preferences.restore_output.lines = lines;
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
        if let Some((_, focus)) = self.owner_dialog(owner)
            && !matches!(focus, Desktop::Settings(Message::Focus(_)))
        {
            return self.update(focus);
        }
        if let Some(dialog) = &mut self.settings.0 {
            let id = dialog.id;
            let reveal = section.map_or_else(Task::none, |section| {
                dialog.select_section(section);
                dialog.search.clear();
                dialog.reveal_page_start()
            });
            return self.update_settings(Message::Focus(id)).chain(reveal);
        }
        let draft = Draft {
            preferences: self.window_settings.clone(),
            restore: self.session_restore.preference(),
            backup: self.backup_preferences.clone(),
        };
        let highlighting = self.highlight.preferences();
        let quick_select = quick_select_settings::Editor::new(&draft.preferences.quick_select);
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::GlobalSettings,
            window::Settings {
                size: Size::new(1120.0, 960.0),
                min_size: Some(Size::new(620.0, 450.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.settings.0 = Some(Dialog {
            id,
            owner,
            initial: draft.clone(),
            draft,
            installed_sftp_conflict_policy: SftpConflictPolicy::load(),
            sftp_xml: sftp_settings::java::Exchange::default(),
            initial_highlighting: highlighting.clone(),
            highlighting,
            section: section.unwrap_or(Section::Font),
            search_input: widget::Id::unique(),
            search_scroll: widget::Id::unique(),
            tab_scroll: widget::Id::unique(),
            page_scroll: widget::Id::unique(),
            revealed_row: widget::Id::unique(),
            revealed_input: widget::Id::unique(),
            search: String::new(),
            search_selection: 0,
            font_size_input: None,
            editor_cursor_color_input: None,
            silence_seconds_input: None,
            terminal_columns_input: None,
            terminal_rows_input: None,
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
            snippet_appearance: snippet_editor_settings::Editor::default(),
            themes: theme_settings::Editor::default(),
            agent_design: agent_design_settings::Editor::default(),
            backup: backup::Editor::default(),
            logging: logging_settings::Editor::default(),
            export: export_settings::Editor::default(),
            quick_select,
            video: video_settings::Editor::new(self.video.preferences()),
        });
        opened.chain(
            self.settings
                .0
                .as_ref()
                .map_or_else(Task::none, Dialog::reveal_tab),
        )
    }

    pub(super) fn open_journal_options(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some((_, focus)) = self.owner_dialog(owner)
            && !matches!(focus, Desktop::Settings(Message::Focus(_)))
        {
            return self.update(focus);
        }
        let open = self.update_settings(Message::OpenAt(owner, Section::Logging));
        if let Some(dialog) = self.settings.0.as_mut().filter(|dialog| !dialog.saving()) {
            return open.chain(dialog.reveal(Setting::JournalAutomaticVision));
        }
        open
    }

    fn save_settings(&mut self, id: window::Id) -> Task<Desktop> {
        if let Some(dialog) = self.openpgp_for_source(id) {
            return window::gain_focus(dialog);
        }
        if let Some(dialog) = self.highlight_rules_for_settings(id) {
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
        let task = self.apply_settings_to(id, save, &mut prepared, profile::root);
        self.abort_app_logging(prepared);
        task
    }

    fn apply_settings_to(
        &mut self,
        id: window::Id,
        save: impl FnOnce(&WindowSettings) -> std::io::Result<()>,
        prepared: &mut Option<crate::app_logging::Candidate>,
        highlight_root: impl FnOnce() -> std::io::Result<std::path::PathBuf>,
    ) -> Task<Desktop> {
        if let Some(dialog) = self.openpgp_for_source(id) {
            return window::gain_focus(dialog);
        }
        let recording_allowed = self.recording_allowed();
        let manager_profile_ready = self.manager_journal_profile_ready();
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        dialog.installed_sftp_conflict_policy = SftpConflictPolicy::load();
        let sftp_conflict_policy = dialog
            .installed_sftp_conflict_policy
            .restrict(&self.clipboard.policy);
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
        let initial_highlighting = dialog.initial_highlighting.clone();
        let edited_highlighting = dialog.highlighting.clone();
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
        let restore = draft.restore.filter(|mode| {
            self.clipboard.policy.session_restore_mode.is_none() && Some(*mode) != initial.restore
        });
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
        if !next.sftp_archive.valid() {
            if let Some(dialog) = self.settings.dialog_mut(id) {
                dialog.select_section(Section::Sftp);
                dialog.search.clear();
                dialog.revealed = Some(Setting::SftpArchive);
                dialog.problem =
                    Some("Enter an absolute SFTP archive directory or leave it blank.".into());
            }
            return Task::none();
        }
        if !sftp_conflict_policy.editable() {
            next.sftp_conflict_default = self.window_settings.sftp_conflict_default;
        }
        if !sftp_conflict_policy.transfer_allowed || sftp_conflict_policy.parallel_cap == 1 {
            next.sftp_parallel_transfers = self.window_settings.sftp_parallel_transfers;
        } else if next.sftp_parallel_transfers != self.window_settings.sftp_parallel_transfers {
            next.sftp_parallel_transfers = next
                .sftp_parallel_transfers
                .min(sftp_conflict_policy.parallel_cap);
        }
        if !sftp_conflict_policy.transfer_allowed {
            next.sftp_resume_partial_transfers = self.window_settings.sftp_resume_partial_transfers;
            next.sftp_keep_partial_on_cancel = self.window_settings.sftp_keep_partial_on_cancel;
        }
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
        let backgrounds_changed = !next
            .themes
            .background_assets()
            .collect::<std::collections::BTreeSet<_>>()
            .eq(&self.window_settings.themes.background_assets().collect());
        let appearance = next.terminal_appearance();
        let appearance_changed = appearance != self.window_settings.terminal_appearance()
            || next.themes != self.window_settings.themes;
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
        let mut geometry = Task::none();
        if preferences_changed {
            let saved = self
                .settings
                .0
                .as_ref()
                .filter(|dialog| dialog.id == id)
                .map_or(Ok(()), |dialog| {
                    if dialog.themes.has_staged_backgrounds() {
                        profile::root()
                            .and_then(|root| dialog.themes.publish_backgrounds(&next.themes, &root))
                    } else {
                        Ok(())
                    }
                })
                .and_then(|()| save(&next));
            if let Err(error) = saved {
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
            let scrollback_changed =
                next.scrollback_config() != self.window_settings.scrollback_config();
            let detection_changed = next.coding_agent_detection_enabled
                != self.window_settings.coding_agent_detection_enabled;
            let dock_changed = next.coding_agent_dock_automatic
                != self.window_settings.coding_agent_dock_automatic;
            if detection_changed {
                for pane in self.sessions.values_mut() {
                    pane.session
                        .set_coding_agent_detection(next.coding_agent_detection_enabled);
                    pane.coding_agent.clear_detection();
                }
            }
            let journal_ai_changed = next.journal_ai != self.window_settings.journal_ai;
            geometry = self
                .dialog_placement
                .set_remembering(next.startup.remember_geometry);
            self.window_settings = next;
            if backgrounds_changed {
                self.refresh_terminal_backgrounds();
            }
            if scrollback_changed {
                self.sync_open_scrollback();
            }
            self.refresh_sftp_preferences();
            if detection_changed || dock_changed {
                self.reconcile_coding_agent_docks();
            }
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
        self.refresh_sftp_conflict_defaults(sftp_conflict_policy);
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
        match self.commit_highlight_preferences(
            &initial_highlighting,
            &edited_highlighting,
            highlight_root,
        ) {
            Ok(committed) => {
                if let Some(dialog) = self.settings.dialog_mut(id) {
                    dialog.initial_highlighting = committed.clone();
                    dialog.highlighting = committed;
                }
            }
            Err(error) => {
                if let Some(dialog) = self.settings.dialog_mut(id) {
                    dialog.select_section(Section::Terminal);
                    dialog.search.clear();
                    dialog.revealed = Some(Setting::Highlighting);
                    dialog.problem =
                        Some(format!("Could not save keyword highlighting ({error})."));
                }
                return Task::batch([restore, sidebar, journals, geometry]);
            }
        }
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
        Task::batch([restore, sidebar, journals, geometry, complete])
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
                    dialog
                        .themes
                        .exchanging()
                        .unwrap_or("Saving toolbar icon size…")
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
        let mut matches = dialog.search_results(transparency.is_some()).peekable();
        let input = row![
            search_input::view(
                text_input("Search settings…", &dialog.search)
                    .id(dialog.search_input.clone())
                    .on_input(move |query| msg(Message::Search(id, query)))
                    .on_submit(msg(Message::SearchChoose(id))),
                id,
            ),
            button("×")
                .style(button::secondary)
                .on_press_maybe((!dialog.search.is_empty()).then(|| msg(Message::SearchClear(id)))),
        ]
        .spacing(6);
        let search = container(container(input).max_width(SEARCH_MAX_WIDTH)).center_x(Length::Fill);
        let results = matches.peek().is_some().then(|| {
            let items = column(matches.enumerate().map(|(index, setting)| {
                button(
                    text(format!(
                        "{} · {}",
                        setting.label(),
                        setting.section().label()
                    ))
                    .wrapping(text::Wrapping::None),
                )
                .style(if dialog.search_selection == index {
                    button::primary
                } else {
                    button::secondary
                })
                .height(SEARCH_ROW_HEIGHT)
                .width(Length::Fill)
                .on_press(msg(Message::Reveal(id, setting)))
                .into()
            }))
            .spacing(4)
            .width(Length::Fill);
            let panel = container(
                scrollable(items)
                    .id(dialog.search_scroll.clone())
                    .height(Length::Shrink),
            )
            .padding(6)
            .width(Length::Fill)
            .max_width(SEARCH_MAX_WIDTH)
            .max_height(SEARCH_RESULTS_MAX_HEIGHT)
            .style(container::bordered_box);
            container(opaque(panel)).center_x(Length::Fill)
        });

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
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().spacing(4),
            ))
            .width(Length::Fill);
        let page = match dialog.section {
            Section::Font => font_settings_page(dialog),
            Section::Colors => highlighted(
                dialog,
                Setting::Colors,
                dialog
                    .colors
                    .view(
                        &dialog.draft.preferences,
                        dialog.input_target(Setting::Colors, true),
                    )
                    .map(move |edit| msg(Message::Color(id, edit))),
            ),
            Section::Themes => highlighted(
                dialog,
                Setting::Themes,
                dialog
                    .themes
                    .view(
                        &dialog.draft.preferences,
                        &self.terminal_backgrounds,
                        dialog.input_target(Setting::Themes, true),
                    )
                    .map(move |edit| msg(Message::Theme(id, edit))),
            ),
            Section::Appearance => highlighted(
                dialog,
                Setting::Appearance,
                appearance_settings::view(dialog.draft.preferences.appearance, id).map(msg),
            ),
            Section::Terminal => self.terminal_settings_page(dialog),
            Section::Sftp => self.sftp_settings_page(dialog),
            Section::Messaging => messaging_settings_page(
                dialog,
                self.clipboard.policy.desktop_notifications_allowed
                    && self.clipboard.policy.problem.is_none(),
            ),
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
            Section::Editor => editor_settings::view(dialog),
            Section::SnippetEditor => snippet_editor_settings::view(dialog),
            Section::AgentDesign => {
                let preferences = &dialog.draft.preferences;
                highlighted(
                    dialog,
                    Setting::AgentDesign,
                    dialog.agent_design.view(
                        &preferences.ai_agent_designs,
                        preferences.ai_agent_design,
                        id,
                        dialog.input_target(Setting::AgentDesign, true),
                    ),
                )
            }
            Section::Updates => {
                let preferences = dialog.draft.preferences.application_updates;
                let allowed = self.clipboard.policy.problem.is_none()
                    && self.clipboard.policy.updates_allowed;
                let interval: Element<'_, Desktop> = if allowed && preferences.enabled {
                    pick_list(
                        crate::application_update::Preferences::INTERVAL_DAYS.collect::<Vec<_>>(),
                        Some(preferences.interval_days),
                        move |days| msg(Message::ApplicationUpdateInterval(id, days)),
                    )
                    .into()
                } else {
                    text(preferences.interval_days.to_string()).into()
                };
                column![
                    highlighted(
                        dialog,
                        Setting::ApplicationUpdates,
                        checkbox(preferences.enabled)
                            .label(Setting::ApplicationUpdates.label())
                            .on_toggle_maybe(allowed.then_some(move |enabled| {
                                msg(Message::ApplicationUpdates(id, enabled))
                            })),
                    ),
                    highlighted(
                        dialog,
                        Setting::ApplicationUpdateInterval,
                        row![
                            text(Setting::ApplicationUpdateInterval.label()),
                            interval,
                            text("days")
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    ),
                    text(self.application_update_channel()).size(14),
                    text("Review available archives in Help → About kortty.").size(14),
                    text(self.application_update_status()).size(14),
                ]
                .spacing(10)
                .into()
            }
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
        let page = column![
            navigation,
            column![rule::horizontal(1), tabs].spacing(4),
            container(
                scrollable(container(page).width(Length::Fill).padding(20))
                    .id(dialog.page_scroll.clone())
            )
            .height(Length::Fill)
            .width(Length::Fill)
            .style(container::bordered_box),
        ]
        .spacing(10)
        .height(Length::Fill);
        // The suggestions occupy a layer over the section, leaving the footer in place.
        let mut body = stack![page].height(Length::Fill).width(Length::Fill);
        if let Some(results) = results {
            body = body.push(results);
        }

        let content = column![
            container(text("Global Terminal Settings").size(19))
                .width(Length::Fill)
                .padding([12, 14])
                .style(container::secondary),
            column![search, body, footer]
                .spacing(10)
                .padding(12)
                .height(Length::Fill),
        ]
        .height(Length::Fill)
        .into();
        let content = on_escape(content, msg(Message::Escape(id)));
        let modal = self
            .highlight_rules_for_settings(id)
            .map(|id| Desktop::Highlight(highlight::Message::Focus(id)));
        Some(super::profile_tools::vault_modal::gate(content, modal))
    }

    fn dismiss_settings(&mut self, id: window::Id) -> Task<Desktop> {
        if let Some(child) = self
            .openpgp_for_source(id)
            .or_else(|| self.highlight_rules_for_settings(id))
        {
            return window::gain_focus(child);
        }
        if self.capture_settings_shortcut(id, &Key::Named(Named::Escape), Modifiers::empty()) {
            return Task::none();
        }
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        let message = if !dialog.search.is_empty() {
            Message::SearchClear(id)
        } else {
            match dialog.section {
                Section::Themes => dialog
                    .themes
                    .cancel_message()
                    .map_or(Message::Cancel(id), |edit| Message::Theme(id, edit)),
                Section::Backup if dialog.backup.browsing() => {
                    dialog.backup.close_browser();
                    return Task::none();
                }
                Section::Logging if dialog.logging.browsing() => {
                    dialog.logging.close_browser();
                    return Task::none();
                }
                _ => Message::Cancel(id),
            }
        };
        self.update_settings(message)
    }

    fn terminal_settings_page<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let id = dialog.id;
        let settings = &dialog.draft.preferences;
        let policy = &self.clipboard.policy;
        let threshold = settings.paste_large_warning_kib;
        let delay = settings.paste_line_delay_ms;
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
            highlighted(
                dialog,
                Setting::PasteLineDelay,
                column![
                    row![
                        text(Setting::PasteLineDelay.label()),
                        slider(
                            0..=crate::connection_store::MAX_PASTE_LINE_DELAY_MS,
                            delay,
                            move |value| { Desktop::Settings(Message::PasteLineDelay(id, value)) }
                        )
                        .width(180),
                        text(if delay == 0 {
                            "Off".to_owned()
                        } else {
                            format!("{delay} ms")
                        }),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text("Press Esc in the terminal to stop a paced paste.").size(13),
                ]
                .spacing(5),
            ),
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
            initial_terminal_size(dialog),
            highlighted(
                dialog,
                Setting::SerialDefaultEncoding,
                column![
                    text(Setting::SerialDefaultEncoding.label()),
                    pick_list(TextEncoding::ALL, Some(settings.serial_default_encoding), move |encoding| {
                        Desktop::Settings(Message::SerialDefaultEncoding(id, encoding))
                    }),
                    text("Applies when a serial connection uses the default. Reconnect to apply changes.").size(13),
                ].spacing(5),
            ),
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
                Setting::ScrollbackLines,
                column![
                    text(format!("{} · {}", Setting::ScrollbackLines.label(), settings.scrollback_lines)),
                    slider(ScrollbackConfig::MIN_LINES..=ScrollbackConfig::MAX_LINES, settings.scrollback_lines, move |lines| {
                        Desktop::Settings(Message::ScrollbackLines(id, lines))
                    }),
                ].spacing(3),
            ),
            highlighted(
                dialog,
                Setting::ScrollbackMemory,
                column![
                    text(format!("{} · {} MiB", Setting::ScrollbackMemory.label(), settings.scrollback_memory_mib)),
                    slider(ScrollbackConfig::MIN_MEMORY_MIB..=ScrollbackConfig::MAX_MEMORY_MIB, settings.scrollback_memory_mib, move |memory| {
                        Desktop::Settings(Message::ScrollbackMemory(id, memory))
                    }),
                    text("The first limit reached removes the oldest lines. Applies to open panes when saved.").size(12),
                ].spacing(3),
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
                Setting::SshKeepAlive,
                column![
                    checkbox(settings.ssh_keepalive_enabled)
                        .label(Setting::SshKeepAlive.label())
                        .on_toggle(move |enabled| Desktop::Settings(Message::SshKeepAlive(id, enabled))),
                    text("Sends an encrypted heartbeat when an SSH connection is idle. Applies to new connections and reconnects.").size(12),
                ].spacing(3),
            ),
            highlighted(
                dialog,
                Setting::SshKeepAliveInterval,
                column![
                    text(format!("{} · {} seconds", Setting::SshKeepAliveInterval.label(), settings.ssh_keepalive_interval)),
                    settings.ssh_keepalive_enabled.then(|| slider(
                        KeepAlive::MIN_SECONDS..=KeepAlive::MAX_SECONDS, settings.ssh_keepalive_interval,
                        move |seconds| Desktop::Settings(Message::SshKeepAliveInterval(id, seconds)),
                    )),
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
            self.highlight_settings_section(dialog),
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
            .push(rule::horizontal(1))
            .push(text("Coding agents").size(16))
            .push(highlighted(
                dialog,
                Setting::CodingAgentDetection,
                column![
                    checkbox(settings.coding_agent_detection_enabled)
                        .label(Setting::CodingAgentDetection.label())
                        .on_toggle(move |enabled| {
                            Desktop::Settings(Message::CodingAgentDetection(id, enabled))
                        }),
                    text("Shows the state of supported coding agents in local terminal tabs. Applies to open panes when saved.").size(12),
                ]
                .spacing(3),
            ))
            .push(highlighted(
                dialog,
                Setting::CodingAgentAppBadge,
                checkbox(settings.coding_agent_app_badge_enabled)
                    .label(Setting::CodingAgentAppBadge.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::CodingAgentAppBadge(id, enabled))
                    }),
            ))
            .push(highlighted(
                dialog,
                Setting::CodingAgentDockAutomatic,
                checkbox(settings.coding_agent_dock_automatic)
                    .label(Setting::CodingAgentDockAutomatic.label())
                    .on_toggle(move |enabled| {
                        Desktop::Settings(Message::CodingAgentDockAutomatic(id, enabled))
                    }),
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
            text("Restore the last size and position of main windows and native dialogs, and the maximized state of main windows.").size(13),
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
        page = match (
            dialog.draft.restore,
            self.clipboard.policy.session_restore_mode,
        ) {
            (Some(_), Some(mode)) => page.push(highlighted(
                dialog,
                Setting::Restore,
                column![
                    row![
                        text(format!("{}:", Setting::Restore.label())).width(128),
                        text(RestoreChoice(mode).to_string()),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text("Managed by your organization.").size(13),
                ]
                .spacing(4),
            )),
            (Some(mode), None) => page.push(highlighted(
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
            (None, _) => page.push(text("Session recovery is unavailable.").size(14)),
        };
        let master_password_managed = self.clipboard.policy.require_master_password;
        let edit_master_password =
            !master_password_managed && self.clipboard.policy.problem.is_none();
        page = page.push(highlighted(
            dialog,
            Setting::StartupMasterPassword,
            column![
                checkbox(master_password_managed || startup.require_master_password)
                    .label(Setting::StartupMasterPassword.label())
                    .on_toggle_maybe(edit_master_password.then_some(move |enabled| {
                        Desktop::Settings(Message::Startup(id, startup_settings::Edit::MasterPassword(enabled)))
                    })),
                text(if master_password_managed {
                    "Managed by your organization."
                } else {
                    "Unlock or create the credential vault before opening the workspace. Takes effect at the next start."
                }).size(13),
            ].spacing(4),
        ));
        let output = dialog.draft.preferences.restore_output;
        let managed = self.clipboard.policy.session_restore_output;
        let enabled = managed.unwrap_or(output.enabled);
        let editable = self.clipboard.policy.problem.is_none() && managed.is_none();
        let lines: Element<'_, Desktop> = if editable && enabled {
            slider(
                session_restore::OutputPreferences::LINES,
                output.lines,
                move |lines| Desktop::Settings(Message::RestoreOutputLines(id, lines)),
            )
            .width(180)
            .into()
        } else {
            space::horizontal().width(0).into()
        };
        page = page.push(
            column![
                checkbox(enabled)
                    .label("Restore terminal output (encrypted)")
                    .on_toggle_maybe(editable.then_some(move |enabled| {
                        Desktop::Settings(Message::RestoreOutput(id, enabled))
                    })),
                row![
                    text(format!("Lines of output per pane · {}", output.lines)),
                    lines
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                managed
                    .is_some()
                    .then(|| text("Managed by your organization.").size(13)),
                text("Saving and restoring output requires an unlocked credential vault.").size(13),
            ]
            .spacing(6),
        );
        page = page.push(rule::horizontal(1)).push(highlighted(
            dialog,
            Setting::FixedGeometry,
            dialog
                .startup
                .view(startup, dialog.input_target(Setting::FixedGeometry, true))
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

fn update_grid_dimension(
    dimension: &mut Option<u16>,
    invalid: &mut Option<String>,
    mut input: String,
    range: RangeInclusive<u16>,
) {
    input.truncate(input.floor_char_boundary(16));
    let value = input.trim();
    if value.is_empty() {
        *dimension = None;
        *invalid = None;
    } else if let Some(value) = value.parse().ok().filter(|value| range.contains(value)) {
        *dimension = Some(value);
        *invalid = None;
    } else {
        *invalid = Some(input);
    }
}

fn initial_terminal_size(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let settings = &dialog.draft.preferences;
    let columns = settings
        .terminal_columns
        .map_or_else(String::new, |v| v.to_string());
    let rows = settings
        .terminal_rows
        .map_or_else(String::new, |v| v.to_string());
    highlighted(
        dialog,
        Setting::InitialTerminalSize,
        column![
            text(Setting::InitialTerminalSize.label()),
            row![
                column![
                    text("Columns"),
                    dialog.reveal_input(
                        Setting::InitialTerminalSize,
                        true,
                        text_input("Automatic", dialog.terminal_columns_input.as_deref().unwrap_or(&columns))
                            .on_input(move |value| Desktop::Settings(Message::TerminalColumns(id, value))),
                    ),
                ]
                .spacing(3)
                .width(120),
                column![
                    text("Rows"),
                    text_input("Automatic", dialog.terminal_rows_input.as_deref().unwrap_or(&rows))
                        .on_input(move |value| Desktop::Settings(Message::TerminalRows(id, value))),
                ]
                .spacing(3)
                .width(120),
            ]
            .spacing(8)
            .wrap(),
            (dialog.terminal_columns_input.is_some() || dialog.terminal_rows_input.is_some())
                .then(|| text("Enter 40 to 500 columns and 10 to 200 rows, or leave either field blank.")
                    .style(text::danger)
                    .size(13)),
            text("Applies when starting local shells and SSH sessions. Blank fields use the terminal emulation’s default.")
                .size(13),
        ]
        .spacing(5),
    )
}

fn messaging_settings_page(
    dialog: &Dialog,
    desktop_notifications_allowed: bool,
) -> Element<'_, Desktop> {
    let id = dialog.id;
    let seconds = dialog.draft.preferences.terminal_silence_seconds;
    let saved = seconds.to_string();
    let value = dialog.silence_seconds_input.as_deref().unwrap_or(&saved);
    column![
        text("Notifications").size(18),
        highlighted(
            dialog,
            Setting::TerminalNotifications,
            dialog.draft.preferences.terminal_notifications.view(
                id,
                desktop_notifications_allowed,
                dialog.draft.preferences.prompt_navigation_enabled,
            ),
        ),
        highlighted(
            dialog,
            Setting::CodingAgentNotifications,
            column![
                checkbox(
                    desktop_notifications_allowed
                        && dialog.draft.preferences.coding_agent_notifications_enabled,
                )
                .label(Setting::CodingAgentNotifications.label())
                .on_toggle_maybe(desktop_notifications_allowed.then_some(move |enabled| {
                    Desktop::Settings(Message::CodingAgentNotifications(id, enabled))
                })),
                text(if desktop_notifications_allowed {
                    "Uses the system notification service. Private tabs stay out of desktop notifications."
                } else {
                    "Desktop notifications are disabled by managed policy."
                })
                .size(13),
            ]
            .spacing(5),
        ),
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
                    dialog.reveal_input(
                        Setting::SilenceThreshold,
                        true,
                        text_input("30", value)
                            .width(72)
                            .on_input(move |value| Desktop::Settings(Message::SilenceSeconds(id, value))),
                    ),
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
                dialog.reveal_input(
                    Setting::FontSize,
                    true,
                    text_input(
                        "8–72",
                        dialog.font_size_input.as_deref().unwrap_or(&formatted_size)
                    )
                    .width(50)
                    .on_input(move |value| Desktop::Settings(Message::FontSizeInput(id, value))),
                ),
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
    let row = container(content).width(Length::Fill).padding(4);
    if dialog.revealed == Some(setting) {
        row.id(dialog.revealed_row.clone())
            .style(container::secondary)
            .into()
    } else {
        row.style(container::transparent).into()
    }
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
    fn settings_keep_their_owner_and_route_close_requests_until_dismissed() {
        use super::super::chrome::Action;
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::Open(owner));
        let dialog = workspace.settings.for_owner(owner).unwrap();
        #[cfg(target_os = "windows")]
        let _ = workspace.finish_owned_dialog_open(dialog, owner, None);
        let _ = workspace.update_settings(Message::OpenAt(other, Section::Appearance));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().section,
            Section::Appearance
        );
        assert!(matches!(
            workspace.owner_dialog(owner),
            Some((id, Desktop::Settings(Message::Focus(target)))) if id == dialog && target == dialog
        ));
        assert!(workspace.owner_dialog(other).is_none());
        assert!(!workspace.chrome_enabled(owner, Action::QuickConnect));
        assert!(workspace.chrome_enabled(other, Action::QuickConnect));

        let close_owner = workspace.update(Desktop::Window(owner, window::Event::CloseRequested));
        let mut actions = task::into_stream(close_owner).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == dialog
        ));
        assert!(workspace.windows.contains_key(&owner));

        let close = workspace.update_settings(Message::Cancel(dialog));
        let mut actions = task::into_stream(close).unwrap();
        #[cfg(target_os = "windows")]
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == owner
        ));
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.chrome_enabled(owner, Action::QuickConnect));
    }

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
    fn java_theme_import_is_reviewed_and_only_saved_through_settings() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action, task};
        use theme_settings::{Edit, exchange};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("themes.xml");
        let settings_path = profile.0.join("window-settings.json");
        std::fs::write(
            &path,
            r#"<themes><theme id="test-appearance"><name>Portable colors</name></theme></themes>"#,
        )
        .unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        for save in [false, true] {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Themes));
            let id = workspace.settings.for_owner(owner).unwrap();
            let send = |workspace: &mut Workspace, action| {
                workspace.update_settings(Message::Theme(id, Edit::Java(action)))
            };
            let _ = send(
                &mut workspace,
                exchange::Action::Open(exchange::Mode::Import),
            );
            let _ = send(
                &mut workspace,
                exchange::Action::Path(path.to_str().unwrap().to_owned()),
            );
            let work = send(&mut workspace, exchange::Action::Run);
            let mut events = task::into_stream(work).unwrap();
            let Some(Action::Output(Desktop::Settings(finished))) = block_on(events.next()) else {
                panic!("expected the import result");
            };
            let _ = workspace.update_settings(finished);
            let draft = &workspace.settings.0.as_ref().unwrap().draft.preferences;
            assert!(draft.themes.find_java("test-appearance").is_none());
            let _ = send(&mut workspace, exchange::Action::Apply);
            let draft = &workspace.settings.0.as_ref().unwrap().draft.preferences;
            assert_eq!(
                draft.themes.find_java("test-appearance").unwrap().name,
                "Portable colors"
            );
            assert!(
                workspace
                    .window_settings
                    .themes
                    .find_java("test-appearance")
                    .is_none()
            );
            if save {
                let _ =
                    workspace.save_settings_to(id, |preferences| preferences.save(&settings_path));
            } else {
                let _ = workspace.update_settings(Message::Cancel(id));
                assert!(!settings_path.exists());
            }
        }
        let saved: WindowSettings =
            serde_json::from_slice(&std::fs::read(settings_path).unwrap()).unwrap();
        assert_eq!(
            saved.themes.find_java("test-appearance").unwrap().name,
            "Portable colors"
        );
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
            [Setting::FontSize, Setting::SnippetFontSize]
        );
        assert_eq!(
            search_matches("font face").collect::<Vec<_>>(),
            [Setting::FontFamily, Setting::SnippetFontFamily]
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
    fn editor_cursor_settings_apply_only_after_valid_save() {
        use crate::editor_window::caret::{Preferences, Shape};
        use crate::terminal_theme::Rgb;
        use editor_settings::Edit;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let inherited = workspace.window_settings.editor_appearance();
        assert_eq!(
            workspace.window_settings.editor_caret,
            Preferences::default()
        );

        let _ = workspace.update_settings(Message::Open(owner));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Search(id, "editor cursor style".into()));
        let _ = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().section,
            Section::Editor
        );
        let _ = workspace.update_settings(Message::EditorCaret(id, Edit::Color("bad".into())));
        let _ =
            workspace.save_settings_to(id, |_| panic!("invalid cursor color reached persistence"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::EditorCursorColor)
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.editor_caret,
            Preferences::default()
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Editor));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::EditorCaret(id, Edit::Shape(Shape::Underscore)));
        let _ = workspace.update_settings(Message::EditorCaret(id, Edit::Color("#12A0BC".into())));
        assert_eq!(
            workspace.window_settings.editor_caret,
            Preferences::default()
        );
        let mut saved = Vec::new();
        let _ = workspace.save_settings_to(id, |settings| {
            saved = serde_json::to_vec(settings)?;
            Ok(())
        });
        let restored: WindowSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(workspace.window_settings, restored);
        assert_eq!(
            restored.editor_caret,
            Preferences {
                shape: Shape::Underscore,
                color: Rgb::from_hex(0x12a0bc),
            }
        );
        assert_eq!(restored.editor_appearance(), inherited);
        assert!(workspace.settings.for_owner(owner).is_none());
    }

    #[test]
    fn snippet_editor_appearance_inherits_and_only_applies_after_valid_save() {
        use crate::terminal_theme::Rgb;
        use snippet_editor_settings::{Color, Edit};

        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_font = 22;
        workspace.window_settings.colors.foreground = Some(Rgb::from_hex(0xbbccdd));
        workspace.window_settings.colors.background = Some(Rgb::from_hex(0x112233));
        let terminal = workspace.window_settings.terminal_appearance();
        let file_editor = workspace.window_settings.editor_appearance();
        let inherited = workspace.window_settings.snippet_editor_appearance();
        assert_eq!(file_editor, inherited);
        assert_eq!(inherited.size, 22);
        assert_eq!(inherited.foreground, terminal.foreground);
        assert_eq!(inherited.background, terminal.background);

        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::Open(owner));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Search(id, "snippet foreground".into()));
        let _ = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().section,
            Section::SnippetEditor
        );
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.update_settings(Message::SnippetAppearance(id, Edit::Size("73".into())));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid size reached persistence"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::SnippetFontSize)
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.snippet_editor_appearance(),
            inherited
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::SnippetEditor));
        let id = workspace.settings.for_owner(owner).unwrap();
        for edit in [
            Edit::Family(Some(MONOSPACED.into())),
            Edit::Size("30".into()),
            Edit::Color(Color::Foreground, "#AABBCC".into()),
            Edit::Color(Color::Background, "#223344".into()),
            Edit::Color(Color::Background, String::new()),
        ] {
            let _ = workspace.update_settings(Message::SnippetAppearance(id, edit));
        }
        assert_eq!(
            workspace.window_settings.snippet_editor_appearance(),
            inherited
        );
        let mut saved = Vec::new();
        let _ = workspace.save_settings_to(id, |settings| {
            saved = serde_json::to_vec(settings)?;
            Ok(())
        });
        let restored: WindowSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(workspace.window_settings, restored);
        let actual = restored.snippet_editor_appearance();
        assert_eq!(
            restored.snippet_appearance.font_family.as_deref(),
            Some(MONOSPACED)
        );
        assert_eq!(actual.size, 30);
        assert_eq!(actual.foreground, Rgb::from_hex(0xaabbcc));
        assert_eq!(actual.background, terminal.background);
        assert_eq!(restored.editor_appearance(), file_editor);
        assert_eq!(restored.terminal_appearance(), terminal);
        assert_eq!(restored.terminal_font, 22);
        assert!(workspace.settings.for_owner(owner).is_none());
    }

    #[test]
    fn snippet_cursor_overrides_save_and_reset_independently() {
        use crate::editor_window::caret::{Preferences, Shape};
        use crate::terminal_theme::Rgb;
        use snippet_editor_settings::{Color, Edit};

        let mut workspace = Workspace {
            window_settings: serde_json::from_str(r#"{"snippet_appearance":{"font_size":20}}"#)
                .unwrap(),
            ..Workspace::default()
        };
        assert_eq!(workspace.window_settings.snippet_appearance.font_size, 20);
        let file_cursor = Preferences {
            shape: Shape::Underscore,
            color: Rgb::from_hex(0x123456),
        };
        workspace.window_settings.editor_caret = file_cursor;
        assert_eq!(
            workspace.window_settings.snippet_editor_caret(),
            file_cursor
        );

        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::Open(owner));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Search(id, "snippet cursor style".into()));
        let _ = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::SnippetCursorStyle)
        );
        let _ = workspace.update_settings(Message::SnippetAppearance(
            id,
            Edit::CursorStyle(Some(Shape::Line)),
        ));
        let _ = workspace.update_settings(Message::SnippetAppearance(
            id,
            Edit::Color(Color::Cursor, "invalid".into()),
        ));
        let _ =
            workspace.save_settings_to(id, |_| panic!("invalid cursor color reached persistence"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::SnippetCursorColor)
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.snippet_editor_caret(),
            file_cursor
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::SnippetEditor));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::SnippetAppearance(
            id,
            Edit::CursorStyle(Some(Shape::Line)),
        ));
        let mut saved = Vec::new();
        let _ = workspace.save_settings_to(id, |settings| {
            saved = serde_json::to_vec(settings)?;
            Ok(())
        });
        let restored: WindowSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(workspace.window_settings, restored);
        assert_eq!(restored.editor_caret, file_cursor);
        assert_eq!(
            restored.snippet_editor_caret(),
            Preferences {
                shape: Shape::Line,
                ..file_cursor
            }
        );

        let _ = workspace.update_settings(Message::OpenAt(owner, Section::SnippetEditor));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::SnippetAppearance(id, Edit::CursorStyle(None)));
        let _ = workspace.update_settings(Message::SnippetAppearance(
            id,
            Edit::Color(Color::Cursor, "#ABCDEF".into()),
        ));
        assert_eq!(workspace.window_settings, restored);
        let _ = workspace.save_settings_to(id, |settings| {
            saved = serde_json::to_vec(settings)?;
            Ok(())
        });
        let restored: WindowSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(workspace.window_settings, restored);
        assert_eq!(restored.editor_caret, file_cursor);
        assert_eq!(
            restored.snippet_editor_caret(),
            Preferences {
                color: Rgb::from_hex(0xabcdef),
                ..file_cursor
            }
        );
    }

    #[test]
    fn settings_search_selects_the_active_result_and_clears_before_closing() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Window));
        let id = workspace.settings.for_owner(owner).unwrap();

        let _ = workspace.update_settings(Message::Search(id, "quick select".into()));
        let _ = workspace.update_settings(Message::SearchStep(id, -1));
        assert_eq!(workspace.settings.0.as_ref().unwrap().search_selection, 0);
        let _ = workspace.update_settings(Message::SearchStep(id, 1));
        let _ = workspace.update_settings(Message::SearchStep(id, 1));
        assert_eq!(workspace.settings.0.as_ref().unwrap().search_selection, 1);
        let _ = workspace.update_settings(Message::SearchChoose(id));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Terminal);
        assert_eq!(dialog.revealed, Some(Setting::QuickSelectPatterns));
        assert!(dialog.search.is_empty());

        let _ = workspace.update_settings(Message::Search(id, "z".into()));
        assert_eq!(workspace.settings.0.as_ref().unwrap().search_selection, 0);
        let _ = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(workspace.settings.for_owner(owner), Some(id));
        let _ = workspace.update_settings(Message::Escape(id));
        assert!(workspace.settings.0.as_ref().unwrap().search.is_empty());
        let _ = workspace.update_settings(Message::Escape(id));
        assert!(workspace.settings.for_owner(owner).is_none());
    }

    #[test]
    fn settings_escape_cancels_focused_theme_and_parent_fields_separately() {
        use iced::advanced::{
            clipboard,
            widget::operation::{self, Operation, focusable},
        };
        use iced::keyboard::{self, Location, key};
        use iced::{Event, Font, Renderer, event, mouse};
        use iced_runtime::{UserInterface, user_interface::Cache};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let original_font_size = workspace.window_settings.terminal_font;
        let edited_font_size = if original_font_size == 24 { 25 } else { 24 };
        let _ = workspace.update_settings(Message::FontSize(id, edited_font_size));
        let _ = workspace.update_settings(Message::Section(id, Section::Themes));
        let _ = workspace.update_settings(Message::Theme(id, theme_settings::Edit::New));

        let type_and_escape = |workspace: &Workspace| {
            let key_press = |key: Key, text: Option<&str>| {
                Event::Keyboard(keyboard::Event::KeyPressed {
                    modified_key: key.clone(),
                    key,
                    physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                    location: Location::Standard,
                    modifiers: Modifiers::empty(),
                    text: text.map(Into::into),
                    repeat: false,
                })
            };
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                Size::new(800.0, 550.0),
                Cache::default(),
                &mut renderer,
            );
            for _ in 0..2 {
                let mut focus = focusable::focus_next();
                interface.operate(&renderer, &mut focus);
                let operation::Outcome::Chain(mut focus) = focus.finish() else {
                    panic!("expected a focusable Settings field");
                };
                interface.operate(&renderer, focus.as_mut());
            }
            let mut messages = Vec::new();
            let (_, statuses) = interface.update(
                &[
                    key_press(Key::Character("x".into()), Some("x")),
                    key_press(Key::Named(Named::Escape), None),
                ],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(statuses, [event::Status::Captured; 2]);
            messages
        };

        let messages = type_and_escape(&workspace);
        assert!(matches!(messages.as_slice(),
                [Desktop::Settings(Message::Theme(theme, theme_settings::Edit::Name(name))),
                    Desktop::Settings(Message::Escape(escape))]
                if *theme == id && *escape == id && name == "x"));
        for message in messages {
            let _ = workspace.update(message);
        }
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert!(!dialog.themes.editing());
        assert_eq!(dialog.draft.preferences.terminal_font, edited_font_size);

        let _ = workspace.update_settings(Message::Section(id, Section::Font));
        let messages = type_and_escape(&workspace);
        assert!(matches!(messages.as_slice(),
                [Desktop::Settings(Message::FontSizeInput(font, _)),
                    Desktop::Settings(Message::Escape(escape))]
                if *font == id && *escape == id));
        for message in messages {
            let _ = workspace.update(message);
        }
        assert!(workspace.settings.for_owner(owner).is_none());
        assert_eq!(workspace.window_settings.terminal_font, original_font_size);
    }

    #[test]
    fn settings_search_enter_focuses_an_editable_result_and_keeps_search_for_disabled_fields() {
        use iced::advanced::widget::operation::focusable;
        use iced::{Font, Renderer};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::settings_search::test_input;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Window));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ =
            workspace.update_settings(Message::Startup(id, startup_settings::Edit::Fixed(false)));
        let search = workspace.settings.0.as_ref().unwrap().search_input.clone();
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let bounds = Size::new(800.0, 360.0);

        for (query, setting) in [
            ("terminal font size", Setting::FontSize),
            ("fixed geometry", Setting::FixedGeometry),
        ] {
            let _ = workspace.update_settings(Message::Search(id, query.into()));
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                bounds,
                Cache::default(),
                &mut renderer,
            );
            interface.operate(&renderer, &mut focusable::focus(search.clone()));
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Enter),
                None,
            );
            assert!(
                matches!(messages.as_slice(), [Desktop::Settings(Message::SearchChoose(target))] if *target == id)
            );
            let cache = interface.into_cache();
            let reveal = workspace.update(messages.into_iter().next().unwrap());
            assert_eq!(
                workspace.settings.0.as_ref().unwrap().revealed,
                Some(setting)
            );
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                bounds,
                cache,
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, reveal);
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("2".into()),
                Some("2"),
            );
            if setting == Setting::FontSize {
                assert!(
                    matches!(messages.as_slice(), [Desktop::Settings(Message::FontSizeInput(target, value))] if *target == id && value.ends_with('2'))
                );
            } else {
                assert!(
                    matches!(messages.as_slice(), [Desktop::Settings(Message::Search(target, value))] if *target == id && value == "2")
                );
            }
        }
    }

    #[test]
    fn settings_search_scrolls_the_chosen_row_into_the_page_viewport() {
        use iced::advanced::widget::{Operation, Tree, operation};
        use iced::advanced::{Layout, layout};
        use iced::futures::{StreamExt, executor::block_on};
        use iced::{Font, Rectangle, Renderer, Vector};

        struct Measure {
            page: widget::Id,
            row: widget::Id,
            viewport: Rectangle,
            row_y: f32,
            scroll: f32,
        }

        impl Operation for Measure {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }

            fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
                if id == Some(&self.row) {
                    self.row_y = bounds.y;
                }
            }

            fn scrollable(
                &mut self,
                id: Option<&widget::Id>,
                bounds: Rectangle,
                _: Rectangle,
                translation: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                if id == Some(&self.page) {
                    self.viewport = bounds;
                    self.scroll = translation.y;
                }
            }
        }

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Search(id, "fixed geometry".into()));
        let reveal = workspace.update_settings(Message::SearchChoose(id));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.revealed, Some(Setting::FixedGeometry));
        let mut measure = Measure {
            page: dialog.page_scroll.clone(),
            row: dialog.revealed_row.clone(),
            viewport: Rectangle::default(),
            row_y: 0.0,
            scroll: 0.0,
        };
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut view = workspace.settings_view(id).unwrap();
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(620.0, 300.0)),
        );
        let mut apply = |op: &mut dyn Operation| {
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, op);
        };
        apply(&mut measure);
        assert!(measure.row_y >= measure.viewport.y + measure.viewport.height);

        let mut actions = iced_runtime::task::into_stream(reveal).unwrap();
        while let Some(iced_runtime::Action::Widget(mut operation)) = block_on(actions.next()) {
            loop {
                apply(operation.as_mut());
                let operation::Outcome::Chain(next) = operation.finish() else {
                    break;
                };
                operation = next;
            }
        }
        apply(&mut measure);
        assert!(measure.scroll > 0.0);
        let visible_y = measure.row_y - measure.scroll;
        assert!(visible_y >= measure.viewport.y);
        assert!(visible_y < measure.viewport.y + measure.viewport.height);
    }

    #[test]
    fn settings_search_results_scroll_without_displacing_the_section_or_footer() {
        use iced::advanced::{Layout, layout, widget::Tree};
        use iced::{Font, Renderer};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Window));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let compact = Size::new(620.0, 300.0);
        let measure = |workspace: &Workspace| {
            let mut view = workspace.settings_view(id).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, compact),
            );
            let pane = Layout::new(&node).children().nth(1).unwrap();
            let mut pane = pane.children();
            let search = pane.next().unwrap().bounds();
            let body = pane.next().unwrap();
            let footer = pane.next().unwrap().bounds();
            let results = body.children().nth(1).map(|overlay| {
                let panel = overlay.children().next().unwrap();
                let viewport = panel.children().next().unwrap();
                let list = viewport.children().next().unwrap();
                (panel.bounds(), viewport.bounds(), list.bounds())
            });
            (search, body.bounds(), footer, results)
        };
        let (search, body, footer, results) = measure(&workspace);
        assert!(results.is_none());
        assert!(body.height > 0.0);
        assert!(footer.height > 0.0 && footer.y + footer.height <= compact.height);

        let query = "in";
        assert!(search_matches(query).count() > 10);
        let _ = workspace.update_settings(Message::Search(id, query.into()));
        let (with_search, with_body, with_footer, results) = measure(&workspace);
        assert_eq!(
            (with_search, with_body, with_footer),
            (search, body, footer)
        );
        let (panel, viewport, list) = results.unwrap();
        assert!(panel.height <= SEARCH_RESULTS_MAX_HEIGHT);
        assert!(panel.y >= body.y && panel.y + panel.height <= body.y + body.height);
        assert!(list.height > viewport.height && viewport.height > 0.0);
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
        draft.preferences.startup.require_master_password = true;
        let current = WindowSettings {
            terminal_transparency: 42,
            open_tool_windows_as_tabs: true,
            prompt_navigation_enabled: false,
            ..initial.preferences.clone()
        };
        let policy = ClipboardPolicy {
            read_allowed: false,
            minimum_paste_warning: WarningMode::Always,
            require_master_password: true,
            ..ClipboardPolicy::default()
        };
        let next = draft.merge_preferences(&initial, current.clone(), &policy);
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
        assert!(!next.startup.require_master_password);
        let next = draft.merge_preferences(&initial, current, &ClipboardPolicy::default());
        assert!(next.startup.require_master_password);
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
    fn ssh_keepalive_settings_save_keeps_disabled_interval() {
        let legacy: WindowSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.ssh_keepalive(), KeepAlive::default());
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::SshKeepAliveInterval(id, 120));
        let _ = workspace.update_settings(Message::SshKeepAlive(id, false));
        assert_eq!(
            workspace.window_settings.ssh_keepalive(),
            KeepAlive::default()
        );
        let expected = KeepAlive::new(false, 120).unwrap();
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        assert_eq!(workspace.window_settings.ssh_keepalive(), expected);
        assert_eq!(
            WindowSettings::read(&path).unwrap().ssh_keepalive(),
            expected
        );
    }

    #[test]
    fn initial_terminal_size_settings_validate_save_and_clear_each_dimension() {
        let legacy: WindowSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            (legacy.terminal_columns, legacy.terminal_rows),
            (None, None)
        );
        assert_eq!(
            search_matches("initial columns").next(),
            Some(Setting::InitialTerminalSize)
        );
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::TerminalColumns(id, "160".into()));
        let _ = workspace.update_settings(Message::TerminalRows(id, "9".into()));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid dimensions must not be saved"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::InitialTerminalSize)
        );
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let _ = workspace.update_settings(Message::TerminalRows(id, "48".into()));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::TerminalColumns(id, String::new()));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let restored = WindowSettings::read(&path).unwrap();
        assert_eq!(
            (restored.terminal_columns, restored.terminal_rows),
            (None, Some(48))
        );
        assert_eq!(workspace.window_settings, restored);
        assert!(WindowSettings::validate_backup(br#"{"terminal_columns":501}"#).is_err());
    }

    #[test]
    fn scrollback_settings_save_updates_open_and_new_streams() {
        use kortty_terminal::{ScrollbackConfig, TerminalProfile};

        let legacy: WindowSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.scrollback_config(), ScrollbackConfig::default());
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let start_stream = |workspace: &mut Workspace| {
            workspace
                .store_local_session(TerminalProfile::Xterm, None, None)
                .unwrap()
        };
        let requested = |workspace: &Workspace, id| {
            workspace.sessions[&id]
                .session
                .scrollback_config_requested()
        };
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let existing = start_stream(&mut workspace);
        assert_eq!(requested(&workspace, existing), ScrollbackConfig::default());
        let edit = |workspace: &mut Workspace, id| {
            let _ = workspace.update_settings(Message::ScrollbackLines(id, 125));
            let _ = workspace.update_settings(Message::ScrollbackMemory(id, 2));
            let _ = workspace.update_settings(Message::ScrollbackCompression(id, false));
        };
        let configured = ScrollbackConfig {
            lines: 125,
            memory_mib: 2,
            compression: false,
        };
        let id = open(&mut workspace);
        edit(&mut workspace, id);
        // Test double for a failed preference write. The live request stays unchanged.
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert_eq!(requested(&workspace, existing), ScrollbackConfig::default());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.scrollback_config(),
            ScrollbackConfig::default()
        );

        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let id = open(&mut workspace);
        edit(&mut workspace, id);
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let restored: WindowSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.scrollback_config(), configured);
        assert_eq!(requested(&workspace, existing), configured);
        let new = start_stream(&mut workspace);
        assert_eq!(requested(&workspace, new), configured);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::ScrollbackCompression(id, true));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let compressed = ScrollbackConfig {
            compression: true,
            ..configured
        };
        assert_eq!(requested(&workspace, existing), compressed);
        assert_eq!(requested(&workspace, new), compressed);
        for pane in workspace.sessions.values() {
            let _ = pane.session.stop().recv_blocking();
        }
    }

    #[test]
    fn serial_default_encoding_is_searchable_and_commits_only_on_save() -> std::io::Result<()> {
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert_eq!(legacy.serial_default_encoding, TextEncoding::Utf8);
        assert!(
            search_matches("serial charset")
                .any(|setting| setting == Setting::SerialDefaultEncoding)
        );

        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::SerialDefaultEncoding(id, TextEncoding::Latin1));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(
            workspace.window_settings.serial_default_encoding,
            TextEncoding::Utf8
        );

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::SerialDefaultEncoding(id, TextEncoding::Latin9));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let saved: WindowSettings = serde_json::from_slice(&std::fs::read(path)?)?;
        assert_eq!(saved.serial_default_encoding, TextEncoding::Latin9);
        assert_eq!(
            workspace.window_settings.serial_default_encoding,
            TextEncoding::Latin9
        );
        Ok(())
    }

    #[test]
    fn coding_agent_detection_is_saved_and_updates_open_and_new_panes() -> std::io::Result<()> {
        use kortty_terminal::TerminalProfile;

        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.coding_agent_detection_enabled);
        assert!(
            search_matches("coding agents").any(|setting| setting == Setting::CodingAgentDetection)
        );
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let store = |workspace: &mut Workspace| {
            workspace.store_local_session(TerminalProfile::Xterm, None, None)
        };
        let requested = |workspace: &Workspace, session| {
            workspace.sessions[&session]
                .session
                .coding_agent_detection_requested()
        };
        let existing = store(&mut workspace)?;
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentDetection(id, false));
        let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("test write error")));
        assert!(requested(&workspace, existing));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.coding_agent_detection_enabled);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentDetection(id, false));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let saved: WindowSettings = serde_json::from_slice(&std::fs::read(path)?)?;
        assert!(!saved.coding_agent_detection_enabled);
        assert!(!requested(&workspace, existing));
        let new = store(&mut workspace)?;
        assert!(!requested(&workspace, new));
        for pane in workspace.sessions.values() {
            let _ = pane.session.stop().recv_blocking();
        }
        Ok(())
    }

    #[test]
    fn coding_agent_app_badge_defaults_on_and_saves_independently() -> std::io::Result<()> {
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.coding_agent_app_badge_enabled);
        assert!(
            search_matches("application badge")
                .any(|setting| setting == Setting::CodingAgentAppBadge)
        );
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentAppBadge(id, false));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.coding_agent_app_badge_enabled);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentAppBadge(id, false));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let saved: WindowSettings = serde_json::from_slice(&std::fs::read(path)?)?;
        assert!(!saved.coding_agent_app_badge_enabled);
        assert!(saved.coding_agent_detection_enabled);
        assert!(!workspace.window_settings.coding_agent_app_badge_enabled);
        Ok(())
    }

    #[test]
    fn coding_agent_notifications_save_independently_and_respect_managed_policy()
    -> std::io::Result<()> {
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.coding_agent_notifications_enabled);
        assert!(
            search_matches("coding agents desktop notifications")
                .any(|setting| setting == Setting::CodingAgentNotifications)
        );
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Messaging));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentNotifications(id, false));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.coding_agent_notifications_enabled);

        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentNotifications(id, false));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        let saved: WindowSettings = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert!(!saved.coding_agent_notifications_enabled);
        assert!(saved.coding_agent_detection_enabled && saved.coding_agent_app_badge_enabled);

        workspace.clipboard.policy.desktop_notifications_allowed = false;
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::CodingAgentNotifications(id, true));
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        assert!(!workspace.window_settings.coding_agent_notifications_enabled);
        assert!(!workspace.coding_agent_notifications_enabled());
        Ok(())
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
    fn agent_design_menu_reaches_the_saved_global_preference() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.invoke_chrome(owner, super::super::chrome::Action::AgentDesign);
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::AgentDesign);
        let id = dialog.id;
        assert!(workspace.settings_view(id).is_some());
        let _ = workspace.update_settings(Message::AgentDesignCatalog(
            id,
            agent_design_settings::Edit::Default("classic".into()),
        ));
        assert_eq!(
            workspace.window_settings.ai_agent_design,
            AgentDesign::Standard
        );
        let _ = workspace.save_settings_to(id, |preferences| {
            let restored: WindowSettings =
                serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
            assert_eq!(restored.ai_agent_design, AgentDesign::Classic);
            Ok(())
        });
        assert_eq!(
            workspace.window_settings.ai_agent_design,
            AgentDesign::Classic
        );
        let old_profile: WindowSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old_profile.ai_agent_design, AgentDesign::Standard);
    }

    #[test]
    fn custom_agent_design_is_edited_previewed_and_saved_through_settings() {
        use crate::terminal_agent::design::{Approval, Arrangement, History, Status};
        use agent_design_settings::{Edit, Module};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.invoke_chrome(owner, super::super::chrome::Action::AgentDesign);
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::AgentDesignCatalog(
            id,
            Edit::Duplicate("floating".into()),
        ));
        let choice = workspace
            .settings
            .0
            .as_ref()
            .unwrap()
            .draft
            .preferences
            .ai_agent_designs
            .choices()
            .pop()
            .unwrap();
        for edit in [
            Edit::Name(choice.id.clone(), "Review cards".into()),
            Edit::Rename(choice.id.clone()),
            Edit::Module(choice.id.clone(), Module::History(History::StepCards)),
            Edit::Module(choice.id.clone(), Module::Status(Status::Pill)),
            Edit::Module(choice.id.clone(), Module::Approval(Approval::AboveActivity)),
            Edit::Default(choice.id.clone()),
        ] {
            let _ = workspace.update_settings(Message::AgentDesignCatalog(id, edit));
        }
        assert!(workspace.settings_view(id).is_some());
        assert!(
            workspace
                .window_settings
                .ai_agent_designs
                .layout(&choice.id)
                .is_none()
        );
        let _ = workspace.save_settings_to(id, |preferences| {
            let restored: WindowSettings =
                serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
            let catalog = restored.ai_agent_designs;
            assert_eq!(catalog.default_id(restored.ai_agent_design), choice.id);
            assert_eq!(catalog.choices().last().unwrap().name, "Review cards");
            let layout = catalog.layout(&choice.id).unwrap();
            assert_eq!(layout.history, History::StepCards);
            assert_eq!(layout.status, Status::Pill);
            assert_eq!(layout.approval, Approval::AboveActivity);
            assert_eq!(layout.arrangement, Arrangement::Floating);
            Ok(())
        });
        assert_eq!(
            workspace
                .window_settings
                .ai_agent_designs
                .default_id(AgentDesign::Standard),
            choice.id
        );
    }

    #[test]
    fn application_update_opt_in_and_interval_apply_only_after_settings_save() -> std::io::Result<()>
    {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(!legacy.application_updates.enabled);
        assert_eq!(legacy.application_updates.interval_days, 1);
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Updates));
        let id = workspace.settings.0.as_ref().unwrap().id;
        assert!(workspace.settings_view(id).is_some());
        assert_eq!(Setting::ApplicationUpdates.section(), Section::Updates);
        let _ = workspace.update_settings(Message::ApplicationUpdates(id, true));
        let _ = workspace.update_settings(Message::ApplicationUpdateInterval(id, 12));
        let _ = workspace.update_settings(Message::ApplicationUpdateInterval(id, 31));
        assert!(!workspace.window_settings.application_updates.enabled);
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let saved = WindowSettings::read(&path)?.application_updates;
        assert!(saved.enabled);
        assert_eq!(saved.interval_days, 12);
        assert_eq!(workspace.window_settings.application_updates, saved);
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
                None,
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
