//! Streaming capture evidence with physical coordinates for journal citations.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read};

use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::{FileStamp, require_single_link};

use super::{
    CaptureRecord, CaptureStatus, CaptureText, Compression, ContentReader, LocalSource,
    MAX_LINE_BYTES, MAX_PART_BYTES, Part, PartEnvelope, PartLine, check_content_cancel,
    discover_parts, invalid,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Position {
    pub(crate) part: u32,
    pub(crate) sequence: u64,
    line: u64,
    digest: Vec<u8>,
}

pub(in crate::session_journal) struct Row<'a> {
    part: u32,
    line: u64,
    bytes: &'a [u8],
    pub(in crate::session_journal) record: CaptureRecord,
}

impl Row<'_> {
    pub(in crate::session_journal) fn matches_position(&self, position: &Position) -> bool {
        self.part == position.part
            && self.line == position.line
            && self.record.sequence == position.sequence
            && self
                .position()
                .as_ref()
                .is_ok_and(|current| current == position)
    }

    pub(in crate::session_journal) fn position(&self) -> io::Result<Position> {
        Ok(Position {
            part: self.part,
            sequence: self.record.sequence,
            line: self.line,
            digest: HashAlgorithm::Sha256
                .digest(self.bytes)
                .map_err(io::Error::other)?,
        })
    }
}

/// Visits each selected part once. The caller's budget covers both compressed and decoded bytes.
/// Returning false stops at the current row, after validating the opened part again.
pub(in crate::session_journal) fn visit(
    source: &LocalSource,
    id: &str,
    declared_parts: u32,
    only_part: Option<u32>,
    budget: &mut u64,
    cancelled: &dyn Fn() -> bool,
    mut visitor: impl FnMut(Row<'_>) -> bool,
) -> io::Result<CaptureStatus> {
    check_content_cancel(cancelled)?;
    let (parts, mut status) = discover_parts(source, declared_parts)?;
    for part in parts
        .values()
        .filter(|part| only_part.is_none_or(|n| n == part.number))
    {
        check_content_cancel(cancelled)?;
        if *budget == 0 {
            status.limited = true;
            break;
        }
        let name = OsStr::new(&part.name);
        let Ok(file) = source.open_sibling(name) else {
            status.unreadable_parts += 1;
            continue;
        };
        if require_single_link(&file).is_err() {
            status.unreadable_parts += 1;
            continue;
        }
        let stamp = FileStamp::read(&file)?;
        let size = file.metadata()?.len();
        if size > MAX_PART_BYTES {
            status.limited = true;
            // A skipped part can contain the start of a sensitive block in a later part.
            break;
        }
        let limit = (*budget).min(MAX_PART_BYTES);
        let mut raw = (&file).take(limit);
        let (result, remaining) = {
            let decoded = part.decode(ContentReader::new(&mut raw, cancelled), cancelled);
            let mut reader = BufReader::new(ContentReader::new(decoded, cancelled).take(limit));
            let result = scan(part, &mut reader, id, cancelled, &mut status, &mut visitor);
            (result, reader.get_ref().limit())
        };
        *budget -= limit - raw.limit().min(remaining);
        status.limited |= raw.limit() == 0 && size > limit
            || remaining == 0 && part.compression != Compression::Plain;
        check_content_cancel(cancelled)?;
        stamp.require_unchanged(&file)?;
        let current = source.open_sibling(name)?;
        require_single_link(&current)?;
        stamp.require_unchanged(&current)?;
        match result {
            Ok(false) => break,
            Ok(true) => {}
            Err(_) => status.unreadable_parts += 1,
        }
    }
    check_content_cancel(cancelled)?;
    Ok(status)
}

fn scan(
    part: &Part,
    reader: &mut impl BufRead,
    id: &str,
    cancelled: &dyn Fn() -> bool,
    status: &mut CaptureStatus,
    visitor: &mut impl FnMut(Row<'_>) -> bool,
) -> io::Result<bool> {
    let mut bytes = Vec::new();
    let mut envelope = PartEnvelope::default();
    let mut line = 0;
    loop {
        check_content_cancel(cancelled)?;
        bytes.clear();
        let n = (&mut *reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            envelope.finish(status)?;
            return Ok(true);
        }
        line += 1;
        let overlong = n as u64 > MAX_LINE_BYTES;
        let complete = bytes.last() == Some(&b'\n');
        if overlong || !complete {
            status.skipped_lines += 1;
            if overlong && !complete {
                reader.skip_until(b'\n')?;
            }
            continue;
        }
        match envelope.line(&bytes, part, id, CaptureText::Persisted)? {
            PartLine::Record(record, _) => {
                if !visitor(Row {
                    part: part.number,
                    line,
                    bytes: &bytes,
                    record,
                }) {
                    return Ok(false);
                }
            }
            PartLine::Skipped => status.skipped_lines += 1,
            _ => {}
        }
    }
}

pub(in crate::session_journal) fn resolve(
    source: &LocalSource,
    id: &str,
    declared_parts: u32,
    position: &Position,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<CaptureRecord> {
    let mut found = None;
    let mut budget = MAX_PART_BYTES;
    visit(
        source,
        id,
        declared_parts,
        Some(position.part),
        &mut budget,
        cancelled,
        |row| {
            if row.line < position.line {
                return true;
            }
            if row.matches_position(position) {
                found = Some(row.record);
            }
            false
        },
    )?;
    found.ok_or_else(|| invalid("The cited capture record changed or is unavailable"))
}
