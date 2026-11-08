//! IBM-3179-2 and IBM-3180-2 presentation spaces for the RFC 1205 display profile.

mod save;
mod stream;

use std::error::Error;
use std::fmt;

use zeroize::Zeroizing;

use crate::{Record, ebcdic};
use save::SavedScreens;

const MAX_CELLS: usize = ScreenSize::Wide.cells();
const MAX_FIELDS: usize = 256;

/// Maximum UTF-8 input bytes accepted by the widest supported presentation space.
pub const MAX_TEXT_BYTES: usize = 2 * MAX_CELLS;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Model5250 {
    /// IBM-3179-2, an 80×24 color display.
    #[default]
    Ibm3179Model2,
    /// IBM-3180-2, an 80×24 or 132×27 monochrome display.
    Ibm3180Model2,
}

impl Model5250 {
    /// Name used in Telnet TERMINAL-TYPE negotiation.
    pub const fn terminal_type(self) -> &'static str {
        match self {
            Self::Ibm3179Model2 => "IBM-3179-2",
            Self::Ibm3180Model2 => "IBM-3180-2",
        }
    }
}

impl fmt::Display for Model5250 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = match self {
            Self::Ibm3179Model2 => "80×24 color",
            Self::Ibm3180Model2 => "up to 132×27 monochrome",
        };
        write!(formatter, "{} ({size})", self.terminal_type())
    }
}

#[derive(Clone, Copy)]
enum ScreenSize {
    Standard,
    Wide,
}

impl ScreenSize {
    const fn rows(self) -> usize {
        match self {
            Self::Standard => 24,
            Self::Wide => 27,
        }
    }

    const fn columns(self) -> usize {
        match self {
            Self::Standard => 80,
            Self::Wide => 132,
        }
    }

    const fn cells(self) -> usize {
        self.rows() * self.columns()
    }

    fn index(self, row: usize, column: usize) -> Option<usize> {
        (row < self.rows() && column < self.columns()).then(|| row * self.columns() + column)
    }

    fn position(self, address: usize) -> Position5250 {
        let columns = self.columns();
        Position5250 {
            row: (address / columns) as u16,
            column: (address % columns) as u16,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position5250 {
    pub column: u16,
    pub row: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color5250 {
    Green,
    White,
    Red,
    Turquoise,
    Yellow,
    Pink,
    Blue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell5250 {
    /// A space for attributes and all nondisplay data.
    pub character: char,
    pub color: Color5250,
    pub reverse: bool,
    pub underline: bool,
    pub blink: bool,
    pub column_separator: bool,
    pub hidden: bool,
    pub editable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot5250 {
    pub columns: u16,
    pub rows: u16,
    pub cursor: Position5250,
    pub keyboard_locked: bool,
    pub invited: bool,
    pub message_waiting: bool,
    pub insert_mode: bool,
    /// Row-major cells. Nondisplay data never appears in the snapshot.
    pub cells: Vec<Cell5250>,
}

#[derive(Default)]
pub struct Update5250 {
    pub alarm: bool,
    pub reply: Option<Record>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Aid5250 {
    /// Signals the host even while the keyboard is locked or input is uninvited.
    Attention,
    /// Requests the host's System Request menu independently of invited input.
    SystemRequest,
    Enter,
    Clear,
    Help,
    RollUp,
    RollDown,
    Pf(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edit5250 {
    Left,
    Right,
    Up,
    Down,
    Home,
    Tab,
    BackTab,
    Backspace,
    Delete,
    EraseEof,
    ToggleInsert,
    Reset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error5250 {
    WrongProfile,
    Malformed(&'static str),
    UnsupportedOpcode(u8),
    UnsupportedCommand(u8),
    UnsupportedOrder(u8),
    UnsupportedField,
    SaveUnavailable,
    KeyboardLocked,
    NotInvited,
    ProtectedField,
    FieldFull,
    FieldValidation,
    UnsupportedCharacter,
    InvalidAid,
    InvalidPosition,
}

impl fmt::Display for Error5250 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongProfile => formatter.write_str("record is not TN5250"),
            Self::Malformed(message) => formatter.write_str(message),
            Self::UnsupportedOpcode(code) => {
                write!(formatter, "unsupported 5250 GDS opcode {code:#04x}")
            }
            Self::UnsupportedCommand(code) => {
                write!(formatter, "unsupported 5250 command {code:#04x}")
            }
            Self::UnsupportedOrder(code) => write!(formatter, "unsupported 5250 order {code:#04x}"),
            Self::UnsupportedField => {
                formatter.write_str("unsupported 5250 field format or control")
            }
            Self::SaveUnavailable => formatter.write_str("could not save the 5250 screen"),
            Self::KeyboardLocked => formatter.write_str("5250 keyboard is locked"),
            Self::NotInvited => formatter.write_str("5250 host has not invited input"),
            Self::ProtectedField => formatter.write_str("5250 field is protected"),
            Self::FieldFull => formatter.write_str("5250 field has no more input space"),
            Self::FieldValidation => {
                formatter.write_str("input does not satisfy the 5250 field format")
            }
            Self::UnsupportedCharacter => {
                formatter.write_str("character is unavailable in IBM CCSID 37")
            }
            Self::InvalidAid => formatter.write_str("invalid 5250 function key"),
            Self::InvalidPosition => {
                formatter.write_str("5250 cursor position is outside the screen")
            }
        }
    }
}

impl Error for Error5250 {}

#[derive(Clone, Copy)]
struct Field {
    start: usize,
    end: usize,
    format: u16,
    transparent: bool,
    hidden: bool,
}

impl Field {
    fn bypass(self) -> bool {
        self.format & 0x2000 != 0
    }

    fn modified(self) -> bool {
        self.format & 0x0800 != 0
    }

    fn contains(self, address: usize) -> bool {
        (self.start..self.end).contains(&address)
    }

    fn accepts(self, character: char) -> bool {
        match (self.format >> 8) & 7 {
            1 => character.is_alphabetic() || matches!(character, ' ' | ',' | '.' | '-'),
            3 => character.is_ascii_digit() || matches!(character, ' ' | ',' | '.' | '-' | '+'),
            5 => character.is_ascii_digit(),
            _ => true,
        }
    }
}

#[derive(Clone, Copy)]
struct Input {
    mode: ReadMode,
    control: [u8; 2],
}

#[derive(Clone, Copy)]
enum ReadMode {
    All,
    Modified,
    ModifiedAlternate,
}

#[derive(Clone)]
struct ErrorLine {
    cells: Zeroizing<Vec<u8>>,
    awaiting_help: bool,
}

#[derive(Clone)]
pub struct Tn5250Screen {
    model: Model5250,
    size: ScreenSize,
    cells: Zeroizing<Vec<u8>>,
    fields: Vec<Field>,
    cursor: usize,
    home: Option<usize>,
    error_row: usize,
    error_line: Option<ErrorLine>,
    keyboard_locked: bool,
    operator_error: bool,
    insert_mode: bool,
    message_waiting: bool,
    pending: Option<Input>,
    input_suspended: bool,
    saved: SavedScreens,
}

impl Default for Tn5250Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Tn5250Screen {
    pub fn new() -> Self {
        Self::with_model(Model5250::default())
    }

    /// Creates a display in 80×24 mode. The IBM-3180-2 can switch to
    /// 132×27 when the host sends Clear Unit Alternate.
    pub fn with_model(model: Model5250) -> Self {
        let size = ScreenSize::Standard;
        Self {
            model,
            size,
            cells: Zeroizing::new(vec![0; size.cells()]),
            fields: Vec::new(),
            cursor: 0,
            home: None,
            error_row: size.rows() - 1,
            error_line: None,
            keyboard_locked: true,
            operator_error: false,
            insert_mode: false,
            message_waiting: false,
            pending: None,
            input_suspended: false,
            saved: SavedScreens::default(),
        }
    }

    fn cell_attributes(&self) -> impl Iterator<Item = (u8, Option<&Field>)> {
        let mut attribute = 0x20;
        let mut fields = self.fields.iter().peekable();
        self.cells.iter().enumerate().map(move |(index, &byte)| {
            while fields.peek().is_some_and(|field| field.end <= index) {
                fields.next();
            }
            if is_attribute(byte) {
                attribute = byte;
            }
            let field = fields.peek().copied().filter(|field| field.contains(index));
            (attribute, field)
        })
    }

    pub fn snapshot(&self) -> Snapshot5250 {
        let columns = self.size.columns();
        let mut error_attribute = 0x20;
        let cells = self
            .cells
            .iter()
            .zip(self.cell_attributes())
            .enumerate()
            .map(|(index, (&byte, (attribute, field)))| {
                let (byte, attribute, field) = match &self.error_line {
                    Some(line) if index / columns == self.error_row => {
                        let byte = line.cells[index % columns];
                        if is_attribute(byte) {
                            error_attribute = byte;
                        }
                        (byte, error_attribute, None)
                    }
                    _ => (byte, attribute, field),
                };
                let marker = is_attribute(byte);
                let hidden = is_hidden(attribute, field);
                Cell5250 {
                    character: if marker || hidden {
                        ' '
                    } else {
                        ebcdic::decode(byte)
                    },
                    color: self.color(attribute),
                    reverse: attribute & 1 != 0 && !hidden,
                    underline: attribute & 4 != 0 && !hidden,
                    blink: attribute & 0x1a == 0x0a && !hidden,
                    column_separator: attribute & 0x18 == 0x10 && attribute != 0x36 && !hidden,
                    hidden,
                    editable: !marker && field.is_some_and(|field| !field.bypass()),
                }
            })
            .collect();
        Snapshot5250 {
            columns: columns as u16,
            rows: self.size.rows() as u16,
            cursor: self.size.position(self.cursor),
            keyboard_locked: self.keyboard_inhibited(),
            invited: self.pending.is_some() && !self.input_suspended,
            message_waiting: self.message_waiting,
            insert_mode: self.insert_mode,
            cells,
        }
    }

    fn color(&self, attribute: u8) -> Color5250 {
        if self.model == Model5250::Ibm3180Model2 {
            return Color5250::Green;
        }
        match attribute & 0x1a {
            0x02 => Color5250::White,
            0x08 | 0x0a => Color5250::Red,
            0x10 => Color5250::Turquoise,
            0x12 => Color5250::Yellow,
            0x18 => Color5250::Pink,
            0x1a => Color5250::Blue,
            _ => Color5250::Green,
        }
    }

    /// Applies a GDS record atomically, preserving state on unsupported data.
    pub fn apply_record(&mut self, record: &Record) -> Result<Update5250, Error5250> {
        let Record::Tn5250e(bytes) = record else {
            return Err(Error5250::WrongProfile);
        };
        if !crate::valid_gds(bytes) {
            return Err(Error5250::Malformed("invalid 5250 GDS header"));
        }
        let mut screen = self.clone();
        let update = screen.apply_gds(bytes)?;
        *self = screen;
        Ok(update)
    }

    pub fn set_cursor(&mut self, position: Position5250) -> Result<(), Error5250> {
        self.ensure_keyboard()?;
        let row = usize::from(position.row);
        let column = usize::from(position.column);
        self.cursor = self
            .size
            .index(row, column)
            .ok_or(Error5250::InvalidPosition)?;
        Ok(())
    }

    /// Applies text atomically. A field error leaves the field data unchanged.
    pub fn insert_text(&mut self, text: &str) -> Result<(), Error5250> {
        self.ensure_keyboard()?;
        if text.len() > 2 * self.size.cells() {
            return Err(Error5250::FieldFull);
        }
        let mut screen = self.clone();
        let mut visited = [false; MAX_CELLS];
        for character in text.chars() {
            if std::mem::replace(&mut visited[screen.cursor], true) {
                return self.operator_result(Err(Error5250::FieldFull));
            }
            if let Err(error) = screen.type_character(character) {
                return self.operator_result(Err(error));
            }
        }
        *self = screen;
        Ok(())
    }

    pub fn edit(&mut self, key: Edit5250) -> Result<(), Error5250> {
        if key == Edit5250::Reset {
            if self
                .error_line
                .as_ref()
                .is_some_and(|line| line.awaiting_help)
            {
                return Ok(());
            }
            self.error_line = None;
            self.operator_error = false;
            self.insert_mode = false;
            return Ok(());
        }
        self.ensure_keyboard()?;
        let result = match key {
            Edit5250::Left => self.move_cursor(self.size.cells() - 1),
            Edit5250::Right => self.move_cursor(1),
            Edit5250::Up => self.move_cursor(self.size.cells() - self.size.columns()),
            Edit5250::Down => self.move_cursor(self.size.columns()),
            Edit5250::Home => {
                self.cursor = self.home.or_else(|| self.first_input()).unwrap_or(0);
                Ok(())
            }
            Edit5250::Tab => {
                self.cursor = self.next_input(self.cursor + 1).unwrap_or(0);
                Ok(())
            }
            Edit5250::BackTab => {
                self.cursor = self
                    .fields
                    .iter()
                    .rev()
                    .find(|field| !field.bypass() && field.start < self.cursor)
                    .or_else(|| self.fields.iter().rev().find(|field| !field.bypass()))
                    .map_or(0, |field| field.start);
                Ok(())
            }
            Edit5250::Delete | Edit5250::Backspace | Edit5250::EraseEof => self.edit_field(key),
            Edit5250::ToggleInsert => {
                self.insert_mode = !self.insert_mode;
                Ok(())
            }
            Edit5250::Reset => Ok(()),
        };
        self.operator_result(result)
    }

    /// Services the invited READ or sends an Attention, System Request or Help signal.
    pub fn send_aid(&mut self, aid: Aid5250) -> Result<Update5250, Error5250> {
        let code = match aid {
            Aid5250::Attention | Aid5250::SystemRequest => {
                // RFC 1205 section 3 numbers flags from the most significant bit.
                let flags = if aid == Aid5250::Attention {
                    0x40
                } else {
                    0x04
                };
                return Ok(Update5250 {
                    reply: Some(stream::signal_record(flags)),
                    ..Update5250::default()
                });
            }
            Aid5250::Enter => 0xf1,
            Aid5250::Help if self.error_line.is_some() => return self.request_error_help(),
            Aid5250::Help => 0xf3,
            Aid5250::RollDown => 0xf4,
            Aid5250::RollUp => 0xf5,
            Aid5250::Clear => 0xbd,
            Aid5250::Pf(key @ 1..=12) => 0x30 + key,
            Aid5250::Pf(key @ 13..=24) => 0xb0 + (key - 12),
            Aid5250::Pf(_) => return Err(Error5250::InvalidAid),
        };
        if self.display_keyboard_inhibited() {
            return Err(Error5250::KeyboardLocked);
        }
        let Some(input) = self.pending.filter(|_| !self.input_suspended) else {
            return Err(Error5250::NotInvited);
        };
        let include_data = !matches!(aid, Aid5250::Clear | Aid5250::Help);
        if include_data && !self.valid_input() {
            self.operator_error = true;
            return Err(Error5250::FieldValidation);
        }
        let reply = self.input_reply(input.mode, code, include_data);
        self.pending = None;
        self.keyboard_locked = true;
        self.insert_mode = false;
        self.before_control(input.control[0]);
        let alarm = self.after_control(input.control[1], None);
        Ok(Update5250 {
            alarm,
            reply: Some(reply),
        })
    }

    fn ensure_keyboard(&self) -> Result<(), Error5250> {
        if self.keyboard_inhibited() {
            Err(Error5250::KeyboardLocked)
        } else {
            Ok(())
        }
    }

    fn keyboard_inhibited(&self) -> bool {
        self.input_suspended || self.display_keyboard_inhibited()
    }

    fn display_keyboard_inhibited(&self) -> bool {
        self.keyboard_locked || self.operator_error || self.error_line.is_some()
    }

    fn operator_result(&mut self, result: Result<(), Error5250>) -> Result<(), Error5250> {
        if matches!(
            result,
            Err(Error5250::ProtectedField | Error5250::FieldFull | Error5250::FieldValidation)
        ) {
            self.operator_error = true;
        }
        result
    }

    fn move_cursor(&mut self, amount: usize) -> Result<(), Error5250> {
        self.cursor = (self.cursor + amount) % self.size.cells();
        Ok(())
    }

    fn type_character(&mut self, character: char) -> Result<(), Error5250> {
        let index = self.editable_field()?;
        let field = self.fields[index];
        let character = if field.format & 0x0020 != 0 {
            let mut upper = character.to_uppercase();
            let first = upper.next().unwrap_or(character);
            if upper.next().is_some() {
                return Err(Error5250::UnsupportedCharacter);
            }
            first
        } else {
            character
        };
        if !field.accepts(character) {
            return Err(Error5250::FieldValidation);
        }
        let byte = ebcdic::encode(character).ok_or(Error5250::UnsupportedCharacter)?;
        if self.insert_mode {
            let null = (self.cursor..field.end)
                .find(|&address| self.cells[address] == 0)
                .ok_or(Error5250::FieldFull)?;
            self.cells.copy_within(self.cursor..null, self.cursor + 1);
        }
        self.cells[self.cursor] = byte;
        self.fields[index].format |= 0x0800;
        self.cursor += 1;
        if self.cursor == field.end {
            self.cursor = self.next_input(self.cursor).unwrap_or(field.start);
        }
        Ok(())
    }

    fn edit_field(&mut self, key: Edit5250) -> Result<(), Error5250> {
        let index = self.editable_field()?;
        let field = self.fields[index];
        if key == Edit5250::Backspace {
            if self.cursor == field.start {
                return Ok(());
            }
            self.cursor -= 1;
        }
        if key == Edit5250::EraseEof {
            self.cells[self.cursor..field.end].fill(0);
        } else {
            self.cells
                .copy_within(self.cursor + 1..field.end, self.cursor);
            self.cells[field.end - 1] = 0;
        }
        self.fields[index].format |= 0x0800;
        Ok(())
    }

    fn editable_field(&self) -> Result<usize, Error5250> {
        self.fields
            .iter()
            .position(|field| field.contains(self.cursor) && !field.bypass())
            .filter(|_| !is_attribute(self.cells[self.cursor]))
            .ok_or(Error5250::ProtectedField)
    }

    fn first_input(&self) -> Option<usize> {
        self.fields
            .iter()
            .find(|field| !field.bypass())
            .map(|field| field.start)
    }

    fn next_input(&self, address: usize) -> Option<usize> {
        self.fields
            .iter()
            .find(|field| field.start >= address && !field.bypass())
            .map(|field| field.start)
            .or_else(|| self.first_input())
    }

    fn valid_input(&self) -> bool {
        self.fields
            .iter()
            .filter(|field| !field.bypass())
            .all(|field| {
                (field.format & 8 == 0 || field.modified())
                    && (field.format & 7 != 7
                        || !field.modified()
                        || !self.cells[field.start..field.end].contains(&0))
            })
    }
}

fn is_attribute(byte: u8) -> bool {
    (0x20..=0x3f).contains(&byte)
}

fn is_hidden(attribute: u8, field: Option<&Field>) -> bool {
    attribute & 7 == 7 || field.is_some_and(|field| field.hidden)
}
