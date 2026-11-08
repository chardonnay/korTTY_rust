//! The native Wyse byte stream used by the WY-50, WY-60 and WY-160 profiles.

use crate::{
    CharacterSet, Color, KeyModifiers, Model, Style, TerminalKey, TerminalProfile, is_bidi_control,
};

const ESC: u8 = 0x1b;
const MAX_STATUS_BYTES: usize = 1024;
const MAX_TITLE_CHARS: usize = 80;

#[derive(Clone, Copy)]
enum Command {
    Address,
    PreviousField,
    Attribute { enhanced: bool },
    Color,
    ScrollRegion,
    CursorVisibility,
    ColumnMode,
    IgnoredParameter,
}

impl Command {
    const fn length(self) -> usize {
        match self {
            Self::PreviousField => 3,
            Self::Address | Self::Color | Self::ScrollRegion => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy)]
enum Message {
    Status,
    KeyDefinition,
}

#[derive(Clone, Copy)]
enum State {
    Ground,
    Escape,
    Parameters {
        command: Command,
        bytes: [u8; 3],
        used: usize,
    },
    Message {
        kind: Message,
        selector: bool,
        discarded: bool,
    },
    ForeignString {
        osc: bool,
        escaped: bool,
        utf8_remaining: usize,
        utf8_c1: bool,
    },
    ForeignCsi,
}

impl State {
    const fn parameters(command: Command) -> Self {
        Self::Parameters {
            command,
            bytes: [0; 3],
            used: 0,
        }
    }

    const fn foreign_string(osc: bool) -> Self {
        Self::ForeignString {
            osc,
            escaped: false,
            utf8_remaining: 0,
            utf8_c1: false,
        }
    }
}

pub(crate) struct Wyse {
    profile: TerminalProfile,
    state: State,
    status: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: usize,
}

impl Wyse {
    pub(crate) fn new(profile: TerminalProfile) -> Self {
        Self {
            profile,
            state: State::Ground,
            status: Vec::new(),
            utf8: [0; 4],
            utf8_len: 0,
        }
    }

    pub(crate) fn advance(&mut self, model: &mut Model, bytes: &[u8]) {
        for &byte in bytes {
            self.state = match self.state {
                State::Ground => self.ground(model, byte),
                State::Escape => self.escape(model, byte),
                State::Parameters {
                    command,
                    bytes,
                    used,
                } => self.parameter(model, command, bytes, used, byte),
                State::Message {
                    kind,
                    selector,
                    discarded,
                } => self.message(model, kind, selector, discarded, byte),
                State::ForeignString {
                    osc,
                    escaped,
                    utf8_remaining,
                    utf8_c1,
                } => skip_foreign_string(osc, escaped, utf8_remaining, utf8_c1, byte),
                State::ForeignCsi if matches!(byte, 0x18 | 0x1a | 0x40..=0x7e) => State::Ground,
                State::ForeignCsi => State::ForeignCsi,
            };
        }
    }

    fn ground(&mut self, model: &mut Model, byte: u8) -> State {
        if self.utf8_len != 0 {
            if (0x80..=0xbf).contains(&byte) {
                self.utf8[self.utf8_len] = byte;
                self.utf8_len += 1;
                if self.utf8_len < utf8_width(self.utf8[0]) {
                    return State::Ground;
                }
                let ch = std::str::from_utf8(&self.utf8[..self.utf8_len])
                    .ok()
                    .and_then(|text| text.chars().next())
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                self.utf8_len = 0;
                if matches!(ch, '\u{80}'..='\u{9f}') {
                    return c1(ch as u8);
                }
                model.put_char(ch);
                return State::Ground;
            }
            self.utf8_len = 0;
            model.put_char(char::REPLACEMENT_CHARACTER);
        }

        match byte {
            ESC => return State::Escape,
            0x07 => model.ring_bell(),
            0x08 => backspace(model),
            0x09 => model.tab(1, true),
            0x0a => model.line_feed(),
            0x0d => model.carriage_return(),
            0x0e => model.charset = 1,
            0x0f => model.charset = 0,
            0x20..=0x7e => {
                let ch = char::from(byte);
                let ch = if model.charset == 1 {
                    CharacterSet::DecGraphics.map(ch)
                } else {
                    ch
                };
                model.put_character(ch, 1);
            }
            0x80..=0x9f => return c1(byte),
            _ if utf8_width(byte) > 1 => {
                self.utf8[0] = byte;
                self.utf8_len = 1;
            }
            0xa0..=0xff => model.put_char(char::REPLACEMENT_CHARACTER),
            _ => {}
        }
        State::Ground
    }

    fn escape(&mut self, model: &mut Model, byte: u8) -> State {
        let wy50 = self.profile == TerminalProfile::Wy50;
        let wy160 = self.profile == TerminalProfile::Wy160;
        let cursor = model.cursor();
        match byte {
            ESC => return State::Escape,
            b'[' => return State::ForeignCsi,
            b']' => return State::foreign_string(true),
            b'P' | b'_' | b'X' => return State::foreign_string(false),
            b'=' => return State::parameters(Command::Address),
            b'-' if wy50 => return State::parameters(Command::PreviousField),
            b'G' => return State::parameters(Command::Attribute { enhanced: false }),
            b'd' if !wy50 => return State::parameters(Command::Attribute { enhanced: true }),
            b'e' if wy160 => return State::parameters(Command::Color),
            b'r' if wy160 => return State::parameters(Command::ScrollRegion),
            b'`' => return State::parameters(Command::CursorVisibility),
            b'~' => return State::parameters(Command::ColumnMode),
            // The pinned emulator records personality and display-page selectors,
            // but neither changes the active parser or display contents.
            b'c' | b'w' if !wy50 => return State::parameters(Command::IgnoredParameter),
            b'z' | b'A' => {
                self.status.clear();
                let kind = if byte == b'z' {
                    Message::Status
                } else {
                    Message::KeyDefinition
                };
                return State::Message {
                    kind,
                    selector: true,
                    discarded: false,
                };
            }
            b'T' => model.erase_in_line(0, false),
            b't' => model.erase_in_line(1, false),
            b'Y' => model.erase_display(0, false),
            b'y' => model.erase_display(1, false),
            b'*' | b'+' => {
                model.erase_display(2, false);
                model.move_cursor(0, 0);
            }
            0x0c => model.move_columns(1),
            b'.' => model.move_cursor(cursor.row + 1, cursor.column),
            // ESC ^ is a native cursor command, not a privacy message introducer.
            b'^' => model.move_cursor(cursor.row.saturating_sub(1), cursor.column),
            b'E' if cursor.row <= model.scroll_bottom => {
                model.scroll_down(cursor.row, model.scroll_bottom, 1);
            }
            b'R' if cursor.row <= model.scroll_bottom => {
                model.scroll_up(cursor.row, model.scroll_bottom, 1, false);
            }
            b'Q' | b'W' => edit_character(model, byte == b'Q'),
            b')' | b'(' => model.style.dim = byte == b')',
            b'&' | b'\'' => model.protected = byte == b'&',
            b'#' => model.save_cursor(),
            b'"' => model.restore_cursor(),
            b'/' => model.cursor_visible = false,
            b'F' if wy50 => model.cursor_visible = true,
            b'I' if wy50 => model.tab(1, true),
            b'e' | b'r' if !wy160 => model.insert_mode = byte == b'e',
            b'{' | b'}' if !wy50 => model.auto_wrap = byte == b'{',
            b'Z' if wy160 => model.reply(b"160\r"),
            b'Z' if !wy50 => model.reply(b"60\r"),
            0x1a if wy50 => {
                let title = std::mem::take(&mut model.title);
                model.reset(false);
                model.title = title;
            }
            _ => {}
        }
        State::Ground
    }

    fn parameter(
        &mut self,
        model: &mut Model,
        command: Command,
        mut bytes: [u8; 3],
        used: usize,
        byte: u8,
    ) -> State {
        match byte {
            ESC => return State::Escape,
            0x18 | 0x1a => return State::Ground,
            _ => {}
        }
        bytes[used] = byte;
        let used = used + 1;
        if used < command.length() {
            return State::Parameters {
                command,
                bytes,
                used,
            };
        }
        let [first, second, third] = bytes;
        match command {
            Command::Address => model.address_cursor(position(first), position(second)),
            Command::PreviousField => model.address_cursor(position(second), position(third)),
            Command::Attribute { enhanced } => attributes(model, first, enhanced),
            Command::Color => {
                model.style = Style {
                    foreground: Color::Indexed(color(first, 7)),
                    background: Color::Indexed(color(second, 0)),
                    ..Style::default()
                };
            }
            Command::ScrollRegion => model.set_margins(position(first), position(second)),
            Command::CursorVisibility => match first {
                b'0' => model.cursor_visible = false,
                b'1' | b'5' => model.cursor_visible = true,
                _ => {}
            },
            Command::ColumnMode => match first {
                b'!' => model.resize(80, model.rows),
                b'"' => model.resize(132, model.rows),
                _ => {}
            },
            Command::IgnoredParameter => {}
        }
        State::Ground
    }

    fn message(
        &mut self,
        model: &mut Model,
        kind: Message,
        selector: bool,
        mut discarded: bool,
        byte: u8,
    ) -> State {
        if selector {
            return State::Message {
                kind,
                selector: false,
                discarded,
            };
        }
        if byte == b'\r' {
            if matches!(kind, Message::Status) && !discarded && !model.strict {
                model.title = String::from_utf8_lossy(&self.status)
                    .chars()
                    .filter(|&ch| !ch.is_control() && !is_bidi_control(ch))
                    .take(MAX_TITLE_CHARS)
                    .collect();
            }
            self.status.clear();
            return State::Ground;
        }
        if matches!(kind, Message::Status) && !discarded && !model.strict {
            if self.status.len() == MAX_STATUS_BYTES {
                discarded = true;
                self.status.clear();
            } else {
                self.status.push(byte);
            }
        }
        State::Message {
            kind,
            selector: false,
            discarded,
        }
    }
}

fn backspace(model: &mut Model) {
    let cursor = model.cursor();
    match (cursor.row, cursor.column) {
        (_, 1..) => model.move_columns(-1),
        (1.., 0) => model.move_cursor(cursor.row - 1, model.columns - 1),
        _ => {}
    }
}

fn edit_character(model: &mut Model, insert: bool) {
    let style = model.style.erase();
    let right = model.columns - 1;
    if insert {
        model.screen_mut().insert_cells(1, style, right);
    } else {
        model.screen_mut().delete_cells(1, style, right);
    }
}

fn position(byte: u8) -> usize {
    usize::from(byte.saturating_sub(b' '))
}

fn color(byte: u8, default: u8) -> u8 {
    byte.checked_sub(b'0')
        .filter(|&value| value < 16)
        .unwrap_or(default)
}

fn attributes(model: &mut Model, byte: u8, enhanced: bool) {
    let bits = byte.wrapping_sub(b'0');
    model.style = Style {
        hidden: bits & 0x01 != 0,
        blink: bits & 0x02 != 0,
        inverse: bits & 0x04 != 0,
        underline: bits & 0x08 != 0,
        dim: bits & 0x10 != 0,
        bold: enhanced && bits & 0x20 != 0,
        ..Style::default()
    };
}

fn c1(byte: u8) -> State {
    match byte {
        0x90 | 0x98 | 0x9e | 0x9f => State::foreign_string(false),
        0x9b => State::ForeignCsi,
        0x9d => State::foreign_string(true),
        _ => State::Ground,
    }
}

fn utf8_width(byte: u8) -> usize {
    match byte {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

fn skip_foreign_string(
    osc: bool,
    escaped: bool,
    utf8_remaining: usize,
    utf8_c1: bool,
    byte: u8,
) -> State {
    if (escaped && byte == b'\\') || (osc && byte == 0x07) || matches!(byte, 0x18 | 0x1a) {
        return State::Ground;
    }
    if utf8_remaining != 0 && (0x80..=0xbf).contains(&byte) {
        if utf8_c1 && byte == 0x9c {
            return State::Ground;
        }
        return State::ForeignString {
            osc,
            escaped: false,
            utf8_remaining: utf8_remaining - 1,
            utf8_c1: false,
        };
    }
    if byte == 0x9c {
        return State::Ground;
    }
    State::ForeignString {
        osc,
        escaped: byte == ESC,
        utf8_remaining: utf8_width(byte) - 1,
        utf8_c1: byte == 0xc2,
    }
}

pub(crate) fn encode_key(
    profile: TerminalProfile,
    key: TerminalKey,
    modifiers: KeyModifiers,
) -> Option<Vec<u8>> {
    let max_function = match profile {
        TerminalProfile::Wy50 => 12,
        TerminalProfile::Wy60 | TerminalProfile::Wy160 => 24,
        _ => return None,
    };

    // The reference encoder inherits Shift+Tab and platform cursor shortcuts.
    if key == TerminalKey::Tab && modifiers.shift && !modifiers.alt && !modifiers.control {
        return Some(b"\x1b[Z".to_vec());
    }
    if matches!(key, TerminalKey::Left | TerminalKey::Right) && !modifiers.shift {
        if modifiers.alt && !modifiers.control {
            return Some(match (cfg!(target_os = "macos"), key) {
                (true, TerminalKey::Left) => b"\x1bb".to_vec(),
                (true, _) => b"\x1bf".to_vec(),
                (false, TerminalKey::Left) => b"\x1b[1;3D".to_vec(),
                (false, _) => b"\x1b[1;3C".to_vec(),
            });
        }
        if modifiers.control && !modifiers.alt && !cfg!(target_os = "macos") {
            return Some(if key == TerminalKey::Left {
                b"\x1b[1;5D".to_vec()
            } else {
                b"\x1b[1;5C".to_vec()
            });
        }
    }

    let code = match key {
        TerminalKey::Left | TerminalKey::Backspace => [0x08, 0],
        TerminalKey::Right => [0x0c, 0],
        TerminalKey::Up => [0x0b, 0],
        TerminalKey::Down => [0x0a, 0],
        TerminalKey::Home => [0x1e, 0],
        TerminalKey::End => [0x19, 0],
        TerminalKey::Insert => [ESC, b'q'],
        TerminalKey::Delete => [ESC, b'W'],
        TerminalKey::PageUp => [ESC, b'J'],
        TerminalKey::PageDown => [ESC, b'K'],
        TerminalKey::Enter => [b'\r', 0],
        TerminalKey::Tab => [b'\t', 0],
        TerminalKey::Escape => [ESC, 0],
        TerminalKey::Keypad(digit @ 0..=9) => [b'0' + digit, 0],
        TerminalKey::Function(number) if (1..=max_function).contains(&number) => {
            [0x01, b'@' + number - 1]
        }
        _ => return None,
    };
    let mut result = Vec::with_capacity(3);
    if modifiers.alt {
        result.push(ESC);
    }
    result.push(code[0]);
    if code[1] != 0 {
        result.push(code[1]);
    }
    Some(result)
}
