use zeroize::{Zeroize, Zeroizing};

use super::save::TOKEN_BYTES;
use super::{
    Error5250, ErrorLine, Field, Input, MAX_CELLS, MAX_FIELDS, Model5250, ReadMode, ScreenSize,
    Tn5250Screen, Update5250, is_attribute, is_hidden,
};
use crate::Record;

const ESC: u8 = 0x04;
const SBA: u8 = 0x11;
const SF: u8 = 0x1d;
const BEFORE_SCREEN: usize = usize::MAX;
const SAVE_SCREEN: u8 = 0x02;
const RESTORE_SCREEN: u8 = 0x12;
const SAVE_OPERATION: u8 = 0x04;
const RESTORE_OPERATION: u8 = 0x05;

impl Tn5250Screen {
    pub(super) fn apply_gds(&mut self, bytes: &[u8]) -> Result<Update5250, Error5250> {
        let mut data = Data(&bytes[6 + usize::from(bytes[6])..]);
        let opcode = bytes[9];
        let mut update = Update5250::default();
        if bytes[7..9] != [0, 0] {
            return Err(Error5250::Malformed(
                "unsupported flags on a host 5250 record",
            ));
        }
        match opcode {
            0x0a if data.0.is_empty() => {
                self.pending = None;
                self.input_suspended = true;
                update.reply = Some(gds(0x0a, &[]));
                return Ok(update);
            }
            0x0b | 0x0c if data.0.is_empty() => {
                self.message_waiting = opcode == 0x0b;
                return Ok(update);
            }
            0 if data.0.is_empty() => return Ok(update),
            SAVE_OPERATION if data.0 != [ESC, SAVE_SCREEN] => {
                return Err(Error5250::Malformed("invalid 5250 Save Screen operation"));
            }
            RESTORE_OPERATION if !data.0.starts_with(&[ESC, RESTORE_SCREEN]) => {
                return Err(Error5250::Malformed(
                    "invalid 5250 Restore Screen operation",
                ));
            }
            0..=6 | 8 if !data.0.is_empty() => {}
            _ => return Err(Error5250::UnsupportedOpcode(opcode)),
        }
        while !data.0.is_empty() {
            if data.byte()? != ESC {
                return Err(Error5250::Malformed(
                    "5250 command requires an Escape prefix",
                ));
            }
            let command = data.byte()?;
            match command {
                SAVE_SCREEN if data.0.is_empty() => {
                    let token = self.save_screen()?;
                    let mut payload = [0; 2 + TOKEN_BYTES];
                    payload[..2].copy_from_slice(&[ESC, RESTORE_SCREEN]);
                    payload[2..].copy_from_slice(&token);
                    update.reply = Some(gds(SAVE_OPERATION, &payload));
                }
                SAVE_SCREEN => {
                    return Err(Error5250::Malformed(
                        "5250 Save Screen must be the last command",
                    ));
                }
                RESTORE_SCREEN => {
                    let token = data.take()?;
                    if !data.0.is_empty() {
                        return Err(Error5250::Malformed("invalid 5250 saved screen length"));
                    }
                    self.restore_screen(&token)?;
                }
                0x40 => self.clear_unit(ScreenSize::Standard),
                0x20 if self.model == Model5250::Ibm3180Model2 => {
                    if data.byte()? != 0 {
                        return Err(Error5250::Malformed(
                            "invalid 5250 Clear Unit Alternate parameter",
                        ));
                    }
                    self.clear_unit(ScreenSize::Wide);
                }
                0x21 => self.write_error_code(&mut data)?,
                0x11 | 0x23 | 0x50 | 0x62 | 0x72 | 0x83 if self.error_line.is_some() => {
                    return Err(Error5250::KeyboardLocked);
                }
                0x50 => {
                    self.fields.clear();
                    self.home = None;
                    self.error_row = self.size.rows() - 1;
                    self.lock();
                }
                0x11 => update.alarm |= self.write_display(&mut data)?,
                0x23 => self.roll(&mut data)?,
                0x42 | 0x52 | 0x82 => {
                    let mode = match command {
                        0x42 => ReadMode::All,
                        0x52 => ReadMode::Modified,
                        _ => ReadMode::ModifiedAlternate,
                    };
                    self.pending = Some(Input {
                        mode,
                        control: data.take()?,
                    });
                    self.input_suspended = false;
                }
                0x62 | 0x72 | 0x83 if data.0.is_empty() => {
                    update.reply = Some(match command {
                        0x62 => gds(0, &self.cells),
                        0x72 => self.input_reply(ReadMode::All, 0, true),
                        _ => self.input_reply(ReadMode::ModifiedAlternate, 0, true),
                    });
                }
                0x62 | 0x72 | 0x83 => {
                    return Err(Error5250::Malformed(
                        "5250 immediate read must be the last command",
                    ));
                }
                0xf3 if data.0 == [0, 5, 0xd9, 0x70, 0] => {
                    update.reply = Some(device_query(self.model));
                    data.0 = &[];
                }
                _ => return Err(Error5250::UnsupportedCommand(command)),
            }
        }
        Ok(update)
    }

    // IBM SC30-3533-02 pp. 12-4–12-5 defines both Clear Unit sizes.
    fn clear_unit(&mut self, size: ScreenSize) {
        self.size = size;
        self.fields.clear();
        self.cells.zeroize();
        self.cells.resize(size.cells(), 0);
        self.cells[0] = 0x20;
        self.cursor = 1;
        self.home = None;
        self.error_row = size.rows() - 1;
        self.error_line = None;
        self.pending = None;
        self.lock();
    }

    // IBM SA21-9247-6 pp. 2-11–2-12 and 5494 Functions Reference pp. 12-108–12-109.
    fn roll(&mut self, data: &mut Data<'_>) -> Result<(), Error5250> {
        let [flags, top, bottom] = data.take()?;
        let (top, bottom) = (usize::from(top), usize::from(bottom));
        let lines = usize::from(flags & 0x1f);
        if flags & 0x60 != 0
            || top == 0
            || bottom > self.size.rows()
            || top >= bottom
            || lines > bottom - top
        {
            return Err(Error5250::Malformed("invalid 5250 Roll parameters"));
        }
        if lines == 0 {
            return Ok(());
        }

        let columns = self.size.columns();
        let start = (top - 1) * columns;
        let end = bottom * columns;
        let shift = lines * columns;
        let (source, destination) = if flags & 0x80 == 0 {
            (start + shift..end, start)
        } else {
            (start..end - shift, start + shift)
        };
        #[derive(Clone, Copy)]
        enum Redaction {
            None,
            Attribute,
            Field,
        }
        let mut redaction = [Redaction::None; MAX_CELLS];
        let mut has_hidden_data = false;
        for ((&byte, (attribute, field)), protection) in self
            .cells
            .iter()
            .zip(self.cell_attributes())
            .zip(&mut redaction)
        {
            if byte != 0 && byte != 0x40 && !is_attribute(byte) && is_hidden(attribute, field) {
                has_hidden_data = true;
                *protection = if field.is_some_and(|field| field.hidden) {
                    Redaction::Field
                } else {
                    Redaction::Attribute
                };
            }
        }

        // Vacated rows and format-table addresses stay fixed. Hidden input
        // must remain in a nondisplay field even if the destination's screen
        // attribute would temporarily hide it.
        self.cells.copy_within(source.clone(), destination);
        if !has_hidden_data {
            return Ok(());
        }
        redaction.copy_within(source, destination);
        if redaction.into_iter().zip(self.cell_attributes()).any(
            |(protection, (attribute, field))| match protection {
                Redaction::None => false,
                Redaction::Attribute => !is_hidden(attribute, field),
                Redaction::Field => !field.is_some_and(|field| field.hidden),
            },
        ) {
            return Err(Error5250::Malformed(
                "5250 Roll would expose nondisplay data",
            ));
        }
        Ok(())
    }

    // IBM SA21-9247-6 pp. 2-12–2-13 and 5494 Functions Reference pp. 12-98–12-99.
    fn write_error_code(&mut self, data: &mut Data<'_>) -> Result<(), Error5250> {
        if self
            .error_line
            .as_ref()
            .is_some_and(|line| !line.awaiting_help)
        {
            return Err(Error5250::KeyboardLocked);
        }
        let mut line = ErrorLine {
            cells: Zeroizing::new(vec![0; self.size.columns()]),
            awaiting_help: false,
        };
        let mut column = 0;
        let mut cursor = None;
        while data.0.first().is_some_and(|&byte| byte != ESC) {
            let byte = data.byte()?;
            if byte == 0x13 {
                cursor = Some(data.address(self.size)?);
            } else {
                let cell = line.cells.get_mut(column).ok_or(Error5250::Malformed(
                    "5250 Write Error Code data exceeds one line",
                ))?;
                *cell = byte;
                column += 1;
            }
        }
        if column == 0 && cursor.is_none() {
            return Err(Error5250::Malformed("empty 5250 Write Error Code"));
        }
        self.error_line = Some(line);
        self.keyboard_locked = false;
        self.operator_error = false;
        self.insert_mode = false;
        if let Some(cursor) = cursor {
            self.cursor = cursor;
        }
        Ok(())
    }

    pub(super) fn request_error_help(&mut self) -> Result<Update5250, Error5250> {
        let line = self.error_line.as_mut().ok_or(Error5250::KeyboardLocked)?;
        if line.awaiting_help {
            return Err(Error5250::KeyboardLocked);
        }
        let code = &line.cells[1..5];
        if !code.iter().all(|byte| matches!(byte, 0xf0..=0xf9)) {
            return Err(Error5250::FieldValidation);
        }
        let packed = [
            ((code[0] & 0x0f) << 4) | (code[1] & 0x0f),
            ((code[2] & 0x0f) << 4) | (code[3] & 0x0f),
        ];
        line.cells[0] = 0x22; // High intensity, without blink.
        line.awaiting_help = true;
        // RFC 1205 §3. HLP is bit 7 of the first flag byte (IBM bit numbering).
        Ok(Update5250 {
            reply: Some(gds_with_flags(0, 0x01, &packed)),
            ..Update5250::default()
        })
    }

    fn write_display(&mut self, data: &mut Data<'_>) -> Result<bool, Error5250> {
        let [before, after] = data.take()?;
        self.before_control(before);
        let mut address = 0;
        let mut next_field = None;
        let mut move_cursor = None;
        while let Some(&order) = data.0.first() {
            if order == ESC {
                break;
            }
            data.byte()?;
            match order {
                SBA => {
                    let [row, column] = data.take()?;
                    address = if row == 1 && column == 0 && data.0.first() == Some(&SF) {
                        BEFORE_SCREEN
                    } else {
                        display_address(self.size, row, column)?
                    };
                    next_field = None;
                }
                0x13 => {
                    self.home = Some(data.address(self.size)?);
                    move_cursor = None;
                }
                0x14 => move_cursor = Some(data.address(self.size)?),
                SF => {
                    let end = self.start_field(next_field.unwrap_or(address), data)?;
                    let start = next_field.unwrap_or(address);
                    address = if start == BEFORE_SCREEN { 0 } else { start + 1 };
                    next_field = Some(end);
                }
                0x02 => {
                    let end = data.address(self.size)?;
                    let byte = data.byte()?;
                    let span = self
                        .cells
                        .get_mut(address..=end)
                        .filter(|span| !span.is_empty())
                        .ok_or(Error5250::Malformed("5250 repeat range is invalid"))?;
                    span.fill(byte);
                    address = end + 1;
                }
                0x03 => {
                    let end = data.address(self.size)?;
                    let length = data.byte()?;
                    if !matches!(
                        data.slice(usize::from(length.saturating_sub(1)))?,
                        [0 | 0xff]
                    ) {
                        return Err(Error5250::UnsupportedOrder(order));
                    }
                    let span = self
                        .cells
                        .get_mut(address..=end)
                        .filter(|span| !span.is_empty())
                        .ok_or(Error5250::Malformed("5250 erase range is invalid"))?;
                    span.fill(0);
                    address = end + 1;
                }
                0x01 => {
                    let length = usize::from(data.byte()?);
                    let header = data.slice(length)?;
                    if length == 0 || length > 7 {
                        return Err(Error5250::Malformed(
                            "invalid 5250 format-table header length",
                        ));
                    }
                    if header[0] != 0
                        || header.get(2).is_some_and(|&value| value != 0)
                        || header.get(4..7).is_some_and(|keys| keys != [0; 3])
                    {
                        return Err(Error5250::UnsupportedOrder(order));
                    }
                    self.fields.clear();
                    self.home = None;
                    self.error_row = header
                        .get(3)
                        .copied()
                        .map(usize::from)
                        .filter(|row| (1..=self.size.rows()).contains(row))
                        .unwrap_or(self.size.rows())
                        - 1;
                }
                0x10 => {
                    let length = usize::from(u16::from_be_bytes(data.take()?));
                    let source = data.slice(length)?;
                    let end = address.checked_add(length).ok_or(Error5250::Malformed(
                        "invalid 5250 transparent data address",
                    ))?;
                    let destination =
                        self.cells
                            .get_mut(address..end)
                            .ok_or(Error5250::Malformed(
                                "5250 transparent data exceeds the screen",
                            ))?;
                    destination.copy_from_slice(source);
                    address = end;
                }
                0 | 0x1c | 0x1e | 0x20..=0xff => {
                    let cell = self
                        .cells
                        .get_mut(address)
                        .ok_or(Error5250::Malformed("5250 write exceeds the screen"))?;
                    *cell = order;
                    address += 1;
                }
                _ => return Err(Error5250::UnsupportedOrder(order)),
            }
        }
        Ok(self.after_control(after, move_cursor))
    }

    fn start_field(&mut self, marker: usize, data: &mut Data<'_>) -> Result<usize, Error5250> {
        let first = data.byte()?;
        let mut transparent = false;
        let (format, attribute) = if is_attribute(first) {
            (None, first)
        } else {
            let format = u16::from_be_bytes([first, data.byte()?]);
            if first & 0xc0 != 0x40 {
                return Err(Error5250::Malformed("invalid 5250 field format word"));
            }
            if format & 0x00d0 != 0
                || matches!(format & 7, 1..=6)
                || matches!((format >> 8) & 7, 4 | 6 | 7)
            {
                return Err(Error5250::UnsupportedField);
            }
            let attribute = loop {
                let byte = data.byte()?;
                if is_attribute(byte) {
                    break byte;
                }
                if byte != 0x84 {
                    return Err(Error5250::UnsupportedField);
                }
                data.byte()?;
                transparent = true;
            };
            (Some(format), attribute)
        };
        let length = usize::from(u16::from_be_bytes(data.take()?));
        let start = if marker == BEFORE_SCREEN {
            0
        } else {
            marker + 1
        };
        let mut end = start + length;
        if let Some(format) = format {
            if marker == BEFORE_SCREEN && attribute & 7 == 7 {
                return Err(Error5250::Malformed(
                    "nondisplay 5250 field has no leading attribute cell",
                ));
            }
            self.lock();
            if let Some(field) = self.fields.iter_mut().find(|field| field.start == start) {
                field.format = format;
                field.hidden = attribute & 7 == 7;
                end = field.end;
            } else {
                if length == 0 || end > self.size.cells() {
                    return Err(Error5250::Malformed("invalid 5250 field extent"));
                }
                if self.fields.len() == MAX_FIELDS
                    || self.fields.last().is_some_and(|field| field.end >= start)
                {
                    return Err(Error5250::Malformed(
                        "5250 input fields overlap, exceed limits or are out of order",
                    ));
                }
                self.fields.push(Field {
                    start,
                    end,
                    format,
                    transparent,
                    hidden: attribute & 7 == 7,
                });
                if let Some(cell) = self.cells.get_mut(end) {
                    *cell = 0x20;
                }
            }
        } else if length == 0 || end > self.size.cells() {
            return Err(Error5250::Malformed("invalid 5250 output field extent"));
        } else if let Some(cell) = self.cells.get_mut(end) {
            *cell = 0x20;
        }
        if marker != BEFORE_SCREEN {
            self.cells[marker] = attribute;
        } else if is_attribute(self.cells[0]) {
            // The leading attribute occupies no screen cell for a row 1,
            // column 1 field. Its first cell must remain usable after Clear.
            self.cells[0] = 0;
        }
        Ok(end)
    }

    pub(super) fn before_control(&mut self, control: u8) {
        let operation = control >> 5;
        if operation == 0 {
            return;
        }
        self.lock();
        for field in &mut self.fields {
            if !field.bypass()
                && (matches!(operation, 5 | 7) || (matches!(operation, 4 | 6) && field.modified()))
            {
                self.cells[field.start..field.end].fill(0);
            }
            if matches!(operation, 3 | 7) || (!field.bypass() && matches!(operation, 2 | 5 | 6)) {
                field.format &= !0x0800;
            }
        }
    }

    pub(super) fn after_control(&mut self, control: u8, movement: Option<usize>) -> bool {
        if control & 0x08 != 0 && (self.keyboard_locked || self.operator_error) {
            self.keyboard_locked = false;
            self.operator_error = false;
            if control & 0x40 == 0 {
                self.cursor = self.home.or_else(|| self.first_input()).unwrap_or(0);
            }
        }
        if let Some(address) = movement {
            self.cursor = address;
        }
        if control & 0x03 != 0 {
            self.message_waiting = control & 1 != 0;
        }
        control & 0x04 != 0
    }

    fn lock(&mut self) {
        self.keyboard_locked = true;
        self.operator_error = false;
        self.insert_mode = false;
    }

    pub(super) fn input_reply(&self, mode: ReadMode, aid: u8, include: bool) -> Record {
        let [row, column] = wire_address(self.size, self.cursor);
        let mut reply = vec![row, column, aid];
        if include && self.fields.iter().any(|field| field.modified()) {
            for field in &self.fields {
                let bytes = &self.cells[field.start..field.end];
                match mode {
                    ReadMode::All => push_field(&mut reply, bytes, field.transparent),
                    ReadMode::Modified | ReadMode::ModifiedAlternate if field.modified() => {
                        let [row, column] = wire_address(self.size, field.start);
                        reply.extend([SBA, row, column]);
                        let length = if field.transparent {
                            bytes.len()
                        } else {
                            bytes
                                .iter()
                                .rposition(|&byte| byte != 0)
                                .map_or(0, |index| index + 1)
                        };
                        let preserve_nulls =
                            field.transparent || matches!(mode, ReadMode::ModifiedAlternate);
                        push_field(&mut reply, &bytes[..length], preserve_nulls);
                    }
                    _ => {}
                }
            }
        }
        gds(0, &reply)
    }
}

struct Data<'a>(&'a [u8]);

impl<'a> Data<'a> {
    fn slice(&mut self, length: usize) -> Result<&'a [u8], Error5250> {
        let (bytes, rest) = self
            .0
            .split_at_checked(length)
            .ok_or(Error5250::Malformed("truncated 5250 command or order"))?;
        self.0 = rest;
        Ok(bytes)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error5250> {
        let Some((&bytes, rest)) = self.0.split_first_chunk::<N>() else {
            return Err(Error5250::Malformed("truncated 5250 command or order"));
        };
        self.0 = rest;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Error5250> {
        Ok(self.take::<1>()?[0])
    }

    fn address(&mut self, size: ScreenSize) -> Result<usize, Error5250> {
        let [row, column] = self.take()?;
        display_address(size, row, column)
    }
}

fn display_address(size: ScreenSize, row: u8, column: u8) -> Result<usize, Error5250> {
    let (row, column) = (usize::from(row), usize::from(column));
    row.checked_sub(1)
        .zip(column.checked_sub(1))
        .and_then(|(row, column)| size.index(row, column))
        .ok_or(Error5250::Malformed(
            "5250 display address is out of bounds",
        ))
}

fn wire_address(size: ScreenSize, address: usize) -> [u8; 2] {
    let position = size.position(address);
    [(position.row + 1) as u8, (position.column + 1) as u8]
}

fn push_field(reply: &mut Vec<u8>, bytes: &[u8], preserve_nulls: bool) {
    reply.extend(bytes.iter().map(|&byte| {
        if byte == 0 && !preserve_nulls {
            0x40
        } else {
            byte
        }
    }));
}

fn gds(opcode: u8, payload: &[u8]) -> Record {
    gds_with_flags(opcode, 0, payload)
}

pub(super) fn signal_record(flags: u8) -> Record {
    gds_with_flags(0, flags, &[])
}

fn gds_with_flags(opcode: u8, flags: u8, payload: &[u8]) -> Record {
    // All callers build replies from the fixed screen and at most 256 fields.
    let [high, low] = ((10 + payload.len()) as u16).to_be_bytes();
    let mut bytes = vec![high, low, 0x12, 0xa0, 0, 0, 4, flags, 0, opcode];
    bytes.extend_from_slice(payload);
    Record::Tn5250e(bytes)
}

fn device_query(model: Model5250) -> Record {
    let mut query = [0; 61];
    query[..13].copy_from_slice(&[0, 0, 0x88, 0, 0x3a, 0xd9, 0x70, 0x80, 0x06, 0, 0, 1, 0]);
    let (device, display_capability) = match model {
        Model5250::Ibm3179Model2 => ([0xf3, 0xf1, 0xf7, 0xf9], 0x11),
        Model5250::Ibm3180Model2 => ([0xf3, 0xf1, 0xf8, 0xf0], 0x30),
    };
    query[29] = 1;
    query[30..34].copy_from_slice(&device);
    query[34..38].copy_from_slice(&[0xf0, 0xf0, 0xf2, 2]);
    query[44..46].copy_from_slice(&(MAX_FIELDS as u16).to_be_bytes());
    // RFC 1205 section 5.3. Row 1/column 1, both alternate MDT reads and MC.
    query[49] = 0x63;
    query[50] = display_capability;
    gds(0, &query)
}
