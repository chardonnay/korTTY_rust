//! IBM-3278-2 model 2 presentation space for the basic TN3270E display profile.

mod keyboard;
mod reply;
mod stream;

use std::error::Error;
use std::fmt;

use crate::{Record, Tn3270eData, ebcdic};
use reply::ReplyMode;

const COLUMNS: usize = 80;
const ROWS: usize = 24;
const CELLS: usize = COLUMNS * ROWS;
const NO_AID: u8 = 0x60;
const NULL: Cell = Cell::Character {
    byte: 0,
    style: Style3270 {
        foreground: None,
        background: None,
        highlighting: None,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position3270 {
    pub column: u16,
    pub row: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Keyboard3270 {
    Ready,
    HostLocked,
    OperatorError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Aid3270 {
    Enter,
    Clear,
    Pf(u8),
    Pa(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edit3270 {
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
pub enum Color3270 {
    Black,
    Blue,
    Red,
    Pink,
    Green,
    Turquoise,
    Yellow,
    White,
    DeepBlue,
    Orange,
    Purple,
    PaleGreen,
    PaleTurquoise,
    Grey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Highlight3270 {
    Normal,
    Blink,
    Reverse,
    Underline,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style3270 {
    pub foreground: Option<Color3270>,
    pub background: Option<Color3270>,
    pub highlighting: Option<Highlight3270>,
}

impl Style3270 {
    fn over(self, field: Self) -> Self {
        Self {
            foreground: self.foreground.or(field.foreground),
            background: self.background.or(field.background),
            highlighting: self.highlighting.or(field.highlighting),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Field3270 {
    pub protected: bool,
    pub numeric: bool,
    pub intensified: bool,
    pub hidden: bool,
    pub modified: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell3270 {
    /// A space for both attribute markers and nondisplay fields.
    pub character: char,
    pub field_marker: bool,
    /// Attributes of the enclosing field, if the presentation space is formatted.
    pub field: Option<Field3270>,
    pub style: Style3270,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot3270 {
    pub columns: u16,
    pub rows: u16,
    pub cursor: Position3270,
    pub keyboard: Keyboard3270,
    pub insert_mode: bool,
    /// An RFC 2355 mode switch cleared the presentation space. The caller can
    /// handle the original NVT record separately.
    pub nvt_mode: bool,
    /// Row-major cells. Nondisplay data never appears in the snapshot.
    pub cells: Vec<Cell3270>,
}

#[derive(Default)]
pub struct Update3270 {
    pub alarm: bool,
    pub reply: Option<Record>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error3270 {
    WrongProfile,
    NvtMode,
    MissingCommand,
    Malformed(&'static str),
    UnsupportedCommand(u8),
    UnsupportedOrder(u8),
    UnsupportedAttribute(u8),
    UnsupportedStructuredField(u8),
    KeyboardLocked,
    ProtectedField,
    NumericField,
    FieldFull,
    UnsupportedCharacter,
    InvalidAid,
    InvalidPosition,
}

impl fmt::Display for Error3270 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongProfile => formatter.write_str("record is not TN3270E"),
            Self::NvtMode => formatter.write_str("TN3270E is in NVT mode"),
            Self::MissingCommand => formatter.write_str("missing 3270 command"),
            Self::Malformed(reason) => formatter.write_str(reason),
            Self::UnsupportedCommand(code) => {
                write!(formatter, "unsupported 3270 command {code:#04x}")
            }
            Self::UnsupportedOrder(code) => write!(formatter, "unsupported 3270 order {code:#04x}"),
            Self::UnsupportedAttribute(code) => {
                write!(formatter, "unsupported 3270 attribute {code:#04x}")
            }
            Self::UnsupportedStructuredField(code) => {
                write!(formatter, "unsupported 3270 structured field {code:#04x}")
            }
            Self::KeyboardLocked => formatter.write_str("3270 keyboard is locked"),
            Self::ProtectedField => formatter.write_str("3270 field is protected"),
            Self::NumericField => formatter.write_str("3270 field requires numeric input"),
            Self::FieldFull => formatter.write_str("3270 field has no more input space"),
            Self::UnsupportedCharacter => {
                formatter.write_str("character is unavailable in IBM CCSID 37")
            }
            Self::InvalidAid => formatter.write_str("invalid 3270 function or attention key"),
            Self::InvalidPosition => {
                formatter.write_str("3270 cursor position is outside the screen")
            }
        }
    }
}

impl Error for Error3270 {}

#[derive(Clone, Copy)]
enum Cell {
    Character { byte: u8, style: Style3270 },
    Field(Field),
}

impl Cell {
    fn field(self) -> Option<Field> {
        match self {
            Self::Field(field) => Some(field),
            Self::Character { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Field {
    bits: u8,
    style: Style3270,
}

impl Field {
    fn protected(self) -> bool {
        self.bits & 0x20 != 0
    }

    fn numeric(self) -> bool {
        self.bits & 0x10 != 0
    }

    fn modified(self) -> bool {
        self.bits & 0x01 != 0
    }

    fn presentation(self) -> Field3270 {
        Field3270 {
            protected: self.protected(),
            numeric: self.numeric(),
            intensified: self.bits & 0x0c == 0x08,
            hidden: self.bits & 0x0c == 0x0c,
            modified: self.modified(),
        }
    }
}

/// Presentation and local keyboard state for an IBM-3278-2 display.
///
/// Screen commands are transactional. Malformed or unsupported records leave
/// the previous screen intact. Retain one screen per transport session.
#[derive(Clone)]
pub struct Tn3270Screen {
    cells: Vec<Cell>,
    cursor: usize,
    keyboard: Keyboard3270,
    insert_mode: bool,
    last_aid: u8,
    nvt_mode: bool,
    reply_mode: ReplyMode,
}

impl Default for Tn3270Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Tn3270Screen {
    pub fn new() -> Self {
        Self {
            cells: vec![NULL; CELLS],
            cursor: 0,
            keyboard: Keyboard3270::Ready,
            insert_mode: false,
            last_aid: NO_AID,
            nvt_mode: false,
            reply_mode: ReplyMode::Field,
        }
    }

    pub fn snapshot(&self) -> Snapshot3270 {
        let mut current = self.cells.iter().rev().find_map(|cell| cell.field());
        let mut cells = Vec::with_capacity(CELLS);
        for &cell in &self.cells {
            let (byte, style, field_marker) = match cell {
                Cell::Character { byte, style } => (byte, style, false),
                Cell::Field(field) => {
                    current = Some(field);
                    (0, Style3270::default(), true)
                }
            };
            let field = current.map(Field::presentation);
            cells.push(Cell3270 {
                character: if field.is_some_and(|field| field.hidden) {
                    ' '
                } else {
                    ebcdic::decode(byte)
                },
                field_marker,
                field,
                style: style.over(current.map_or_else(Style3270::default, |field| field.style)),
            });
        }
        Snapshot3270 {
            columns: COLUMNS as u16,
            rows: ROWS as u16,
            cursor: position(self.cursor),
            keyboard: self.keyboard,
            insert_mode: self.insert_mode,
            nvt_mode: self.nvt_mode,
            cells,
        }
    }

    pub fn apply_record(&mut self, record: &Record) -> Result<Update3270, Error3270> {
        let Record::Tn3270e { kind, payload } = record else {
            return Err(Error3270::WrongProfile);
        };
        if *kind == Tn3270eData::Nvt {
            if !self.nvt_mode {
                self.reset_partition();
                self.restore_keyboard();
                self.nvt_mode = true;
            }
            return Ok(Update3270::default());
        }
        if payload.len() > crate::MAX_RECORD - 5 {
            return Err(Error3270::Malformed("3270 record exceeds the wire limit"));
        }
        let mut screen = self.clone();
        screen.nvt_mode = false;
        let update = screen.apply_display(payload)?;
        *self = screen;
        Ok(update)
    }

    fn clear(&mut self) {
        self.cells.fill(NULL);
        self.cursor = 0;
    }

    fn reset_partition(&mut self) {
        self.clear();
        self.reply_mode = ReplyMode::Field;
    }

    fn restore_keyboard(&mut self) {
        self.keyboard = Keyboard3270::Ready;
        self.insert_mode = false;
        self.last_aid = NO_AID;
    }

    fn field_owner(&self, address: usize) -> Option<(usize, Field)> {
        self.cells[..=address]
            .iter()
            .rposition(|cell| cell.field().is_some())
            .or_else(|| self.cells.iter().rposition(|cell| cell.field().is_some()))
            .and_then(|index| self.cells[index].field().map(|field| (index, field)))
    }

    fn input_start_at(&self, marker: usize) -> Option<usize> {
        let first = next(marker);
        self.cells[marker]
            .field()
            .filter(|field| !field.protected() && self.cells[first].field().is_none())
            .map(|_| first)
    }

    fn first_input(&self) -> Option<usize> {
        (0..CELLS).find_map(|index| self.input_start_at(index))
    }

    fn set_modified(&mut self, owner: Option<usize>) {
        if let Some(index) = owner
            && let Cell::Field(field) = &mut self.cells[index]
        {
            field.bits |= 0x01;
        }
    }
}

fn next(index: usize) -> usize {
    (index + 1) % CELLS
}

fn position(address: usize) -> Position3270 {
    Position3270 {
        column: (address % COLUMNS) as u16,
        row: (address / COLUMNS) as u16,
    }
}

fn display_record(payload: Vec<u8>) -> Record {
    Record::Tn3270e {
        kind: Tn3270eData::Display,
        payload,
    }
}
