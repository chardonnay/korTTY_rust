use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use bzip2::read::MultiBzDecoder;
use flate2::read::MultiGzDecoder;

use crate::sftp_window::StagedDownload;

use super::catalog::{Compression, Entry, Format, check_cancel, check_path, invalid};
use super::range::Range;

const MAX_PLAIN: u64 = 512 * 1024 * 1024;
const MAX_PACKED_VIEW: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT: u64 = 512 * 1024 * 1024;
const MAX_ENTRY: usize = 16 * 1024 * 1024;
const MAX_PREAMBLE: u64 = 64 * 1024;
const MAX_LINES: usize = 2 * 1024 * 1024;
const PREVIEW_BYTES: usize = 32 * 1024;
const PREVIEW_CHARS: usize = 8192;
pub const PAGE_LINES: usize = 200;
const META: &str = "# kortty-meta ";
const CONTEXT: &str = "@kortty-context ";
const CONTEXT_VERSION: &str = "kortty-context-v1";

fn decoded<'a>(
    input: impl Read + 'a,
    compression: Compression,
    cancelled: &'a impl Fn() -> bool,
) -> Box<dyn Read + 'a> {
    match compression {
        Compression::None => Box::new(input),
        Compression::Gzip => Box::new(MultiGzDecoder::new(input)),
        Compression::Bzip2 => Box::new(MultiBzDecoder::new(input)),
        Compression::Zstd => Box::new(super::zstandard::Decoder::new(input, cancelled)),
    }
}

pub(super) fn preamble(
    input: File,
    format: Format,
    compression: Compression,
    cancelled: &impl Fn() -> bool,
) -> io::Result<(String, String)> {
    let input = decoded(input.take(128 * 1024), compression, cancelled);
    let mut input = BufReader::new(input.take(MAX_PREAMBLE));
    let mut fields = Fields::new();
    for _ in 0..12 {
        let Some((line, _)) = physical_line(&mut input, MAX_PREAMBLE as usize, false, cancelled)?
        else {
            break;
        };
        let line = std::str::from_utf8(&line).map_err(io::Error::other)?;
        if is_entry(line, format) {
            break;
        }
        read_metadata(line, format, &mut fields)?;
        if !fields.is_empty() && format != Format::Json {
            break;
        }
    }
    let trimmed = |value: Option<String>| {
        value
            .unwrap_or_default()
            .chars()
            .take(512)
            .map(|char| if char.is_control() { '�' } else { char })
            .collect()
    };
    Ok((
        trimmed(fields.remove("connection")),
        trimmed(fields.remove("host")),
    ))
}

enum Image {
    Plain(Mutex<File>),
    Packed(Vec<u8>),
}

pub struct Document {
    image: Image,
    offsets: Vec<u64>,
    length: u64,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogDocument")
            .field("lines", &self.offsets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Page {
    pub first: usize,
    pub lines: Vec<String>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogPage")
            .field("first", &self.first)
            .field("count", &self.lines.len())
            .finish()
    }
}

impl Document {
    pub fn open(entry: &Entry, cancelled: impl Fn() -> bool) -> io::Result<Self> {
        let source = entry.read()?;
        let file = source.file().try_clone()?;
        let length = source.size();
        if length > MAX_PLAIN {
            return Err(invalid("The logfile exceeds the 512 MiB input limit"));
        }
        if entry.compression() == Compression::None {
            let offsets = index(BufReader::new((&file).take(length)), length, &cancelled)?;
            if !entry.live {
                source.verify()?;
            }
            Ok(Self {
                image: Image::Plain(Mutex::new(file)),
                offsets,
                length,
            })
        } else {
            let mut bytes = Vec::new();
            copy_bounded(
                decoded(file.take(length), entry.compression(), &cancelled),
                &mut bytes,
                MAX_PACKED_VIEW,
                &cancelled,
            )?;
            let length = bytes.len() as u64;
            let offsets = index(Cursor::new(&bytes), length, &cancelled)?;
            if !entry.live {
                source.verify()?;
            }
            Ok(Self {
                image: Image::Packed(bytes),
                offsets,
                length,
            })
        }
    }

    pub fn lines(&self) -> usize {
        self.offsets.len()
    }

    pub fn page(&self, first: usize, cancelled: impl Fn() -> bool) -> io::Result<Page> {
        let first = first.min(self.lines().saturating_sub(1));
        let mut lines = Vec::new();
        if first < self.lines() {
            self.at(first, |input| {
                for _ in 0..PAGE_LINES {
                    check_cancel(&cancelled)?;
                    let Some((bytes, clipped)) =
                        physical_line(input, PREVIEW_BYTES, true, &cancelled)?
                    else {
                        break;
                    };
                    lines.push(display(&bytes, clipped));
                }
                Ok(())
            })?;
        }
        Ok(Page { first, lines })
    }

    pub fn search(
        &self,
        query: &str,
        after: Option<usize>,
        backwards: bool,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Option<usize>> {
        let query = query.trim().to_lowercase();
        let count = self.lines();
        if count == 0 || query.is_empty() {
            return Ok(None);
        }
        let boundary = after.filter(|line| *line < count);
        if backwards {
            let split = boundary.unwrap_or(count);
            let ranges = [(0, split), (split, count)];
            for (from, to) in ranges {
                for first in (from..to).rev().step_by(PAGE_LINES) {
                    let start = (first + 1).saturating_sub(PAGE_LINES).max(from);
                    let page = self.page(start, &cancelled)?;
                    for (index, line) in page.lines.iter().enumerate().take(first + 1 - start).rev()
                    {
                        if line.to_lowercase().contains(&query) {
                            return Ok(Some(start + index));
                        }
                    }
                }
            }
        } else {
            let split = boundary.map_or(0, |line| line + 1);
            for (from, to) in [(split, count), (0, split)] {
                for start in (from..to).step_by(PAGE_LINES) {
                    let page = self.page(start, &cancelled)?;
                    for (index, line) in page.lines.iter().enumerate().take(to - start) {
                        if line.to_lowercase().contains(&query) {
                            return Ok(Some(start + index));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn at<T>(
        &self,
        row: usize,
        read: impl FnOnce(&mut dyn BufRead) -> io::Result<T>,
    ) -> io::Result<T> {
        let offset = self.offsets[row];
        match &self.image {
            Image::Plain(file) => {
                let mut file = file.lock().unwrap_or_else(|error| error.into_inner());
                if file.metadata()?.len() < self.length {
                    return Err(invalid("The logfile shortened. Refresh the viewer."));
                }
                file.seek(SeekFrom::Start(offset))?;
                read(&mut BufReader::new((&*file).take(self.length - offset)))
            }
            Image::Packed(bytes) => read(&mut Cursor::new(&bytes[offset as usize..])),
        }
    }
}

fn index(
    mut input: impl BufRead,
    length: u64,
    cancelled: &impl Fn() -> bool,
) -> io::Result<Vec<u64>> {
    let mut offsets = Vec::new();
    if length == 0 {
        return Ok(offsets);
    }
    offsets.push(0);
    let mut position = 0_u64;
    loop {
        check_cancel(cancelled)?;
        let bytes = input.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        for (index, byte) in bytes.iter().enumerate() {
            let next = position + index as u64 + 1;
            if *byte == b'\n' && next < length {
                if offsets.len() == MAX_LINES {
                    return Err(invalid(
                        "The logfile exceeds the two-million-line viewer limit",
                    ));
                }
                offsets.push(next);
            }
        }
        let consumed = bytes.len();
        position += consumed as u64;
        input.consume(consumed);
    }
    if position != length {
        return Err(invalid("The logfile shortened while reading the snapshot"));
    }
    Ok(offsets)
}

fn display(bytes: &[u8], clipped: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars();
    let mut display: String = chars
        .by_ref()
        .take(PREVIEW_CHARS)
        .map(|char| match char {
            '\t' => ' ',
            char if char.is_control() => '�',
            char => char,
        })
        .collect();
    if clipped || chars.next().is_some() {
        display.push('…');
    }
    display
}

fn physical_line(
    input: &mut dyn BufRead,
    limit: usize,
    clip: bool,
    cancelled: &impl Fn() -> bool,
) -> io::Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::new();
    let mut clipped = false;
    loop {
        check_cancel(cancelled)?;
        let bytes = input.fill_buf()?;
        if bytes.is_empty() {
            return Ok((!line.is_empty() || clipped).then_some((line, clipped)));
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        let retained = count.min(limit - line.len());
        line.extend_from_slice(&bytes[..retained]);
        if retained != count {
            if !clip {
                return Err(invalid("A logfile entry exceeds the conversion limit"));
            }
            clipped = true;
        }
        input.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            if !clipped && line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some((line, clipped)));
        }
    }
}

fn copy_bounded(
    mut source: impl Read,
    mut output: impl Write,
    limit: u64,
    cancelled: &impl Fn() -> bool,
) -> io::Result<u64> {
    let mut chunk = [0; 32 * 1024];
    let mut count = 0;
    loop {
        check_cancel(cancelled)?;
        let read = source.read(&mut chunk)?;
        if read == 0 {
            return Ok(count);
        }
        count += read as u64;
        if count > limit {
            return Err(invalid("The decoded logfile exceeds its size limit"));
        }
        output.write_all(&chunk[..read])?;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportFormat {
    Unchanged,
    Convert(Format),
}

impl ExportFormat {
    pub const ALL: [Self; 4] = [
        Self::Unchanged,
        Self::Convert(Format::Text),
        Self::Convert(Format::Xml),
        Self::Convert(Format::Json),
    ];
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unchanged => "Unchanged",
            Self::Convert(format) => format.label(),
        })
    }
}

#[cfg(test)]
pub fn export(
    entry: &Entry,
    target: &Path,
    format: ExportFormat,
    compression: Compression,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    export_range(
        entry,
        target,
        format,
        compression,
        Range::default(),
        cancelled,
    )
}

pub(super) fn export_range(
    entry: &Entry,
    target: &Path,
    format: ExportFormat,
    compression: Compression,
    range: Range,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    check_path(target)?;
    check_cancel(&cancelled)?;
    if target == entry.path {
        return Err(invalid("Choose a new file for the logfile export"));
    }
    let source = entry.require_closed()?;
    if source.size() > MAX_PLAIN {
        return Err(invalid("The logfile exceeds the 512 MiB input limit"));
    }
    let reader = decoded(
        source.file().try_clone()?.take(source.size()),
        entry.compression(),
        &cancelled,
    );
    let staging = StagedDownload::create(target)?;
    let file = staging.writer()?;
    let write = |output: &mut dyn Write| -> io::Result<()> {
        let mut bounded = BoundedWriter {
            output,
            remaining: MAX_OUTPUT,
        };
        match format {
            ExportFormat::Unchanged if range.is_open() => {
                copy_bounded(reader, &mut bounded, MAX_PLAIN, &cancelled)?;
            }
            format => {
                let output_format = match format {
                    ExportFormat::Unchanged => entry.format(),
                    ExportFormat::Convert(format) => format,
                };
                let input = BufReader::new(BoundedReader {
                    input: reader,
                    remaining: MAX_PLAIN,
                });
                convert(
                    input,
                    &mut bounded,
                    entry.format(),
                    output_format,
                    entry.slug(),
                    range,
                    &cancelled,
                )?;
            }
        }
        Ok(())
    };
    match compression {
        Compression::None => write(&mut { file })?,
        Compression::Gzip => {
            let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            write(&mut gzip)?;
            gzip.finish()?;
        }
        Compression::Bzip2 => {
            let mut bzip = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
            write(&mut bzip)?;
            bzip.finish()?;
        }
        Compression::Zstd => {
            let mut zstd = super::zstandard::Encoder::new(file);
            write(&mut zstd)?;
            zstd.finish()?;
        }
    }
    source.verify()?;
    entry.require_closed()?;
    staging.publish(cancelled)
}

struct BoundedReader<R> {
    input: R,
    remaining: u64,
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let limit = buffer.len().min(self.remaining.saturating_add(1) as usize);
        let read = self.input.read(&mut buffer[..limit])?;
        self.remaining = self
            .remaining
            .checked_sub(read as u64)
            .ok_or_else(|| invalid("The decoded logfile exceeds the conversion limit"))?;
        Ok(read)
    }
}

struct BoundedWriter<W> {
    output: W,
    remaining: u64,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(invalid("The converted logfile exceeds the size limit"));
        }
        let written = self.output.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

pub(crate) type Fields = BTreeMap<String, String>;

fn convert(
    mut input: impl BufRead,
    out: &mut dyn Write,
    source: Format,
    target: Format,
    fallback: &str,
    range: Range,
    cancelled: &impl Fn() -> bool,
) -> io::Result<()> {
    let mut metadata = Fields::from([("connection".into(), fallback.into())]);
    let mut current: Option<String> = None;
    let mut header = false;
    let mut closed = source == Format::Text;
    let mut json_array_closed = false;
    let mut source_header = false;
    let mut count = 0;
    while let Some((bytes, _)) = physical_line(&mut input, MAX_ENTRY, false, cancelled)? {
        let line = String::from_utf8(bytes)
            .map_err(|_| invalid("Logfile conversion requires valid UTF-8"))?;
        let trimmed = line.trim();
        if source == Format::Xml
            && (trimmed.starts_with("<!DOCTYPE") || trimmed.starts_with("<!ENTITY"))
        {
            return Err(invalid(
                "XML declarations of entities or DTDs are forbidden in logfiles",
            ));
        }
        let entry = is_entry(&line, source);
        let footer = match source {
            Format::Text => false,
            Format::Xml => trimmed == "</terminal-log>",
            Format::Json => trimmed == "]" || (trimmed == "}" && json_array_closed),
        };
        if entry || footer {
            if source != Format::Text
                && (!source_header || closed || (json_array_closed && trimmed != "}"))
            {
                return Err(invalid("Unexpected entry or footer in structured logfile"));
            }
            if !header {
                write_header(out, target, &metadata)?;
                header = true;
            }
            if let Some(raw) = current.take() {
                write_candidate(
                    out,
                    target,
                    &raw,
                    source,
                    metadata
                        .get("contextEncoding")
                        .is_some_and(|encoding| encoding == CONTEXT_VERSION),
                    range,
                    &mut count,
                )?;
            }
            if entry {
                current = Some(line);
            } else if source == Format::Json && trimmed == "]" {
                json_array_closed = true;
            } else {
                closed = true;
            }
        } else if let Some(raw) = &mut current {
            if raw.len().saturating_add(line.len() + 1) > MAX_ENTRY {
                return Err(invalid(
                    "A logfile entry exceeds the 16 MiB conversion limit",
                ));
            }
            raw.push('\n');
            raw.push_str(&line);
        } else if !header {
            if source == Format::Xml && trimmed.starts_with("<terminal-log ")
                || source == Format::Json && trimmed == "\"entries\": ["
            {
                source_header = true;
            }
            read_metadata(&line, source, &mut metadata)?;
        } else if !trimmed.is_empty() {
            return Err(invalid("Unexpected content after logfile footer"));
        }
    }
    if !closed {
        return Err(invalid(
            "The structured logfile does not have a complete footer",
        ));
    }
    if !header {
        write_header(out, target, &metadata)?;
    }
    if let Some(raw) = current {
        write_candidate(
            out,
            target,
            &raw,
            source,
            metadata
                .get("contextEncoding")
                .is_some_and(|encoding| encoding == CONTEXT_VERSION),
            range,
            &mut count,
        )?;
    }
    if !range.is_open() && count == 0 {
        return Err(invalid("No logfile entries match the selected time range"));
    }
    match target {
        Format::Text => {}
        Format::Xml => out.write_all(b"</terminal-log>\n")?,
        Format::Json => {
            if count > 0 {
                out.write_all(b"\n")?;
            }
            out.write_all(b"  ]\n}\n")?;
        }
    }
    out.flush()
}

fn write_candidate(
    out: &mut dyn Write,
    target: Format,
    raw: &str,
    source: Format,
    context: bool,
    range: Range,
    count: &mut usize,
) -> io::Result<()> {
    let fields = parse_entry(raw, source, context)?;
    if range.is_open() || range.contains(fields["timestamp"].parse().map_err(io::Error::other)?) {
        write_entry(out, target, &fields, *count)?;
        *count += 1;
    }
    Ok(())
}

fn is_entry(line: &str, format: Format) -> bool {
    match format {
        Format::Text => plain_prefix(line).is_some(),
        Format::Xml => line.trim_start().starts_with("<entry "),
        Format::Json => line.trim_start().starts_with("{\"timestamp\""),
    }
}

fn plain_prefix(text: &str) -> Option<(&str, &str)> {
    let timestamp = text.get(1..24)?;
    if !text.starts_with('[')
        || text.as_bytes().get(24) != Some(&b']')
        || !timestamp
            .as_bytes()
            .get(19)
            .is_some_and(|byte| *byte == b'.')
    {
        return None;
    }
    let remainder = text.get(25..)?;
    Some((timestamp, remainder.strip_prefix(' ').unwrap_or(remainder)))
}

fn parse_entry(raw: &str, source: Format, context: bool) -> io::Result<Fields> {
    let fields = match source {
        Format::Json => json_fields(raw.trim().trim_end_matches(','))?,
        Format::Xml => xml_fields(raw.trim(), "entry")?,
        Format::Text => {
            let (timestamp, mut line) =
                plain_prefix(raw).ok_or_else(|| invalid("Invalid plain-text log entry"))?;
            let mut fields = Fields::new();
            if context
                && let Some((fields_text, content)) = line
                    .strip_prefix(CONTEXT)
                    .and_then(|rest| rest.split_once('\t'))
            {
                fields = json_fields(fields_text)?;
                line = content;
            }
            fields.insert("timestamp".into(), timestamp.into());
            fields.insert("line".into(), line.into());
            fields
        }
    };
    if fields
        .get("timestamp")
        .is_none_or(|value| value.parse::<jiff::civil::DateTime>().is_err())
        || !fields.contains_key("line")
    {
        return Err(invalid(
            "Logfile entry is missing valid timestamped content",
        ));
    }
    Ok(fields)
}

fn read_metadata(line: &str, source: Format, metadata: &mut Fields) -> io::Result<()> {
    let trimmed = line.trim();
    let fields = match source {
        Format::Text => {
            if trimmed.is_empty() {
                return Ok(());
            }
            let meta = line
                .strip_prefix(META)
                .ok_or_else(|| invalid("Unrecognized plain-text log preamble"))?;
            json_fields(meta)?
        }
        Format::Xml if trimmed.starts_with("<terminal-log") => {
            let open = trimmed
                .strip_suffix('>')
                .ok_or_else(|| invalid("Malformed XML logfile header"))?;
            xml_fields(&format!("{open}/>"), "terminal-log")?
        }
        Format::Json if trimmed.starts_with('"') && !trimmed.starts_with("\"entries\"") => {
            json_fields(&format!("{{{}}}", trimmed.trim_end_matches(',')))?
        }
        _ => return Ok(()),
    };
    metadata.extend(fields);
    if metadata
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum::<usize>()
        > MAX_PREAMBLE as usize
    {
        return Err(invalid("The logfile preamble exceeds its size limit"));
    }
    valid_fields(metadata)
}

fn json_fields(json: &str) -> io::Result<Fields> {
    let fields = serde_json::from_str(json).map_err(io::Error::other)?;
    valid_fields(&fields)?;
    Ok(fields)
}

fn xml_fields(raw: &str, tag: &str) -> io::Result<Fields> {
    let xml = roxmltree::Document::parse(raw).map_err(io::Error::other)?;
    let root = xml.root_element();
    if root.tag_name().name() != tag
        || root.tag_name().namespace().is_some()
        || root.children().any(|node| !node.is_text())
    {
        return Err(invalid("Invalid XML logfile element"));
    }
    let mut fields = Fields::new();
    for attr in root.attributes() {
        if attr.namespace().is_some() {
            return Err(invalid("Namespaced XML log fields are unsupported"));
        }
        fields.insert(attr.name().to_owned(), attr.value().to_owned());
    }
    if tag == "entry" {
        fields.insert(
            "line".into(),
            root.children()
                .filter_map(|node| node.text())
                .collect::<String>(),
        );
    }
    valid_fields(&fields)?;
    Ok(fields)
}

fn valid_fields(fields: &Fields) -> io::Result<()> {
    if fields.len() > 64
        || fields.keys().any(|name| {
            name.len() > 128
                || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(invalid("Malformed logfile field name or count"));
    }
    Ok(())
}

pub(crate) fn write_header(
    out: &mut dyn Write,
    format: Format,
    metadata: &Fields,
) -> io::Result<()> {
    match format {
        Format::Text => {
            let mut fields = metadata.clone();
            fields.insert("contextEncoding".into(), CONTEXT_VERSION.into());
            writeln!(
                out,
                "{META}{}",
                serde_json::to_string(&fields).map_err(io::Error::other)?
            )
        }
        Format::Xml => {
            write!(
                out,
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<terminal-log"
            )?;
            xml_attributes(out, metadata, false)?;
            writeln!(out, ">")
        }
        Format::Json => {
            writeln!(out, "{{")?;
            for (name, value) in metadata {
                if name == "entries" {
                    return Err(invalid("Reserved logfile metadata field"));
                }
                writeln!(
                    out,
                    "  \"{name}\": {},",
                    serde_json::to_string(value).map_err(io::Error::other)?
                )?;
            }
            writeln!(out, "  \"entries\": [")
        }
    }
}

pub(crate) fn write_entry(
    out: &mut dyn Write,
    format: Format,
    fields: &Fields,
    count: usize,
) -> io::Result<()> {
    let timestamp = &fields["timestamp"];
    let line = &fields["line"];
    match format {
        Format::Text => {
            write!(out, "[{timestamp}] ")?;
            if fields.len() > 2 || line.starts_with(CONTEXT) {
                let context: BTreeMap<_, _> = fields
                    .iter()
                    .filter(|(key, _)| key.as_str() != "line")
                    .collect();
                write!(
                    out,
                    "{CONTEXT}{}\t",
                    serde_json::to_string(&context).map_err(io::Error::other)?
                )?;
            }
            writeln!(out, "{line}")
        }
        Format::Xml => {
            write!(out, "  <entry")?;
            xml_attributes(out, fields, true)?;
            write!(out, ">")?;
            xml_escape(out, line)?;
            writeln!(out, "</entry>")
        }
        Format::Json => {
            if count > 0 {
                out.write_all(b",\n")?;
            }
            write!(
                out,
                "    {{\"timestamp\":{}",
                serde_json::to_string(timestamp).map_err(io::Error::other)?
            )?;
            for (name, value) in fields
                .iter()
                .filter(|(name, _)| name.as_str() != "timestamp")
            {
                write!(
                    out,
                    ",\"{name}\":{}",
                    serde_json::to_string(value).map_err(io::Error::other)?
                )?;
            }
            write!(out, "}}")
        }
    }
}

fn xml_attributes(out: &mut dyn Write, fields: &Fields, skip_line: bool) -> io::Result<()> {
    for (name, value) in fields {
        if skip_line && name == "line" {
            continue;
        }
        write!(out, " {name}=\"")?;
        xml_escape(out, value)?;
        write!(out, "\"")?;
    }
    Ok(())
}

fn xml_escape(out: &mut dyn Write, text: &str) -> io::Result<()> {
    let mut remainder = text;
    for (index, char) in text.char_indices() {
        let replacement = match char {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&apos;",
            '\r' => "&#13;",
            '\n' => "&#10;",
            '\t' => "&#9;",
            '\0'..='\u{001f}' | '\u{fffe}' | '\u{ffff}' => {
                return Err(invalid("The entry has characters XML cannot represent"));
            }
            _ => continue,
        };
        let prefix = &text[..index];
        let length = prefix.len() - (text.len() - remainder.len());
        out.write_all(&remainder.as_bytes()[..length])?;
        out.write_all(replacement.as_bytes())?;
        remainder = &text[index + char.len_utf8()..];
    }
    out.write_all(remainder.as_bytes())
}
