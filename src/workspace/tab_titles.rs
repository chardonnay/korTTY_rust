use std::io::{self, Read};
use std::path::Path;
use std::{fmt, fmt::Display};

use iced::{Font, Task, window};
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use crate::profile;
use crate::terminal_theme::{
    Appearance, BoldTextMode, Colors, MONOSPACED, Scheme, Themes, installed_font,
};
use crate::terminal_view::quick_select::{
    DEFAULT_ALPHABET, MAX_PATTERNS, Preferences as QuickSelectPreferences,
};

use super::{
    DEFAULT_FONT, Message, Tab, Workspace, appearance, key_bindings::Bindings, paste_protection,
    window_lifecycle::StartupPreferences,
};

// Preserve room for the other settings alongside 16 validated Quick Select regexes.
const PREFERENCE_LIMIT: u64 = 48 * 1024;
const DEFAULT_TERMINAL_TITLE: &str = "Terminal";
pub(super) const REMOTE_SIDEBAR_MIN_WIDTH: u16 = 200;
pub(super) const REMOTE_SIDEBAR_MAX_WIDTH: u16 = 1200;
const REMOTE_SIDEBAR_DEFAULT_WIDTH: u16 = 300;
pub(super) const DEFAULT_JOURNAL_EDIT_SPLIT: u16 = 6200;
pub(super) const MIN_TERMINAL_SILENCE_SECONDS: u16 = 5;
pub(super) const MAX_TERMINAL_SILENCE_SECONDS: u16 = 3600;

fn default_journal_edit_split(value: &u16) -> bool {
    *value == DEFAULT_JOURNAL_EDIT_SPLIT
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteSidebarPosition {
    Left,
    Right,
    #[default]
    #[serde(other)]
    Hidden,
}

impl RemoteSidebarPosition {
    pub(super) const ALL: [Self; 3] = [Self::Hidden, Self::Left, Self::Right];
}

impl Display for RemoteSidebarPosition {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Hidden => "Hidden",
            Self::Left => "Left",
            Self::Right => "Right",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct WindowSettings {
    pub(super) startup: StartupPreferences,
    pub(super) prevent_system_sleep: bool,
    #[serde(skip_serializing_if = "super::open_recent::Preferences::is_default")]
    pub(super) open_recent: super::open_recent::Preferences,
    pub(super) app_logging: crate::app_logging::Settings,
    pub(super) session_journal: crate::session_journal::Settings,
    #[serde(
        skip_serializing_if = "crate::session_journal::topic_ai_settings::Preferences::is_default"
    )]
    pub(super) journal_ai: crate::session_journal::topic_ai_settings::Preferences,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) journal_note_translation_language: String,
    #[serde(
        skip_serializing_if = "crate::session_journal::export_branding::Preferences::is_default"
    )]
    pub(super) export_branding: crate::session_journal::export_branding::Preferences,
    pub(super) open_tool_windows_as_tabs: bool,
    #[serde(skip_serializing_if = "crate::snippet_window::structure::Preferences::is_default")]
    pub(super) snippet_structure: crate::snippet_window::structure::Preferences,
    pub(super) job_menu_status_enabled: bool,
    pub(super) connection_color_border_enabled: bool,
    pub(super) split_pane_focus_ring_enabled: bool,
    pub(super) tab_title_from_shell_enabled: bool,
    pub(super) tab_switch_most_recent_first: bool,
    pub(super) prompt_navigation_enabled: bool,
    pub(super) command_timestamps_enabled: bool,
    pub(super) terminal_silence_seconds: u16,
    pub(super) terminal_link_detection_enabled: bool,
    pub(super) terminal_copy_on_select_enabled: bool,
    #[serde(skip_serializing_if = "QuickSelectPreferences::is_default")]
    pub(super) quick_select: QuickSelectPreferences,
    pub(super) show_terminal_scrollbar: bool,
    pub(super) scrollback_compression_enabled: bool,
    #[serde(skip_serializing_if = "BoldTextMode::is_default")]
    pub(super) terminal_bold_text_mode: BoldTextMode,
    pub(super) hide_terminal_scrollbars_in_fullscreen: bool,
    pub(super) terminal_transparency: u8,
    pub(super) clipboard_read_enabled: bool,
    pub(super) clipboard_write_enabled: bool,
    pub(super) terminal_font: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) terminal_font_family: Option<String>,
    pub(super) paste_warning: paste_protection::WarningMode,
    pub(super) paste_large_warning_kib: u8,
    pub(super) remote_sidebar_position: RemoteSidebarPosition,
    #[serde(deserialize_with = "WindowSettings::decode_sidebar_width")]
    pub(super) remote_sidebar_width: u16,
    pub(super) key_bindings: Bindings,
    #[serde(skip_serializing_if = "Themes::is_default")]
    pub(super) themes: Themes,
    #[serde(skip_serializing_if = "Colors::is_default")]
    pub(super) colors: Colors,
    #[serde(skip_serializing_if = "appearance::Preferences::is_default")]
    pub(super) appearance: appearance::Preferences,
    #[serde(skip_serializing_if = "crate::terminal_effects::Preferences::is_default")]
    pub(super) terminal_effects: crate::terminal_effects::Preferences,
    pub(super) journal_dock: super::journal_dock::Preferences,
    #[serde(skip_serializing_if = "super::journal_appearance::Preferences::is_default")]
    pub(super) journal_appearance: super::journal_appearance::Preferences,
    #[serde(skip_serializing_if = "default_journal_edit_split")]
    pub(super) journal_edit_split: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) journal_marker_geometry: Option<super::journal_access::dialog_geometry::Geometry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) journal_screenshot_geometry:
        Option<super::journal_access::dialog_geometry::Geometry>,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            startup: StartupPreferences::default(),
            prevent_system_sleep: false,
            open_recent: super::open_recent::Preferences::default(),
            app_logging: crate::app_logging::Settings::default(),
            session_journal: crate::session_journal::Settings::default(),
            journal_ai: crate::session_journal::topic_ai_settings::Preferences::default(),
            journal_note_translation_language: String::new(),
            journal_marker_geometry: None,
            journal_screenshot_geometry: None,
            export_branding: crate::session_journal::export_branding::Preferences::default(),
            open_tool_windows_as_tabs: false,
            snippet_structure: crate::snippet_window::structure::Preferences::default(),
            job_menu_status_enabled: true,
            connection_color_border_enabled: true,
            split_pane_focus_ring_enabled: true,
            tab_title_from_shell_enabled: true,
            tab_switch_most_recent_first: false,
            prompt_navigation_enabled: true,
            command_timestamps_enabled: false,
            terminal_silence_seconds: 30,
            terminal_link_detection_enabled: true,
            terminal_copy_on_select_enabled: true,
            quick_select: QuickSelectPreferences::default(),
            show_terminal_scrollbar: true,
            scrollback_compression_enabled: true,
            terminal_bold_text_mode: BoldTextMode::default(),
            hide_terminal_scrollbars_in_fullscreen: false,
            terminal_transparency: 0,
            clipboard_read_enabled: false,
            clipboard_write_enabled: false,
            terminal_font: DEFAULT_FONT,
            terminal_font_family: None,
            paste_warning: paste_protection::WarningMode::default(),
            paste_large_warning_kib: 5,
            remote_sidebar_position: RemoteSidebarPosition::Hidden,
            remote_sidebar_width: REMOTE_SIDEBAR_DEFAULT_WIDTH,
            key_bindings: Bindings::default(),
            themes: Themes::default(),
            colors: Colors::default(),
            appearance: appearance::Preferences::default(),
            terminal_effects: crate::terminal_effects::Preferences::default(),
            journal_dock: super::journal_dock::Preferences::default(),
            journal_appearance: super::journal_appearance::Preferences::default(),
            journal_edit_split: DEFAULT_JOURNAL_EDIT_SPLIT,
        }
    }
}

impl WindowSettings {
    pub(super) fn terminal_scrollbar_visible(&self, fullscreen: bool) -> bool {
        self.show_terminal_scrollbar && !(fullscreen && self.hide_terminal_scrollbars_in_fullscreen)
    }

    fn decode_sidebar_width<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<u16, D::Error> {
        let width = Option::<f64>::deserialize(deserializer)?
            .unwrap_or(f64::from(REMOTE_SIDEBAR_DEFAULT_WIDTH));
        Ok(width
            .clamp(
                f64::from(REMOTE_SIDEBAR_MIN_WIDTH),
                f64::from(REMOTE_SIDEBAR_MAX_WIDTH),
            )
            .round() as u16)
    }

    pub(super) fn terminal_appearance(&self) -> Appearance {
        Appearance {
            bold_text_mode: self.terminal_bold_text_mode,
            ..self.colors.appearance(self.themes.appearance())
        }
    }

    pub(super) fn set_terminal_font_family(&mut self, name: String) {
        self.terminal_font_family = (name != MONOSPACED).then_some(name);
    }

    pub(super) fn terminal_font_face(&self, theme: Option<Scheme>) -> Font {
        if let Some(theme) = theme.filter(|_| self.themes.apply_font) {
            return theme.family.font();
        }
        self.terminal_font_family
            .as_deref()
            .and_then(installed_font)
            .unwrap_or(Font::MONOSPACE)
    }

    pub(super) fn load() -> Self {
        profile::window_settings_path()
            .map_or_else(|_| Self::default(), |path| Self::read_or_default(&path))
    }

    fn read_or_default(path: &Path) -> Self {
        match Self::read(path) {
            Ok(settings) => settings,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self {
                appearance: appearance::Preferences::fresh(),
                ..Self::default()
            },
            Err(_) => Self::default(),
        }
    }

    fn read(path: &Path) -> io::Result<Self> {
        let file = profile::open_preference(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("window settings must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(PREFERENCE_LIMIT + 1).read_to_end(&mut bytes)?;
        let mut settings = Self::decode(&bytes)?;
        settings.open_recent.normalize();
        settings.startup.fixed = settings.startup.fixed.clamped();
        settings.key_bindings.preserve_user_shortcuts();
        settings.terminal_transparency = settings.terminal_transparency.min(100);
        settings.terminal_silence_seconds = settings
            .terminal_silence_seconds
            .clamp(MIN_TERMINAL_SILENCE_SECONDS, MAX_TERMINAL_SILENCE_SECONDS);
        settings.terminal_font = settings
            .terminal_font
            .clamp(profile::MIN_TERMINAL_FONT, profile::MAX_TERMINAL_FONT);
        settings.terminal_font_family = settings.terminal_font_family.filter(|name| {
            !name.trim().is_empty() && name != MONOSPACED && !name.chars().any(char::is_control)
        });
        settings.paste_large_warning_kib = settings
            .paste_large_warning_kib
            .min(paste_protection::MAX_PASTE_KIB);
        if !settings.themes.valid() {
            settings.themes = Themes::default();
        }
        if settings.journal_ai.validate().is_err() {
            settings.journal_ai = crate::session_journal::topic_ai_settings::Preferences {
                auto_screenshot_analysis: settings.journal_ai.auto_screenshot_analysis,
                ..Default::default()
            };
        }
        if QuickSelectPreferences::validate_alphabet(&settings.quick_select.alphabet).is_err() {
            settings.quick_select.alphabet = DEFAULT_ALPHABET.into();
        }
        settings.quick_select.patterns.truncate(MAX_PATTERNS);
        settings
            .quick_select
            .patterns
            .retain(|pattern| QuickSelectPreferences::validate_pattern(pattern).is_ok());
        Ok(settings)
    }

    pub(super) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
        let settings = Self::decode(bytes)?;
        settings.app_logging.validate()?;
        settings.session_journal.validate()?;
        settings.journal_ai.validate()?;
        settings.quick_select.validate()?;
        settings.validate_silence_threshold()?;
        if !settings.themes.valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "profile backup contains invalid terminal themes",
            ));
        }
        Ok(())
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > PREFERENCE_LIMIT {
            return Err(io::Error::other("window settings exceed the size limit"));
        }
        Ok(serde_json::from_slice(bytes)?)
    }

    fn validate_silence_threshold(&self) -> io::Result<()> {
        if !(MIN_TERMINAL_SILENCE_SECONDS..=MAX_TERMINAL_SILENCE_SECONDS)
            .contains(&self.terminal_silence_seconds)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal silence threshold is outside 5 to 3600 seconds",
            ));
        }
        Ok(())
    }

    pub(super) fn save(&self, path: &Path) -> io::Result<()> {
        self.app_logging.validate()?;
        self.session_journal.validate()?;
        self.journal_ai.validate()?;
        self.quick_select.validate()?;
        self.validate_silence_threshold()?;
        if !self.themes.valid() {
            return Err(io::Error::other("invalid terminal theme settings"));
        }
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > PREFERENCE_LIMIT {
            return Err(io::Error::other("window settings exceed the size limit"));
        }
        profile::save_atomic(path, &bytes)
    }
}

impl Workspace {
    pub(super) fn change_shell_title_visibility(&mut self, id: window::Id, enabled: bool) {
        if enabled == self.window_settings.tab_title_from_shell_enabled {
            return;
        }
        self.window_settings.tab_title_from_shell_enabled = enabled;
        self.save_window_settings(Some(id));
    }

    pub(super) fn change_prompt_navigation(&mut self, id: window::Id, enabled: bool) {
        if enabled == self.window_settings.prompt_navigation_enabled {
            return;
        }
        self.window_settings.prompt_navigation_enabled = enabled;
        self.apply_prompt_navigation();
        self.save_window_settings(Some(id));
    }

    pub(super) fn apply_prompt_navigation(&self) {
        let enabled = self.window_settings.prompt_navigation_enabled;
        for pane in self.sessions.values() {
            pane.session.set_prompt_navigation_enabled(enabled);
        }
    }

    pub(super) fn set_remote_sidebar_position(
        &mut self,
        position: RemoteSidebarPosition,
        owner: Option<window::Id>,
    ) -> Task<Message> {
        if position == RemoteSidebarPosition::Hidden
            && self.window_settings.remote_sidebar_position != position
            && let Some(confirm) = self.confirm_remote_sidebar_hide(owner)
        {
            return confirm;
        }
        self.apply_remote_sidebar_position(position, owner)
    }

    pub(super) fn apply_remote_sidebar_position(
        &mut self,
        position: RemoteSidebarPosition,
        owner: Option<window::Id>,
    ) -> Task<Message> {
        if self.window_settings.remote_sidebar_position == position {
            return Task::none();
        }
        self.window_settings.remote_sidebar_position = position;
        self.save_window_settings(owner);
        self.remote_sidebar_preferences_changed()
    }

    pub(super) fn save_remote_sidebar_width(&mut self, width: u16, owner: window::Id) {
        self.window_settings.remote_sidebar_width =
            width.clamp(REMOTE_SIDEBAR_MIN_WIDTH, REMOTE_SIDEBAR_MAX_WIDTH);
        self.save_window_settings(Some(owner));
    }

    pub(super) fn save_window_settings(&mut self, owner: Option<window::Id>) {
        if let Err(error) =
            profile::window_settings_path().and_then(|path| self.window_settings.save(&path))
        {
            let message = format!("Could not save window settings ({error})");
            if let Some(id) = owner {
                self.notice(id, message);
            } else {
                eprintln!("{message}");
            }
        }
    }

    /// The visible name and, when it came from the shell, the connection label.
    pub(super) fn tab_names(&self, tab: &Tab) -> (String, Option<String>) {
        let (automatic, connection) = self.automatic_tab_names(tab);
        if let Some(title) = &tab.custom_title {
            return (title.clone(), Some(connection.unwrap_or(automatic)));
        }
        (automatic, connection)
    }

    pub(super) fn automatic_tab_names(&self, tab: &Tab) -> (String, Option<String>) {
        let Some(pane) = tab
            .focused_session()
            .and_then(|session| self.sessions.get(&session))
        else {
            return (DEFAULT_TERMINAL_TITLE.into(), None);
        };
        let connection = if let Some(ssh) = &pane.ssh {
            ssh.label()
        } else if let Some(ibm) = &pane.frame.ibm {
            ibm.kind.label().into()
        } else if let Some(serial) = &pane.frame.serial {
            format!(
                "{} @ {}",
                serial.device.display(),
                serial.settings.baud_rate
            )
        } else if let Some(title) = pane
            .local_security
            .as_ref()
            .and_then(|settings| settings.shell.preset_title())
        {
            title.into()
        } else if pane.frame.screen.profile == TerminalProfile::Xterm {
            DEFAULT_TERMINAL_TITLE.into()
        } else {
            pane.frame.screen.profile.to_string()
        };
        let shell_title = &pane.frame.screen.title;
        if self.window_settings.tab_title_from_shell_enabled
            && !shell_title.is_empty()
            && shell_title != DEFAULT_TERMINAL_TITLE
        {
            (shell_title.clone(), Some(connection))
        } else {
            (connection, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_scrollbar_preferences_keep_legacy_profiles_visible() {
        let mut settings: WindowSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.terminal_scrollbar_visible(false));
        assert!(settings.terminal_scrollbar_visible(true));
        settings.hide_terminal_scrollbars_in_fullscreen = true;
        assert!(settings.terminal_scrollbar_visible(false));
        assert!(!settings.terminal_scrollbar_visible(true));
        settings.show_terminal_scrollbar = false;
        assert!(!settings.terminal_scrollbar_visible(false));
        assert!(!settings.terminal_scrollbar_visible(true));
        let saved = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<WindowSettings>(&saved).unwrap(),
            settings
        );
    }

    #[test]
    fn settings_save_atomically_and_old_profiles_keep_clipboard_off() {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "kortty-window-settings-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("window-settings.json");
        assert!(WindowSettings::default().tab_title_from_shell_enabled);
        assert!(WindowSettings::default().prompt_navigation_enabled);
        assert!(!WindowSettings::default().command_timestamps_enabled);
        assert_eq!(
            WindowSettings::read(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let fresh = WindowSettings::read_or_default(&path);
        assert!(fresh.job_menu_status_enabled);
        assert!(fresh.journal_ai.auto_screenshot_analysis);
        assert_eq!(
            fresh.appearance.design,
            appearance::Design::AtlantaFxPrimerDark
        );
        assert!(fresh.appearance.animations);
        let preferences = WindowSettings {
            open_tool_windows_as_tabs: true,
            job_menu_status_enabled: false,
            connection_color_border_enabled: false,
            split_pane_focus_ring_enabled: false,
            tab_title_from_shell_enabled: false,
            prompt_navigation_enabled: false,
            command_timestamps_enabled: true,
            clipboard_read_enabled: true,
            clipboard_write_enabled: true,
            terminal_font: profile::MAX_TERMINAL_FONT,
            terminal_font_family: Some("Font absent from this machine 76201".into()),
            paste_warning: paste_protection::WarningMode::Always,
            paste_large_warning_kib: 10,
            remote_sidebar_position: RemoteSidebarPosition::Left,
            remote_sidebar_width: 420,
            ..WindowSettings::default()
        };
        preferences.save(&path).unwrap();
        let loaded = WindowSettings::read(&path).unwrap();
        assert_eq!(loaded, preferences);
        assert_eq!(loaded.terminal_font_face(None), Font::MONOSPACE);
        std::fs::write(&path, r#"{"tab_title_from_shell_enabled":false}"#).unwrap();
        let old_profile = WindowSettings::read(&path).unwrap();
        assert!(old_profile.job_menu_status_enabled);
        assert!(old_profile.journal_ai.auto_screenshot_analysis);
        assert!(!old_profile.open_tool_windows_as_tabs);
        assert!(old_profile.connection_color_border_enabled);
        assert!(old_profile.split_pane_focus_ring_enabled);
        assert_eq!(old_profile.appearance.design, appearance::Design::Normal);
        assert!(old_profile.appearance.animations);
        assert!(old_profile.prompt_navigation_enabled);
        assert!(!old_profile.command_timestamps_enabled);
        assert!(!old_profile.clipboard_read_enabled && !old_profile.clipboard_write_enabled);
        assert_eq!(old_profile.terminal_font, DEFAULT_FONT);
        assert!(old_profile.terminal_font_family.is_none());
        assert_eq!(
            old_profile.remote_sidebar_position,
            RemoteSidebarPosition::Hidden
        );
        assert_eq!(
            old_profile.remote_sidebar_width,
            REMOTE_SIDEBAR_DEFAULT_WIDTH
        );
        assert_eq!(
            old_profile.paste_warning,
            paste_protection::WarningMode::UnlessBracketed
        );
        assert_eq!(old_profile.paste_large_warning_kib, 5);
        std::fs::write(
            &path,
            r#"{"remote_sidebar_position":"unknown","remote_sidebar_width":99999}"#,
        )
        .unwrap();
        let out_of_range = WindowSettings::read(&path).unwrap();
        assert_eq!(
            out_of_range.remote_sidebar_position,
            RemoteSidebarPosition::Hidden
        );
        assert_eq!(out_of_range.remote_sidebar_width, REMOTE_SIDEBAR_MAX_WIDTH);
        std::fs::write(
            &path,
            r#"{"journal_ai":{"enabled":true,"endpoint":"invalid","vision_enabled":true,"auto_screenshot_analysis":false}}"#,
        )
        .unwrap();
        let recovered = WindowSettings::read(&path).unwrap();
        assert!(!recovered.journal_ai.auto_screenshot_analysis);
        assert!(recovered.journal_ai.vision_profile().unwrap().is_none());
        recovered.save(&path).unwrap();
        assert_eq!(WindowSettings::read(&path).unwrap(), recovered);
        WindowSettings::default().save(&path).unwrap();
        assert!(
            WindowSettings::read(&path)
                .unwrap()
                .tab_title_from_shell_enabled
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prompt_navigation_shortcut_is_local_even_with_broadcast_enabled() {
        use std::time::{Duration, Instant};

        use iced::keyboard::{Key, Location, Modifiers, key::Named};
        use iced::widget::pane_grid;

        use crate::session::{Frame, GridSize, Session, Status};

        fn wait(session: &Session, reason: &str, ready: impl Fn(&Frame) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let frame = session.frame();
                if ready(&frame) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "PTY frame did not arrive: {reason} ({})",
                    crate::workspace::tests::describe_test_pty(session, &frame),
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(true);
        let other = workspace.windows[&window]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        let _ = workspace.split(window, pane_grid::Axis::Vertical);
        workspace.wait_for_test_split(window);
        let tab = workspace.active_tab_mut(window).unwrap();
        tab.broadcast = true;
        let focused = tab.focused_session().unwrap();
        assert_ne!(other, focused);
        for id in [focused, other] {
            let session = &workspace.sessions[&id].session;
            session.resize(GridSize {
                rows: 4,
                ..GridSize::default()
            });
            wait(session, "four-row grid", |frame| {
                frame.status == Status::Running && frame.screen.rows == 4
            });
            session
                .send(
                    concat!(
                        r"printf '\033]133;A\007%s\033]133;C\007\n\n\n\n\n%s-live\n' ",
                        "'prompt-shortcut' 'prompt-shortcut'\r",
                    )
                    .as_bytes()
                    .to_vec(),
                )
                .unwrap();
            wait(session, "prompt marker output", |frame| {
                frame.screen.scrollback_lines >= 2
                    && frame
                        .screen
                        .cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("prompt-shortcut-live")
            });
        }
        let secondary = &workspace.sessions[&other].session;
        secondary.scroll(1);
        wait(secondary, "secondary pane scroll", |frame| {
            frame.scroll_offset == 1
        });
        let _ = workspace.key(
            window,
            Key::Named(Named::ArrowUp),
            Location::Standard,
            Modifiers::COMMAND | Modifiers::SHIFT,
            None,
        );
        wait(
            &workspace.sessions[&focused].session,
            "focused prompt jump",
            |frame| {
                frame.scroll_offset > 0
                    && frame.screen.row(0).is_some_and(|row| {
                        row.iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            == "prompt-shortcut"
                    })
            },
        );
        assert_eq!(workspace.sessions[&other].session.frame().scroll_offset, 1);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_title_follows_the_focused_live_pty_and_can_be_hidden() {
        use std::time::{Duration, Instant};

        use iced::widget::pane_grid;

        use crate::session::SessionId;
        use crate::workspace::Message;

        fn host_title(workspace: &mut Workspace, id: SessionId, command: &[u8], title: &str) {
            let session = &workspace.sessions[&id].session;
            session.send(command.to_vec()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let frame = session.frame();
                if frame.screen.title == title {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "PTY never published title {title:?} ({})",
                    crate::workspace::tests::describe_test_pty(session, &frame),
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = workspace.update(Message::SessionUpdate(id));
        }

        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(true);
        let initial_window_title = workspace.title(window);
        let tab = workspace.windows[&window].tab().unwrap();
        let first_pane = tab.focused;
        let first = tab.focused_session().unwrap();
        let _ = workspace.split(window, pane_grid::Axis::Vertical);
        workspace.wait_for_test_split(window);
        let tab = workspace.windows[&window].tab().unwrap();
        let second_pane = tab.focused;
        let second = tab.focused_session().unwrap();
        assert_ne!(first, second);

        host_title(
            &mut workspace,
            first,
            b"printf '\\033]0; first \\007'\r",
            "first",
        );
        let names =
            |workspace: &Workspace| workspace.tab_names(workspace.windows[&window].tab().unwrap());
        assert_eq!(names(&workspace), ("Terminal".into(), None));
        host_title(
            &mut workspace,
            second,
            b"printf '\\033]2;second\\007'\r",
            "second",
        );
        assert_eq!(
            names(&workspace),
            ("second".into(), Some("Terminal".into()))
        );
        let _ = workspace.update(Message::FocusPane(window, first_pane));
        assert_eq!(names(&workspace), ("first".into(), Some("Terminal".into())));
        assert_eq!(workspace.title(window), initial_window_title);

        workspace.window_settings.tab_title_from_shell_enabled = false;
        host_title(
            &mut workspace,
            first,
            b"printf '\\033[22;0t\\033]2;vim\\007'\r",
            "vim",
        );
        assert_eq!(names(&workspace), ("Terminal".into(), None));
        workspace.window_settings.tab_title_from_shell_enabled = true;
        assert_eq!(names(&workspace), ("vim".into(), Some("Terminal".into())));
        host_title(&mut workspace, first, b"printf '\\033[23;0t'\r", "first");
        assert_eq!(names(&workspace), ("first".into(), Some("Terminal".into())));
        host_title(
            &mut workspace,
            first,
            b"printf '\\033]2;Terminal\\007'\r",
            "Terminal",
        );
        assert_eq!(names(&workspace), ("Terminal".into(), None));

        let _ = workspace.update(Message::FocusPane(window, second_pane));
        assert_eq!(names(&workspace).0, "second");
        let _ = workspace.close_pane(window, second_pane);
        let _ = workspace.update(Message::SessionUpdate(second));
        assert_eq!(names(&workspace), ("Terminal".into(), None));
    }
}
