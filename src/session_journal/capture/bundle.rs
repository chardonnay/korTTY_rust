//! Capture parts for a portable journal bundle. Archive publication is owned by the caller.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{self, Read as _};

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

pub(in crate::session_journal) enum Output<'a> {
    Part(PartOutput<'a>),
    Record(PreviewRecord),
}

pub(in crate::session_journal) struct PartOutput<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
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

/// Emits each bounded decoded part once and releases it before opening the next.
/// The caller stages all output, since a later part or callback can still fail.
pub(in crate::session_journal) fn export(
    source: &LocalSource,
    journal_id: &str,
    declared_parts: u32,
    selection: Option<Selection<'_>>,
    budget: &mut u64,
    cancelled: &dyn Fn() -> bool,
    mut emit: impl FnMut(Output<'_>) -> io::Result<()>,
) -> io::Result<Exported> {
    check_content_cancel(cancelled)?;
    if selection
        .as_ref()
        .is_some_and(|filter| filter.ranges.0.is_empty())
    {
        return Ok(Exported::default());
    }
    let (parts, status) = discover_parts(source, declared_parts)?;
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
        let mut bytes = read_part(source, part, budget, cancelled)?;
        let scan = PartScan {
            part,
            journal_id,
            selection: selection.as_ref(),
            cancelled,
        };
        let kept = scan.run(&mut bytes, &mut exported, &mut emit)?;
        if selection.is_none() || kept {
            let name = match part.compression {
                Compression::Plain => part.name.as_str(),
                Compression::Zstd => part.name.trim_end_matches(".zst"),
                Compression::Gzip => part.name.trim_end_matches(".gz"),
            };
            check_content_cancel(cancelled)?;
            emit(Output::Part(PartOutput {
                name,
                bytes: &bytes,
            }))?;
        }
    }
    check_content_cancel(cancelled)?;
    Ok(exported)
}

fn read_part(
    source: &LocalSource,
    part: &Part,
    budget: &mut u64,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Vec<u8>> {
    let file = source.open_sibling(OsStr::new(&part.name))?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let size = file.metadata()?.len();
    let limit = (*budget).min(MAX_PART_BYTES);
    if size > limit {
        return Err(invalid(
            "Journal capture part exceeds the bundle byte limit",
        ));
    }
    let mut bytes = Vec::new();
    let mut raw = (&file).take(size + 1);
    {
        let decoded = part.decode(ContentReader::new(&mut raw, cancelled), cancelled);
        let mut bounded = ContentReader::new(decoded, cancelled).take(limit);
        let result = bounded.read_to_end(&mut bytes);
        check_content_cancel(cancelled)?;
        result?;
        if bounded.limit() == 0 {
            let result = bounded.get_mut().read(&mut [0]);
            check_content_cancel(cancelled)?;
            if result? != 0 {
                return Err(invalid(
                    "Decoded journal capture part exceeds the bundle byte limit",
                ));
            }
        }
    }
    if raw.limit() != 1 {
        return Err(invalid(
            "Journal capture part changed or was incompletely decoded",
        ));
    }
    stamp.require_unchanged(&file)?;
    require_single_link(&file)?;
    *budget -= size.max(bytes.len() as u64);
    Ok(bytes)
}

struct PartScan<'a> {
    part: &'a Part,
    journal_id: &'a str,
    selection: Option<&'a Selection<'a>>,
    cancelled: &'a dyn Fn() -> bool,
}

impl PartScan<'_> {
    fn run(
        &self,
        bytes: &mut Vec<u8>,
        exported: &mut Exported,
        emit: &mut impl FnMut(Output<'_>) -> io::Result<()>,
    ) -> io::Result<bool> {
        let mut envelope = PartEnvelope::default();
        let mut offset = 0;
        let mut written = 0;
        let mut kept = false;
        let mut body_started = false;
        let mut yaml_entries = false;
        while offset < bytes.len() {
            check_content_cancel(self.cancelled)?;
            let end = bytes[offset..]
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(bytes.len(), |length| offset + length + 1);
            let line = &bytes[offset..end];
            if line.len() as u64 > MAX_LINE_BYTES {
                if self.selection.is_some() {
                    return Err(invalid(
                        "Journal capture line exceeds the bundle line limit",
                    ));
                }
                exported.partial = true;
                offset = end;
                continue;
            }
            let parsed = envelope.line(line, self.part, self.journal_id, CaptureText::Bundle)?;
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
                        kept = true;
                        Self::record(exported, emit, record, reference.as_deref())?;
                    }
                    retain
                }
                PartLine::Metadata if !body_started && !yaml_entries => {
                    yaml_entries = self.part.format == Format::Yaml;
                    true
                }
                PartLine::Preamble | PartLine::Header => true,
                PartLine::Skipped | PartLine::Metadata => {
                    exported.partial = true;
                    false
                }
                _ => false,
            };
            if self.selection.is_some() && retain {
                bytes.copy_within(offset..end, written);
                written += end - offset;
            }
            offset = end;
        }
        exported.partial |= !envelope.header || !envelope.footer;
        if self.selection.is_some() && kept {
            bytes.truncate(written);
            if bytes.last() != Some(&b'\n') {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(match self.part.format {
                Format::Xml => b"</session-log>\n",
                Format::Json => b"{\"type\":\"end\"}\n",
                Format::Yaml => b"end: true\n",
            });
            if bytes.len() as u64 > MAX_PART_BYTES {
                return Err(invalid(
                    "Rewritten journal capture part exceeds the bundle byte limit",
                ));
            }
        }
        Ok(kept)
    }

    fn retains(&self, record: &CaptureRecord) -> bool {
        self.selection.is_none_or(|selection| {
            selection.ranges.contains(record.sequence) && (selection.matches_time)(record.timestamp)
        })
    }

    fn record(
        exported: &mut Exported,
        emit: &mut impl FnMut(Output<'_>) -> io::Result<()>,
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
        emit(Output::Record(PreviewRecord { record, screenshot }))
    }
}

#[cfg(test)]
mod tests;
