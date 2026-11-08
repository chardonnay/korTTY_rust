use super::{
    CELLS, COLUMNS, Cell, Color3270, Error3270, Field, Highlight3270, Keyboard3270, NULL, ROWS,
    ReplyMode, Style3270, Tn3270Screen, Update3270, display_record, next,
};

const SBA: u8 = 0x11;
const SF: u8 = 0x1d;

impl Tn3270Screen {
    pub(super) fn apply_display(&mut self, bytes: &[u8]) -> Result<Update3270, Error3270> {
        let Some((&command, data)) = bytes.split_first() else {
            return Err(Error3270::MissingCommand);
        };
        let mut update = Update3270::default();
        match command {
            0xf1 | 0x01 => update.alarm = self.write(data, false)?,
            0xf5 | 0x05 | 0x7e | 0x0d => update.alarm = self.write(data, true)?,
            0xf2 | 0x02 if data.is_empty() => {
                update.reply = Some(display_record(self.read_buffer(self.last_aid)));
            }
            0xf6 | 0x06 | 0x6e if data.is_empty() => {
                update.reply = Some(display_record(
                    self.read_modified(self.last_aid, command == 0x6e),
                ));
            }
            0x6f | 0x0f if data.is_empty() => self.erase_all_unprotected(),
            0xf3 | 0x11 => return self.structured_fields(data),
            0xf2 | 0x02 | 0xf6 | 0x06 | 0x6e | 0x6f | 0x0f => {
                return Err(Error3270::Malformed("unexpected data after a 3270 command"));
            }
            _ => return Err(Error3270::UnsupportedCommand(command)),
        }
        Ok(update)
    }

    fn write(&mut self, bytes: &[u8], erase: bool) -> Result<bool, Error3270> {
        let Some((&wcc, data)) = bytes.split_first() else {
            return Ok(false);
        };
        if erase {
            if wcc & 0x40 != 0 {
                self.reset_partition();
            } else {
                self.clear();
            }
        }
        if wcc & 0x01 != 0 {
            for cell in &mut self.cells {
                if let Cell::Field(field) = cell {
                    field.bits &= !1;
                }
            }
        }
        let mut data = Data(data);
        let mut address = self.cursor;
        let mut style = Style3270::default();
        let mut previous_was_data = false;
        let mut continued_tab = false;
        while !data.0.is_empty() {
            let order = data.byte()?;
            let tab_erases = previous_was_data || continued_tab;
            previous_was_data = false;
            continued_tab = false;
            match order {
                SBA => address = data.address()?,
                SF => {
                    self.cells[address] = Cell::Field(Field {
                        bits: data.byte()? & 0x3f,
                        style: Style3270::default(),
                    });
                    address = next(address);
                }
                0x29 => {
                    self.cells[address] = Cell::Field(data.field(Field::default())?);
                    address = next(address);
                }
                0x2c => {
                    let Some(field) = self.cells[address].field() else {
                        return Err(Error3270::Malformed(
                            "3270 Modify Field requires a field marker",
                        ));
                    };
                    self.cells[address] = Cell::Field(data.field(field)?);
                    address = next(address);
                }
                0x28 => {
                    let [kind, value] = data.take()?;
                    set_style(&mut style, kind, value)?;
                }
                0x13 => self.cursor = address,
                0x05 => (address, continued_tab) = self.program_tab(address, tab_erases),
                0x3c => {
                    let stop = data.address()?;
                    let byte = data.byte()?;
                    if !is_display_data(byte) {
                        return Err(Error3270::UnsupportedOrder(byte));
                    }
                    let length = range_length(address, stop);
                    for offset in 0..length {
                        self.cells[(address + offset) % CELLS] = Cell::Character { byte, style };
                    }
                    address = stop;
                }
                0x12 => {
                    let stop = data.address()?;
                    self.erase_unprotected(address, stop);
                    address = stop;
                }
                byte if is_display_data(byte) => {
                    self.cells[address] = Cell::Character { byte, style };
                    address = next(address);
                    previous_was_data = true;
                }
                _ => return Err(Error3270::UnsupportedOrder(order)),
            }
        }
        if wcc & 0x02 != 0 {
            self.restore_keyboard();
        }
        Ok(wcc & 0x04 != 0)
    }

    fn program_tab(&mut self, address: usize, erase: bool) -> (usize, bool) {
        if let Some(first) = self.input_start_at(address) {
            return (first, false);
        }
        let mut search = address;
        let mut continued = false;
        if erase {
            while search < CELLS && self.cells[search].field().is_none() {
                self.cells[search] = NULL;
                search += 1;
            }
            continued = search == CELLS;
        }
        let next_field = (search..CELLS).find_map(|index| self.input_start_at(index));
        (next_field.unwrap_or(0), continued)
    }

    fn erase_unprotected(&mut self, start: usize, stop: usize) {
        let mut protected = self
            .field_owner(start)
            .is_some_and(|(_, field)| field.protected());
        for offset in 0..range_length(start, stop) {
            let address = (start + offset) % CELLS;
            match self.cells[address] {
                Cell::Field(field) => protected = field.protected(),
                Cell::Character { .. } if !protected => self.cells[address] = NULL,
                Cell::Character { .. } => {}
            }
        }
    }

    fn erase_all_unprotected(&mut self) {
        self.erase_unprotected(0, 0);
        for cell in &mut self.cells {
            if let Cell::Field(field) = cell
                && !field.protected()
            {
                field.bits &= !1;
            }
        }
        self.cursor = self.first_input().unwrap_or(0);
        self.restore_keyboard();
    }

    // IBM GA23-0059-07 §§5.1, 5.9, 5.16, 5.19 and 5.30. The screen
    // commits a chain only after all its structured fields have succeeded.
    fn structured_fields(&mut self, mut bytes: &[u8]) -> Result<Update3270, Error3270> {
        let mut update = Update3270::default();
        while !bytes.is_empty() {
            let length = usize::from(u16::from_be_bytes(Data(bytes).take()?));
            let length = if length == 0 { bytes.len() } else { length };
            let Some(([_, _, id, data @ ..], remaining)) = bytes.split_at_checked(length) else {
                return Err(Error3270::Malformed("invalid 3270 structured field length"));
            };
            bytes = remaining;
            match (*id, data) {
                (0x01, _) if bytes.is_empty() => {
                    update.reply = Some(display_record(self.read_partition(data)?));
                }
                (0x01, _) => {
                    return Err(Error3270::Malformed(
                        "3270 Read Partition must be the last structured field",
                    ));
                }
                (0x03, [0x00 | 0x80]) => {
                    // Both implicit sizes of model 2 are 24×80.
                    self.reset_partition();
                    self.restore_keyboard();
                }
                (0x03, _) => return Err(Error3270::Malformed("invalid 3270 Erase/Reset field")),
                (0x09, [0, 0]) => self.reply_mode = ReplyMode::Field,
                (0x09, [0, 1]) => self.reply_mode = ReplyMode::ExtendedField,
                (0x09, [0, 2, attributes @ ..]) => {
                    self.reply_mode = ReplyMode::character(attributes)?
                }
                (0x09, _) => return Err(Error3270::Malformed("invalid 3270 Set Reply Mode field")),
                (0x40, [0, command, data @ ..]) => {
                    update.alarm |= self.outbound_stream(*command, data)?
                }
                (0x40, _) => {
                    return Err(Error3270::Malformed(
                        "invalid 3270 Outbound 3270DS partition or command",
                    ));
                }
                (id, _) => return Err(Error3270::UnsupportedStructuredField(id)),
            }
        }
        Ok(update)
    }

    fn outbound_stream(&mut self, command: u8, data: &[u8]) -> Result<bool, Error3270> {
        match command {
            0xf1 => self.write(data, false),
            0xf5 | 0x7e => self.write(data, true),
            0x6f if data.is_empty() => {
                self.erase_all_unprotected();
                Ok(false)
            }
            0x6f => Err(Error3270::Malformed(
                "unexpected data after 3270 Erase All Unprotected",
            )),
            _ => Err(Error3270::UnsupportedCommand(command)),
        }
    }

    fn read_partition(&mut self, data: &[u8]) -> Result<Vec<u8>, Error3270> {
        let reply = match data {
            [0xff, 0x02] => query_reply(true, &[]),
            [0xff, 0x03, 0x00, codes @ ..] => query_reply(false, codes),
            [0xff, 0x03, 0x40 | 0x80, ..] => query_reply(true, &[]),
            [0, 0xf2] => self.read_buffer(0x61),
            [0, 0xf6] => self.read_modified(0x61, false),
            [0, 0x6e] => self.read_modified(0x61, true),
            _ => return Err(Error3270::Malformed("invalid 3270 Read Partition request")),
        };
        self.keyboard = Keyboard3270::HostLocked;
        self.insert_mode = false;
        Ok(reply)
    }
}

struct Data<'a>(&'a [u8]);

impl Data<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error3270> {
        let Some((&bytes, rest)) = self.0.split_first_chunk::<N>() else {
            return Err(Error3270::Malformed("truncated 3270 order or field"));
        };
        self.0 = rest;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Error3270> {
        Ok(self.take::<1>()?[0])
    }

    fn address(&mut self) -> Result<usize, Error3270> {
        let [high, low] = self.take()?;
        let address = match high & 0xc0 {
            0x00 => usize::from(u16::from_be_bytes([high, low])),
            0x40 | 0xc0 if matches!(low & 0xc0, 0x40 | 0xc0) => {
                (usize::from(high & 0x3f) << 6) | usize::from(low & 0x3f)
            }
            _ => return Err(Error3270::Malformed("invalid 3270 buffer address encoding")),
        };
        if address >= CELLS {
            return Err(Error3270::Malformed(
                "3270 buffer address is outside the screen",
            ));
        }
        Ok(address)
    }

    fn field(&mut self, mut field: Field) -> Result<Field, Error3270> {
        let count = self.byte()?;
        for _ in 0..count {
            let [kind, value] = self.take()?;
            if kind == 0xc0 {
                field.bits = value & 0x3f;
            } else if kind != 0 {
                set_style(&mut field.style, kind, value)?;
            } else {
                return Err(Error3270::UnsupportedAttribute(kind));
            }
        }
        Ok(field)
    }
}

fn range_length(start: usize, stop: usize) -> usize {
    if stop <= start {
        CELLS + stop - start
    } else {
        stop - start
    }
}

fn is_display_data(byte: u8) -> bool {
    matches!(
        byte,
        0 | 0x0c | 0x0d | 0x15 | 0x19 | 0x1c | 0x1e | 0x3f | 0x40..=0xff
    )
}

fn set_style(style: &mut Style3270, kind: u8, value: u8) -> Result<(), Error3270> {
    match kind {
        0 if value == 0 => *style = Style3270::default(),
        0x41 => {
            style.highlighting = match value {
                0 => None,
                0xf0 => Some(Highlight3270::Normal),
                0xf1 => Some(Highlight3270::Blink),
                0xf2 => Some(Highlight3270::Reverse),
                0xf4 => Some(Highlight3270::Underline),
                _ => return Err(Error3270::UnsupportedAttribute(kind)),
            };
        }
        0x42 => style.foreground = color(value)?,
        0x45 => style.background = color(value)?,
        _ => return Err(Error3270::UnsupportedAttribute(kind)),
    }
    Ok(())
}

fn color(value: u8) -> Result<Option<Color3270>, Error3270> {
    use Color3270 as Color;
    Ok(Some(match value {
        0 => return Ok(None),
        0xf0 | 0xf8 => Color::Black,
        0xf1 => Color::Blue,
        0xf2 => Color::Red,
        0xf3 => Color::Pink,
        0xf4 => Color::Green,
        0xf5 => Color::Turquoise,
        0xf6 => Color::Yellow,
        0xf7 | 0xff => Color::White,
        0xf9 => Color::DeepBlue,
        0xfa => Color::Orange,
        0xfb => Color::Purple,
        0xfc => Color::PaleGreen,
        0xfd => Color::PaleTurquoise,
        0xfe => Color::Grey,
        _ => return Err(Error3270::Malformed("invalid 3270 color")),
    }))
}

fn query_reply(all: bool, codes: &[u8]) -> Vec<u8> {
    let mut reply = vec![0x88];
    if all || codes.contains(&0x80) {
        reply.extend([0, 9, 0x81, 0x80, 0x80, 0x81, 0x86, 0x87, 0x88]);
    }
    if all || codes.contains(&0x81) {
        reply.extend([0, 21, 0x81, 0x81, 0x01, 0x40]);
        reply.extend((COLUMNS as u16).to_be_bytes());
        reply.extend((ROWS as u16).to_be_bytes());
        // A virtual, non-matrix display reports no physical cell metrics.
        reply.extend([0; 11]);
    }
    if all || codes.contains(&0x86) {
        // IBM GA23-0059-07 §6.13. Color zero uses the display defaults.
        reply.extend([0, 44, 0x81, 0x86, 0, 17, 0, 0xf4]);
        for color in 0xf0..=0xff {
            reply.extend([color, color]);
        }
        reply.extend([4, 0x02, 0, 0xf0]);
    }
    if all || codes.contains(&0x87) {
        // §6.28. Normal, blink, reverse and underscore.
        reply.extend([
            0, 15, 0x81, 0x87, 5, 0, 0xf0, 0xf0, 0xf0, 0xf1, 0xf1, 0xf2, 0xf2, 0xf4, 0xf4,
        ]);
    }
    if all || codes.contains(&0x88) {
        reply.extend([0, 7, 0x81, 0x88, 0x00, 0x01, 0x02]);
    }
    if reply.len() == 1 {
        reply.extend([0, 4, 0x81, 0xff]);
    }
    reply
}
