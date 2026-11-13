use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use unicode_width::UnicodeWidthChar;

use super::{Hp, ParameterKind};
use crate::{Cell, Input, MAX_RESPONSE_BYTES, Model, Position, Style, Terminal};

const DC2: u8 = 0x12;
const RECORD_SEPARATOR: u8 = 0x1e;
const UNIT_SEPARATOR: u8 = 0x1f;

/// Distinguishes a keyboard event from pasted text. Text cannot submit a form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hp2392Input {
    Text,
    Key {
        enter: bool,
    },
    /// The original shifted Enter is a printer key.
    ShiftEnter,
}

impl Terminal {
    /// Applies operator input in the documented HP 2392A block mode.
    ///
    /// Returns false when the caller should send the original bytes. On true,
    /// the caller sends only any protocol bytes returned by `take_responses`.
    pub fn hp2392_local_input(&mut self, bytes: &[u8], kind: Hp2392Input) -> bool {
        match &mut self.input {
            Input::Hp(hp) => hp.local_input(&mut self.model, bytes, kind),
            _ => false,
        }
    }
}

impl Hp {
    fn local_input(&mut self, model: &mut Model, bytes: &[u8], kind: Hp2392Input) -> bool {
        if !self.forms.block {
            return false;
        }
        self.forms.input(model, bytes, kind);
        true
    }
}

/// Live screen metadata. Scrollback and screen snapshots contain only visible
/// cells, so concealed field contents and block markers cannot leak into them.
#[derive(Default)]
pub(crate) struct FormLine {
    markers: BTreeMap<usize, bool>,
    styles: BTreeMap<usize, Style>,
    concealed: BTreeMap<usize, u8>,
    terminators: BTreeSet<usize>,
}

#[derive(Clone, Copy)]
struct Field {
    row: usize,
    start: usize,
    end: usize,
}

impl Field {
    fn contains(self, cursor: Position) -> bool {
        self.row == cursor.row && (self.start..self.end).contains(&cursor.column)
    }

    fn start(self) -> (usize, usize) {
        (self.row, self.start)
    }
}

enum Pending {
    FirstDc1(Vec<u8>),
    FinalDc1(Vec<u8>),
}

#[derive(Default)]
pub(super) struct Forms {
    pub(super) block: bool,
    pub(super) format: bool,
    pub(super) insert: bool,
    page: bool,
    inhibit_dc2: bool,
    auto_line_feed: bool,
    host_ready: bool,
    pending: Option<Pending>,
}

impl Forms {
    pub(super) fn configure(&mut self, kind: ParameterKind, selector: u8, enabled: bool) {
        match (kind, selector) {
            (ParameterKind::Keyboard, b'A') => self.auto_line_feed = enabled,
            (ParameterKind::Keyboard, b'B') => {
                self.block = enabled;
                self.cancel_transfer();
            }
            (ParameterKind::DisplayFunctions, b'D') => {
                self.page = enabled;
                self.cancel_transfer();
            }
            (ParameterKind::DisplayFunctions, b'H') => {
                self.inhibit_dc2 = enabled;
                self.cancel_transfer();
            }
            _ => {}
        }
    }

    pub(super) fn cancel_transfer(&mut self) {
        self.host_ready = false;
        self.pending = None;
    }

    pub(super) fn reset(&mut self, model: &mut Model) {
        *self = Self::default();
        self.clear_fields(model);
        model.style = Style::default();
        model.scroll_top = 0;
        model.scroll_bottom = model.rows - 1;
        model.erase_display(2, false);
        model.move_cursor(0, 0);
    }

    pub(super) fn clear_fields(&mut self, model: &mut Model) {
        self.cancel_transfer();
        for line in &mut model.screen_mut().lines {
            line.hp_form = None;
        }
    }

    pub(super) fn field_marker(&mut self, model: &mut Model, editable: bool) {
        let cursor = model.cursor();
        let form = model.screen_mut().lines[cursor.row]
            .hp_form
            .get_or_insert_with(Box::default);
        form.markers.insert(cursor.column, editable);
    }

    pub(super) fn style(&self, model: &mut Model) {
        if self.block || self.format {
            let cursor = model.cursor();
            let style = model.style;
            let line = &mut model.screen_mut().lines[cursor.row];
            line.hp_form
                .get_or_insert_with(Box::default)
                .styles
                .insert(cursor.column, style);
        }
    }

    pub(super) fn host_character(&self, model: &mut Model, ch: char) {
        let Some(width) = ch.width().filter(|&width| width > 0) else {
            return;
        };
        let width = width.min(model.columns);
        let cursor = model.cursor();
        let screen = model.screen();
        let next_column = cursor.column + usize::from(screen.wrap_pending);
        let column = next_column.saturating_sub(width);
        let hidden = model.style.hidden;
        if let Some(form) = &mut model.screen_mut().lines[cursor.row].hp_form {
            for position in column..next_column {
                form.terminators.remove(&position);
                form.concealed.remove(&position);
            }
            if hidden
                && matches!(ch, ' '..='~')
                && form
                    .markers
                    .range(..=column)
                    .next_back()
                    .is_some_and(|(_, editable)| *editable)
            {
                form.concealed.insert(column, ch as u8);
            }
        }
    }

    pub(super) fn host_terminator(&mut self, model: &mut Model) {
        model.put_character(' ', 1);
        let cursor = model.cursor();
        let screen = model.screen_mut();
        let column = cursor.column + usize::from(screen.wrap_pending) - 1;
        let form = screen.lines[cursor.row]
            .hp_form
            .get_or_insert_with(Box::default);
        form.concealed.remove(&column);
        form.terminators.insert(column);
    }

    pub(super) fn set_format(&mut self, model: &mut Model, enabled: bool) {
        self.format = enabled;
        self.cancel_transfer();
        if enabled && let Some(field) = fields(model).first() {
            model.move_cursor(field.row, field.start);
        }
    }

    pub(super) fn tab(&self, model: &mut Model, forward: bool) {
        let available = fields(model);
        let cursor = form_cursor(model);
        let position = (cursor.row, cursor.column);
        let field = if forward {
            available
                .iter()
                .find(|field| field.start() > position)
                .or(available.first())
        } else {
            available
                .iter()
                .rev()
                .find(|field| field.start() < position)
                .or(available.last())
        };
        if let Some(field) = field {
            model.move_cursor(field.row, field.start);
        }
    }

    pub(super) fn erase(&mut self, model: &mut Model, display: bool) {
        if self.format {
            let cursor = form_cursor(model);
            let available = fields(model);
            for field in available {
                if (display
                    && field.row >= cursor.row
                    && (field.row > cursor.row || field.end > cursor.column))
                    || (!display && field.contains(cursor))
                {
                    let start = if field.row == cursor.row {
                        field.start.max(cursor.column)
                    } else {
                        field.start
                    };
                    erase_cells(model, field.row, start..field.end, true);
                }
            }
        } else {
            let cursor = if self.block {
                model.cursor()
            } else {
                form_cursor(model)
            };
            if !display && cursor.column == model.columns {
                model.screen_mut().lines[cursor.row].wrap_length = None;
                return;
            }
            let end = if display { model.rows } else { cursor.row + 1 };
            for row in cursor.row..end {
                let start = if row == cursor.row { cursor.column } else { 0 };
                erase_cells(model, row, start..model.columns, false);
            }
        }
    }

    pub(super) fn delete(&self, model: &mut Model) {
        let cursor = if self.format {
            form_cursor(model)
        } else {
            model.cursor()
        };
        let end = if self.format {
            let Some(field) = fields(model)
                .into_iter()
                .find(|field| field.contains(cursor))
            else {
                return;
            };
            field.end
        } else {
            model.columns
        };
        shift(model, cursor.row, cursor.column..end, false);
    }

    fn input(&mut self, model: &mut Model, bytes: &[u8], kind: Hp2392Input) {
        if self.pending.is_some() || kind == Hp2392Input::ShiftEnter {
            return;
        }
        if let Hp2392Input::Key { enter } = kind {
            let cursor = model.cursor();
            match bytes {
                b"\r" if enter => return self.enter(model),
                b"\r" => {
                    model.carriage_return();
                    if self.auto_line_feed {
                        model.line_feed();
                    }
                    return;
                }
                b"\n" => return model.line_feed(),
                b"\t" if self.format => return self.tab(model, true),
                b"\x1b[Z" | b"\x1bi" if self.format => return self.tab(model, false),
                b"\t" => return model.tab(1, true),
                b"\x1b[Z" | b"\x1bi" => return model.tab(1, false),
                b"\x08" | b"\x1bD" => return model.move_columns(-1),
                b"\x1bC" => return model.move_columns(1),
                b"\x1bA" => return model.move_rows(-1, cursor.column),
                b"\x1bB" => return model.move_rows(1, cursor.column),
                b"\x1bh" => return model.move_cursor(0, 0),
                b"\x1bF" => return model.move_cursor(model.rows - 1, 0),
                b"\x1bP" => return self.delete(model),
                b"\x1bQ" => {
                    self.insert = !self.insert;
                    return;
                }
                b"\x1bK" => return self.erase(model, false),
                b"\x1bJ" => return self.erase(model, true),
                _ if !bytes.iter().all(|byte| (b' '..=b'~').contains(byte)) => return,
                _ => {}
            }
        }
        let available = self.format.then(|| fields(model));
        let capacity = model.rows * model.columns;
        for &byte in bytes
            .iter()
            .take(capacity)
            .filter(|byte| (b' '..=b'~').contains(*byte))
        {
            if let Some(fields) = &available {
                let cursor = form_cursor(model);
                let current =
                    fields.partition_point(|field| field.start() <= (cursor.row, cursor.column));
                let in_field = current
                    .checked_sub(1)
                    .filter(|&index| fields[index].contains(cursor));
                let index = in_field.unwrap_or(if current == fields.len() { 0 } else { current });
                let Some(&field) = fields.get(index) else {
                    break;
                };
                let column = if in_field.is_some() {
                    cursor.column
                } else {
                    field.start
                };
                if self.insert {
                    shift(model, field.row, column..field.end, true);
                }
                write_cell(model, field.row, column, byte);
                if column + 1 == field.end
                    && let Some(next) = fields.get(index + 1)
                {
                    model.move_cursor(next.row, next.start);
                } else {
                    move_after(model, field.row, column + 1);
                }
            } else {
                let cursor = model.cursor();
                if self.insert {
                    shift(model, cursor.row, cursor.column..model.columns, true);
                }
                write_cell(model, cursor.row, cursor.column, byte);
                if cursor.column + 1 == model.columns {
                    model.carriage_return();
                    model.line_feed();
                } else {
                    model.move_cursor(cursor.row, cursor.column + 1);
                }
            }
        }
    }

    fn enter(&mut self, model: &mut Model) {
        let mut cursor = if self.format {
            form_cursor(model)
        } else {
            model.cursor()
        };
        if self.page && self.inhibit_dc2 {
            cursor = Position::default();
        }
        let mut bytes = Vec::new();
        if self.format {
            let available = fields(model);
            let selected = available.iter().copied().filter(|field| {
                field.contains(cursor) || field.start() > (cursor.row, cursor.column)
            });
            if self.page {
                let mut separated = false;
                for field in selected {
                    if separated {
                        bytes.push(UNIT_SEPARATOR);
                    }
                    separated = true;
                    let start = if field.contains(cursor) {
                        cursor.column
                    } else {
                        field.start
                    };
                    if append_field(model, field.row, start..field.end, &mut bytes) {
                        break;
                    }
                    if bytes.len() > MAX_RESPONSE_BYTES {
                        return;
                    }
                }
                if bytes.last() != Some(&RECORD_SEPARATOR) {
                    bytes.push(RECORD_SEPARATOR);
                }
                if self.inhibit_dc2 {
                    model.move_cursor(0, 0);
                }
            } else {
                if let Some(field) = selected.into_iter().next() {
                    let start = if field.contains(cursor) {
                        cursor.column
                    } else {
                        field.start
                    };
                    let stopped = append_field(model, field.row, start..field.end, &mut bytes);
                    if stopped {
                        let position =
                            terminator(model, field.row, start..field.end).unwrap_or(field.end);
                        move_after(model, field.row, position + 1);
                    } else if !bytes.is_empty() {
                        move_after(model, field.row, field.end);
                    }
                }
                self.line_end(&mut bytes);
            }
        } else if self.page {
            let last = (cursor.row..model.rows).rfind(|&row| {
                let line = &model.screen().lines[row];
                let start = if row == cursor.row { cursor.column } else { 0 };
                (start..model.columns).any(|column| {
                    read_cell(&line.cells[column], line.hp_form.as_deref(), column).is_some()
                })
            });
            for row in cursor.row..=last.unwrap_or(cursor.row) {
                let start = if row == cursor.row { cursor.column } else { 0 };
                if append_field(model, row, start..model.columns, &mut bytes) {
                    break;
                }
                bytes.extend_from_slice(b"\r\n");
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return;
                }
            }
            if bytes.last() != Some(&RECORD_SEPARATOR) {
                bytes.push(RECORD_SEPARATOR);
            }
            if self.inhibit_dc2 {
                model.move_cursor(0, 0);
            }
        } else {
            let start = if self.inhibit_dc2 { 0 } else { cursor.column };
            let stopped = append_field(model, cursor.row, start..model.columns, &mut bytes);
            if stopped {
                let position = terminator(model, cursor.row, start..model.columns)
                    .unwrap_or(model.columns - 1);
                move_after(model, cursor.row, position + 1);
            } else if !bytes.is_empty() {
                model.carriage_return();
                if self.auto_line_feed {
                    model.line_feed();
                }
            }
            self.line_end(&mut bytes);
        }
        if bytes.len() > MAX_RESPONSE_BYTES {
            return;
        }
        if self.inhibit_dc2 {
            model.reply(&bytes);
        } else {
            self.pending = Some(Pending::FirstDc1(bytes));
            if std::mem::take(&mut self.host_ready) {
                self.host_trigger(model);
            }
        }
    }

    fn line_end(&self, bytes: &mut Vec<u8>) {
        if bytes.is_empty() {
            bytes.push(RECORD_SEPARATOR);
        }
        bytes.push(b'\r');
        if self.auto_line_feed {
            bytes.push(b'\n');
        }
    }

    pub(super) fn host_trigger(&mut self, model: &mut Model) {
        match self.pending.take() {
            Some(Pending::FirstDc1(bytes)) => {
                if self.page {
                    model.reply(&[DC2]);
                } else if self.auto_line_feed {
                    model.reply(&[DC2, b'\r', b'\n']);
                } else {
                    model.reply(&[DC2, b'\r']);
                }
                self.pending = Some(Pending::FinalDc1(bytes));
            }
            Some(Pending::FinalDc1(bytes)) => model.reply(&bytes),
            None if self.block && !self.inhibit_dc2 => self.host_ready = true,
            None => {}
        }
    }
}

fn fields(model: &Model) -> Vec<Field> {
    let mut fields = Vec::new();
    for (row, line) in model.screen().lines.iter().enumerate() {
        if let Some(form) = &line.hp_form {
            let mut markers = form.markers.iter().peekable();
            while let Some((&start, &editable)) = markers.next() {
                if editable {
                    let end = markers.peek().map_or(model.columns, |marker| *marker.0);
                    fields.push(Field { row, start, end });
                }
            }
        }
    }
    fields
}

fn move_after(model: &mut Model, row: usize, column: usize) {
    if column < model.columns {
        model.move_cursor(row, column);
    } else if row + 1 < model.rows {
        model.move_cursor(row + 1, 0);
    } else {
        model.move_cursor(row, model.columns - 1);
        model.screen_mut().wrap_pending = true;
    }
}

fn form_cursor(model: &Model) -> Position {
    let mut cursor = model.cursor();
    if model.screen().wrap_pending {
        cursor.column += 1;
    }
    cursor
}

fn write_cell(model: &mut Model, row: usize, column: usize, byte: u8) {
    let line = &mut model.screen_mut().lines[row];
    let cell = &mut line.cells[column];
    // Existing non-ASCII wide glyphs can straddle a host-defined boundary.
    if cell.width != 1 {
        return;
    }
    let form = line.hp_form.get_or_insert_with(Box::default);
    form.terminators.remove(&column);
    if let Some((_, style)) = form.styles.range(..=column).next_back() {
        cell.style = *style;
    }
    cell.text.clear();
    if cell.style.hidden {
        form.concealed.insert(column, byte);
        cell.text.push(' ');
    } else {
        form.concealed.remove(&column);
        cell.text.push(char::from(byte));
    }
}

fn erase_cells(model: &mut Model, row: usize, range: Range<usize>, preserve_markers: bool) {
    let line = &mut model.screen_mut().lines[row];
    if let Some(form) = &mut line.hp_form {
        if !preserve_markers {
            form.markers.retain(|column, _| !range.contains(column));
        }
        form.styles.retain(|column, _| !range.contains(column));
        form.concealed.retain(|column, _| !range.contains(column));
        form.terminators.retain(|column| !range.contains(column));
    }
    if preserve_markers {
        for cell in &mut line.cells[range] {
            if cell.width == 1 {
                *cell = Cell::blank(Style::default());
            }
        }
    } else {
        model.erase_line(row, range.start, range.end, false);
    }
}

fn shift(model: &mut Model, row: usize, range: Range<usize>, insert: bool) {
    let line = &mut model.screen_mut().lines[row];
    let form = line.hp_form.get_or_insert_with(Box::default);
    let hidden = |column: usize| {
        form.styles
            .range(..=column)
            .next_back()
            .map_or(line.cells[column].style.hidden, |(_, style)| style.hidden)
    };
    let start_hidden = hidden(range.start);
    let end = (range.start..range.end)
        .find(|&column| line.cells[column].width != 1 || hidden(column) != start_hidden)
        .unwrap_or(range.end);
    let range = range.start..end;
    // Style and field boundaries stay fixed. Only the character bytes move.
    let values = range
        .clone()
        .map(|column| {
            if !start_hidden && form.concealed.contains_key(&column) {
                None
            } else {
                read_cell(&line.cells[column], Some(form), column)
            }
        })
        .collect::<Vec<_>>();
    for column in range.clone() {
        let index = column - range.start;
        let byte = if insert {
            index.checked_sub(1).and_then(|index| values[index])
        } else {
            values.get(index + 1).copied().flatten()
        };
        let cell = &mut line.cells[column];
        form.concealed.remove(&column);
        form.terminators.remove(&column);
        cell.text.clear();
        match byte {
            Some(RECORD_SEPARATOR) => {
                form.terminators.insert(column);
            }
            Some(byte) if start_hidden => {
                form.concealed.insert(column, byte);
                cell.style.hidden = true;
                cell.text.push(' ');
            }
            Some(byte) => cell.text.push(char::from(byte)),
            None => {}
        }
    }
}

fn read_cell(cell: &Cell, form: Option<&FormLine>, column: usize) -> Option<u8> {
    if let Some(form) = form {
        if form.terminators.contains(&column) {
            return Some(RECORD_SEPARATOR);
        }
        if let Some(&byte) = form.concealed.get(&column) {
            return Some(byte);
        }
    }
    if cell.style.hidden || cell.width != 1 {
        return None;
    }
    match cell.text.as_bytes() {
        [byte @ b' '..=b'~'] => Some(*byte),
        _ => None,
    }
}

fn terminator(model: &Model, row: usize, range: Range<usize>) -> Option<usize> {
    model.screen().lines[row]
        .hp_form
        .as_ref()?
        .terminators
        .range(range)
        .next()
        .copied()
}

fn append_field(model: &Model, row: usize, range: Range<usize>, output: &mut Vec<u8>) -> bool {
    let line = &model.screen().lines[row];
    let mut pending_spaces = 0;
    for column in range {
        if let Some(byte) = read_cell(&line.cells[column], line.hp_form.as_deref(), column) {
            output.resize(output.len() + pending_spaces, b' ');
            pending_spaces = 0;
            output.push(byte);
            if byte == RECORD_SEPARATOR {
                return true;
            }
        } else {
            pending_spaces += 1;
        }
    }
    false
}
