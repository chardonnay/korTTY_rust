use crate::{
    CharacterSet, KeyModifiers, Model, Style, TerminalKey, TerminalProfile, is_bidi_control,
    legacy_modified_key, strict_osc,
};

const ESC: u8 = 0x1b;
const UTF8_ADDRESS_LEAD: u8 = 0xc2;

#[derive(Clone, Copy)]
enum State {
    Ground,
    Escape,
    Coordinate(Coordinate),
    CoordinateTail(Coordinate),
    Attribute,
    CursorType,
    IgnoreByte,
    Status,
    FunctionDefinition { escape: bool },
    PageSelect,
    ColumnMode,
}

#[derive(Clone, Copy)]
enum Coordinate {
    CursorRow,
    CursorColumn(u8),
    RegionTop,
    RegionBottom(u8),
}

impl Coordinate {
    fn read(self, model: &mut Model, byte: u8) -> State {
        let extent = match self {
            Self::CursorColumn(_) => model.columns,
            _ => model.rows,
        };
        if byte == UTF8_ADDRESS_LEAD
            && !model.text_encoding().is_single_byte()
            && coordinate(byte) >= extent
        {
            // C2 introduces a UTF-8 coordinate only outside the valid raw address range.
            State::CoordinateTail(self)
        } else {
            self.apply(model, byte)
        }
    }

    fn apply(self, model: &mut Model, byte: u8) -> State {
        match self {
            Self::CursorRow => State::Coordinate(Self::CursorColumn(byte)),
            Self::CursorColumn(row) => {
                model.move_cursor(coordinate(row), coordinate(byte));
                State::Ground
            }
            Self::RegionTop => State::Coordinate(Self::RegionBottom(byte)),
            Self::RegionBottom(top) => {
                let top = coordinate(top).min(model.rows - 1);
                let bottom = coordinate(byte).clamp(top, model.rows - 1);
                set_region(model, top, bottom);
                State::Ground
            }
        }
    }
}

pub(crate) struct Tvi {
    profile: TerminalProfile,
    state: State,
    status: Vec<u8>,
    status_limit: usize,
    status_overflowed: bool,
    utf8: [u8; 4],
    utf8_len: usize,
    page: u8,
}

impl Tvi {
    pub(crate) fn new(profile: TerminalProfile) -> Self {
        Self {
            profile,
            state: State::Ground,
            status: Vec::new(),
            status_limit: 0,
            status_overflowed: false,
            utf8: [0; 4],
            utf8_len: 0,
            page: 0,
        }
    }

    pub(crate) fn advance(&mut self, model: &mut Model, bytes: &[u8]) {
        for &byte in bytes {
            self.state = match self.state {
                State::Ground => self.ground(model, byte),
                State::Escape => self.escape(model, byte),
                State::Coordinate(field) => field.read(model, byte),
                State::CoordinateTail(field) if matches!(byte, 0x80..=0xbf) => {
                    field.apply(model, byte)
                }
                State::CoordinateTail(field) => match field.apply(model, UTF8_ADDRESS_LEAD) {
                    State::Coordinate(next) if byte != ESC => next.read(model, byte),
                    _ => self.ground(model, byte),
                },
                State::Attribute => {
                    model.style = attribute(byte);
                    State::Ground
                }
                State::CursorType => {
                    model.cursor_visible = !matches!(byte, b'0' | b'4');
                    State::Ground
                }
                State::Status => self.read_status(model, byte),
                State::FunctionDefinition { escape } if !(escape && byte == b'\\') => {
                    // The reference consumes host key definitions without installing them.
                    // Even an oversized definition remains inert until its terminator.
                    State::FunctionDefinition {
                        escape: byte == ESC,
                    }
                }
                State::PageSelect => {
                    if let b'0'..=b'3' = byte {
                        self.page = byte - b'0';
                    }
                    State::Ground
                }
                State::ColumnMode => {
                    // The reference WideColumn mode clears the grid and resets the margins.
                    model.erase_display(2, false);
                    set_region(model, 0, model.rows - 1);
                    State::Ground
                }
                State::IgnoreByte | State::FunctionDefinition { .. } => State::Ground,
            };
        }
    }

    fn ground(&mut self, model: &mut Model, byte: u8) -> State {
        if self.utf8_len != 0 && byte & 0xc0 != 0x80 {
            self.utf8_len = 0;
            print(model, '\u{fffd}');
        }
        match byte {
            ESC => return State::Escape,
            0x07 => model.ring_bell(),
            0x08 => backspace(model),
            0x09 => model.horizontal_tab(),
            0x0a | 0x0b => line_motion(model, Model::line_feed),
            0x0d => model.carriage_return(),
            0x0e => model.charset = 1,
            0x0f => model.charset = 0,
            0x1a => clear_and_home(model),
            0x20..=0x7f => print(model, char::from(byte)),
            0x80..=0xff if model.text_encoding().is_single_byte() => {
                let ch = model.text_encoding().decode_byte(byte);
                if !ch.is_control() {
                    print_character(model, ch);
                }
            }
            0x80..=0xff => self.read_utf8(model, byte),
            _ => {}
        }
        State::Ground
    }

    fn read_utf8(&mut self, model: &mut Model, byte: u8) {
        self.utf8[self.utf8_len] = byte;
        self.utf8_len += 1;
        match std::str::from_utf8(&self.utf8[..self.utf8_len]) {
            Ok(text) => {
                for ch in text.chars() {
                    if model.text_encoding.is_some() {
                        print_character(model, ch);
                    } else {
                        print(model, ch);
                    }
                }
                self.utf8_len = 0;
            }
            Err(error) if error.error_len().is_some() => {
                print(model, '\u{fffd}');
                self.utf8_len = 0;
            }
            Err(_) => {}
        }
    }

    fn escape(&mut self, model: &mut Model, byte: u8) -> State {
        let extended = self.profile != TerminalProfile::Tvi910;
        let tvi925 = self.profile == TerminalProfile::Tvi925;
        match byte {
            b'=' => return State::Coordinate(Coordinate::CursorRow),
            b'T' | b't' => erase_line_end(model),
            b'Y' | b'y' => {
                erase_line_end(model);
                let cursor = model.cursor();
                for row in cursor.row + 1..model.rows {
                    model.erase_line(row, 0, model.columns, false);
                }
                if cursor.row + 1 < model.rows {
                    model.move_cursor(cursor.row, cursor.column);
                }
            }
            b'*' | b':' | b'+' | b',' => clear_and_home(model),
            b'G' | b'g' => return State::Attribute,
            b'.' => return State::CursorType,
            b'/' => return State::IgnoreByte,
            b'A' => model.cursor_visible = true,
            b'b' => model.move_cursor(model.rows - 1, 0),
            b'j' => line_motion(model, Model::reverse_index),
            b'E' | b'R' => edit_line(model, byte == b'E'),
            b'Q' | b'W' => edit_character(model, byte == b'Q'),
            b'q' | b'r' => model.insert_mode = byte == b'q',
            b'#' => model.save_cursor(),
            b'"' => model.restore_cursor(),
            b'I' | b'i' => {
                let cursor = model.cursor();
                let column = cursor.column + usize::from(model.screen().wrap_pending);
                if column != 0 {
                    model.move_cursor(cursor.row, (column - 1) / 8 * 8);
                }
            }
            b'K' | b'k' => model.newline_mode = byte == b'K',
            b'l' | b'o' => {
                self.status.clear();
                self.status_limit = (model.columns + 1) * 4;
                self.status_overflowed = false;
                return State::Status;
            }
            b'n' => report_cursor(model),
            b'v' | b'w' => model.auto_wrap = byte == b'v',
            b'~' if extended => return State::Coordinate(Coordinate::RegionTop),
            b'p' if extended => return State::FunctionDefinition { escape: false },
            b'z' if extended => {
                let title = std::mem::take(&mut model.title);
                let insert = model.insert_mode;
                let saved = model.saved;
                model.reset(false);
                model.title = title;
                model.insert_mode = insert;
                // The factory reset leaves the cursor saved by ESC # available to ESC ".
                model.saved = saved;
                self.page = 0;
            }
            b'-' if tvi925 => return State::Attribute,
            b'P' if tvi925 => return State::PageSelect,
            b'N' if tvi925 => self.page = (self.page + 1).min(3),
            b'O' if tvi925 => self.page = self.page.saturating_sub(1),
            b'c' if tvi925 => return State::ColumnMode,
            b'd' if tvi925 => model.style = Style::default(),
            // Protection, keyboard locking, duplex and enhanced attribute modes
            // have no visible effect in the pinned emulator.
            _ => {}
        }
        State::Ground
    }

    fn read_status(&mut self, model: &mut Model, byte: u8) -> State {
        if byte != b'\r' {
            if self.status.len() < self.status_limit {
                self.status.push(byte);
            } else {
                self.status_overflowed = true;
            }
            return State::Status;
        }
        let status = model.text_encoding().decode(&self.status);
        if !model.strict || !self.status_overflowed && strict_osc("2", &status) {
            model.title = status
                .chars()
                .filter(|&ch| !ch.is_control() && !is_bidi_control(ch))
                .take(model.columns + 1)
                .collect();
        }
        self.status.clear();
        State::Ground
    }
}

fn coordinate(byte: u8) -> usize {
    usize::from(byte.saturating_sub(b' '))
}

fn set_region(model: &mut Model, top: usize, bottom: usize) {
    model.scroll_top = top;
    model.scroll_bottom = bottom;
    model.move_cursor(0, 0);
}

fn attribute(byte: u8) -> Style {
    let bits = byte.wrapping_sub(b'0');
    Style {
        hidden: bits & 1 != 0,
        blink: bits & 2 != 0,
        inverse: bits & 4 != 0,
        underline: bits & 8 != 0,
        bold: bits & 16 != 0,
        dim: bits & 32 != 0,
        ..Style::default()
    }
}

fn print(model: &mut Model, ch: char) {
    let ch = match ch {
        '\u{80}'..='\u{9f}' => return,
        '\x7f' if model.charset == 1 => ' ',
        ch if model.charset == 1 => CharacterSet::DecGraphics.map(ch),
        ch => ch,
    };
    print_character(model, ch);
}

fn print_character(model: &mut Model, ch: char) {
    if !model.auto_wrap
        && model.screen().wrap_pending
        && !ch.is_control()
        && unicode_width::UnicodeWidthChar::width(ch).is_some_and(|width| width != 0)
    {
        model.carriage_return();
    }
    model.put_char(ch);
}

fn backspace(model: &mut Model) {
    let cursor = model.cursor();
    if model.screen().wrap_pending {
        model.move_cursor(cursor.row, cursor.column);
    } else if cursor.column == 0 {
        model.move_cursor(cursor.row.saturating_sub(1), model.columns - 1);
    } else {
        model.move_columns(-1);
    }
}

fn line_motion(model: &mut Model, motion: fn(&mut Model)) {
    let wrap_pending = model.screen().wrap_pending;
    motion(model);
    model.screen_mut().wrap_pending = wrap_pending;
}

fn clear_and_home(model: &mut Model) {
    model.erase_display(2, false);
    model.move_cursor(0, 0);
}

fn erase_line_end(model: &mut Model) {
    if !model.screen().wrap_pending {
        model.erase_in_line(0, false);
    }
}

fn edit_line(model: &mut Model, insert: bool) {
    let row = model.cursor().row;
    if row <= model.scroll_bottom {
        if insert {
            model.scroll_down(row, model.scroll_bottom, 1);
        } else {
            model.scroll_up(row, model.scroll_bottom, 1, false);
        }
    }
}

fn edit_character(model: &mut Model, insert: bool) {
    if model.screen().wrap_pending {
        return;
    }
    let style = model.style.erase();
    let right = model.columns - 1;
    if insert {
        model.screen_mut().insert_cells(1, style, right);
    } else {
        model.screen_mut().delete_cells(1, style, right);
    }
}

fn report_cursor(model: &mut Model) {
    let cursor = model.cursor();
    let column = cursor.column + usize::from(model.screen().wrap_pending);
    let row = 0x20 + cursor.row;
    let column = 0x20 + column;
    if model.text_encoding().is_single_byte() {
        // A serial coordinate byte keeps its protocol value in every text encoding.
        model.reply(&[
            ESC,
            b'=',
            u8::try_from(row).unwrap_or(b'?'),
            u8::try_from(column).unwrap_or(b'?'),
        ]);
        return;
    }
    // The Java transport encodes the biased cursor coordinates as text in UTF-8.
    let row = char::from_u32(row as u32).unwrap_or(' ');
    let column = char::from_u32(column as u32).unwrap_or(' ');
    let mut response = [0; 10];
    response[..2].copy_from_slice(b"\x1b=");
    let row_end = 2 + row.encode_utf8(&mut response[2..]).len();
    let end = row_end + column.encode_utf8(&mut response[row_end..]).len();
    model.reply(&response[..end]);
}

pub(crate) fn encode_key(
    profile: TerminalProfile,
    key: TerminalKey,
    modifiers: KeyModifiers,
) -> Option<Vec<u8>> {
    if !matches!(
        profile,
        TerminalProfile::Tvi910 | TerminalProfile::Tvi920 | TerminalProfile::Tvi925
    ) {
        return None;
    }
    if let Some(sequence) = legacy_modified_key(key, modifiers) {
        return Some(sequence.to_vec());
    }
    let extended = profile != TerminalProfile::Tvi910;
    let code: &[u8] = match key {
        TerminalKey::Up => b"\x0b",
        TerminalKey::Down => b"\n",
        TerminalKey::Left | TerminalKey::Backspace => b"\x08",
        TerminalKey::Right => b"\x0c",
        TerminalKey::Home => b"\x1e",
        TerminalKey::Enter => b"\r",
        TerminalKey::Tab => b"\t",
        TerminalKey::Escape => b"\x1b",
        TerminalKey::Function(number @ 1..=12) => &[1, b'@' + number - 1],
        TerminalKey::Insert if extended => b"\x1bQ",
        TerminalKey::Delete if extended => b"\x1bW",
        TerminalKey::PageUp if extended => b"\x1bJ",
        TerminalKey::PageDown if extended => b"\x1bK",
        TerminalKey::End if extended => b"\x1bT",
        TerminalKey::Insert => b"\x1b[2~",
        TerminalKey::Delete => b"\x1b[3~",
        TerminalKey::PageUp => b"\x1b[5~",
        TerminalKey::PageDown => b"\x1b[6~",
        TerminalKey::End => b"\x1b[F",
        TerminalKey::Keypad(number @ 0..=9) => &[b'0' + number],
        _ => return None,
    };
    let mut result = Vec::with_capacity(code.len() + 3);
    if modifiers.alt {
        result.push(ESC);
    }
    if !modifiers.alt && code.len() > 2 && (modifiers.shift || modifiers.control) {
        let modifier = 1 + u8::from(modifiers.shift) + 4 * u8::from(modifiers.control);
        let (last, prefix) = code.split_last()?;
        result.extend_from_slice(prefix);
        if code.len() == 3 {
            result.push(b'1');
        }
        result.extend_from_slice(&[b';', b'0' + modifier, *last]);
    } else {
        result.extend_from_slice(code);
    }
    Some(result)
}
