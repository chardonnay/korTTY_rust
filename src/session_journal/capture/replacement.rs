use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::Range;

use flate2::write::GzEncoder;
use serde::Deserialize;
use serde_json::value::RawValue;
use zeroize::{Zeroize as _, Zeroizing};

use crate::private_files::{Directory, EntryIdentity, FileStamp};

use super::super::metadata::{require_single_link, xml_character};
use super::super::replace::rule::Rule;
use super::{
    CaptureStatus, CaptureText, Compression, ContentReader, Format, MAX_DIRECTORY_NAMES,
    MAX_LINE_BYTES, MAX_PART_BYTES, MAX_PARTS, PartEnvelope, PartLine, check_content_cancel,
    invalid, parse_xml, zstandard,
};

pub(in crate::session_journal) struct Part {
    format: super::Part,
    identity: EntryIdentity,
}

#[derive(Clone, Copy)]
pub(in crate::session_journal) enum Mode {
    Preview { allow_open: bool },
    Rewrite,
}

/// A replacement must account for every physical part, including unlisted new parts.
pub(in crate::session_journal) fn inventory(
    directory: &Directory,
    declared: u32,
) -> io::Result<Vec<Part>> {
    if declared > MAX_PARTS {
        return Err(invalid("Session journal capture exceeds the part limit"));
    }
    let mut parts = BTreeMap::new();
    let volume = directory.identity()?.0;
    for name in directory.names(MAX_DIRECTORY_NAMES)? {
        const PREFIX: &[u8] = b"session-log";
        if !name
            .as_encoded_bytes()
            .get(..PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
        {
            continue;
        }
        let format = name
            .into_string()
            .ok()
            .and_then(super::Part::parse)
            .filter(|part| part.number <= MAX_PARTS)
            .ok_or_else(|| invalid("Unrecognized session journal capture part"))?;
        let identity = entry_identity(directory, &format.name)?;
        if identity.0 != volume {
            return Err(invalid(
                "Session journal capture part is outside its directory",
            ));
        }
        if parts
            .insert(format.number, Part { format, identity })
            .is_some()
        {
            return Err(invalid("Duplicate session journal capture part"));
        }
    }
    let expected = declared.max(parts.last_key_value().map_or(0, |(&number, _)| number));
    if parts.len() != expected as usize {
        return Err(invalid("Missing session journal capture part"));
    }
    Ok(parts.into_values().collect())
}

impl Part {
    pub(in crate::session_journal) fn name(&self) -> &str {
        &self.format.name
    }

    fn checked_file(&self, directory: &Directory) -> io::Result<File> {
        let file = directory.open_file(self.name())?;
        if file_identity(&file)? != self.identity
            || entry_identity(directory, self.name())? != self.identity
        {
            return Err(invalid("Session journal capture part changed"));
        }
        require_single_link(&file)?;
        Ok(file)
    }

    /// Reads an unchanged, single-link regular part without crossing filesystem boundaries.
    pub(in crate::session_journal) fn read(
        &self,
        directory: &Directory,
        budget: &mut u64,
    ) -> io::Result<(FileStamp, Zeroizing<Vec<u8>>)> {
        let file = self.checked_file(directory)?;
        let stamp = FileStamp::read(&file)?;
        let size = file.metadata()?.len();
        if size > (*budget).min(MAX_PART_BYTES) {
            return Err(part_limit());
        }
        let mut raw = Zeroizing::new(Vec::new());
        (&file).take(size + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 != size {
            return Err(invalid("Session journal capture part changed"));
        }
        stamp.require_unchanged(&file)?;
        stamp.require_unchanged(&self.checked_file(directory)?)?;
        *budget -= size;
        Ok((stamp, raw))
    }

    /// Counts changed physical records. Only a complete changed part produces output bytes.
    pub(in crate::session_journal) fn rewrite(
        &self,
        raw: &[u8],
        journal_id: &str,
        rule: &Rule,
        mode: Mode,
        budget: &mut u64,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<(u64, Option<Zeroizing<Vec<u8>>>)> {
        if raw.len() as u64 > MAX_PART_BYTES {
            return Err(part_limit());
        }
        let limit = (*budget).min(MAX_PART_BYTES);
        let mut remaining = raw;
        let mut scan = Scan::new(&self.format, journal_id, rule, limit);
        let mut rewritten = matches!(mode, Mode::Rewrite).then(|| Zeroizing::new(Vec::new()));
        {
            let decoded = self
                .format
                .decode(ContentReader::new(&mut remaining, cancelled), cancelled);
            let mut reader = BufReader::new(ContentReader::new(decoded, cancelled).take(limit + 1));
            let mut line = Zeroizing::new(Vec::new());
            loop {
                check_content_cancel(cancelled)?;
                line.as_mut_slice().zeroize();
                line.clear();
                let size = (&mut reader)
                    .take(MAX_LINE_BYTES + 1)
                    .read_until(b'\n', &mut line)?;
                if size == 0 {
                    break;
                }
                let replacement = scan.line(&line)?;
                if let Some(output) = &mut rewritten {
                    let bytes = replacement
                        .as_deref()
                        .map_or(line.as_slice(), String::as_bytes);
                    output.extend_from_slice(bytes);
                }
            }
        }
        check_content_cancel(cancelled)?;
        if !remaining.is_empty() {
            return Err(invalid(
                "Unread data after the session journal capture stream",
            ));
        }
        let mut status = CaptureStatus::default();
        scan.envelope.finish(&mut status)?;
        if status.unclosed_parts != 0 && !matches!(mode, Mode::Preview { allow_open: true }) {
            return Err(invalid(
                "Session journal capture part is still open or truncated",
            ));
        }
        let rewritten = rewritten
            .filter(|_| scan.changed != 0)
            .map(|output| encode(self.format.compression, output, cancelled))
            .transpose()?;
        *budget -= scan.read.max(scan.written);
        Ok((scan.changed, rewritten))
    }
}

#[cfg(unix)]
fn entry_identity(directory: &Directory, name: &str) -> io::Result<EntryIdentity> {
    // Linux's no-crossing lookup also excludes same-device bind mounts.
    let (identity, is_directory) = directory.deletion_entry(OsStr::new(name))?;
    if is_directory {
        return Err(invalid("Session journal capture part is a directory"));
    }
    Ok(identity)
}

#[cfg(windows)]
fn entry_identity(directory: &Directory, name: &str) -> io::Result<EntryIdentity> {
    // The read handle rejects reparse points without requiring DELETE access for previews.
    file_identity(&directory.open_file(name)?)
}

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<EntryIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<EntryIdentity> {
    kortty_files::tree::file_identity(file)
}

struct Scan<'a> {
    part: &'a super::Part,
    id: &'a str,
    rule: &'a Rule,
    envelope: PartEnvelope,
    yaml_preamble: u8,
    limit: u64,
    read: u64,
    written: u64,
    changed: u64,
}

impl<'a> Scan<'a> {
    fn new(part: &'a super::Part, id: &'a str, rule: &'a Rule, limit: u64) -> Self {
        Self {
            part,
            id,
            rule,
            envelope: PartEnvelope::default(),
            yaml_preamble: 0,
            limit,
            read: 0,
            written: 0,
            changed: 0,
        }
    }

    fn line(&mut self, bytes: &[u8]) -> io::Result<Option<Zeroizing<String>>> {
        let size = bytes.len() as u64;
        self.read += size;
        if size > MAX_LINE_BYTES || self.read > self.limit {
            return Err(part_limit());
        }
        let replacement =
            match self
                .envelope
                .line(bytes, self.part, self.id, CaptureText::Persisted)?
            {
                PartLine::Skipped => {
                    return Err(invalid("Malformed session journal capture record"));
                }
                PartLine::Record(record, _) => {
                    let text = Zeroizing::new(record.text);
                    if text.is_empty() {
                        None
                    } else {
                        self.rule
                            .rewrite(&text)?
                            .map(|replacement| patch_line(self.part.format, bytes, &replacement))
                            .transpose()?
                    }
                }
                PartLine::Preamble if self.part.format == Format::Yaml => {
                    self.check_yaml_preamble(bytes)?;
                    None
                }
                _ => None,
            };
        self.changed += u64::from(replacement.is_some());
        let size = replacement.as_ref().map_or(size, |line| line.len() as u64);
        self.written += size;
        if size > MAX_LINE_BYTES || self.written > self.limit {
            return Err(part_limit());
        }
        Ok(replacement)
    }

    fn check_yaml_preamble(&mut self, bytes: &[u8]) -> io::Result<()> {
        let line = as_line(bytes)?.trim();
        let field = if line == "formatVersion: 1" {
            1
        } else if line
            .strip_prefix("journalId: ")
            .and_then(|id| serde_json::from_str::<String>(id).ok())
            .is_some_and(|id| id == self.id)
        {
            2
        } else if line
            .strip_prefix("part: ")
            .and_then(|part| part.parse::<u32>().ok())
            == Some(self.part.number)
        {
            4
        } else {
            return Err(invalid(
                "Session journal YAML capture preamble does not match",
            ));
        };
        if self.yaml_preamble & field != 0 {
            return Err(invalid("Duplicate session journal YAML capture preamble"));
        }
        self.yaml_preamble |= field;
        Ok(())
    }
}

fn as_line(bytes: &[u8]) -> io::Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| invalid("Session journal capture line is not UTF-8"))
}

fn patch_line(format: Format, bytes: &[u8], replacement: &str) -> io::Result<Zeroizing<String>> {
    let line = as_line(bytes)?;
    let trimmed = line.trim();
    match format {
        Format::Xml => patch_xml(line, trimmed, replacement),
        Format::Json => patch_json(line, trimmed, replacement),
        Format::Yaml => patch_json(
            line,
            trimmed
                .strip_prefix("- ")
                .ok_or_else(|| invalid("Malformed session journal YAML record"))?,
            replacement,
        ),
    }
}

#[derive(Deserialize)]
struct JsonText<'a> {
    #[serde(borrow)]
    x: &'a RawValue,
}

fn patch_json(line: &str, json: &str, replacement: &str) -> io::Result<Zeroizing<String>> {
    let field = serde_json::from_str::<JsonText<'_>>(json)
        .map_err(|_| invalid("Missing session journal capture text token"))?;
    let range = source_range(line, field.x.get())?;
    let mut output = Zeroizing::new(String::with_capacity(line.len()));
    output.push_str(&line[..range.start]);
    let encoded = Zeroizing::new(serde_json::to_string(replacement).map_err(io::Error::other)?);
    output.push_str(&encoded);
    output.push_str(&line[range.end..]);
    Ok(output)
}

fn source_range(line: &str, source: &str) -> io::Result<Range<usize>> {
    let start = source
        .as_ptr()
        .addr()
        .checked_sub(line.as_ptr().addr())
        .ok_or_else(|| invalid("Invalid session journal capture text range"))?;
    let end = start
        .checked_add(source.len())
        .ok_or_else(|| invalid("Invalid session journal capture text range"))?;
    let range = start..end;
    if line.get(range.clone()) != Some(source) {
        return Err(invalid("Invalid session journal capture text range"));
    }
    Ok(range)
}

fn patch_xml(line: &str, xml: &str, replacement: &str) -> io::Result<Zeroizing<String>> {
    let document = parse_xml(xml, 8)?;
    let root = document.root_element();
    let element = root.range();
    let opening = xml[element.clone()]
        .char_indices()
        .scan(None, |quote, (index, character)| {
            if *quote == Some(character) {
                *quote = None;
            } else if quote.is_none() && matches!(character, '\'' | '"') {
                *quote = Some(character);
            }
            Some((index, character == '>' && quote.is_none()))
        })
        .find_map(|(index, closed)| closed.then_some(element.start + index + 1))
        .ok_or_else(|| invalid("Missing session journal capture opening tag"))?;
    let closing = xml[opening..element.end]
        .rfind("</")
        .map(|index| opening + index)
        .ok_or_else(|| invalid("Missing session journal capture closing tag"))?;
    let range = source_range(line, xml)?;
    let mut output = Zeroizing::new(String::with_capacity(line.len()));
    output.push_str(&line[..range.start + opening]);
    let mut replacement = Some(replacement);
    let mut offset = opening;
    // Text and CDATA can be split around comments or processing instructions. Preserve
    // those nodes and replace the combined scalar in its first original text segment.
    for node in root.children().filter(|node| !node.is_text()) {
        let node = node.range();
        if offset < node.start
            && let Some(text) = replacement.take()
        {
            append_xml_text(&mut output, text)?;
        }
        output.push_str(&xml[node.clone()]);
        offset = node.end;
    }
    if let Some(text) = replacement {
        append_xml_text(&mut output, text)?;
    }
    output.push_str(&line[range.start + closing..]);
    Ok(output)
}

fn append_xml_text(output: &mut String, text: &str) -> io::Result<()> {
    for character in text.chars() {
        output.push_str(match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '\r' => "&#13;",
            '\n' => "&#10;",
            character if xml_character(character) => {
                output.push(character);
                continue;
            }
            _ => return Err(invalid("Replacement contains an invalid XML character")),
        });
    }
    Ok(())
}

fn part_limit() -> io::Error {
    invalid("Session journal capture part exceeds the replacement size limit")
}

fn encode(
    compression: Compression,
    bytes: Zeroizing<Vec<u8>>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut output = Encoded {
        bytes: Zeroizing::new(Vec::new()),
        cancelled,
    };
    match compression {
        Compression::Plain => return Ok(bytes),
        Compression::Gzip => {
            let mut encoder = GzEncoder::new(&mut output, flate2::Compression::default());
            write_chunks(&mut encoder, &bytes, cancelled)?;
            encoder.finish()?;
        }
        Compression::Zstd => {
            let mut encoder = zstandard::Encoder::new(&mut output);
            write_chunks(&mut encoder, &bytes, cancelled)?;
            encoder.finish()?;
        }
    }
    check_content_cancel(cancelled)?;
    Ok(output.bytes)
}

fn write_chunks(
    output: &mut impl Write,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    for chunk in bytes.chunks(256 * 1024) {
        check_content_cancel(cancelled)?;
        output.write_all(chunk)?;
    }
    Ok(())
}

struct Encoded<'a> {
    bytes: Zeroizing<Vec<u8>>,
    cancelled: &'a dyn Fn() -> bool,
}

impl Write for Encoded<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Write::write_all retries Interrupted, so cancellation must terminate that loop.
        check_content_cancel(self.cancelled).map_err(io::Error::other)?;
        if (self.bytes.len() + bytes.len()) as u64 > MAX_PART_BYTES {
            return Err(part_limit());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
