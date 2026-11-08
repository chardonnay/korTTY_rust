use crate::{Color, Model, TerminalKey};

/// The C64 character modes share punctuation and shifted graphics.
/// These Unicode approximations match the SithTermFX v1.2.7 mapping.
const SHIFTED_GRAPHICS: [char; 32] = [
    '\u{a0}', '▌', '▄', '▔', '▁', '▏', '▒', '▕', '▞', '◤', '▜', '▘', '▔', '▀', '▐', '▚', '▖', '▞',
    '▗', '▃', '▝', '▂', '▖', '▞', '▚', '▙', '▛', '▜', '█', '▟', '▛', '█',
];

const GRAPHICS: [char; 32] = [
    '─', '♠', '│', '─', '▗', '▐', '▁', '▖', '▝', '┘', '┌', '┴', '┬', '┤', '└', '┼', '┐', '▂', '▄',
    '▎', '▌', '▃', '▅', '▆', '▇', '█', '▘', '▞', '▙', '▛', 'π', '◥',
];

pub(crate) struct Petscii {
    pub(crate) uppercase_graphics: bool,
}

impl Default for Petscii {
    fn default() -> Self {
        Self {
            uppercase_graphics: true,
        }
    }
}

impl Petscii {
    pub(crate) fn advance(&mut self, model: &mut Model, bytes: &[u8]) {
        for &byte in bytes {
            let cursor = model.cursor();
            match byte {
                0x07 => model.ring_bell(),
                0x0d => {
                    model.carriage_return();
                    model.line_feed();
                }
                0x0e => self.uppercase_graphics = false,
                0x8e => self.uppercase_graphics = true,
                0x11 => model.move_rows(1, cursor.column),
                0x91 => model.move_rows(-1, cursor.column),
                0x1d => model.move_cursor(cursor.row, cursor.column + 1),
                0x9d => model.move_cursor(cursor.row, cursor.column.saturating_sub(1)),
                0x13 => model.move_cursor(0, 0),
                0x12 => model.style.inverse = true,
                0x92 => model.style.inverse = false,
                0x14 => {
                    model.move_cursor(cursor.row, cursor.column.saturating_sub(1));
                    let style = model.style.erase();
                    let right = model.columns - 1;
                    model.screen_mut().delete_cells(1, style, right);
                }
                0x93 => {
                    model.erase_display(2, false);
                    model.move_cursor(0, 0);
                    model.style.inverse = false;
                }
                0x94 => {
                    let style = model.style.erase();
                    let right = model.columns - 1;
                    model.screen_mut().insert_cells(1, style, right);
                }
                _ => {
                    if let Some(color) = color(byte) {
                        model.style.foreground = Color::Indexed(color);
                    } else if let Some(ch) = decode(byte, self.uppercase_graphics) {
                        model.put_character(ch, 1);
                    }
                }
            }
        }
    }
}

fn color(byte: u8) -> Option<u8> {
    let index = match byte {
        0x90 => 0,
        0x05 => 1,
        0x1c => 2,
        0x9f => 3,
        0x9c => 4,
        0x1e => 5,
        0x1f => 6,
        0x9e => 7,
        0x81 | 0x85 => 8,
        0x95 | 0x86 => 9,
        0x96 | 0x87 => 10,
        0x97 | 0x88 => 11,
        0x98 | 0x89 => 12,
        0x99 | 0x8a => 13,
        0x9a => 14,
        0x9b => 15,
        _ => return None,
    };
    Some(index)
}

pub(super) fn decode(byte: u8, uppercase_graphics: bool) -> Option<char> {
    let ch = match byte {
        0x20..=0x40 => char::from(byte),
        0x41..=0x5a if uppercase_graphics => char::from(byte),
        0x41..=0x5a => char::from(byte + 0x20),
        0x5b => '[',
        0x5c => '£',
        0x5d => ']',
        0x5e => '↑',
        0x5f => '←',
        0x60..=0x7e if uppercase_graphics => GRAPHICS[usize::from(byte - 0x60)],
        0x60 | 0xc0 if !uppercase_graphics => '─',
        0x61..=0x7a if !uppercase_graphics => char::from(byte - 0x20),
        0x7b..=0x7e if !uppercase_graphics => ['┼', '▒', '│', 'π'][usize::from(byte - 0x7b)],
        0xc0..=0xdf if uppercase_graphics => GRAPHICS[usize::from(byte - 0xc0)],
        0xc1..=0xda => char::from(byte - 0x60),
        0xdb..=0xdf => ['┼', '▒', '│', 'π', '◥'][usize::from(byte - 0xdb)],
        0xa0..=0xbf => SHIFTED_GRAPHICS[usize::from(byte - 0xa0)],
        0xe0..=0xfe => SHIFTED_GRAPHICS[usize::from(byte - 0xe0)],
        0xff => 'π',
        _ => return None,
    };
    Some(ch)
}

pub(crate) fn encode_text(text: &str, uppercase_graphics: bool) -> Vec<u8> {
    let mut input = text.chars().peekable();
    let mut output = Vec::with_capacity(text.len());
    while let Some(ch) = input.next() {
        let code = match ch {
            '\r' | '\n' => {
                if ch == '\r' && input.peek() == Some(&'\n') {
                    input.next();
                }
                0x0d
            }
            'A'..='Z' if !uppercase_graphics => ch as u8 + 0x20,
            'A'..='Z' => ch as u8,
            'a'..='z' => ch as u8 - 0x20,
            ' '..='@' | '[' | ']' => ch as u8,
            _ => (0x20..=0xff)
                .find(|&code| decode(code, uppercase_graphics) == Some(ch))
                .unwrap_or(b'?'),
        };
        output.push(code);
    }
    output
}

pub(crate) fn encode_key(key: TerminalKey) -> Option<u8> {
    let code = match key {
        TerminalKey::Up => 0x91,
        TerminalKey::Down => 0x11,
        TerminalKey::Left => 0x9d,
        TerminalKey::Right => 0x1d,
        TerminalKey::Home => 0x13,
        TerminalKey::Delete | TerminalKey::Backspace => 0x14,
        TerminalKey::Insert => 0x94,
        TerminalKey::Enter => 0x0d,
        TerminalKey::Function(number @ 1..=8) => 0x84 + number,
        _ => return None,
    };
    Some(code)
}
