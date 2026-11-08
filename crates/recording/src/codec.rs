use std::io::{Read, Write};
use std::time::Duration;

use kortty_terminal::{Cell, Color, Cursor, CursorShape, ScreenSnapshot, Style};

use crate::{Error, RecordedCell, RecordedScreen};

pub(crate) const HEADER: &[u8; 12] = b"korttyrec\x01\0\0";
pub(crate) const EVENT_HEADER: usize = 13;
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_EVENTS: usize = 1_000_000;
const MAX_COLUMNS: u16 = 1024;
const MAX_ROWS: u16 = 512;
const MAX_GLYPH_BYTES: usize = 1024;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Start = 1,
    Screen = 2,
    InputActivity = 3,
    Pause = 4,
    Resume = 5,
    Stop = 6,
    End = 7,
}

pub(crate) struct EventHeader {
    pub kind: Kind,
    pub at: u64,
    pub length: usize,
}

pub(crate) fn nanos(at: Duration) -> Result<u64, Error> {
    u64::try_from(at.as_nanos()).map_err(|_| Error::Limit("nanosecond timestamp"))
}

pub(crate) fn write_event(
    writer: &mut impl Write,
    kind: Kind,
    at: u64,
    payload: &[u8],
) -> Result<(), Error> {
    let length = u32::try_from(payload.len()).map_err(|_| Error::Limit("event length"))?;
    let mut header = [0; EVENT_HEADER];
    header[0] = kind as u8;
    header[1..9].copy_from_slice(&at.to_le_bytes());
    header[9..].copy_from_slice(&length.to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    Ok(())
}

pub(crate) fn read_event(reader: &mut impl Read) -> Result<EventHeader, Error> {
    let mut bytes = [0; EVENT_HEADER];
    reader.read_exact(&mut bytes)?;
    let mut decoder = Decoder(&bytes);
    let kind = match decoder.byte()? {
        1 => Kind::Start,
        2 => Kind::Screen,
        3 => Kind::InputActivity,
        4 => Kind::Pause,
        5 => Kind::Resume,
        6 => Kind::Stop,
        7 => Kind::End,
        _ => return Err(Error::Format("unknown event type")),
    };
    let at = decoder.u64()?;
    let length = decoder.u32()? as usize;
    if kind == Kind::Screen {
        if !(21..=MAX_FRAME_BYTES).contains(&length) {
            return Err(Error::Format("invalid screen length"));
        }
    } else if length != 0 {
        return Err(Error::Format("unexpected event payload"));
    }
    Ok(EventHeader { kind, at, length })
}

pub(crate) fn encode_screen(
    pane: u64,
    screen: &ScreenSnapshot,
    pixels: (u16, u16),
    include_style: bool,
) -> Result<Vec<u8>, Error> {
    let expected = grid_len(screen.columns, screen.rows)
        .ok_or(Error::Invalid("screen dimensions are out of range"))?;
    if screen.cells.len() != expected
        || screen.cursor.column >= screen.columns
        || screen.cursor.row >= screen.rows
        || !screen
            .cells
            .chunks_exact(usize::from(screen.columns))
            .all(|row| valid_widths(row.iter().map(|cell| cell.width)))
    {
        return Err(Error::Invalid(
            "screen cells or cursor do not match its dimensions",
        ));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&pane.to_le_bytes());
    for value in [
        screen.columns,
        screen.rows,
        pixels.0,
        pixels.1,
        screen.cursor.column,
        screen.cursor.row,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let shape = match screen.cursor_shape {
        CursorShape::Block => 0,
        CursorShape::Underline => 1,
        CursorShape::Bar => 2,
    };
    bytes.push(
        shape
            | u8::from(screen.cursor.visible) << 2
            | u8::from(screen.cursor_blinking) << 3
            | u8::from(include_style && screen.reverse_video) << 4,
    );

    let mut cells = screen
        .cells
        .iter()
        .map(|cell| VisualCell::new(cell, include_style));
    let mut current = cells.next().ok_or(Error::Invalid("empty screen grid"))?;
    let mut expanded_text = current.text.len();
    let mut count = 1u32;
    for cell in cells {
        expanded_text += cell.text.len();
        if expanded_text > MAX_FRAME_BYTES {
            return Err(Error::Limit("decoded screen text"));
        }
        if cell == current {
            count += 1;
        } else {
            encode_run(&mut bytes, count, current)?;
            current = cell;
            count = 1;
        }
    }
    encode_run(&mut bytes, count, current)?;
    Ok(bytes)
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct VisualCell<'a> {
    width: u8,
    text: &'a str,
    style: Style,
}

impl<'a> VisualCell<'a> {
    fn new(cell: &'a Cell, include_style: bool) -> Self {
        Self {
            width: cell.width,
            text: if cell.style.hidden || cell.width == 0 {
                ""
            } else {
                &cell.text
            },
            style: if include_style {
                cell.style
            } else {
                Style::default()
            },
        }
    }
}

fn encode_run(bytes: &mut Vec<u8>, count: u32, cell: VisualCell<'_>) -> Result<(), Error> {
    if cell.text.len() > MAX_GLYPH_BYTES {
        return Err(Error::Limit("cell text length"));
    }
    if bytes.len() + 16 + cell.text.len() > MAX_FRAME_BYTES {
        return Err(Error::Limit("screen size"));
    }
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.push(cell.width);
    let style = cell.style;
    let options = [
        style.bold,
        style.italic,
        style.underline,
        style.inverse,
        style.dim,
        style.blink,
        style.hidden,
        style.strikethrough,
    ];
    bytes.push(
        options
            .into_iter()
            .enumerate()
            .fold(0, |flags, (bit, enabled)| {
                flags | (u8::from(enabled) << bit)
            }),
    );
    encode_color(bytes, style.foreground);
    encode_color(bytes, style.background);
    bytes.extend_from_slice(&(cell.text.len() as u16).to_le_bytes());
    bytes.extend_from_slice(cell.text.as_bytes());
    Ok(())
}

fn encode_color(bytes: &mut Vec<u8>, color: Color) {
    match color {
        Color::Default => bytes.push(0),
        Color::Indexed(index) => bytes.extend_from_slice(&[1, index]),
        Color::Rgb(red, green, blue) => bytes.extend_from_slice(&[2, red, green, blue]),
    }
}

pub(crate) fn decode_screen(bytes: &[u8]) -> Result<(u64, RecordedScreen), Error> {
    let mut decoder = Decoder(bytes);
    let pane = decoder.u64()?;
    let columns = decoder.u16()?;
    let rows = decoder.u16()?;
    let pixels = (decoder.u16()?, decoder.u16()?);
    let column = decoder.u16()?;
    let row = decoder.u16()?;
    let flags = decoder.byte()?;
    let expected = grid_len(columns, rows).ok_or(Error::Format("invalid grid dimensions"))?;
    if column >= columns || row >= rows || flags & !0x1f != 0 {
        return Err(Error::Format("invalid cursor position or screen flags"));
    }
    let cursor_shape = match flags & 3 {
        0 => CursorShape::Block,
        1 => CursorShape::Underline,
        2 => CursorShape::Bar,
        _ => return Err(Error::Format("invalid cursor shape")),
    };
    let mut cells = Vec::with_capacity(expected);
    let mut text_bytes = 0usize;
    while cells.len() < expected {
        let count = decoder.u32()? as usize;
        let width = decoder.byte()?;
        let options = decoder.byte()?;
        let foreground = decoder.color()?;
        let background = decoder.color()?;
        let length = usize::from(decoder.u16()?);
        if count == 0
            || count > expected - cells.len()
            || width > 2
            || length > MAX_GLYPH_BYTES
            || length != 0 && (width == 0 || options & 64 != 0)
        {
            return Err(Error::Format("invalid cell run"));
        }
        text_bytes += count * length;
        if text_bytes > MAX_FRAME_BYTES {
            return Err(Error::Limit("decoded screen text"));
        }
        let text = std::str::from_utf8(decoder.take(length)?)
            .map_err(|_| Error::Format("invalid cell UTF-8"))?;
        let style = Style {
            foreground,
            background,
            bold: options & 1 != 0,
            italic: options & 2 != 0,
            underline: options & 4 != 0,
            inverse: options & 8 != 0,
            dim: options & 16 != 0,
            blink: options & 32 != 0,
            hidden: options & 64 != 0,
            strikethrough: options & 128 != 0,
        };
        cells.resize(
            cells.len() + count,
            RecordedCell {
                text: text.into(),
                width,
                style,
            },
        );
    }
    if !decoder.0.is_empty()
        || !cells
            .chunks_exact(usize::from(columns))
            .all(|row| valid_widths(row.iter().map(|cell| cell.width)))
    {
        return Err(Error::Format("invalid screen grid"));
    }
    Ok((
        pane,
        RecordedScreen {
            columns,
            rows,
            pixels,
            cursor: Cursor {
                column,
                row,
                visible: flags & 4 != 0,
            },
            cursor_shape,
            cursor_blinking: flags & 8 != 0,
            reverse_video: flags & 16 != 0,
            cells,
        },
    ))
}

fn valid_widths(widths: impl Iterator<Item = u8>) -> bool {
    let mut continuation = false;
    for width in widths {
        if width > 2 || (width == 0) != continuation {
            return false;
        }
        continuation = width == 2;
    }
    !continuation
}

fn grid_len(columns: u16, rows: u16) -> Option<usize> {
    ((1..=MAX_COLUMNS).contains(&columns) && (1..=MAX_ROWS).contains(&rows))
        .then_some(usize::from(columns) * usize::from(rows))
}

struct Decoder<'a>(&'a [u8]);

impl<'a> Decoder<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let (field, rest) = self
            .0
            .split_at_checked(length)
            .ok_or(Error::Format("truncated event"))?;
        self.0 = rest;
        Ok(field)
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.bytes::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.bytes()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn color(&mut self) -> Result<Color, Error> {
        match self.byte()? {
            0 => Ok(Color::Default),
            1 => Ok(Color::Indexed(self.byte()?)),
            2 => {
                let [red, green, blue] = self.bytes()?;
                Ok(Color::Rgb(red, green, blue))
            }
            _ => Err(Error::Format("invalid cell color")),
        }
    }
}
