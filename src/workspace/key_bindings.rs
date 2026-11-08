use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use iced::keyboard::{Key, Modifiers, key::Named};
use serde::{Deserialize, Serialize};

use super::chrome::Action;

const MENU_BAR_SHORTCUT_REQUIRED: &str = "Keep a shortcut available to restore the menu bar.";

mod editor;
pub use editor::Edit;
pub(super) use editor::Editor;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
enum MainKey {
    Character(char),
    Function(u8),
    Backspace,
    Delete,
    Down,
    End,
    Enter,
    Escape,
    Home,
    Insert,
    Left,
    PageDown,
    PageUp,
    Right,
    Space,
    Tab,
    Up,
}

impl MainKey {
    fn supported_character(ch: char) -> bool {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ",./\\;'[]-=`+".contains(ch)
    }

    fn from_event(key: &Key) -> Option<Self> {
        use Named as N;

        Some(match key {
            Key::Character(text) => {
                let bytes = text.as_bytes();
                let [byte] = bytes else { return None };
                let ch = char::from(*byte).to_ascii_lowercase();
                if !Self::supported_character(ch) {
                    return None;
                }
                Self::Character(ch)
            }
            Key::Named(key) => match key {
                N::F1 => Self::Function(1),
                N::F2 => Self::Function(2),
                N::F3 => Self::Function(3),
                N::F4 => Self::Function(4),
                N::F5 => Self::Function(5),
                N::F6 => Self::Function(6),
                N::F7 => Self::Function(7),
                N::F8 => Self::Function(8),
                N::F9 => Self::Function(9),
                N::F10 => Self::Function(10),
                N::F11 => Self::Function(11),
                N::F12 => Self::Function(12),
                N::Backspace => Self::Backspace,
                N::Delete => Self::Delete,
                N::ArrowDown => Self::Down,
                N::End => Self::End,
                N::Enter => Self::Enter,
                N::Escape => Self::Escape,
                N::Home => Self::Home,
                N::Insert => Self::Insert,
                N::ArrowLeft => Self::Left,
                N::PageDown => Self::PageDown,
                N::PageUp => Self::PageUp,
                N::ArrowRight => Self::Right,
                N::Space => Self::Space,
                N::Tab => Self::Tab,
                N::ArrowUp => Self::Up,
                _ => return None,
            },
            Key::Unidentified => return None,
        })
    }

    fn printable(self) -> bool {
        matches!(self, Self::Character(_) | Self::Space)
    }
}

impl fmt::Display for MainKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Character(ch) => return write!(formatter, "{}", ch.to_ascii_uppercase()),
            Self::Function(number) => return write!(formatter, "F{number}"),
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Down => "Down",
            Self::End => "End",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Home => "Home",
            Self::Insert => "Insert",
            Self::Left => "Left",
            Self::PageDown => "Page Down",
            Self::PageUp => "Page Up",
            Self::Right => "Right",
            Self::Space => "Space",
            Self::Tab => "Tab",
            Self::Up => "Up",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(super) struct Chord {
    key: MainKey,
    shortcut: bool,
    control: bool,
    shift: bool,
    alt: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Platform {
    Mac,
    Windows,
    Linux,
}

const PLATFORM: Platform = if cfg!(target_os = "macos") {
    Platform::Mac
} else if cfg!(target_os = "windows") {
    Platform::Windows
} else {
    Platform::Linux
};

impl Chord {
    pub(super) fn capture(key: &Key, modifiers: Modifiers) -> Option<Self> {
        Self::from_event(key, modifiers, PLATFORM)
    }

    fn from_event(key: &Key, modifiers: Modifiers, platform: Platform) -> Option<Self> {
        let mac = platform == Platform::Mac;
        if !mac && modifiers.logo() {
            return None;
        }
        Some(Self {
            key: MainKey::from_event(key)?,
            shortcut: if mac {
                modifiers.logo()
            } else {
                modifiers.control()
            },
            control: mac && modifiers.control(),
            shift: modifiers.shift(),
            alt: modifiers.alt(),
        })
    }

    const fn primary(ch: char, shift: bool) -> Self {
        Self::primary_key(MainKey::Character(ch), shift, false)
    }

    const fn primary_key(key: MainKey, shift: bool, alt: bool) -> Self {
        Self {
            key,
            shortcut: true,
            control: false,
            shift,
            alt,
        }
    }

    fn physical(mut self, platform: Platform) -> Self {
        if platform != Platform::Mac {
            self.shortcut |= self.control;
            self.control = false;
        }
        self
    }

    fn terminal_zoom(self, platform: Platform) -> Option<Action> {
        let chord = self.physical(platform);
        let zoom_modifier = if platform == Platform::Mac {
            chord.shortcut && !chord.control && !chord.alt
        } else {
            chord.shortcut != chord.alt
        };
        if !zoom_modifier {
            return None;
        }
        match chord.key {
            MainKey::Character('+' | '=') => Some(Action::ZoomIn),
            MainKey::Character('-') => Some(Action::ZoomOut),
            MainKey::Character('0') => Some(Action::ResetZoom),
            _ => None,
        }
    }

    fn rejected(self, platform: Platform) -> Option<&'static str> {
        use MainKey::*;

        let mac = platform == Platform::Mac;
        let pressed = self.physical(platform);
        let ctrl = if mac {
            pressed.control
        } else {
            pressed.shortcut
        };
        let meta = mac && pressed.shortcut;
        let Self {
            key, shift, alt, ..
        } = pressed;
        if matches!(key, Function(12) | Tab | Escape)
            || (pressed.shortcut
                && !pressed.control
                && !alt
                && (matches!(key, Character('1'..='9')) || (shift && matches!(key, Up | Down))))
            || (ctrl && alt && matches!(key, Left | Right | Up | Down))
        {
            return Some("kortty reserves this key for navigation, window controls or zoom.");
        }
        if (pressed.shortcut
            && !pressed.control
            && !alt
            && !shift
            && (matches!(key, Up | Down) || (mac && key == Character('k'))))
            || (!ctrl && !meta && !alt && shift && matches!(key, PageUp | PageDown))
        {
            return Some("The terminal reserves this key combination.");
        }
        if (mac
            && ((meta && matches!(key, Space | Character('h')))
                || (meta && shift && matches!(key, Character('q' | '3'..='6')))
                || (meta && alt && matches!(key, Character('d')))
                || (meta && !ctrl && !alt && !shift && key == Character('`'))
                || (meta && ctrl && !alt && !shift && matches!(key, Character('q' | 'f')))
                || (ctrl && !meta && !alt && matches!(key, Up | Down))))
            || (!mac
                && ((alt && !ctrl && matches!(key, Function(4) | Space))
                    || (alt && ctrl && matches!(key, Delete))
                    || (platform == Platform::Linux
                        && alt
                        && ctrl
                        && matches!(key, Backspace | Function(1..=12)))))
        {
            return Some("This key combination is reserved by the operating system.");
        }
        if !meta
            && !alt
            && ctrl
            && ((!shift && matches!(key, Character('a'..='z' | '[' | ']' | '\\' | '/') | Space))
                || (shift && matches!(key, Character('2' | '6' | '-'))))
            || (!meta && ctrl != alt && matches!(key, Left | Right | Backspace | Delete))
            || (!mac && !ctrl && alt && key.printable())
        {
            return Some("The terminal uses this key combination for shell input.");
        }
        if key.printable()
            && ((mac && alt && !meta && !ctrl) || (platform == Platform::Windows && alt && ctrl))
        {
            return Some("This key combination types a character on some keyboard layouts.");
        }
        if !ctrl && !meta && !alt && !matches!(key, Function(1..=12)) {
            return Some("Use a key with Ctrl, Alt or Command, or choose a function key.");
        }
        if matches!(key, Function(0 | 13..=u8::MAX))
            || matches!(key, Character(ch) if !MainKey::supported_character(ch))
        {
            return Some("This key cannot be used for a shortcut.");
        }
        None
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mac = PLATFORM == Platform::Mac;
        let pressed = self.physical(PLATFORM);
        if mac && pressed.shortcut {
            formatter.write_str("Cmd+")?;
        }
        if pressed.control || (!mac && pressed.shortcut) {
            formatter.write_str("Ctrl+")?;
        }
        if pressed.alt {
            formatter.write_str(if mac { "Option+" } else { "Alt+" })?;
        }
        if pressed.shift {
            formatter.write_str("Shift+")?;
        }
        write!(formatter, "{}", pressed.key)
    }
}

pub(super) struct Command {
    pub(super) action: Action,
    pub(super) title: &'static str,
    pub(super) menu: &'static str,
    id: String,
    aliases: Vec<&'static str>,
}

impl Command {
    pub(super) fn matches_filter(&self, query: &str, shortcut: &str) -> bool {
        [self.title, self.menu, shortcut]
            .into_iter()
            .chain(self.aliases.iter().copied())
            .any(|field| field.to_ascii_lowercase().contains(query))
    }

    pub(super) fn fixed(&self) -> bool {
        matches!(
            self.action,
            Action::CopySelection
                | Action::Paste
                | Action::Find
                | Action::Quit
                | Action::NextTab
                | Action::PreviousTab
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ResetZoom
        )
    }

    pub(super) fn default_chord(&self) -> Option<Chord> {
        self.defaults(PLATFORM)[0]
    }

    fn defaults(&self, platform: Platform) -> [Option<Chord>; 2] {
        use Action::*;

        let mac = platform == Platform::Mac;
        let key = match self.action {
            QuickConnect if !mac => {
                return [
                    Some(Chord::primary_key(MainKey::Function(2), true, false)),
                    None,
                ];
            }
            QuickConnect => 't',
            NewWindow => 'n',
            OpenProject => return [Some(Chord::primary('o', false)), None],
            SaveProject => return [Some(Chord::primary('s', false)), None],
            CloseTab => 'w',
            CloseWindow if mac => return [Some(Chord::primary('w', true)), None],
            Quit => 'q',
            CopySelection => 'c',
            Paste => return [Some(Chord::primary('v', !mac)), None],
            Find => return [mac.then_some(Chord::primary('f', false)), None],
            Manual => {
                return [
                    Some(Chord {
                        key: MainKey::Function(1),
                        shortcut: false,
                        control: false,
                        shift: false,
                        alt: false,
                    }),
                    None,
                ];
            }
            QuickSelect => return [Some(Chord::primary_key(MainKey::Space, true, false)), None],
            TerminalOnlyFullscreen => return [Some(Chord::primary('f', true)), None],
            ShowCommandTimestamps => return [Some(Chord::primary('t', true)), None],
            ShowMenuBar => return [Some(Chord::primary('l', true)), None],
            ZoomIn => return [Some(Chord::primary('+', false)), None],
            ZoomOut => return [Some(Chord::primary('-', false)), None],
            ResetZoom => return [Some(Chord::primary('0', false)), None],
            ReopenClosedTab => {
                return [
                    Some(Chord::primary_key(MainKey::Character('t'), true, true)),
                    None,
                ];
            }
            ManageConnections => 'm',
            Sftp => return [Some(Chord::primary('u', true)), None],
            Settings => ',',
            Recordings => return [mac.then_some(Chord::primary('v', true)), None],
            ToggleRecording => return [Some(Chord::primary('e', true)), None],
            LogfileManager => return [Some(Chord::primary('x', true)), None],
            AsciiArt => return [Some(Chord::primary('a', true)), None],
            Snippets => return [Some(Chord::primary('s', true)), None],
            Scheduler => return [Some(Chord::primary('j', true)), None],
            SessionJournals => {
                return [
                    Some(Chord::primary_key(MainKey::Character('j'), false, true)),
                    None,
                ];
            }
            ToggleSessionJournal | SessionJournalScreenshot | ToggleJournalDock
                if platform != Platform::Windows =>
            {
                let key = match self.action {
                    ToggleSessionJournal => 't',
                    SessionJournalScreenshot => 'c',
                    _ => 'l',
                };
                return [
                    Some(Chord::primary_key(MainKey::Character(key), false, true)),
                    None,
                ];
            }
            OpenPgpRecipients => return [Some(Chord::primary('g', true)), None],
            SshKeys => return [Some(Chord::primary('i', true)), None],
            CreateBackup => return [Some(Chord::primary('b', true)), None],
            CommandPalette => return [Some(Chord::primary('p', true)), None],
            NextTab | PreviousTab => {
                return [
                    Some(Chord {
                        key: MainKey::Tab,
                        shortcut: !mac,
                        control: mac,
                        shift: self.action == PreviousTab,
                        alt: false,
                    }),
                    None,
                ];
            }
            SplitPane => return [Some(Chord::primary('o', true)), None],
            ZoomPane => return [Some(Chord::primary_key(MainKey::Enter, true, false)), None],
            FocusPaneLeft | FocusPaneRight | FocusPaneAbove | FocusPaneBelow => {
                let direction = match self.action {
                    FocusPaneLeft => MainKey::Left,
                    FocusPaneRight => MainKey::Right,
                    FocusPaneAbove => MainKey::Up,
                    _ => MainKey::Down,
                };
                return [Some(Chord::primary_key(direction, false, true)), None];
            }
            FilesLeft => return [Some(Chord::primary('k', true)), None],
            FilesRight => return [Some(Chord::primary('r', true)), None],
            _ => return [None, None],
        };
        let alternate = if mac && self.action == QuickConnect {
            Some(Chord::primary('k', false))
        } else if mac && !matches!(self.action, CloseTab | Quit) {
            Some(Chord::primary(key, true))
        } else {
            None
        };
        [Some(Chord::primary(key, !mac)), alternate]
    }
}

pub(super) fn catalog() -> &'static [Command] {
    static CATALOG: OnceLock<Vec<Command>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut entries = Vec::<Command>::new();
        for (action, title, menu) in Action::shortcut_catalog() {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.action == action) {
                entry.aliases.extend([title, menu]);
                continue;
            }
            let Some(serde_json::Value::String(id)) = serde_json::to_value(action).ok() else {
                continue;
            };
            entries.push(Command {
                action,
                title,
                menu,
                id,
                aliases: Vec::new(),
            });
        }
        entries
    })
}

pub(super) fn command(action: Action) -> Option<&'static Command> {
    catalog().iter().find(|entry| entry.action == action)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(super) struct Bindings(BTreeMap<String, Override>);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum Override {
    Parsed(Option<Chord>),
    Unrecognized(serde_json::Value),
}

impl Bindings {
    pub(super) fn preserve_user_shortcuts(&mut self) {
        use Action::*;

        let introduced_defaults = [
            Manual,
            OpenProject,
            SaveProject,
            Sftp,
            Recordings,
            ToggleRecording,
            LogfileManager,
            AsciiArt,
            QuickSelect,
            Snippets,
            Scheduler,
            SessionJournals,
            ToggleSessionJournal,
            SessionJournalScreenshot,
            ToggleJournalDock,
            OpenPgpRecipients,
            SshKeys,
            CreateBackup,
            TerminalOnlyFullscreen,
            ReopenClosedTab,
            CommandPalette,
            SplitPane,
            ZoomPane,
            FocusPaneLeft,
            FocusPaneRight,
            FocusPaneAbove,
            FocusPaneBelow,
            ShowCommandTimestamps,
        ];
        for new_action in introduced_defaults
            .into_iter()
            .chain((PLATFORM != Platform::Mac).then_some(QuickConnect))
            .filter_map(command)
        {
            if !self.0.contains_key(&new_action.id)
                && catalog().iter().any(|entry| {
                    matches!(self.0.get(&entry.id), Some(Override::Parsed(Some(chord)))
                        if Some(chord.physical(PLATFORM)) == new_action.default_chord()
                            && self.problem(entry).is_none())
                })
            {
                self.0.insert(new_action.id.clone(), Override::Parsed(None));
            }
        }

        let Some(menu_bar) = command(ShowMenuBar) else {
            return;
        };
        if self.changed(menu_bar) && self.problem(menu_bar).is_none() {
            return;
        }
        let occupied = |chord| {
            catalog().iter().any(|entry| {
                entry.action != ShowMenuBar && self.effective(entry).contains(&Some(chord))
            })
        };
        if menu_bar.default_chord().is_some_and(occupied)
            && let Some(recovery) = (3..=11)
                .map(|function| Chord::primary_key(MainKey::Function(function), true, false))
                .find(|&chord| !occupied(chord))
        {
            self.0
                .insert(menu_bar.id.clone(), Override::Parsed(Some(recovery)));
        }
    }

    pub(super) fn update(
        &mut self,
        action: Action,
        chord: Option<Chord>,
    ) -> Result<(), &'static str> {
        let entry = command(action).ok_or("This command is unavailable.")?;
        if entry.fixed() {
            return Err("This shortcut is fixed.");
        }
        if action == Action::ShowMenuBar && chord.is_none() {
            return Err(MENU_BAR_SHORTCUT_REQUIRED);
        }
        if chord.is_some_and(|chord| {
            entry
                .defaults(PLATFORM)
                .contains(&Some(chord.physical(PLATFORM)))
        }) {
            self.reset(action);
            return Ok(());
        }
        if let Some(problem) = chord.and_then(|chord| Self::rejection(chord, PLATFORM)) {
            return Err(problem);
        }
        if chord.is_none() && entry.default_chord().is_none() {
            self.reset(action);
        } else {
            self.0.insert(entry.id.clone(), Override::Parsed(chord));
        }
        Ok(())
    }

    pub(super) fn reset(&mut self, action: Action) {
        if let Some(entry) = command(action) {
            self.0.remove(&entry.id);
        }
    }

    pub(super) fn reset_all(&mut self) {
        for entry in catalog() {
            self.0.remove(&entry.id);
        }
    }

    pub(super) fn changed(&self, entry: &Command) -> bool {
        self.0.contains_key(&entry.id)
    }

    pub(super) fn unknown_count(&self) -> usize {
        self.0
            .keys()
            .filter(|id| !catalog().iter().any(|entry| &entry.id == *id))
            .count()
    }

    pub(super) fn problem(&self, entry: &Command) -> Option<&'static str> {
        let stored = self.0.get(&entry.id)?;
        if entry.fixed() {
            return Some("This shortcut is fixed.");
        }
        match stored {
            Override::Unrecognized(_) => Some("The stored shortcut is unreadable here."),
            Override::Parsed(None) if entry.action == Action::ShowMenuBar => {
                Some(MENU_BAR_SHORTCUT_REQUIRED)
            }
            Override::Parsed(Some(chord))
                if !entry
                    .defaults(PLATFORM)
                    .contains(&Some(chord.physical(PLATFORM))) =>
            {
                Self::rejection(*chord, PLATFORM)
            }
            Override::Parsed(_) => None,
        }
    }

    pub(super) fn shortcut(&self, entry: &Command) -> Option<Chord> {
        self.effective(entry)[0]
    }

    pub(super) fn menu_hint(&self, action: Action) -> Option<String> {
        let entry = command(action)?;
        let default = entry.default_chord();
        let effective = self.shortcut(entry);
        let chord = if effective != default && !self.conflicts().is_empty() {
            default
        } else {
            effective
        };
        chord
            .map(|chord| chord.to_string())
            .or_else(|| (action == Action::Fullscreen).then(|| "F12".to_owned()))
    }

    pub(super) fn conflicts(&self) -> Vec<(Chord, Vec<Action>)> {
        if self.0.is_empty() {
            return Vec::new();
        }
        let mut owners: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for entry in catalog() {
            for chord in self.effective(entry).into_iter().flatten() {
                owners.entry(chord).or_default().push(entry.action);
            }
        }
        owners
            .into_iter()
            .filter(|(_, actions)| actions.len() > 1)
            .collect()
    }

    pub(super) fn action_for(&self, key: &Key, modifiers: Modifiers) -> Option<Action> {
        let chord = Chord::capture(key, modifiers)?.physical(PLATFORM);
        if let Some(action) = chord.terminal_zoom(PLATFORM) {
            return Some(action);
        }
        if !chord.shortcut
            && !chord.control
            && !chord.alt
            && !matches!(chord.key, MainKey::Function(_))
        {
            return None;
        }
        let default = catalog()
            .iter()
            .find(|entry| entry.defaults(PLATFORM).contains(&Some(chord)))
            .map(|entry| entry.action);
        if self.0.is_empty() {
            return default;
        }
        let changed = catalog()
            .iter()
            .find(|entry| self.effective(entry).contains(&Some(chord)))
            .map(|entry| entry.action);
        if changed != default && !self.conflicts().is_empty() {
            return default;
        }
        changed
    }

    fn effective(&self, entry: &Command) -> [Option<Chord>; 2] {
        let defaults = entry.defaults(PLATFORM);
        match self.0.get(&entry.id) {
            Some(Override::Parsed(chord)) if !entry.fixed() && self.problem(entry).is_none() => {
                let chord = chord.map(|chord| chord.physical(PLATFORM));
                if chord.is_some() && defaults.contains(&chord) {
                    defaults
                } else {
                    [chord, None]
                }
            }
            _ => defaults,
        }
    }

    fn rejection(chord: Chord, platform: Platform) -> Option<&'static str> {
        if chord.terminal_zoom(platform).is_some()
            || catalog().iter().any(|entry| {
                entry.fixed()
                    && entry
                        .defaults(platform)
                        .contains(&Some(chord.physical(platform)))
            })
        {
            return Some("A fixed shortcut already uses this key combination.");
        }
        chord.rejected(platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_file_shortcuts_use_the_exact_primary_chord() {
        for (action, key) in [(Action::OpenProject, "o"), (Action::SaveProject, "s")] {
            let command = command(action).unwrap();
            for (platform, primary) in [
                (Platform::Mac, Modifiers::LOGO),
                (Platform::Linux, Modifiers::CTRL),
                (Platform::Windows, Modifiers::CTRL),
            ] {
                let key = Key::Character(key.into());
                assert_eq!(
                    command.defaults(platform),
                    [Chord::from_event(&key, primary, platform), None]
                );
            }
        }
    }

    #[test]
    fn sftp_client_uses_primary_shift_u_and_preserves_an_existing_binding() {
        let sftp = command(Action::Sftp).unwrap();
        let key = Key::Character("u".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let expected = Chord::from_event(&key, primary | Modifiers::SHIFT, platform);
            assert_eq!(sftp.defaults(platform), [expected, None]);
        }
        assert!(command(Action::SftpHere).unwrap().default_chord().is_none());
        assert!(
            command(Action::SendSerialBreak)
                .unwrap()
                .default_chord()
                .is_none()
        );
        let mut bindings = Bindings::default();
        bindings
            .update(Action::NewWindow, sftp.default_chord())
            .unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(
            bindings.action_for(&key, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(Action::NewWindow)
        );
        assert!(bindings.shortcut(sftp).is_none());
    }

    #[test]
    fn terminal_zoom_has_fixed_view_shortcuts_and_preserves_option_and_altgr_input() {
        for (key, action) in [
            ("+", Action::ZoomIn),
            ("=", Action::ZoomIn),
            ("-", Action::ZoomOut),
            ("0", Action::ResetZoom),
        ] {
            let entry = command(action).unwrap();
            assert!(entry.fixed());
            assert_eq!(entry.menu, "View");
            let key = Key::Character(key.into());
            for (platform, primary) in [
                (Platform::Mac, Modifiers::LOGO),
                (Platform::Linux, Modifiers::CTRL),
                (Platform::Windows, Modifiers::CTRL),
            ] {
                let match_key = |modifiers| {
                    Chord::from_event(&key, modifiers, platform)
                        .and_then(|chord| chord.terminal_zoom(platform))
                };
                assert_eq!(match_key(primary), Some(action));
                assert_eq!(match_key(primary | Modifiers::SHIFT), Some(action));
                assert_eq!(match_key(primary | Modifiers::ALT), None);
                assert_eq!(
                    match_key(Modifiers::ALT),
                    (platform != Platform::Mac).then_some(action)
                );
            }
        }
    }

    #[test]
    fn backup_ui_create_has_the_reference_shortcut_and_import_has_none() {
        let create = command(Action::CreateBackup).unwrap();
        let import = command(Action::ImportBackup).unwrap();
        let key = Key::Character("b".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let expected = Chord::from_event(&key, primary | Modifiers::SHIFT, platform);
            assert_eq!(create.defaults(platform), [expected, None]);
            assert_eq!(import.defaults(platform), [None, None]);
        }
        assert_eq!(create.menu, "File");
        assert_eq!(import.menu, "File");
    }

    #[test]
    fn ssh_keys_defaults_to_exact_primary_shift_i() {
        let command = command(Action::SshKeys).unwrap();
        let key = Key::Character("i".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let exact = Chord::from_event(&key, primary | Modifiers::SHIFT, platform);
            assert_eq!(command.defaults(platform), [exact, None]);
            assert_ne!(Chord::from_event(&key, primary, platform), exact);
        }
        assert_eq!(command.menu, "Configuration › Security");
        assert_eq!(command.title, "SSH-Keys…");
    }

    #[test]
    fn openpgp_recipients_defaults_to_exact_primary_shift_g_and_preserves_custom_binding() {
        let recipient = command(Action::OpenPgpRecipients).unwrap();
        assert_eq!(recipient.menu, "Configuration › Security");
        assert_eq!(recipient.title, "OpenPGP recipients…");
        let key = Key::Character("G".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let exact = Chord::from_event(&key, primary | Modifiers::SHIFT, platform);
            assert_eq!(recipient.defaults(platform), [exact, None]);
            assert_ne!(Chord::from_event(&key, primary, platform), exact);
        }

        let modifiers = Modifiers::COMMAND | Modifiers::SHIFT;
        let mut bindings = Bindings::default();
        assert_eq!(
            bindings.action_for(&key, modifiers),
            Some(Action::OpenPgpRecipients)
        );
        bindings
            .update(Action::OpenProject, recipient.default_chord())
            .unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(
            bindings.action_for(&key, modifiers),
            Some(Action::OpenProject)
        );
        assert!(bindings.shortcut(recipient).is_none());
        assert!(bindings.conflicts().is_empty());
    }

    #[test]
    fn reopen_closed_tab_defaults_to_exact_primary_alt_shift_t() {
        let reopen = command(Action::ReopenClosedTab).unwrap();
        let key = Key::Character("t".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let exact = primary | Modifiers::ALT | Modifiers::SHIFT;
            let chords = reopen.defaults(platform);
            let expected = Chord::from_event(&key, exact, platform);
            assert_eq!(chords, [expected, None]);
            for dropped in [primary, Modifiers::ALT, Modifiers::SHIFT] {
                assert_ne!(Chord::from_event(&key, exact - dropped, platform), expected);
            }
        }
        let primary = if PLATFORM == Platform::Mac {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        assert_eq!(
            Bindings::default().action_for(&key, primary | Modifiers::ALT | Modifiers::SHIFT),
            Some(Action::ReopenClosedTab),
        );
    }

    #[test]
    fn command_timestamps_use_primary_shift_t_without_stealing_shell_ctrl_t() {
        let timestamps = command(Action::ShowCommandTimestamps).unwrap();
        let quick_connect = command(Action::QuickConnect).unwrap();
        let key = Key::Character("t".into());
        for (platform, primary) in [
            (Platform::Mac, Modifiers::LOGO),
            (Platform::Linux, Modifiers::CTRL),
            (Platform::Windows, Modifiers::CTRL),
        ] {
            let exact = Chord::from_event(&key, primary | Modifiers::SHIFT, platform);
            assert_eq!(timestamps.defaults(platform), [exact, None]);
            assert!(!quick_connect.defaults(platform).contains(&exact));
            if platform != Platform::Mac {
                assert_eq!(
                    quick_connect.defaults(platform),
                    [
                        Some(Chord::primary_key(MainKey::Function(2), true, false)),
                        None
                    ],
                );
                for shell in [
                    Chord::from_event(&key, Modifiers::CTRL, platform),
                    Chord::from_event(&Key::Named(Named::F2), Modifiers::empty(), platform),
                ] {
                    assert!(
                        catalog()
                            .iter()
                            .all(|entry| !entry.defaults(platform).contains(&shell))
                    );
                }
            }
        }
        assert_eq!(timestamps.menu, "View");
        assert_eq!(timestamps.title, "Show Command Timestamps");

        let mut bindings = Bindings::default();
        let shortcut = timestamps.default_chord();
        bindings.update(Action::QuickConnect, shortcut).unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(bindings.shortcut(quick_connect), shortcut);
        assert_eq!(bindings.shortcut(timestamps), None);
        assert!(bindings.conflicts().is_empty());
    }

    #[test]
    fn menu_bar_shortcut_remains_available_and_preserves_existing_overrides() {
        let menu_bar = command(Action::ShowMenuBar).unwrap();
        let shortcut = Chord::primary('l', true);
        for platform in [Platform::Mac, Platform::Linux, Platform::Windows] {
            assert_eq!(menu_bar.defaults(platform), [Some(shortcut), None]);
        }
        let mut bindings = Bindings::default();
        assert!(bindings.update(Action::ShowMenuBar, None).is_err());
        bindings
            .0
            .insert(menu_bar.id.clone(), Override::Parsed(None));
        assert_eq!(bindings.shortcut(menu_bar), Some(shortcut));
        bindings.reset(Action::ShowMenuBar);
        bindings.update(Action::Broadcast, Some(shortcut)).unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(
            bindings.shortcut(command(Action::Broadcast).unwrap()),
            Some(shortcut),
        );
        let recovery = Chord::primary_key(MainKey::Function(3), true, false);
        assert_eq!(bindings.shortcut(menu_bar), Some(recovery));
        assert!(bindings.conflicts().is_empty());
        let custom = Chord::primary_key(MainKey::Function(8), true, false);
        bindings.update(Action::ShowMenuBar, Some(custom)).unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(bindings.shortcut(menu_bar), Some(custom));
    }

    #[test]
    fn terminal_only_fullscreen_and_find_do_not_take_shell_ctrl_f() {
        let find = command(Action::Find).unwrap();
        let fullscreen = command(Action::TerminalOnlyFullscreen).unwrap();
        for platform in [Platform::Mac, Platform::Windows, Platform::Linux] {
            assert_eq!(
                fullscreen.defaults(platform),
                [Some(Chord::primary('f', true)), None]
            );
            assert_eq!(
                find.defaults(platform),
                [
                    (platform == Platform::Mac).then_some(Chord::primary('f', false)),
                    None
                ]
            );
        }
        let other = command(Action::NewWindow).unwrap();
        let mut bindings = Bindings::default();
        let shortcut = fullscreen.default_chord();
        bindings.update(other.action, shortcut).unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(bindings.shortcut(other), shortcut);
        assert_eq!(bindings.shortcut(fullscreen), None);
        assert!(bindings.conflicts().is_empty());
    }

    #[test]
    fn upgrading_preserves_a_user_shortcut_claimed_before_logfiles_were_available() {
        let logfile = command(Action::LogfileManager).unwrap();
        let old = command(Action::NewWindow).unwrap();
        let mut bindings = Bindings::default();
        let shortcut = logfile.default_chord();
        bindings.update(old.action, shortcut).unwrap();
        assert!(!bindings.conflicts().is_empty());

        bindings.preserve_user_shortcuts();
        assert_eq!(bindings.shortcut(old), shortcut);
        assert_eq!(bindings.shortcut(logfile), None);
        assert!(bindings.conflicts().is_empty());

        bindings.reset(old.action);
        bindings.reset(logfile.action);
        assert_eq!(bindings.shortcut(logfile), shortcut);
    }

    #[test]
    fn video_shortcuts_match_java_and_preserve_existing_assignments() {
        for platform in [Platform::Mac, Platform::Windows, Platform::Linux] {
            let mac = platform == Platform::Mac;
            assert_eq!(
                command(Action::Paste).unwrap().defaults(platform),
                [Some(Chord::primary('v', !mac)), None]
            );
            assert_eq!(
                command(Action::Recordings).unwrap().defaults(platform),
                [mac.then_some(Chord::primary('v', true)), None]
            );
            assert_eq!(
                command(Action::ToggleRecording).unwrap().defaults(platform),
                [Some(Chord::primary('e', true)), None]
            );
        }
        for video in [Action::Recordings, Action::ToggleRecording]
            .into_iter()
            .filter_map(command)
            .filter(|entry| entry.default_chord().is_some())
        {
            let mut bindings = Bindings::default();
            let assigned = video.default_chord();
            let old = command(Action::NewWindow).unwrap();
            bindings.update(old.action, assigned).unwrap();
            bindings.preserve_user_shortcuts();
            assert_eq!(bindings.shortcut(old), assigned);
            assert_eq!(bindings.shortcut(video), None);
            assert!(bindings.conflicts().is_empty());
        }
    }

    #[test]
    fn pane_shortcuts_match_reference_and_keep_existing_assignments() {
        for platform in [Platform::Mac, Platform::Windows, Platform::Linux] {
            let modifiers = if platform == Platform::Mac {
                Modifiers::LOGO
            } else {
                Modifiers::CTRL
            };
            for (action, key, extra) in [
                (
                    Action::SplitPane,
                    Key::Character("o".into()),
                    Modifiers::SHIFT,
                ),
                (Action::ZoomPane, Key::Named(Named::Enter), Modifiers::SHIFT),
                (
                    Action::FocusPaneLeft,
                    Key::Named(Named::ArrowLeft),
                    Modifiers::ALT,
                ),
            ] {
                assert_eq!(
                    command(action).unwrap().defaults(platform)[0],
                    Chord::from_event(&key, modifiers | extra, platform)
                );
            }
        }

        let mut bindings = Bindings::default();
        let old = command(Action::NewWindow).unwrap();
        let split = command(Action::SplitPane).unwrap();
        let split_default = split.default_chord();
        bindings.update(old.action, split_default).unwrap();
        bindings.preserve_user_shortcuts();
        assert_eq!(bindings.shortcut(old), split_default);
        assert_eq!(bindings.shortcut(split), None);
        assert!(bindings.conflicts().is_empty());
    }

    #[test]
    fn platform_rules_preserve_shell_typing_and_system_keys() {
        let b = Key::Character("b".into());
        let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
        let windows = Chord::from_event(&b, ctrl_alt, Platform::Windows).unwrap();
        assert!(windows.rejected(Platform::Windows).is_some());
        assert_eq!(windows.rejected(Platform::Linux), None);
        assert_eq!(windows.rejected(Platform::Mac), None);
        assert!(
            Chord::from_event(&b, Modifiers::ALT, Platform::Mac)
                .unwrap()
                .rejected(Platform::Mac)
                .is_some()
        );
        assert!(
            Chord::from_event(&b, Modifiers::CTRL, Platform::Linux)
                .unwrap()
                .rejected(Platform::Linux)
                .is_some()
        );
        assert!(
            Chord::from_event(&Key::Named(Named::F7), ctrl_alt, Platform::Linux)
                .unwrap()
                .rejected(Platform::Linux)
                .is_some()
        );
        assert!(
            Chord::from_event(&Key::Named(Named::F12), Modifiers::empty(), Platform::Mac)
                .unwrap()
                .rejected(Platform::Mac)
                .is_some()
        );
        assert!(Bindings::rejection(Chord::primary('c', true), Platform::Windows).is_some());
        assert_eq!(
            Bindings::rejection(Chord::primary('p', true), Platform::Linux),
            None
        );
    }

    #[test]
    fn conflicting_profile_keeps_defaults_and_reset_preserves_unknown_ids() {
        let mut bindings = Bindings::default();
        let default = command(Action::QuickConnect)
            .unwrap()
            .default_chord()
            .unwrap();
        let (key, modifiers) = if PLATFORM == Platform::Mac {
            (Key::Character("t".into()), Modifiers::COMMAND)
        } else {
            (Key::Named(Named::F2), Modifiers::COMMAND | Modifiers::SHIFT)
        };
        bindings.update(Action::Broadcast, Some(default)).unwrap();
        assert_eq!(
            bindings.conflicts(),
            [(default, vec![Action::QuickConnect, Action::Broadcast])]
        );
        assert_eq!(
            bindings.action_for(&key, modifiers),
            Some(Action::QuickConnect)
        );

        bindings.update(Action::QuickConnect, None).unwrap();
        assert!(bindings.conflicts().is_empty());
        assert_eq!(
            bindings.action_for(&key, modifiers),
            Some(Action::Broadcast)
        );
        assert!(bindings.update(Action::Paste, None).is_err());
        let future = serde_json::json!({ "future-key-format": true });
        bindings.0.insert(
            "future-command".to_owned(),
            Override::Unrecognized(future.clone()),
        );
        let mut bindings: Bindings =
            serde_json::from_str(&serde_json::to_string(&bindings).unwrap()).unwrap();
        assert_eq!(bindings.unknown_count(), 1);
        bindings.reset_all();
        assert_eq!(bindings.unknown_count(), 1);
        assert_eq!(
            serde_json::to_value(&bindings).unwrap()["future-command"],
            future
        );
        assert_eq!(
            bindings.action_for(&key, modifiers),
            Some(Action::QuickConnect)
        );
    }
}
