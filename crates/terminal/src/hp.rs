//! HP stream commands and HP 700/92 ANSI mode from SithTermFX v1.2.7.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use crate::{Color, Input, KeyModifiers, Model, Style, Terminal, TerminalKey, TerminalProfile};

mod forms;

pub(crate) use forms::FormLine;
pub use forms::Hp2392Input;

const ESC: u8 = 0x1b;
const MAX_SEQUENCE_BYTES: u16 = 256;
const MAX_LABEL_BYTES: u16 = 16;
const MAX_VALUE_BYTES: u16 = 80;
const MAX_REPLAY_BYTES: usize = 256;

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Group,
    Enhancement(bool),
    Parameter(Parameter),
    Softkey(SoftkeyPayload),
    Csi(Csi),
    DiscardCsi,
    DiscardString(bool),
    DiscardGroup(u16),
}

#[derive(Clone, Copy)]
enum ParameterKind {
    Absolute,
    Relative,
    Keyboard,
    DisplayFunctions,
    Softkey,
}

struct Parameter {
    kind: ParameterKind,
    number: Option<u16>,
    bytes: u16,
    column: Option<u16>,
    row: Option<u16>,
    softkey: Option<usize>,
}

impl Parameter {
    fn new(kind: ParameterKind) -> Self {
        Self {
            kind,
            number: None,
            bytes: 0,
            column: None,
            row: None,
            softkey: None,
        }
    }
}

struct SoftkeyPayload {
    parameter: Parameter,
    remaining: u16,
    // Labels occupy bytes on the wire but there is no status-label row here.
    value: Option<Vec<u8>>,
}

#[derive(Default)]
struct Csi {
    private: bool,
    current: Option<u16>,
    bytes: u16,
    values: [u16; 16],
    count: usize,
}

impl Csi {
    fn push(&mut self) -> bool {
        let Some(slot) = self.values.get_mut(self.count) else {
            return false;
        };
        *slot = self.current.take().unwrap_or(0);
        self.count += 1;
        true
    }
}

#[derive(Default)]
struct Utf8 {
    bytes: [u8; 4],
    length: usize,
}

/// Mode flags observed by the SithTermFX-compatible HP 2392 parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hp2392Modes {
    pub block_mode: bool,
    pub protect_mode: bool,
    /// Zero-based row at which the host enabled the memory lock.
    pub memory_lock_row: Option<u16>,
}

impl Terminal {
    pub fn hp2392_modes(&self) -> Option<Hp2392Modes> {
        match &self.input {
            Input::Hp(hp) if hp.profile == TerminalProfile::Hp2392 => Some(Hp2392Modes {
                block_mode: hp.block || hp.forms.block,
                protect_mode: hp.protect,
                memory_lock_row: hp.memory_lock.map(|row| row as u16),
            }),
            _ => None,
        }
    }
}

pub(crate) struct Hp {
    profile: TerminalProfile,
    state: State,
    utf8: Utf8,
    ansi: bool,
    block: bool,
    protect: bool,
    memory_lock: Option<usize>,
    softkeys: [Vec<u8>; 8],
    forms: forms::Forms,
}

impl Hp {
    pub(crate) fn new(profile: TerminalProfile) -> Self {
        Self {
            profile,
            state: State::Ground,
            utf8: Utf8::default(),
            ansi: false,
            block: false,
            protect: false,
            memory_lock: None,
            softkeys: std::array::from_fn(|_| Vec::new()),
            forms: forms::Forms::default(),
        }
    }

    pub(crate) fn advance(&mut self, model: &mut Model, bytes: &[u8]) {
        self.apply_memory_lock(model);
        for &byte in bytes {
            if let Some(key) = self.step(model, byte) {
                self.replay(model, key);
            }
        }
    }

    pub(crate) fn cancel_transfer(&mut self) {
        self.forms.cancel_transfer();
    }

    fn apply_memory_lock(&self, model: &mut Model) {
        if let Some(row) = self.memory_lock {
            model.scroll_top = (row + 1).min(model.rows - 1);
            model.scroll_bottom = model.rows - 1;
        }
    }

    fn step(&mut self, model: &mut Model, byte: u8) -> Option<usize> {
        let state = std::mem::take(&mut self.state);
        match state {
            State::Ground => {
                if byte == ESC {
                    self.flush_utf8(model);
                    self.state = State::Escape;
                } else {
                    self.ground(model, byte);
                }
            }
            State::Softkey(payload) => self.softkey_payload(payload, byte),
            State::DiscardString(escaped) => {
                if byte != b'\x07'
                    && byte != b'\x18'
                    && byte != b'\x1a'
                    && !(escaped && byte == b'\\')
                {
                    self.state = State::DiscardString(byte == ESC);
                }
            }
            _ if byte == ESC => self.state = State::Escape,
            _ if matches!(byte, 0x18 | 0x1a) => {}
            State::Escape => return self.escape(model, byte),
            State::Group => {
                let kind = match byte {
                    b'a' => ParameterKind::Absolute,
                    b'd' => {
                        self.state = State::Enhancement(false);
                        return None;
                    }
                    b'f' => ParameterKind::Softkey,
                    b'j' => ParameterKind::Relative,
                    b'k' => ParameterKind::Keyboard,
                    b's' => ParameterKind::DisplayFunctions,
                    _ => {
                        self.state = State::DiscardGroup(0);
                        return None;
                    }
                };
                self.state = State::Parameter(Parameter::new(kind));
            }
            State::Enhancement(false) if byte == b's' => self.state = State::Enhancement(true),
            State::Enhancement(hidden) => {
                if enhancement(model, byte, hidden) && self.profile == TerminalProfile::Hp2392 {
                    self.forms.style(model);
                }
            }
            State::Parameter(parameter) => self.parameter(model, parameter, byte),
            State::Csi(csi) => self.csi(model, csi, byte),
            State::DiscardCsi if !(0x40..=0x7e).contains(&byte) && byte >= 0x20 => {
                self.state = State::DiscardCsi;
            }
            State::DiscardGroup(bytes)
                if bytes < MAX_SEQUENCE_BYTES && byte >= 0x20 && !byte.is_ascii_alphabetic() =>
            {
                self.state = State::DiscardGroup(bytes + 1);
            }
            State::DiscardCsi | State::DiscardGroup(_) => {}
        }
        None
    }

    fn ground(&mut self, model: &mut Model, byte: u8) {
        if byte.is_ascii() {
            self.flush_utf8(model);
            match byte {
                0x07 => model.ring_bell(),
                0x08 => {
                    let cursor = model.cursor();
                    if cursor.column == 0 && cursor.row > 0 {
                        model.move_cursor(cursor.row - 1, model.columns - 1);
                    } else {
                        model.move_columns(-1);
                    }
                }
                b'\t' if self.forms.format => self.forms.tab(model, true),
                b'\t' => model.tab(1, true),
                b'\n' | 0x0b | 0x0c => model.line_feed(),
                b'\r' => model.carriage_return(),
                0x11 => self.forms.host_trigger(model),
                0x1e if self.forms.block || self.forms.format => self.forms.host_terminator(model),
                b' '..=b'~' => self.print(model, char::from(byte)),
                _ => {}
            }
            return;
        }

        if self.utf8.length != 0 && byte & 0xc0 != 0x80 {
            self.flush_utf8(model);
        }
        self.utf8.bytes[self.utf8.length] = byte;
        self.utf8.length += 1;
        match std::str::from_utf8(&self.utf8.bytes[..self.utf8.length]) {
            Ok(text) => {
                let character = text.chars().next();
                self.utf8.length = 0;
                if let Some(character) = character {
                    self.print(model, character);
                }
            }
            Err(error) if error.error_len().is_some() || self.utf8.length == 4 => {
                self.flush_utf8(model);
            }
            Err(_) => {}
        }
    }

    fn flush_utf8(&mut self, model: &mut Model) {
        if self.utf8.length != 0 {
            self.utf8.length = 0;
            self.print(model, '�');
        }
    }

    fn print(&mut self, model: &mut Model, character: char) {
        print(model, character);
        if self.profile == TerminalProfile::Hp2392 {
            self.forms.host_character(model, character);
        }
    }

    fn escape(&mut self, model: &mut Model, byte: u8) -> Option<usize> {
        let cursor = model.cursor();
        match byte {
            b'&' => self.state = State::Group,
            b'[' | b'{' if self.forms.block || self.forms.format => {
                self.forms.field_marker(model, true);
            }
            b']' if self.forms.block || self.forms.format => {
                self.forms.field_marker(model, false);
            }
            b'[' => self.state = State::Csi(Csi::default()),
            b']' | b'^' | b'_' => self.state = State::DiscardString(false),
            b')' if self.profile == TerminalProfile::Hp70092 => self.ansi = true,
            b'(' if self.profile == TerminalProfile::Hp70092 => self.ansi = false,
            // The pinned 2392 records these flags. Its common display and
            // keyboard paths do not implement local field protection.
            b'1' | b'2' if self.profile == TerminalProfile::Hp2392 => self.block = byte == b'1',
            b'c' | b'd' if self.profile == TerminalProfile::Hp2392 => self.protect = byte == b'c',
            b'W' | b'X' if self.forms.block || self.forms.format => {
                self.forms.set_format(model, byte == b'W');
            }
            b'E' if self.forms.block || self.forms.format => {
                self.block = false;
                self.protect = false;
                self.memory_lock = None;
                self.forms.reset(model);
            }
            b'`' if self.profile == TerminalProfile::Hp2392 => {
                self.memory_lock = Some(cursor.row);
                self.apply_memory_lock(model);
                model.move_cursor(0, 0);
            }
            b'b' if self.profile == TerminalProfile::Hp2392 => {
                self.memory_lock = None;
                model.scroll_top = 0;
                model.scroll_bottom = model.rows - 1;
                model.move_cursor(0, 0);
            }
            b'A' => model.move_rows(-1, cursor.column),
            b'B' => model.move_rows(1, cursor.column),
            b'C' | b'G' => model.move_columns(1),
            b'i' if self.forms.format => self.forms.tab(model, false),
            b'D' | b'i' => model.move_columns(-1),
            b'F' => model.move_cursor(model.rows - 1, 0),
            b'H' => model.move_cursor(0, 0),
            b'I' => model.reverse_index(),
            b'J' | b'K' if self.profile == TerminalProfile::Hp2392 => {
                self.forms.erase(model, byte == b'J');
            }
            b'J' => model.erase_display(0, false),
            b'K' => model.erase_in_line(0, false),
            b'L' | b'M' if self.forms.format => {}
            b'L' | b'M' => lines(model, byte == b'L', 1),
            b'P' if self.forms.format => self.forms.delete(model),
            b'Q' | b'R' if self.forms.block => self.forms.insert = byte == b'Q',
            b'P' | b'Q' => characters(model, byte == b'Q', 1),
            b'S' => model.scroll_up(model.scroll_top, model.scroll_bottom, 1, true),
            b'T' => model.scroll_down(model.scroll_top, model.scroll_bottom, 1),
            b'U' | b'V' => {
                if self.profile == TerminalProfile::Hp2392 {
                    self.forms.clear_fields(model);
                }
                model.erase_display(2, false);
                model.move_cursor(0, 0);
            }
            b'h' => {
                model.move_cursor(0, 0);
                if self.profile == TerminalProfile::Hp2392 {
                    self.forms.erase(model, true);
                } else {
                    model.erase_display(0, false);
                }
            }
            b'p'..=b'w' => return Some(usize::from(byte - b'p')),
            _ => {}
        }
        None
    }

    fn parameter(&mut self, model: &mut Model, mut parameter: Parameter, byte: u8) {
        parameter.bytes += 1;
        if parameter.bytes > MAX_SEQUENCE_BYTES {
            self.state = State::DiscardGroup(0);
            return;
        }
        if byte.is_ascii_digit() {
            append_digit(&mut parameter.number, byte);
            self.state = State::Parameter(parameter);
            return;
        }
        let number = parameter.number.take();
        match parameter.kind {
            ParameterKind::Absolute => {
                match byte.to_ascii_uppercase() {
                    b'C' => parameter.column = Some(number.unwrap_or(0)),
                    b'R' | b'Y' => parameter.row = Some(number.unwrap_or(0)),
                    _ => return,
                }
                if byte.is_ascii_lowercase() {
                    self.state = State::Parameter(parameter);
                } else {
                    let cursor = model.cursor();
                    model.move_cursor(
                        parameter.row.map_or(cursor.row, usize::from),
                        parameter.column.map_or(cursor.column, usize::from),
                    );
                }
            }
            ParameterKind::Relative => {
                let count = number.unwrap_or(1) as isize;
                let column = model.cursor().column;
                match byte.to_ascii_uppercase() {
                    b'C' => model.move_columns(count),
                    b'L' => model.move_columns(-count),
                    b'R' | b'D' => model.move_rows(count, column),
                    b'U' => model.move_rows(-count, column),
                    _ => {}
                }
            }
            ParameterKind::Softkey => match byte.to_ascii_uppercase() {
                b'A' => {
                    parameter.softkey = number
                        .filter(|key| (1..=8).contains(key))
                        .map(|key| usize::from(key - 1));
                    self.state = State::Parameter(parameter);
                }
                kind @ (b'D' | b'L') => {
                    let length = number.unwrap_or(0);
                    let value = kind == b'L';
                    let limit = if value {
                        MAX_VALUE_BYTES
                    } else {
                        MAX_LABEL_BYTES
                    };
                    if length > limit {
                        return;
                    }
                    let payload = SoftkeyPayload {
                        parameter,
                        remaining: length,
                        value: value.then(|| Vec::with_capacity(usize::from(length))),
                    };
                    if length == 0 {
                        self.finish_softkey(payload);
                    } else {
                        self.state = State::Softkey(payload);
                    }
                }
                _ => {}
            },
            ParameterKind::Keyboard | ParameterKind::DisplayFunctions => {
                if self.profile == TerminalProfile::Hp2392
                    && let Some(number @ 0..=1) = number
                {
                    self.forms
                        .configure(parameter.kind, byte.to_ascii_uppercase(), number == 1);
                    if byte.is_ascii_lowercase() {
                        self.state = State::Parameter(parameter);
                    }
                }
            }
        }
    }

    fn softkey_payload(&mut self, mut payload: SoftkeyPayload, byte: u8) {
        if let Some(value) = &mut payload.value {
            value.push(byte);
        }
        payload.remaining -= 1;
        if payload.remaining == 0 {
            self.finish_softkey(payload);
        } else {
            self.state = State::Softkey(payload);
        }
    }

    fn finish_softkey(&mut self, payload: SoftkeyPayload) {
        if let Some(value) = payload.value {
            if let Some(key) = payload.parameter.softkey {
                self.softkeys[key] = value;
            }
        } else {
            self.state = State::Parameter(payload.parameter);
        }
    }

    fn replay(&mut self, model: &mut Model, key: usize) {
        if self.softkeys[key].is_empty() {
            return;
        }
        let mut pending = VecDeque::from(self.softkeys[key].clone());
        for remaining in (0..MAX_REPLAY_BYTES).rev() {
            let Some(byte) = pending.pop_front() else {
                return;
            };
            if let Some(next) = self.step(model, byte) {
                if pending.len() + self.softkeys[next].len() > remaining {
                    self.state = State::Ground;
                    self.flush_utf8(model);
                    return;
                }
                for &byte in self.softkeys[next].iter().rev() {
                    pending.push_front(byte);
                }
            }
        }
    }

    fn csi(&mut self, model: &mut Model, mut csi: Csi, byte: u8) {
        csi.bytes += 1;
        if csi.bytes > MAX_SEQUENCE_BYTES {
            if !(0x40..=0x7e).contains(&byte) && byte >= 0x20 {
                self.state = State::DiscardCsi;
            }
            return;
        }
        match byte {
            b'?' if self.ansi && csi.bytes == 1 => {
                csi.private = true;
                self.state = State::Csi(csi);
            }
            b'0'..=b'9' => {
                append_digit(&mut csi.current, byte);
                self.state = State::Csi(csi);
            }
            b';' if self.ansi && csi.push() => self.state = State::Csi(csi),
            0x40..=0x7e if csi.push() => self.csi_command(model, &csi, byte),
            0x20..=0x3f => self.state = State::DiscardCsi,
            _ => {}
        }
    }

    fn csi_command(&self, model: &mut Model, csi: &Csi, byte: u8) {
        let first = csi.values[0];
        if csi.private {
            if matches!(byte, b'h' | b'l') {
                for &mode in &csi.values[..csi.count] {
                    if matches!(mode, 7 | 25) {
                        model.set_private_mode(mode, byte == b'h');
                    }
                }
            }
            return;
        }
        let count = usize::from(first.max(1));
        let delta = count as isize;
        let cursor = model.cursor();
        match byte {
            b'A' => model.move_rows(-delta, cursor.column),
            b'B' => model.move_rows(delta, cursor.column),
            b'C' => model.move_columns(delta),
            b'D' => model.move_columns(-delta),
            b'H' | b'f' if self.ansi => {
                model.move_cursor(count - 1, usize::from(csi.values[1].saturating_sub(1)));
            }
            b'J' => model.erase_display(first, false),
            b'K' => model.erase_in_line(first, false),
            b'L' | b'M' => lines(model, byte == b'L', count),
            b'P' | b'@' => characters(model, byte == b'@', count),
            b'm' if self.ansi => ansi_style(model, &csi.values[..csi.count]),
            b'm' => minimal_style(model, first),
            b'r' if self.ansi => {
                if csi.count >= 2 {
                    model.set_margins(
                        usize::from(first.saturating_sub(1)),
                        if csi.values[1] == 0 {
                            model.rows - 1
                        } else {
                            usize::from(csi.values[1] - 1)
                        },
                    );
                } else {
                    model.scroll_top = 0;
                    model.scroll_bottom = model.rows - 1;
                    model.move_cursor(0, 0);
                }
            }
            b'n' if self.ansi && first == 5 => model.reply(b"\x1b[0n"),
            b'n' if self.ansi && first == 6 => model.report_cursor(false),
            _ => {}
        }
    }
}

fn append_digit(number: &mut Option<u16>, byte: u8) {
    *number = Some(
        number
            .unwrap_or(0)
            .saturating_mul(10)
            .saturating_add(u16::from(byte - b'0')),
    );
}

fn print(model: &mut Model, character: char) {
    if model.style.hidden {
        // Keep concealed HP input out of the viewport and its text selection.
        if let Some(width) = character.width().filter(|&width| width > 0) {
            model.put_character(' ', width.min(model.columns));
        }
    } else {
        model.put_char(character);
    }
}

fn lines(model: &mut Model, insert: bool, count: usize) {
    let cursor = model.cursor();
    if (model.scroll_top..=model.scroll_bottom).contains(&cursor.row) {
        if insert {
            model.scroll_down(cursor.row, model.scroll_bottom, count);
        } else {
            model.scroll_up(cursor.row, model.scroll_bottom, count, false);
        }
        model.move_cursor(cursor.row, 0);
    }
}

fn characters(model: &mut Model, insert: bool, count: usize) {
    let style = model.style.erase();
    let right = model.columns - 1;
    if insert {
        model.screen_mut().insert_cells(count, style, right);
    } else {
        model.screen_mut().delete_cells(count, style, right);
    }
}

fn enhancement(model: &mut Model, byte: u8, hidden: bool) -> bool {
    // These assignments retain the pinned emulator's display profile.
    let flags = match byte {
        b'@' | b'S' => 0,
        b'A' => 1,
        b'B' => 4,
        b'D' => 2,
        b'E' => 3,
        b'F' => 5,
        b'H' => 8,
        b'I' => 9,
        b'J' => 12,
        b'K' => 13,
        b'L' => 10,
        _ => return false,
    };
    model.style = Style {
        dim: flags & 1 != 0,
        underline: flags & 2 != 0,
        blink: flags & 4 != 0,
        inverse: flags & 8 != 0,
        hidden: hidden || byte == b'S',
        ..Style::default()
    };
    true
}

fn minimal_style(model: &mut Model, parameter: u16) {
    if matches!(parameter, 0 | 1 | 4 | 5 | 7) {
        model.style = Style {
            bold: parameter == 1,
            underline: parameter == 4,
            blink: parameter == 5,
            inverse: parameter == 7,
            ..Style::default()
        };
    }
}

fn ansi_style(model: &mut Model, parameters: &[u16]) {
    for &parameter in parameters {
        match parameter {
            0 => model.style = Style::default(),
            1 => model.style.bold = true,
            2 => model.style.dim = true,
            4 => model.style.underline = true,
            5 => model.style.blink = true,
            7 => model.style.inverse = true,
            8 => model.style.hidden = true,
            22 => {
                model.style.bold = false;
                model.style.dim = false;
            }
            24 => model.style.underline = false,
            25 => model.style.blink = false,
            27 => model.style.inverse = false,
            30..=37 => model.style.foreground = Color::Indexed((parameter - 30) as u8),
            39 => model.style.foreground = Color::Default,
            40..=47 => model.style.background = Color::Indexed((parameter - 40) as u8),
            49 => model.style.background = Color::Default,
            _ => {}
        }
    }
}

pub(crate) fn encode_key(
    profile: TerminalProfile,
    key: TerminalKey,
    modifiers: KeyModifiers,
) -> Option<Vec<u8>> {
    if !matches!(profile, TerminalProfile::Hp2392 | TerminalProfile::Hp70092) {
        return None;
    }
    let sequence: &[u8] = match key {
        TerminalKey::Up => b"\x1bA",
        TerminalKey::Down => b"\x1bB",
        TerminalKey::Right => b"\x1bC",
        TerminalKey::Left => b"\x1bD",
        TerminalKey::Home => b"\x1bh",
        TerminalKey::End => b"\x1bF",
        TerminalKey::Insert => b"\x1bQ",
        TerminalKey::Delete => b"\x1bP",
        TerminalKey::PageUp => b"\x1bS",
        TerminalKey::PageDown => b"\x1bT",
        TerminalKey::Backspace => b"\x08",
        TerminalKey::Tab if modifiers.shift && !modifiers.alt && !modifiers.control => b"\x1b[Z",
        TerminalKey::Tab => b"\t",
        TerminalKey::Enter => b"\r",
        TerminalKey::Escape => b"\x1b",
        TerminalKey::Keypad(number @ 0..=9) => {
            return Some(with_alt(&[b'0' + number], modifiers.alt));
        }
        TerminalKey::Function(number @ 1..=8) => {
            let base = if profile == TerminalProfile::Hp70092
                && modifiers.shift
                && !modifiers.alt
                && !modifiers.control
            {
                b'P'
            } else {
                b'p'
            };
            let bytes = [ESC, base + number - 1];
            return Some(with_alt(&bytes, modifiers.alt));
        }
        TerminalKey::Function(9) => b"\x1b[20~",
        TerminalKey::Function(10) => b"\x1b[21~",
        TerminalKey::Function(11) => b"\x1b[23~",
        TerminalKey::Function(12) => b"\x1b[24~",
        _ => return None,
    };
    if matches!(key, TerminalKey::Function(9..=12))
        && !modifiers.alt
        && (modifiers.shift || modifiers.control)
    {
        let mut bytes = sequence[..sequence.len() - 1].to_vec();
        let modifier = 1 + u8::from(modifiers.shift) + 4 * u8::from(modifiers.control);
        bytes.extend([b';', b'0' + modifier, b'~']);
        return Some(bytes);
    }
    Some(with_alt(sequence, modifiers.alt))
}

fn with_alt(sequence: &[u8], alt: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sequence.len() + usize::from(alt));
    if alt {
        bytes.push(ESC);
    }
    bytes.extend_from_slice(sequence);
    bytes
}
