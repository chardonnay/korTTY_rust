use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, BufRead, BufReader, Read, Seek as _, SeekFrom};
use std::ops::RangeInclusive;
use std::path::Path;

use flate2::read::MultiGzDecoder;
use jiff::Timestamp;
use serde::Deserialize;

use crate::logfile_window::zstandard;
use crate::private_files::EntryIdentity;
use crate::sftp_window::LocalSource;

use super::{
    LogFormat as Format, display_text, invalid, open_document, parse_xml, scalar_text,
    screenshot_reference,
};
#[cfg(test)]
use super::{child, document_id, document_text, number, parse_document};

use super::writer::file_identity;

pub(super) mod bundle;
mod live_archive;
pub(super) mod replacement;
pub(super) mod retrieval;
use live_archive::ArchiveReader;
pub(super) use live_archive::{LiveArchive, LiveArchives};

const MAX_PARTS: u32 = 1000;
const MAX_DIRECTORY_NAMES: usize = 4096;
const MAX_PART_BYTES: u64 = 128 * 1024 * 1024;
pub(super) const MAX_CAPTURE_BYTES: u64 = MAX_PARTS as u64 * MAX_PART_BYTES;
// The Java writer uses zstd-jni setLong(27), even for small closed parts.
const MAX_JAVA_ZSTD_WINDOW_BYTES: u64 = 1 << 27;
const MAX_SCAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCAN_LINES: u32 = 32_768;
const MAX_LINE_BYTES: u64 = 256 * 1024;
const MAX_PAGE_BYTES: usize = 512 * 1024;
const MAX_PAGE_RECORDS: usize = 256;
const MAX_QUERY_BYTES: usize = 1024;
const LIVE_SCAN_BYTES: u64 = 1024 * 1024;
const LIVE_SCAN_PARTS: u32 = 4;
const LIVE_ANCHOR_BYTES: u64 = 64;

/// Position in the physical stream. Java can persist rotation markers out of sequence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Cursor {
    part: u32,
    line: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureKind {
    Output,
    Input,
    Seed,
    Screenshot,
    Note,
}

impl CaptureKind {
    pub(super) fn parse(value: &str) -> io::Result<Self> {
        match value {
            "out" => Ok(Self::Output),
            "in" => Ok(Self::Input),
            "seed" => Ok(Self::Seed),
            "screenshot" => Ok(Self::Screenshot),
            "note" => Ok(Self::Note),
            _ => Err(invalid("Unknown session journal capture kind")),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureRecord {
    pub sequence: u64,
    pub timestamp: Timestamp,
    pub kind: CaptureKind,
    pub text: String,
    pub redacted: bool,
    pub partial: bool,
    pub repeat: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureStatus {
    pub skipped_lines: usize,
    pub missing_parts: usize,
    pub unreadable_parts: usize,
    pub unclosed_parts: usize,
    pub limited: bool,
}

impl CaptureStatus {
    fn is_partial(&self) -> bool {
        self.limited
            || self.skipped_lines != 0
            || self.missing_parts != 0
            || self.unreadable_parts != 0
            || self.unclosed_parts != 0
    }

    pub(crate) fn notice(&self) -> Option<String> {
        let mut details = Vec::new();
        for (count, description) in [
            (self.skipped_lines, "malformed capture lines skipped"),
            (self.missing_parts, "log parts missing"),
            (self.unreadable_parts, "log parts unreadable"),
            (self.unclosed_parts, "log parts without a complete ending"),
        ] {
            if count != 0 {
                details.push(format!("{count} {description}"));
            }
        }
        if self.limited {
            details.push("reader limit reached".into());
        }
        (!details.is_empty()).then(|| format!("Partial capture. {}.", details.join(", ")))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CapturePage {
    pub records: Vec<CaptureRecord>,
    pub next: Option<Cursor>,
    pub status: CaptureStatus,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveCursor {
    journal: String,
    format: Format,
    part: u32,
    identity: EntryIdentity,
    offset: u64,
    anchor: u64,
    footer: bool,
}

#[derive(Debug)]
pub(crate) struct LivePage {
    pub records: Vec<CaptureRecord>,
    pub cursor: LiveCursor,
    /// The returned newest slice replaces the consumer's previous rows.
    pub reset: bool,
    /// More complete records can be fetched with this cursor immediately.
    pub more: bool,
    pub skipped_lines: usize,
}

/// Reads the bounded latest slice of an active native journal, then continues by byte position.
/// An incomplete line is retained on disk for the next poll.
pub(super) fn capture_live_tail(
    directory: &Path,
    format: Format,
    archives: &LiveArchives,
    after: Option<&LiveCursor>,
) -> io::Result<LivePage> {
    let id = directory
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("journal-"))
        .filter(|id| {
            id.len() == 36
                && id.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        })
        .ok_or_else(|| invalid("Expected a native session journal directory"))?;
    let source = open_document(directory)?;
    if let Some(after) = after.filter(|after| after.journal == id && after.format == format)
        && let Some(page) = live_increment(&source, id, archives, after)?
    {
        return Ok(page);
    }
    live_backfill(&source, id, format, archives)
}

struct LivePart {
    data: LiveData,
    identity: EntryIdentity,
    format: Format,
    length: u64,
    body: u64,
}

enum LiveData {
    Plain(File),
    Archived(ArchiveReader),
}

impl LivePart {
    fn open(
        source: &LocalSource,
        journal: &str,
        format: Format,
        number: u32,
        archives: &LiveArchives,
    ) -> io::Result<Option<Self>> {
        let name = format.part_name(number);
        let plain = source.open_sibling(OsStr::new(&name));
        // Check the registry after opening: a sealed plain file can already be
        // delete-pending on Windows while an earlier reader still holds it.
        let (data, identity, length, body) = match (plain, archives.get(number)) {
            (_, Some(archive)) => {
                let file = source.open_sibling(OsStr::new(&format!("{name}.zst")))?;
                let identity = archive.original;
                let length = archive.length;
                let reader = ArchiveReader::open(file, archive)?;
                // The writer's bounded metadata and XML escaping fit in the first frame.
                let prefix = reader.read(0, length.min(zstandard::ENCODE_CHUNK_BYTES as u64))?;
                let body = live_header(prefix.as_slice(), journal, format, number)?
                    .ok_or_else(|| invalid("Live journal archive has an incomplete header"))?;
                (LiveData::Archived(reader), identity, length, body)
            }
            (Ok(file), None) => {
                let length = file.metadata()?.len();
                if length > MAX_PART_BYTES {
                    return Err(invalid("Live journal part exceeds its size limit"));
                }
                let reader = BufReader::new((&file).take(length));
                let Some(body) = live_header(reader, journal, format, number)? else {
                    return Ok(None);
                };
                let identity = file_identity(&file)?;
                (LiveData::Plain(file), identity, length, body)
            }
            (Err(error), None) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            (Err(error), None) => return Err(error),
        };
        Ok(Some(Self {
            data,
            identity,
            format,
            length,
            body,
        }))
    }

    fn read(&self, offset: u64, bytes: u64) -> io::Result<Vec<u8>> {
        let mut file = match &self.data {
            LiveData::Plain(file) => file,
            LiveData::Archived(reader) => return reader.read(offset, bytes),
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut output = Vec::with_capacity(bytes as usize);
        file.take(bytes).read_to_end(&mut output)?;
        if output.len() as u64 != bytes {
            return Err(invalid("Live journal part changed during the read"));
        }
        Ok(output)
    }

    fn anchor(&self, offset: u64) -> io::Result<u64> {
        let start = offset.saturating_sub(LIVE_ANCHOR_BYTES);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.read(start, offset - start)?.hash(&mut hash);
        Ok(hash.finish())
    }

    fn cursor(
        &self,
        journal: &str,
        part: u32,
        offset: u64,
        footer: bool,
    ) -> io::Result<LiveCursor> {
        Ok(LiveCursor {
            journal: journal.into(),
            format: self.format,
            part,
            identity: self.identity,
            offset,
            anchor: self.anchor(offset)?,
            footer,
        })
    }
}

fn live_header(
    mut reader: impl BufRead,
    journal: &str,
    format: Format,
    number: u32,
) -> io::Result<Option<u64>> {
    let (header_line, header_lines) = match format {
        Format::Xml => (1, 3),
        Format::Json => (0, 1),
        Format::Yaml => (3, 5),
    };
    let mut line = Vec::new();
    let mut body = 0;
    for index in 0..header_lines {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)? as u64;
        if read > MAX_LINE_BYTES {
            return Err(invalid("Live journal header exceeds its size limit"));
        }
        if line.last() != Some(&b'\n') {
            return Ok(None);
        }
        let valid = match live_line(format, &line)? {
            Line::Preamble => index < header_line,
            Line::Header {
                version: 1,
                journal_id,
                part,
            } => index == header_line && journal_id == journal && part == number,
            Line::Metadata => index > header_line,
            _ => false,
        };
        if !valid {
            return Err(invalid("Live journal capture header does not match"));
        }
        body += read;
    }
    Ok(Some(body))
}

fn live_line(format: Format, line: &[u8]) -> io::Result<Line> {
    if line.len() as u64 > MAX_LINE_BYTES {
        return Err(invalid("Live journal record exceeds its size limit"));
    }
    let line = std::str::from_utf8(line).map_err(|_| invalid("Live journal line is not UTF-8"))?;
    parse_line(format, line.trim(), CaptureText::Display)
}

fn live_kind(record: &CaptureRecord) -> bool {
    matches!(
        record.kind,
        CaptureKind::Output | CaptureKind::Seed | CaptureKind::Note
    )
}

fn complete_prefix(bytes: &[u8]) -> io::Result<usize> {
    let complete = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    if (bytes.len() - complete) as u64 > MAX_LINE_BYTES {
        return Err(invalid(
            "Incomplete live journal line exceeds its size limit",
        ));
    }
    Ok(complete)
}

fn newest_live_part(
    source: &LocalSource,
    format: Format,
    archives: &LiveArchives,
) -> io::Result<u32> {
    // The writer creates contiguous parts and registers archives before removing their plain files.
    let (mut low, mut high) = (1, MAX_PARTS + 1);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        match source.open_sibling(OsStr::new(&format.part_name(middle))) {
            Ok(_) => low = middle,
            Err(_) if archives.get(middle).is_some() => low = middle,
            Err(error) if error.kind() == io::ErrorKind::NotFound => high = middle,
            Err(error) => return Err(error),
        }
    }
    Ok(low)
}

fn live_backfill(
    source: &LocalSource,
    id: &str,
    format: Format,
    archives: &LiveArchives,
) -> io::Result<LivePage> {
    let newest = newest_live_part(source, format, archives)?;
    let oldest = newest.saturating_sub(LIVE_SCAN_PARTS - 1).max(1);
    let mut cursor = None;
    let mut records = Vec::new();
    let mut skipped_lines = 0;
    let mut remaining = LIVE_SCAN_BYTES;
    let mut returned = 0;
    'parts: for number in (oldest..=newest).rev() {
        let Some(part) = LivePart::open(source, id, format, number, archives)? else {
            continue;
        };
        let start = part.body.max(part.length.saturating_sub(remaining));
        let bytes = part.read(start, part.length - start)?;
        remaining -= bytes.len() as u64;
        let complete = complete_prefix(&bytes)?;
        let from = if start == part.body || part.read(start - 1, 1)? == b"\n" {
            0
        } else {
            bytes
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(complete, |index| index + 1)
        };
        let body = &bytes[from..complete];
        if body.is_empty() && start > part.body && cursor.is_some() {
            // The shared suffix budget ended inside the preceding part's final line.
            break;
        }
        let mut lines = body
            .strip_suffix(b"\n")
            .unwrap_or(body)
            .rsplit(|&byte| byte == b'\n');
        let last = lines.next().unwrap_or_default();
        let footer = matches!(live_line(format, last), Ok(Line::Footer));
        if footer && start + complete as u64 != part.length {
            return Err(invalid("Unexpected data after a live journal ending"));
        }
        if cursor.is_none() {
            cursor = Some(part.cursor(id, number, start + complete as u64, footer)?);
        } else if !footer {
            return Err(invalid("An older live journal part is missing its ending"));
        }
        for (index, raw) in std::iter::once(last).chain(lines).enumerate() {
            match live_line(format, raw) {
                Ok(Line::Record(record, _)) if live_kind(&record) => {
                    if records.len() == MAX_PAGE_RECORDS
                        || returned + record.text.len() > MAX_PAGE_BYTES
                    {
                        break 'parts;
                    }
                    returned += record.text.len();
                    records.push(record);
                }
                Ok(Line::Footer) if index != 0 => {
                    return Err(invalid("Unexpected data after a live journal ending"));
                }
                Ok(Line::Record(..) | Line::Footer | Line::Ignore) => {}
                _ => skipped_lines += 1,
            }
        }
        if remaining == 0 {
            break;
        }
    }
    let cursor = cursor.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "Live journal header is not ready",
        )
    })?;
    records.reverse();
    Ok(LivePage {
        records,
        cursor,
        reset: true,
        more: false,
        skipped_lines,
    })
}

fn live_increment(
    source: &LocalSource,
    id: &str,
    archives: &LiveArchives,
    after: &LiveCursor,
) -> io::Result<Option<LivePage>> {
    let format = after.format;
    let Some(mut part) = LivePart::open(source, id, format, after.part, archives)? else {
        return Ok(None);
    };
    if after.identity != part.identity
        || !(part.body..=part.length).contains(&after.offset)
        || after.anchor != part.anchor(after.offset)?
    {
        return Ok(None);
    }
    let mut page = LivePage {
        records: Vec::new(),
        cursor: after.clone(),
        reset: false,
        more: false,
        skipped_lines: 0,
    };
    let mut remaining = LIVE_SCAN_BYTES;
    let mut returned = 0;
    for index in 0..LIVE_SCAN_PARTS {
        let LiveCursor {
            part: number,
            mut offset,
            mut footer,
            ..
        } = page.cursor;
        if footer && offset != part.length {
            return Err(invalid("Unexpected data after a live journal ending"));
        }
        if offset < part.length {
            let length = (part.length - offset)
                .min(MAX_LINE_BYTES + 1)
                .min(remaining);
            let read_end = offset + length;
            let bytes = part.read(offset, length)?;
            remaining -= bytes.len() as u64;
            let complete = complete_prefix(&bytes)?;
            for raw in bytes[..complete].split_inclusive(|&byte| byte == b'\n') {
                if footer {
                    return Err(invalid("Unexpected data after a live journal ending"));
                }
                match live_line(format, raw) {
                    Ok(Line::Record(record, _)) if live_kind(&record) => {
                        if page.records.len() == MAX_PAGE_RECORDS
                            || returned + record.text.len() > MAX_PAGE_BYTES
                        {
                            page.cursor = part.cursor(id, number, offset, false)?;
                            page.more = true;
                            return Ok(Some(page));
                        }
                        returned += record.text.len();
                        page.records.push(record);
                    }
                    Ok(Line::Footer) => footer = true,
                    Ok(Line::Record(..) | Line::Ignore) => {}
                    _ => page.skipped_lines += 1,
                }
                offset += raw.len() as u64;
            }
            if footer && offset != part.length {
                return Err(invalid("Unexpected data after a live journal ending"));
            }
            if offset != page.cursor.offset {
                page.cursor = part.cursor(id, number, offset, footer)?;
            }
            if read_end < part.length {
                page.more = true;
                return Ok(Some(page));
            }
        }
        if !footer || number == MAX_PARTS {
            break;
        }
        if index + 1 == LIVE_SCAN_PARTS {
            page.more = archives.get(number + 1).is_some()
                || source.sibling_exists_or_uncertain(OsStr::new(&format.part_name(number + 1)));
            break;
        }
        let Some(next) = LivePart::open(source, id, format, number + 1, archives)? else {
            break;
        };
        page.cursor = next.cursor(id, number + 1, next.body, false)?;
        part = next;
    }
    Ok(Some(page))
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Compression {
    Plain,
    Zstd,
    Gzip,
}

struct Part {
    number: u32,
    name: String,
    format: Format,
    compression: Compression,
}

impl Part {
    fn parse(name: String) -> Option<Self> {
        let (base, compression) = if let Some(base) = name.strip_suffix(".zst") {
            (base, Compression::Zstd)
        } else if let Some(base) = name.strip_suffix(".gz") {
            (base, Compression::Gzip)
        } else {
            (name.as_str(), Compression::Plain)
        };
        let (stem, extension) = base.rsplit_once('.')?;
        let format = match extension {
            "xml" => Format::Xml,
            "json" => Format::Json,
            "yaml" => Format::Yaml,
            _ => return None,
        };
        let number = match stem.strip_prefix("session-log")? {
            "" => 1,
            tail => {
                let digits = tail.strip_prefix('-')?;
                if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                if digits.is_empty() {
                    return None;
                }
                digits.parse::<u32>().unwrap_or(MAX_PARTS + 1)
            }
        };
        if number == 1 && stem != "session-log" {
            return None;
        }
        Some(Self {
            number,
            name,
            format,
            compression,
        })
    }

    fn decode<'a>(
        &self,
        raw: impl Read + 'a,
        cancelled: impl Fn() -> bool + 'a,
    ) -> Box<dyn Read + 'a> {
        match self.compression {
            Compression::Plain => Box::new(raw),
            Compression::Gzip => Box::new(MultiGzDecoder::new(raw)),
            Compression::Zstd => Box::new(zstandard::Decoder::with_window_limit(
                raw,
                cancelled,
                MAX_JAVA_ZSTD_WINDOW_BYTES,
            )),
        }
    }
}

#[cfg(unix)]
fn part_names(source: &LocalSource) -> io::Result<Vec<OsString>> {
    use rustix::fs::Dir;
    use std::os::unix::ffi::OsStringExt as _;

    let mut names = Vec::new();
    for entry in Dir::read_from(source.parent_file())? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if !matches!(name, b"." | b"..") {
            names.push(OsString::from_vec(name.to_vec()));
            if names.len() > MAX_DIRECTORY_NAMES {
                break;
            }
        }
    }
    Ok(names)
}

#[cfg(windows)]
fn part_names(source: &LocalSource) -> io::Result<Vec<OsString>> {
    source.parent_directory().names(MAX_DIRECTORY_NAMES + 1)
}

fn discover_parts(
    source: &LocalSource,
    declared_parts: u32,
) -> io::Result<(BTreeMap<u32, Part>, CaptureStatus)> {
    let names = part_names(source)?;
    let mut status = CaptureStatus {
        limited: declared_parts > MAX_PARTS || names.len() > MAX_DIRECTORY_NAMES,
        ..CaptureStatus::default()
    };
    let mut parts = BTreeMap::<u32, Part>::new();
    for name in names.into_iter().take(MAX_DIRECTORY_NAMES) {
        let Some(part) = name.into_string().ok().and_then(Part::parse) else {
            continue;
        };
        if part.number > MAX_PARTS {
            status.limited = true;
            continue;
        }
        // Follow the pinned Java reader's format and compression probe order.
        if parts.get(&part.number).is_none_or(|current| {
            (part.format, part.compression) < (current.format, current.compression)
        }) {
            parts.insert(part.number, part);
        }
    }
    let observed = parts.last_key_value().map_or(0, |(&part, _)| part);
    let expected = declared_parts.min(MAX_PARTS).max(observed);
    status.missing_parts = expected as usize - parts.len();
    Ok((parts, status))
}

/// Reads a bounded capture page in file order. Continue with the returned cursor,
/// keeping the same journal, sequence range and literal case-insensitive query.
#[cfg(test)]
pub(crate) fn capture_page(
    directory: &Path,
    after: Option<Cursor>,
    range: Option<RangeInclusive<u64>>,
    query: Option<&str>,
) -> io::Result<CapturePage> {
    let source = open_document(directory)?;
    let xml = document_text(&source)?;
    let document = parse_document(&xml)?;
    let id = document_id(&document)?;
    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let declared_parts = number::<u32>(meta, "logParts")?.unwrap_or(1);
    capture_page_in(&source, id, declared_parts, after, range, query)
}

pub(super) fn capture_page_in(
    source: &LocalSource,
    id: &str,
    declared_parts: u32,
    after: Option<Cursor>,
    range: Option<RangeInclusive<u64>>,
    query: Option<&str>,
) -> io::Result<CapturePage> {
    if query.is_some_and(|query| query.len() > MAX_QUERY_BYTES) {
        return Err(invalid("Session journal query exceeds the size limit"));
    }
    let (parts, status) = discover_parts(source, declared_parts)?;
    let mut scan = Scan {
        range,
        query: query
            .filter(|query| !query.is_empty())
            .map(str::to_lowercase),
        ..Scan::default()
    };
    scan.page.status = status;
    for part in parts.values() {
        let resume = match after {
            Some(cursor) if part.number < cursor.part => continue,
            Some(cursor) if part.number == cursor.part => cursor.line,
            _ => 0,
        };
        match scan.part(source, part, id, resume) {
            Ok(Some(next)) => {
                scan.page.next = Some(next);
                break;
            }
            Ok(None) => {}
            Err(_) => scan.page.status.unreadable_parts += 1,
        }
    }
    Ok(scan.page)
}

pub(super) struct ContentMatch {
    pub found: bool,
    pub partial: bool,
}

/// Searches persisted records in file order without restarting compressed streams.
/// Both raw input and decoded output are bounded by the shared remaining byte budget.
pub(super) fn content_match(
    source: &LocalSource,
    journal_id: &str,
    declared_parts: u32,
    normalized_query: &str,
    budget: &mut u64,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<ContentMatch> {
    check_content_cancel(cancelled)?;
    if normalized_query.is_empty() {
        return Ok(ContentMatch {
            found: false,
            partial: false,
        });
    }
    let (parts, mut status) = discover_parts(source, declared_parts)?;
    let mut found = false;
    for part in parts.values() {
        check_content_cancel(cancelled)?;
        if *budget == 0 {
            status.limited = true;
            break;
        }
        match part.content_match(
            source,
            journal_id,
            normalized_query,
            budget,
            cancelled,
            &mut status,
        ) {
            Ok(true) => {
                found = true;
                break;
            }
            Ok(false) => {}
            Err(_) => status.unreadable_parts += 1,
        }
    }
    check_content_cancel(cancelled)?;
    Ok(ContentMatch {
        found,
        partial: status.is_partial(),
    })
}

impl Part {
    fn content_match(
        &self,
        source: &LocalSource,
        id: &str,
        query: &str,
        budget: &mut u64,
        cancelled: &dyn Fn() -> bool,
        status: &mut CaptureStatus,
    ) -> io::Result<bool> {
        let file = source.open_sibling(OsStr::new(&self.name))?;
        if file.metadata()?.len() > MAX_PART_BYTES {
            status.limited = true;
            return Ok(false);
        }
        let limit = (*budget).min(MAX_PART_BYTES);
        let mut raw = (&file).take(limit);
        let (result, decoded_remaining) = {
            let decoded = self.decode(ContentReader::new(&mut raw, cancelled), cancelled);
            let mut reader = BufReader::new(ContentReader::new(decoded, cancelled).take(limit));
            let result = self.match_records(&mut reader, id, query, cancelled, status);
            (result, reader.get_ref().limit())
        };
        // Decoding can read ahead even on error. Charge the larger of the raw and
        // decoded byte counts, so both remain bounded over a multi-journal search.
        *budget -= limit - raw.limit().min(decoded_remaining);
        if matches!(result, Ok(false)) {
            let size = file.metadata()?.len();
            let raw_limited = raw.limit() == 0 && size > limit;
            let decoded_limited = decoded_remaining == 0 && self.compression != Compression::Plain;
            status.limited |= raw_limited || decoded_limited || size > MAX_PART_BYTES;
        }
        result
    }

    fn match_records(
        &self,
        reader: &mut impl BufRead,
        id: &str,
        query: &str,
        cancelled: &dyn Fn() -> bool,
        status: &mut CaptureStatus,
    ) -> io::Result<bool> {
        let mut bytes = Vec::new();
        let mut envelope = PartEnvelope::default();
        loop {
            check_content_cancel(cancelled)?;
            bytes.clear();
            let n = (&mut *reader)
                .take(MAX_LINE_BYTES + 1)
                .read_until(b'\n', &mut bytes)?;
            if n == 0 {
                envelope.finish(status)?;
                return Ok(false);
            }
            let overlong = n as u64 > MAX_LINE_BYTES;
            let complete = bytes.last() == Some(&b'\n');
            if overlong || !complete {
                status.skipped_lines += 1;
                if overlong && !complete {
                    reader.skip_until(b'\n')?;
                }
                continue;
            }
            match envelope.line(&bytes, self, id, CaptureText::Persisted)? {
                PartLine::Record(record, _)
                    if !record.partial && record.text.to_lowercase().contains(query) =>
                {
                    return Ok(true);
                }
                PartLine::Skipped => status.skipped_lines += 1,
                _ => {}
            }
        }
    }
}

fn check_content_cancel(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

struct ContentReader<'a, R> {
    reader: R,
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a, R> ContentReader<'a, R> {
    fn new(reader: R, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self { reader, cancelled }
    }
}

impl<R: Read> Read for ContentReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        // BufRead retries Interrupted. Leave the read loop before reporting cancellation.
        let result = check_content_cancel(self.cancelled).and_then(|()| self.reader.read(output));
        match result {
            Err(error) if error.kind() == io::ErrorKind::Interrupted && (self.cancelled)() => {
                Err(io::Error::other(error))
            }
            result => result,
        }
    }
}

#[derive(Default)]
struct PartEnvelope {
    header: bool,
    footer: bool,
}

enum PartLine {
    Ignored,
    Skipped,
    Header,
    Preamble,
    Metadata,
    Footer,
    Record(CaptureRecord, Option<String>),
}

impl PartEnvelope {
    fn line(
        &mut self,
        bytes: &[u8],
        part: &Part,
        id: &str,
        text: CaptureText,
    ) -> io::Result<PartLine> {
        let parsed = std::str::from_utf8(bytes)
            .map_err(|_| invalid("Session journal capture line is not UTF-8"))
            .and_then(|line| parse_line(part.format, line.trim(), text));
        let line = match parsed {
            Ok(Line::Header {
                version: 1,
                journal_id,
                part: number,
            }) if !self.header && journal_id == id && number == part.number => {
                self.header = true;
                PartLine::Header
            }
            Ok(Line::Header { .. }) => {
                return Err(invalid("Session journal capture header does not match"));
            }
            Ok(Line::Record(record, file)) if self.header && !self.footer => {
                PartLine::Record(record, file)
            }
            Ok(Line::Footer) if self.header && !self.footer => {
                self.footer = true;
                PartLine::Footer
            }
            Ok(Line::Preamble) if !self.header => PartLine::Preamble,
            Ok(Line::Metadata) if self.header && !self.footer => PartLine::Metadata,
            Ok(Line::Ignore) => PartLine::Ignored,
            _ => PartLine::Skipped,
        };
        Ok(line)
    }

    fn finish(&self, status: &mut CaptureStatus) -> io::Result<()> {
        if !self.header {
            return Err(invalid("Missing session journal capture header"));
        }
        if !self.footer {
            status.unclosed_parts += 1;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Scan {
    range: Option<RangeInclusive<u64>>,
    query: Option<String>,
    page: CapturePage,
    work: u64,
    scanned_lines: u32,
    returned_bytes: usize,
}

impl Scan {
    fn part(
        &mut self,
        source: &LocalSource,
        part: &Part,
        id: &str,
        resume: u64,
    ) -> io::Result<Option<Cursor>> {
        let file = source.open_sibling(OsStr::new(&part.name))?;
        if file.metadata()?.len() > MAX_PART_BYTES {
            self.page.status.limited = true;
            return Ok(None);
        }
        let raw = (&file).take(MAX_PART_BYTES + 1);
        let decoded = part.decode(raw, || false);
        let mut reader = BufReader::new(decoded.take(MAX_PART_BYTES + 1));
        let mut bytes = Vec::new();
        let mut line = 0;
        let mut envelope = PartEnvelope::default();
        loop {
            if line >= resume
                && (self.work >= MAX_SCAN_BYTES
                    || self.scanned_lines >= MAX_SCAN_LINES
                    || self.page.records.len() >= MAX_PAGE_RECORDS)
            {
                return Ok(Some(Cursor {
                    part: part.number,
                    line,
                }));
            }
            bytes.clear();
            let before = reader.get_ref().limit() + reader.buffer().len() as u64;
            let n = (&mut reader)
                .take(MAX_LINE_BYTES + 1)
                .read_until(b'\n', &mut bytes)?;
            if n == 0 {
                if file.metadata()?.len() > MAX_PART_BYTES {
                    self.page.status.limited = true;
                    return Ok(None);
                }
                envelope.finish(&mut self.page.status)?;
                return Ok(None);
            }
            line += 1;
            let overlong = n as u64 > MAX_LINE_BYTES;
            if overlong && bytes.last() != Some(&b'\n') {
                reader.skip_until(b'\n')?;
            }
            let after = reader.get_ref().limit() + reader.buffer().len() as u64;
            if after == 0 {
                self.page.status.limited = true;
                return Ok(None);
            }
            if line > resume {
                self.work += before - after;
                self.scanned_lines += 1;
            }
            if overlong || bytes.last() != Some(&b'\n') {
                if line > resume {
                    self.page.status.skipped_lines += 1;
                }
                continue;
            }
            match envelope.line(&bytes, part, id, CaptureText::Display)? {
                PartLine::Record(record, _) if line > resume && self.matches(&record) => {
                    if self.returned_bytes + record.text.len() > MAX_PAGE_BYTES {
                        return Ok(Some(Cursor {
                            part: part.number,
                            line: line - 1,
                        }));
                    }
                    self.returned_bytes += record.text.len();
                    self.page.records.push(record);
                }
                PartLine::Skipped if line > resume => self.page.status.skipped_lines += 1,
                _ => {}
            }
        }
    }

    fn matches(&self, record: &CaptureRecord) -> bool {
        self.range
            .as_ref()
            .is_none_or(|range| range.contains(&record.sequence))
            && self
                .query
                .as_ref()
                .is_none_or(|query| record.text.to_lowercase().contains(query))
    }
}

enum Line {
    Ignore,
    Preamble,
    Metadata,
    Header {
        version: u32,
        journal_id: String,
        part: u32,
    },
    Footer,
    Record(CaptureRecord, Option<String>),
}

#[derive(Clone, Copy)]
enum CaptureText {
    Display,
    Persisted,
    Bundle,
}

fn parse_line(format: Format, line: &str, text: CaptureText) -> io::Result<Line> {
    if line.is_empty() {
        return Ok(Line::Ignore);
    }
    match format {
        Format::Json => parse_json(line, text),
        Format::Xml => parse_xml_line(line, text),
        Format::Yaml => {
            if let Some(json) = line.strip_prefix("meta: ")
                && let Ok(header @ Line::Header { .. }) = parse_json(json, text)
            {
                return Ok(header);
            }
            if let Some(json) = line.strip_prefix("- ")
                && let Ok(record @ Line::Record(..)) = parse_json(json, text)
            {
                return Ok(record);
            }
            if line == "end: true" {
                return Ok(Line::Footer);
            }
            if line == "entries:" {
                return Ok(Line::Metadata);
            }
            if line == "formatVersion: 1"
                || line
                    .strip_prefix("journalId: ")
                    .is_some_and(|id| serde_json::from_str::<String>(id).is_ok())
                || line
                    .strip_prefix("part: ")
                    .is_some_and(|part| part.parse::<u32>().is_ok())
            {
                return Ok(Line::Preamble);
            }
            Err(invalid("Malformed session journal YAML line"))
        }
    }
}

#[derive(Deserialize)]
struct JsonLine {
    #[serde(rename = "type")]
    variant: Option<String>,
    #[serde(rename = "formatVersion")]
    version: Option<u32>,
    #[serde(rename = "journalId")]
    journal_id: Option<String>,
    part: Option<u32>,
    seq: Option<u64>,
    t: Option<String>,
    k: Option<String>,
    #[serde(default)]
    x: String,
    #[serde(default)]
    redacted: bool,
    #[serde(default)]
    partial: bool,
    repeat: Option<u32>,
    file: Option<String>,
}

fn parse_json(line: &str, text: CaptureText) -> io::Result<Line> {
    let row: JsonLine =
        serde_json::from_str(line).map_err(|_| invalid("Malformed session journal JSON line"))?;
    match row.variant.as_deref() {
        Some("meta") => Ok(Line::Header {
            version: required(row.version)?,
            journal_id: required(row.journal_id)?,
            part: required(row.part)?,
        }),
        Some("end") => Ok(Line::Footer),
        None => Ok(Line::Record(
            CaptureRecord {
                sequence: positive(required(row.seq)?)?,
                timestamp: parse(required(row.t.as_deref())?)?,
                kind: CaptureKind::parse(required(row.k.as_deref())?)?,
                text: capture_text(&row.x, row.file.as_deref(), text)?,
                redacted: row.redacted,
                partial: row.partial,
                repeat: positive(row.repeat.unwrap_or(1))?,
            },
            row.file,
        )),
        _ => Err(invalid("Unknown session journal capture record")),
    }
}

fn parse_xml_line(line: &str, text: CaptureText) -> io::Result<Line> {
    if line.starts_with("<?xml ") && line.ends_with("?>") {
        return Ok(Line::Preamble);
    }
    if line == "</session-log>" {
        return Ok(Line::Footer);
    }
    if line.starts_with("<session-log ") && line.ends_with('>') {
        let completed = format!("{line}</session-log>");
        let document = parse_xml(&completed, 8)?;
        let root = document.root_element();
        if !root.has_tag_name("session-log") || root.first_element_child().is_some() {
            return Err(invalid("Malformed session journal capture XML header"));
        }
        return Ok(Line::Header {
            version: parse(required(root.attribute("formatVersion"))?)?,
            journal_id: required(root.attribute("journalId"))?.into(),
            part: parse(required(root.attribute("part"))?)?,
        });
    }
    let document = parse_xml(line, 8)?;
    let root = document.root_element();
    if root.first_element_child().is_some() {
        return Err(invalid("Unexpected nested session journal capture XML"));
    }
    if root.has_tag_name("meta") {
        return Ok(Line::Metadata);
    }
    if root.tag_name().namespace().is_some() {
        return Err(invalid("Unexpected session journal capture XML namespace"));
    }
    let flag = |name| match root.attribute(name) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        _ => Err(invalid("Invalid session journal capture flag")),
    };
    let file = root.attribute("file");
    Ok(Line::Record(
        CaptureRecord {
            sequence: positive(parse::<u64>(required(root.attribute("seq"))?)?)?,
            timestamp: parse(required(root.attribute("t"))?)?,
            kind: CaptureKind::parse(root.tag_name().name())?,
            text: capture_text(&scalar_text(root)?, file, text)?,
            redacted: flag("redacted")?,
            partial: flag("partial")?,
            repeat: positive(
                root.attribute("repeat")
                    .map(parse::<u32>)
                    .transpose()?
                    .unwrap_or(1),
            )?,
        },
        file.map(str::to_owned),
    ))
}

fn capture_text(value: &str, file: Option<&str>, text: CaptureText) -> io::Result<String> {
    if !matches!(text, CaptureText::Bundle)
        && file.is_some_and(|file| screenshot_reference(file).is_none())
    {
        return Err(invalid(
            "Invalid session journal capture screenshot reference",
        ));
    }
    Ok(match text {
        CaptureText::Display => display_text(if value.is_empty() {
            file.unwrap_or(value)
        } else {
            value
        }),
        CaptureText::Persisted | CaptureText::Bundle => value.to_owned(),
    })
}

fn required<T>(value: Option<T>) -> io::Result<T> {
    value.ok_or_else(|| invalid("Missing session journal capture field"))
}

fn parse<T: std::str::FromStr>(value: &str) -> io::Result<T> {
    value
        .parse()
        .map_err(|_| invalid("Invalid session journal capture field"))
}

fn positive<T: From<u8> + PartialOrd>(value: T) -> io::Result<T> {
    if value > T::from(0) {
        Ok(value)
    } else {
        Err(invalid("Invalid zero session journal capture field"))
    }
}

#[cfg(test)]
mod content_tests;

#[cfg(test)]
mod java_zstd_tests;

#[cfg(test)]
mod live_tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::path::PathBuf;

    use super::*;

    pub(super) const ID: &str = "00000000-0000-4000-8000-000000000001";
    const FOOTER: &[u8] = b"{\"type\":\"end\"}\n";

    pub(super) struct Scratch {
        root: PathBuf,
        pub(super) journal: PathBuf,
    }

    impl Scratch {
        pub(super) fn new() -> Self {
            let mut suffix = [0; 8];
            getrandom::fill(&mut suffix).unwrap();
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-live-journal-{:016x}",
                u64::from_ne_bytes(suffix)
            ));
            let journal = root.join(format!("journal-{ID}"));
            fs::create_dir(&root).unwrap();
            fs::create_dir(&journal).unwrap();
            fs::write(
                journal.join("journal.xml"),
                format!(
                    "<session-journal formatVersion=\"1\" id=\"{ID}\"><meta/></session-journal>"
                ),
            )
            .unwrap();
            Self { root, journal }
        }

        fn part(&self, number: u32) -> PathBuf {
            self.journal.join(Format::Json.part_name(number))
        }

        fn read(&self, after: Option<&LiveCursor>) -> LivePage {
            capture_live_tail(&self.journal, Format::Json, &LiveArchives::default(), after).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn header(part: u32) -> String {
        format!(
            "{{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"{ID}\",\"part\":{part}}}\n"
        )
    }

    pub(super) fn record(sequence: u64, kind: &str, text: &str) -> String {
        let value =
            serde_json::json!({"seq": sequence, "t": "2026-10-01T01:02:03Z", "k": kind, "x": text});
        format!("{value}\n")
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn sequences(page: &LivePage) -> Vec<u64> {
        page.records.iter().map(|record| record.sequence).collect()
    }

    #[test]
    fn live_tail_commits_only_complete_lines_and_retries_a_partial_rotated_header() {
        let fixture = Scratch::new();
        let first = fixture.part(1);
        fs::write(&first, header(1) + &record(1, "out", "before")).unwrap();
        let note = record(2, "note", "split note");
        let boundary = note.len() / 2;
        append(&first, &note.as_bytes()[..boundary]);

        let initial = fixture.read(None);
        assert!(initial.reset);
        assert_eq!(sequences(&initial), [1]);
        let waiting = fixture.read(Some(&initial.cursor));
        assert!(!waiting.reset && !waiting.more);
        assert!(waiting.records.is_empty());
        assert_eq!(waiting.skipped_lines, 0);
        assert_eq!(waiting.cursor.offset, initial.cursor.offset);

        append(&first, &note.as_bytes()[boundary..]);
        append(&first, FOOTER);
        let second = fixture.part(2);
        let next_header = header(2);
        fs::write(&second, &next_header.as_bytes()[..20]).unwrap();
        let rolled = fixture.read(Some(&waiting.cursor));
        assert_eq!(sequences(&rolled), [2]);
        assert_eq!(rolled.records[0].kind, CaptureKind::Note);
        assert_eq!(rolled.cursor.part, 1);
        assert!(rolled.cursor.footer);

        append(&second, &next_header.as_bytes()[20..]);
        append(&second, record(3, "in", "excluded input").as_bytes());
        append(&second, record(4, "out", "after rotation").as_bytes());
        let resumed = fixture.read(Some(&rolled.cursor));
        assert_eq!(sequences(&resumed), [4]);
        assert_eq!(resumed.cursor.part, 2);
        assert!(fixture.read(Some(&resumed.cursor)).records.is_empty());
    }

    #[test]
    fn live_tail_backfills_newest_rows_and_pages_incremental_overflow_without_duplicates() {
        let fixture = Scratch::new();
        let first = fixture.part(1);
        let mut bytes = header(1);
        for sequence in 1..=1000 {
            bytes.push_str(&record(sequence, "out", &"x".repeat(1500)));
        }
        fs::write(&first, bytes).unwrap();
        let initial = fixture.read(None);
        assert_eq!(initial.records.len(), MAX_PAGE_RECORDS);
        assert_eq!(initial.records.first().unwrap().sequence, 745);
        assert_eq!(initial.records.last().unwrap().sequence, 1000);

        let mut added = String::new();
        for sequence in 1001..=1300 {
            added.push_str(&record(sequence, "out", "incremental"));
        }
        append(&first, added.as_bytes());
        let page = fixture.read(Some(&initial.cursor));
        assert!(!page.reset && page.more);
        assert_eq!(sequences(&page), (1001..=1256).collect::<Vec<_>>());
        let rest = fixture.read(Some(&page.cursor));
        assert!(!rest.more);
        assert_eq!(sequences(&rest), (1257..=1300).collect::<Vec<_>>());
    }

    #[test]
    fn live_tail_resets_when_the_committed_prefix_changes() {
        let fixture = Scratch::new();
        let first = fixture.part(1);
        fs::write(&first, header(1) + &record(1, "out", "old line")).unwrap();
        let old = fixture.read(None);
        fs::write(
            &first,
            header(1) + &record(1, "out", "new line") + &record(2, "out", "next line"),
        )
        .unwrap();
        let replacement = fixture.read(Some(&old.cursor));
        assert!(replacement.reset);
        assert_eq!(sequences(&replacement), [1, 2]);
        assert_eq!(replacement.records[0].text, "new line");
    }
}
