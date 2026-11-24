//! Capture parts for a portable journal bundle. Archive publication is owned by the caller.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read as _, Write};

use jiff::Timestamp;

use crate::private_files::FileStamp;

use super::super::metadata::require_single_link;
use super::{
    CaptureKind, CaptureRecord, CaptureText, Compression, ContentReader, Format, LocalSource,
    MAX_LINE_BYTES, MAX_PART_BYTES, Part, PartEnvelope, PartLine, check_content_cancel,
    discover_parts, display_text, invalid, screenshot_reference,
};

const MAX_SCREENSHOTS: usize = 4096;

/// Sorted, disjoint inclusive sequence ranges referenced by the selected entries.
pub(in crate::session_journal) struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    pub(in crate::session_journal) fn new(
        anchors: impl IntoIterator<Item = (Option<u64>, Option<u64>)>,
    ) -> Self {
        let mut ranges: Vec<_> = anchors
            .into_iter()
            .filter_map(|(first, last)| {
                let first = first.or(last)?;
                let last = last.unwrap_or(first);
                Some((first.min(last), first.max(last)))
            })
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (from, to) in ranges {
            if let Some((_, last)) = merged.last_mut()
                && from <= last.saturating_add(1)
            {
                *last = (*last).max(to);
            } else {
                merged.push((from, to));
            }
        }
        Self(merged)
    }

    pub(in crate::session_journal) fn contains(&self, sequence: u64) -> bool {
        let index = self.0.partition_point(|&(_, end)| end < sequence);
        self.0
            .get(index)
            .is_some_and(|&(start, _)| start <= sequence)
    }
}

pub(in crate::session_journal) struct Selection<'a> {
    pub ranges: &'a Ranges,
    pub matches_time: &'a dyn Fn(Timestamp) -> bool,
}

/// A safe journal-relative path for the visible screenshot copy.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::session_journal) struct ScreenshotRef(String);

impl ScreenshotRef {
    pub(in crate::session_journal) fn parse(reference: &str) -> Option<Self> {
        Self::visible(reference).map(|reference| Self(reference.to_owned()))
    }

    pub(in crate::session_journal) fn visible(reference: &str) -> Option<&str> {
        let reference = screenshot_reference(reference)?;
        let private = reference
            .get(reference.len().saturating_sub(".orig.png".len())..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".orig.png"));
        (!private).then_some(reference)
    }

    pub(in crate::session_journal) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(in crate::session_journal) type MemberData<'a> =
    dyn FnMut(&mut dyn Write) -> io::Result<()> + 'a;
pub(in crate::session_journal) type WriteMember<'a> =
    dyn FnMut(&str, &mut MemberData<'_>) -> io::Result<()> + 'a;

pub(in crate::session_journal) struct Output<'a, 'b> {
    pub members: Option<&'a mut WriteMember<'b>>,
    pub record: &'a mut dyn FnMut(PreviewRecord) -> io::Result<()>,
}

pub(in crate::session_journal) struct PreviewRecord {
    pub record: CaptureRecord,
    pub screenshot: Option<ScreenshotRef>,
}

#[derive(Default)]
pub(in crate::session_journal) struct Exported {
    pub screenshot_refs: BTreeSet<ScreenshotRef>,
    pub command_count: u64,
    pub partial: bool,
}

/// Streams decoded parts. Filtered members are opened only after a scan finds retained rows.
/// The caller stages all output, since a later read or callback can still fail.
pub(in crate::session_journal) fn export(
    source: &LocalSource,
    journal_id: &str,
    declared_parts: u32,
    selection: Option<Selection<'_>>,
    budget: &mut u64,
    cancelled: &dyn Fn() -> bool,
    mut output: Output<'_, '_>,
) -> io::Result<Exported> {
    check_content_cancel(cancelled)?;
    if selection
        .as_ref()
        .is_some_and(|filter| filter.ranges.0.is_empty())
    {
        return Ok(Exported::default());
    }
    let (parts, status) = discover_parts(source, declared_parts, cancelled)?;
    if status.limited {
        return Err(invalid(
            "Journal capture discovery exceeds the bundle limit",
        ));
    }
    let mut exported = Exported {
        partial: status.missing_parts != 0,
        ..Exported::default()
    };
    for part in parts.values() {
        check_content_cancel(cancelled)?;
        let scan = PartScan {
            part,
            journal_id,
            selection: selection.as_ref(),
            cancelled,
        };
        let name = match part.compression {
            Compression::Plain => part.name.as_str(),
            Compression::Zstd => part.name.trim_end_matches(".zst"),
            Compression::Gzip => part.name.trim_end_matches(".gz"),
        };
        let limit = (*budget).min(MAX_PART_BYTES);
        let mut preview = |record, reference: Option<&str>| {
            PartScan::record(&mut exported, output.record, record, reference)
        };
        let read = match (selection.is_some(), output.members.as_deref_mut()) {
            (false, Some(member)) => {
                let mut scanned = None;
                member(name, &mut |target| {
                    scanned = Some(scan.read(source, limit, None, target, &mut preview)?);
                    Ok(())
                })?;
                scanned.ok_or_else(|| invalid("Journal capture writer skipped a member"))?
            }
            (true, Some(member)) => {
                let reviewed = scan.read(source, limit, None, &mut io::sink(), &mut preview)?;
                if reviewed.kept {
                    member(name, &mut |target| {
                        scan.read(source, limit, Some(&reviewed.stamp), target, &mut |_, _| {
                            Ok(())
                        })?;
                        Ok(())
                    })?;
                }
                reviewed
            }
            (_, None) => scan.read(source, limit, None, &mut io::sink(), &mut preview)?,
        };
        exported.partial |= read.partial;
        *budget -= read.bytes;
    }
    check_content_cancel(cancelled)?;
    Ok(exported)
}

struct ReadPart {
    stamp: FileStamp,
    bytes: u64,
    kept: bool,
    partial: bool,
}

#[derive(Default)]
struct PartResult {
    kept: bool,
    partial: bool,
}

struct PartScan<'a> {
    part: &'a Part,
    journal_id: &'a str,
    selection: Option<&'a Selection<'a>>,
    cancelled: &'a dyn Fn() -> bool,
}

impl PartScan<'_> {
    fn read(
        &self,
        source: &LocalSource,
        limit: u64,
        reviewed: Option<&FileStamp>,
        output: &mut dyn Write,
        preview: &mut dyn FnMut(CaptureRecord, Option<&str>) -> io::Result<()>,
    ) -> io::Result<ReadPart> {
        check_content_cancel(self.cancelled)?;
        let name = OsStr::new(&self.part.name);
        let file = source.open_sibling(name)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        if reviewed.is_some_and(|reviewed| *reviewed != stamp) {
            return Err(invalid("Journal capture part changed during export"));
        }
        let size = file.metadata()?.len();
        if size > limit {
            return Err(invalid(
                "Journal capture part exceeds the bundle byte limit",
            ));
        }
        let mut raw = (&file).take(size + 1);
        let (decoded_size, result) = {
            let decoded = self
                .part
                .decode(ContentReader::new(&mut raw, self.cancelled), self.cancelled);
            let mut reader =
                BufReader::new(ContentReader::new(decoded, self.cancelled).take(limit + 1));
            let result = self.lines(&mut reader, output, preview);
            check_content_cancel(self.cancelled)?;
            (limit + 1 - reader.get_ref().limit(), result?)
        };
        if decoded_size > limit {
            return Err(invalid(
                "Decoded journal capture part exceeds the bundle byte limit",
            ));
        }
        if raw.limit() != 1 {
            return Err(invalid(
                "Journal capture part changed or was incompletely decoded",
            ));
        }
        stamp.require_unchanged(&file)?;
        require_single_link(&file)?;
        let current = source.open_sibling(name)?;
        stamp.require_unchanged(&current)?;
        require_single_link(&current)?;
        Ok(ReadPart {
            stamp,
            bytes: size.max(decoded_size),
            kept: result.kept,
            partial: result.partial,
        })
    }

    fn lines(
        &self,
        reader: &mut impl BufRead,
        output: &mut dyn Write,
        preview: &mut dyn FnMut(CaptureRecord, Option<&str>) -> io::Result<()>,
    ) -> io::Result<PartResult> {
        let mut envelope = PartEnvelope::default();
        let mut line = Vec::new();
        let mut result = PartResult::default();
        let mut written = 0u64;
        let mut newline = true;
        let mut body_started = false;
        let mut yaml_entries = false;
        loop {
            check_content_cancel(self.cancelled)?;
            if read_line(reader, &mut line)? == 0 {
                break;
            }
            if self.selection.is_none() {
                output.write_all(&line)?;
            }
            if line.len() as u64 > MAX_LINE_BYTES {
                if self.selection.is_some() {
                    return Err(invalid(
                        "Journal capture line exceeds the bundle line limit",
                    ));
                }
                result.partial = true;
                while line.last() != Some(&b'\n') && read_line(reader, &mut line)? != 0 {
                    check_content_cancel(self.cancelled)?;
                    output.write_all(&line)?;
                }
                continue;
            }
            let parsed = envelope.line(&line, self.part, self.journal_id, CaptureText::Bundle)?;
            let retain = match parsed {
                PartLine::Record(record, reference) => {
                    body_started = true;
                    if self.selection.is_some() && self.part.format == Format::Yaml && !yaml_entries
                    {
                        return Err(invalid(
                            "Journal capture YAML is missing its entries header",
                        ));
                    }
                    let retain = self.retains(&record);
                    if retain {
                        result.kept = true;
                        preview(record, reference.as_deref())?;
                    }
                    retain
                }
                PartLine::Metadata if !body_started && !yaml_entries => {
                    yaml_entries = self.part.format == Format::Yaml;
                    true
                }
                PartLine::Preamble | PartLine::Header => true,
                PartLine::Skipped | PartLine::Metadata => {
                    result.partial = true;
                    false
                }
                _ => false,
            };
            if self.selection.is_some() && retain {
                output.write_all(&line)?;
                written += line.len() as u64;
                newline = line.last() == Some(&b'\n');
            }
        }
        result.partial |= !envelope.header || !envelope.footer;
        if self.selection.is_some() && result.kept {
            let footer: &[u8] = match self.part.format {
                Format::Xml => b"</session-log>\n",
                Format::Json => b"{\"type\":\"end\"}\n",
                Format::Yaml => b"end: true\n",
            };
            if written + u64::from(!newline) + footer.len() as u64 > MAX_PART_BYTES {
                return Err(invalid(
                    "Rewritten journal capture part exceeds the bundle byte limit",
                ));
            }
            if !newline {
                output.write_all(b"\n")?;
            }
            output.write_all(footer)?;
        }
        Ok(result)
    }

    fn retains(&self, record: &CaptureRecord) -> bool {
        self.selection.is_none_or(|selection| {
            selection.ranges.contains(record.sequence) && (selection.matches_time)(record.timestamp)
        })
    }

    fn record(
        exported: &mut Exported,
        emit: &mut dyn FnMut(PreviewRecord) -> io::Result<()>,
        mut record: CaptureRecord,
        reference: Option<&str>,
    ) -> io::Result<()> {
        if record.kind == CaptureKind::Input && !record.partial {
            exported.command_count += 1;
        }
        let mut screenshot = reference.and_then(ScreenshotRef::parse);
        if let Some(reference) = &screenshot {
            exported.screenshot_refs.insert(reference.clone());
            if exported.screenshot_refs.len() > MAX_SCREENSHOTS {
                return Err(invalid("Journal capture references too many screenshots"));
            }
        }
        if record.redacted {
            record.text.clear();
            screenshot = None;
        } else {
            record.text = display_text(&record.text);
        }
        emit(PreviewRecord { record, screenshot })
    }
}

fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<usize> {
    line.clear();
    reader.take(MAX_LINE_BYTES + 1).read_until(b'\n', line)
}

#[cfg(test)]
mod tests;
