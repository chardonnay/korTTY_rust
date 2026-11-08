use super::{
    Aid3270, CELLS, COLUMNS, Cell, Edit3270, Error3270, Keyboard3270, NULL, Position3270, ROWS,
    Style3270, Tn3270Screen, display_record, next,
};
use crate::{Record, ebcdic};

const PF: [u8; 24] = [
    0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0x7a, 0x7b, 0x7c, 0xc1, 0xc2, 0xc3, 0xc4,
    0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0x4a, 0x4b, 0x4c,
];
const PA: [u8; 3] = [0x6c, 0x6e, 0x6b];

impl Tn3270Screen {
    /// Moves the keyboard cursor using zero-based display coordinates.
    pub fn set_cursor(&mut self, position: Position3270) -> Result<(), Error3270> {
        self.ensure_keyboard_ready()?;
        let row = usize::from(position.row);
        let column = usize::from(position.column);
        if row >= ROWS || column >= COLUMNS {
            return Err(Error3270::InvalidPosition);
        }
        self.cursor = row * COLUMNS + column;
        Ok(())
    }

    /// Applies a text input atomically. A rejected paste leaves the screen
    /// data unchanged. Operator field errors inhibit further input until Reset.
    pub fn insert_text(&mut self, text: &str) -> Result<(), Error3270> {
        self.ensure_keyboard_ready()?;
        // Every supported character uses at most two UTF-8 bytes and one cell.
        if text.len() > CELLS * 2 {
            return self.operator_result(Err(Error3270::FieldFull));
        }
        let mut edited = self.clone();
        let mut visited = [false; CELLS];
        for character in text.chars() {
            if std::mem::replace(&mut visited[edited.cursor], true) {
                return self.operator_result(Err(Error3270::FieldFull));
            }
            let byte = ebcdic::encode(character).ok_or(Error3270::UnsupportedCharacter)?;
            if let Err(error) = edited.type_character(byte) {
                return self.operator_result(Err(error));
            }
        }
        *self = edited;
        Ok(())
    }

    pub fn edit(&mut self, key: Edit3270) -> Result<(), Error3270> {
        if self.nvt_mode {
            return Err(Error3270::NvtMode);
        }
        if key == Edit3270::Reset {
            if self.keyboard == Keyboard3270::OperatorError {
                self.keyboard = Keyboard3270::Ready;
            }
            self.insert_mode = false;
            return Ok(());
        }
        self.ensure_keyboard_ready()?;
        let result = match key {
            Edit3270::Left => {
                self.cursor = (self.cursor + CELLS - 1) % CELLS;
                Ok(())
            }
            Edit3270::Right => {
                self.cursor = next(self.cursor);
                Ok(())
            }
            Edit3270::Up => {
                self.cursor = (self.cursor + CELLS - COLUMNS) % CELLS;
                Ok(())
            }
            Edit3270::Down => {
                self.cursor = (self.cursor + COLUMNS) % CELLS;
                Ok(())
            }
            Edit3270::Home => {
                self.cursor = self.first_input().unwrap_or(0);
                Ok(())
            }
            Edit3270::Tab => {
                self.cursor = self.next_input(self.cursor).unwrap_or(0);
                Ok(())
            }
            Edit3270::BackTab => {
                self.cursor = self.previous_input().unwrap_or(0);
                Ok(())
            }
            Edit3270::Backspace => self.backspace(),
            Edit3270::Delete => self.delete(),
            Edit3270::EraseEof => self.erase_eof(),
            Edit3270::ToggleInsert => {
                self.insert_mode = !self.insert_mode;
                Ok(())
            }
            Edit3270::Reset => Ok(()),
        };
        self.operator_result(result)
    }

    /// Builds the on-wire input record and inhibits input until a host restore.
    pub fn send_aid(&mut self, aid: Aid3270) -> Result<Record, Error3270> {
        self.ensure_keyboard_ready()?;
        let byte = match aid {
            Aid3270::Enter => 0x7d,
            Aid3270::Clear => 0x6d,
            Aid3270::Pf(key) => aid_number(&PF, key)?,
            Aid3270::Pa(key) => aid_number(&PA, key)?,
        };
        if aid == Aid3270::Clear {
            self.reset_partition();
        }
        self.keyboard = Keyboard3270::HostLocked;
        self.insert_mode = false;
        self.last_aid = byte;
        Ok(display_record(self.read_modified(byte, false)))
    }

    fn ensure_keyboard_ready(&self) -> Result<(), Error3270> {
        if self.nvt_mode {
            Err(Error3270::NvtMode)
        } else if self.keyboard == Keyboard3270::Ready {
            Ok(())
        } else {
            Err(Error3270::KeyboardLocked)
        }
    }

    fn operator_result(&mut self, result: Result<(), Error3270>) -> Result<(), Error3270> {
        if matches!(
            result,
            Err(Error3270::ProtectedField | Error3270::NumericField | Error3270::FieldFull)
        ) {
            self.keyboard = Keyboard3270::OperatorError;
        }
        result
    }

    fn type_character(&mut self, byte: u8) -> Result<(), Error3270> {
        let owner = self.editable_owner()?;
        if owner.is_some_and(|(_, field)| field.numeric())
            && !matches!(byte, 0xf0..=0xf9 | 0x4b | 0x60)
        {
            return Err(Error3270::NumericField);
        }
        if self.insert_mode {
            self.insert_space(owner.is_some())?;
        }
        let style = match self.cells[self.cursor] {
            Cell::Character { style, .. } if !self.insert_mode => style,
            _ => Style3270::default(),
        };
        self.cells[self.cursor] = Cell::Character { byte, style };
        self.set_modified(owner.map(|(address, _)| address));
        self.cursor = next(self.cursor);
        if let Some(field) = self.cells[self.cursor].field()
            && (!field.protected() || field.numeric())
        {
            self.cursor = self.next_input(self.cursor).unwrap_or(0);
        }
        Ok(())
    }

    fn insert_space(&mut self, formatted: bool) -> Result<(), Error3270> {
        let length = self.remaining_field(formatted);
        let null = (0..length).find(|offset| {
            matches!(
                self.cells[(self.cursor + offset) % CELLS],
                Cell::Character { byte: 0, .. }
            )
        });
        let Some(null) = null else {
            return Err(Error3270::FieldFull);
        };
        for offset in (1..=null).rev() {
            let from = (self.cursor + offset - 1) % CELLS;
            self.cells[next(from)] = self.cells[from];
        }
        Ok(())
    }

    fn delete(&mut self) -> Result<(), Error3270> {
        let owner = self.editable_owner()?;
        let length = self.remaining_field(owner.is_some());
        for offset in 1..length {
            let from = (self.cursor + offset) % CELLS;
            let to = (self.cursor + offset - 1) % CELLS;
            self.cells[to] = self.cells[from];
        }
        self.cells[(self.cursor + length - 1) % CELLS] = NULL;
        self.set_modified(owner.map(|(address, _)| address));
        Ok(())
    }

    fn backspace(&mut self) -> Result<(), Error3270> {
        let owner = self.editable_owner()?;
        if self.cursor == owner.map_or(0, |(address, _)| next(address)) {
            return Ok(());
        }
        self.cursor = (self.cursor + CELLS - 1) % CELLS;
        self.delete()
    }

    fn erase_eof(&mut self) -> Result<(), Error3270> {
        let owner = self.editable_owner()?;
        let length = self.remaining_field(owner.is_some());
        for offset in 0..length {
            self.cells[(self.cursor + offset) % CELLS] = NULL;
        }
        self.set_modified(owner.map(|(address, _)| address));
        Ok(())
    }

    fn editable_owner(&self) -> Result<Option<(usize, super::Field)>, Error3270> {
        let owner = self.field_owner(self.cursor);
        if owner.is_some_and(|(address, field)| field.protected() || address == self.cursor) {
            Err(Error3270::ProtectedField)
        } else {
            Ok(owner)
        }
    }

    fn remaining_field(&self, formatted: bool) -> usize {
        if formatted {
            (1..CELLS)
                .find(|offset| self.cells[(self.cursor + offset) % CELLS].field().is_some())
                .unwrap_or(CELLS)
        } else {
            CELLS - self.cursor
        }
    }

    fn next_input(&self, from: usize) -> Option<usize> {
        (0..CELLS).find_map(|offset| self.input_start_at((from + offset) % CELLS))
    }

    fn previous_input(&self) -> Option<usize> {
        (1..=CELLS)
            .map(|offset| (self.cursor + CELLS - offset) % CELLS)
            .filter_map(|marker| self.input_start_at(marker))
            .find(|&first| first != self.cursor)
            .or_else(|| self.first_input())
    }
}

fn aid_number(codes: &[u8], key: u8) -> Result<u8, Error3270> {
    key.checked_sub(1)
        .and_then(|index| codes.get(usize::from(index)).copied())
        .ok_or(Error3270::InvalidAid)
}
