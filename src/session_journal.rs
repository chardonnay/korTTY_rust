//! Native access to Java kortty v1 session journal directories.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use roxmltree::{Document, Node, ParsingOptions};

use crate::private_files::Directory;
use crate::sftp_window::LocalSource;

pub(crate) mod annotation;
mod capture;
pub(crate) mod content_search;
pub(crate) mod deletion;
pub(crate) mod entry_edit;
pub(crate) mod export;
pub(crate) mod export_branding;
pub(crate) mod export_output;
pub(crate) mod keywords;
mod links;
pub(crate) mod markers;
mod metadata;
pub(crate) mod note_translation;
pub(crate) mod replace;
pub(crate) mod screenshot_ai;
pub(crate) mod screenshot_analysis_store;
pub(crate) mod screenshot_edit;
mod settings;
mod timeline_details;
pub(crate) mod topic_ai_settings;
pub(crate) mod tree;
pub(crate) mod viewer;
mod writer;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use capture::capture_page;
pub(crate) use capture::{CaptureKind, CapturePage, CaptureRecord, Cursor, LiveCursor, LivePage};
pub(crate) use links::{LinkField, LinkSpan, LinkTarget};
pub(crate) use metadata::{MetadataEdit, edit_metadata, edit_metadata_in};
pub(crate) use settings::{LogFormat, Settings, default_directory};
pub(crate) use writer::{
    Capture, MAX_RECORD_BYTES, MAX_SEED_BYTES, MAX_SEED_LINES, Metadata, PublishedScreenshot,
    RecordKind, Sink, Status,
};

const DOCUMENT: &str = "journal.xml";
const MAX_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CATALOG_NAMES: usize = 4096;
const MAX_CATALOG_ROWS: usize = 512;
const MAX_TIMELINE_ROWS: usize = 2048;
const MAX_PNG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PNG_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub directory: PathBuf,
    pub id: String,
    pub deletion_identity: Option<deletion::DeletionIdentity>,
    pub title: String,
    pub description: String,
    pub connection_name: String,
    pub host: String,
    pub host_present: bool,
    pub port: Option<u16>,
    pub username: String,
    pub started_at: Option<Timestamp>,
    started_offset_seconds: i32,
    pub ended_at: Option<Timestamp>,
    pub entry_count: usize,
    pub log_parts: u32,
    pub source: String,
    pub source_kind: tree::SourceKind,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub run_id: Option<String>,
    pub run_started_at: Option<Timestamp>,
    run_started_offset_seconds: i32,
    pub automation_action: Option<String>,
    pub run_status: Option<tree::RunStatus>,
    pub pinned: bool,
    pub keywords: Vec<String>,
    pub expires_at: Option<Timestamp>,
    expires_offset_seconds: i32,
    pub storage_bytes: u64,
    pub log_entry_count: u64,
    pub duplicate_run_count: u64,
    pub usage: tree::Usage,
}

#[derive(Clone, Debug)]
pub(crate) struct TimelineEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub text: String,
    pub note: String,
    text_links: Vec<links::Link>,
    note_links: Vec<links::Link>,
    pub created_at: Option<Timestamp>,
    pub edited_at: Option<Timestamp>,
    pub marker: String,
    pub marker_key: Option<String>,
    pub marker_name: String,
    pub marker_color: Option<[u8; 3]>,
    pub screenshot: Option<String>,
    pub log_part: Option<u32>,
    pub log_start: Option<u64>,
    pub log_end: Option<u64>,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub extra_details: Vec<String>,
    pub chat_details: Vec<String>,
}

impl TimelineEntry {
    pub(crate) fn screenshot_analysis(&self) -> Option<&[String]> {
        if self.kind != "SCREENSHOT" || self.screenshot.is_none() {
            return None;
        }
        let index = self
            .extra_details
            .iter()
            .position(|value| value == "AI analysis")?;
        Some(&self.extra_details[index..])
    }

    pub(crate) fn link_spans(&self, field: LinkField, char_limit: usize) -> Vec<LinkSpan<'_>> {
        let (text, links) = match field {
            LinkField::Text => (&self.text, &self.text_links),
            LinkField::Note => (&self.note, &self.note_links),
        };
        links::spans(text, links, char_limit)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Journal {
    pub summary: Summary,
    pub timeline: Vec<TimelineEntry>,
    pub timeline_limited: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Catalog {
    pub journals: Vec<Summary>,
    pub top_keywords: Vec<String>,
    pub skipped: usize,
    pub limited: bool,
}

/// Reads only curated metadata. The bounded catalog never opens capture logs.
pub(crate) fn catalog(root: &Path) -> io::Result<Catalog> {
    let names = match directory_names(root, MAX_CATALOG_NAMES) {
        Ok(names) => names,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Catalog::default()),
        Err(error) => return Err(error),
    };
    let mut catalog = Catalog {
        limited: names.len() > MAX_CATALOG_NAMES,
        ..Catalog::default()
    };
    let opened_root = Directory::open(root, false).ok();
    let mut budget = MAX_CATALOG_BYTES;
    for name in names.into_iter().take(MAX_CATALOG_NAMES) {
        if catalog.journals.len() == MAX_CATALOG_ROWS || budget == 0 {
            catalog.limited = true;
            break;
        }
        let directory = root.join(name);
        match LocalSource::open(&directory.join(DOCUMENT)) {
            Ok(source) if source.size() <= MAX_DOCUMENT_BYTES => {
                let limit = budget.min(MAX_DOCUMENT_BYTES);
                if source.size() > limit {
                    catalog.limited = true;
                    break;
                }
                let identity = opened_root
                    .as_ref()
                    .and_then(|root| deletion::DeletionIdentity::capture(root, &source).ok());
                let result = source_text(&source, limit, &mut budget).and_then(|xml| {
                    let mut row = summary(&directory, &parse_document(&xml)?)?;
                    row.deletion_identity = identity
                        .filter(|identity| identity.require_document(source.file()).is_ok());
                    Ok(row)
                });
                match result {
                    Ok(row) => catalog.journals.push(row),
                    Err(_) => catalog.skipped += 1,
                }
            }
            Ok(_) => catalog.skipped += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) if std::fs::symlink_metadata(&directory).is_ok_and(|item| item.is_file()) => {}
            Err(_) => catalog.skipped += 1,
        }
    }
    catalog.journals.sort_by(|left, right| {
        right
            .started_order()
            .cmp(&left.started_order())
            .then_with(|| left.directory.cmp(&right.directory))
    });
    catalog.top_keywords = keywords::rank(&catalog.journals);
    Ok(catalog)
}

/// Reads the curated v1 document with a visible limit on the timeline rows.
pub(crate) fn read(directory: &Path) -> io::Result<Journal> {
    let source = open_document(directory)?;
    let identity = directory
        .parent()
        .and_then(|root| Directory::open(root, false).ok())
        .and_then(|root| deletion::DeletionIdentity::capture(&root, &source).ok());
    let xml = document_text(&source)?;
    let document = parse_document(&xml)?;
    let mut journal = journal(directory, &document)?;
    journal.summary.deletion_identity =
        identity.filter(|identity| identity.require_document(source.file()).is_ok());
    Ok(journal)
}

fn journal(directory: &Path, document: &Document<'_>) -> io::Result<Journal> {
    let summary = summary(directory, document)?;
    let mut timeline = Vec::new();
    let mut timeline_limited = false;
    let mut markers = export::Markers::read(document).unwrap_or_else(|_| {
        timeline_limited = true;
        export::Markers::default()
    });
    let zone = TimeZone::system();
    for entry in entries(document).take(MAX_TIMELINE_ROWS) {
        timeline.push(timeline_entry(
            entry,
            &mut markers,
            &zone,
            &mut timeline_limited,
        )?);
    }
    // Match Java OffsetDateTime ordering while preserving XML order for exact ties.
    timeline.sort_by_key(|(date, _)| (date.is_none(), *date));
    Ok(Journal {
        timeline_limited: timeline_limited || summary.entry_count > timeline.len(),
        summary,
        timeline: timeline.into_iter().map(|(_, entry)| entry).collect(),
    })
}

/// Opens an ordinary PNG inside this journal's screenshots directory.
/// The encoded byte and declared image size bounds also apply to imported journals.
#[cfg(test)]
pub(crate) fn read_screenshot(directory: &Path, reference: &str) -> io::Result<Vec<u8>> {
    let reference = screenshot_reference(reference)
        .ok_or_else(|| invalid("Invalid session journal screenshot reference"))?;
    check_path(directory)?;
    let source = LocalSource::open(&directory.join(reference))?;
    let mut budget = MAX_PNG_BYTES;
    let bytes = source_bytes(&source, MAX_PNG_BYTES, &mut budget)?;
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
        return Err(invalid("Invalid session journal PNG header"));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PNG_PIXELS {
        return Err(invalid("Session journal PNG exceeds the image size limit"));
    }
    Ok(bytes)
}

fn open_document(directory: &Path) -> io::Result<LocalSource> {
    check_path(directory)?;
    LocalSource::open(&directory.join(DOCUMENT))
}

fn document_text(source: &LocalSource) -> io::Result<String> {
    let mut budget = MAX_DOCUMENT_BYTES;
    source_text(source, MAX_DOCUMENT_BYTES, &mut budget)
}

fn source_text(source: &LocalSource, limit: u64, budget: &mut u64) -> io::Result<String> {
    String::from_utf8(source_bytes(source, limit, budget)?)
        .map_err(|_| invalid("Session journal document is not UTF-8"))
}

fn source_bytes(source: &LocalSource, limit: u64, budget: &mut u64) -> io::Result<Vec<u8>> {
    if source.size() > limit {
        return Err(invalid("Session journal file exceeds the size limit"));
    }
    let mut bytes = Vec::new();
    source.file().take(limit + 1).read_to_end(&mut bytes)?;
    *budget = budget.saturating_sub(bytes.len() as u64);
    if bytes.len() as u64 > limit {
        return Err(invalid("Session journal file exceeds the size limit"));
    }
    Ok(bytes)
}

fn parse_document(xml: &str) -> io::Result<Document<'_>> {
    let document = parse_xml(xml, 200_000)?;
    let root = document.root_element();
    if !root.has_tag_name("session-journal") || root.attribute("formatVersion") != Some("1") {
        return Err(invalid("Expected a Java v1 session journal document"));
    }
    for name in ["meta", "entries", "markerDefinitions"] {
        if root
            .children()
            .filter(|node| node.has_tag_name(name))
            .nth(1)
            .is_some()
        {
            return Err(invalid("Duplicate session journal document field"));
        }
    }
    Ok(document)
}

fn parse_xml(xml: &str, nodes_limit: u32) -> io::Result<Document<'_>> {
    if xml.contains("<!DOCTYPE") {
        return Err(invalid("Session journal XML must not contain a DTD"));
    }
    let options = ParsingOptions {
        nodes_limit,
        ..ParsingOptions::default()
    };
    Document::parse_with_options(xml, options)
        .map_err(|_| invalid("Malformed or oversized session journal XML"))
}

fn document_id<'a>(document: &'a Document<'_>) -> io::Result<&'a str> {
    document
        .root_element()
        .attribute("id")
        .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        .ok_or_else(|| invalid("Invalid session journal ID"))
}

fn summary(directory: &Path, document: &Document<'_>) -> io::Result<Summary> {
    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let id = document_id(document)?;
    let title = value(meta, "title")?;
    let source = value(meta, "sourceKind")?;
    let source_kind = tree::SourceKind::parse(&bounded_field(meta, "sourceKind")?);
    let started = number::<tree::DateOrder>(meta, "startedAt")?;
    let run_started = number::<tree::DateOrder>(meta, "runStartedAt")?;
    let expires = number::<tree::DateOrder>(meta, "expiresAt")?;
    Ok(Summary {
        directory: directory.to_owned(),
        id: id.into(),
        deletion_identity: None,
        title: if title.is_empty() { id.into() } else { title },
        description: value(meta, "description")?,
        connection_name: value(meta, "connectionName")?,
        host: value(meta, "host")?,
        host_present: optional_value(meta, "host")?.is_some(),
        port: number(meta, "port")?.filter(|port| *port != 0),
        username: value(meta, "username")?,
        started_at: started.map(tree::DateOrder::timestamp),
        started_offset_seconds: started.map_or(0, tree::DateOrder::offset_seconds),
        ended_at: number(meta, "endedAt")?,
        entry_count: entries(document).count(),
        log_parts: number(meta, "logParts")?.unwrap_or(1),
        source: if source.is_empty() {
            "INTERACTIVE".into()
        } else {
            source
        },
        source_kind,
        source_id: optional_raw_value(meta, "sourceId")?,
        source_name: optional_raw_value(meta, "sourceName")?,
        run_id: optional_raw_value(meta, "runId")?,
        run_started_at: run_started.map(tree::DateOrder::timestamp),
        run_started_offset_seconds: run_started.map_or(0, tree::DateOrder::offset_seconds),
        automation_action: optional_raw_value(meta, "automationAction")?,
        run_status: tree::RunStatus::parse(&bounded_field(meta, "runStatus")?),
        pinned: boolean(meta, "pinned")?,
        keywords: keywords::read(meta)?,
        expires_at: expires.map(tree::DateOrder::timestamp),
        expires_offset_seconds: expires.map_or(0, tree::DateOrder::offset_seconds),
        storage_bytes: number(meta, "storageBytes")?.unwrap_or(0),
        log_entry_count: number(meta, "logEntryCount")?.unwrap_or(0),
        duplicate_run_count: number(meta, "duplicateRunCount")?.unwrap_or(0),
        usage: tree::Usage {
            prompt_tokens: number(meta, "aiPromptTokens")?.unwrap_or(0),
            completion_tokens: number(meta, "aiCompletionTokens")?.unwrap_or(0),
            total_tokens: number(meta, "aiTotalTokens")?.unwrap_or(0),
            call_count: number(meta, "aiCallCount")?.unwrap_or(0),
            cost: finite_cost(meta)?,
            currency: optional_value(meta, "aiCostCurrency")?,
            local: boolean(meta, "aiProfileLocal")?,
        },
    })
}

fn optional_value(parent: Node<'_, '_>, name: &str) -> io::Result<Option<String>> {
    field(parent, name).map(|value| value.map(|text| display_text(&text)))
}

fn optional_raw_value(parent: Node<'_, '_>, name: &str) -> io::Result<Option<String>> {
    field(parent, name).map(|value| value.map(Cow::into_owned))
}

fn boolean(parent: Node<'_, '_>, name: &str) -> io::Result<bool> {
    match bounded_field(parent, name)?.trim() {
        "" | "false" | "0" => Ok(false),
        "true" | "1" => Ok(true),
        _ => Err(invalid("Invalid session journal boolean field")),
    }
}

fn finite_cost(meta: Node<'_, '_>) -> io::Result<f64> {
    let cost = number::<f64>(meta, "aiCost")?.unwrap_or(0.0);
    if !cost.is_finite() {
        return Err(invalid("Invalid session journal AI cost"));
    }
    Ok(cost)
}

fn entries<'a, 'input>(document: &'a Document<'input>) -> impl Iterator<Item = Node<'a, 'input>> {
    child(document.root_element(), "entries")
        .into_iter()
        .flat_map(|entries| entries.children())
        .filter(|entry| entry.has_tag_name("entry"))
}

fn timeline_entry(
    entry: Node<'_, '_>,
    markers: &mut export::Markers,
    zone: &TimeZone,
    limited: &mut bool,
) -> io::Result<(Option<tree::DateOrder>, TimelineEntry)> {
    let screenshot = match field(entry, "screenshotFile")?.filter(|value| !value.is_empty()) {
        Some(value) => match screenshot_reference(&value) {
            Some(reference) => Some(reference.into()),
            None => {
                *limited = true;
                None
            }
        },
        None => None,
    };
    let custom_marker = value(entry, "markerId")?;
    let kind = bounded_field(entry, "kind")?;
    let resolved = markers.visible(entry).unwrap_or_else(|_| {
        *limited = true;
        None
    });
    let (marker_key, marker_name, marker_color) = match resolved {
        Some((key, name, color)) => (
            Some(key.to_owned()),
            timeline_details::marker_name(name, limited),
            Some(color),
        ),
        None => (None, String::new(), None),
    };
    let (extra_details, chat_details) =
        timeline_details::read(entry, &kind, screenshot.is_some(), zone, limited);
    let text_source = match kind.as_ref() {
        "USER_NOTE" => Some(links::Source::Note),
        "AI_SUMMARY" | "SESSION_SUMMARY" => Some(links::Source::Summary),
        _ => None,
    };
    let (text, text_links) = links::load(&bounded_field(entry, "text")?, text_source);
    let (note, note_links) = links::load(
        &bounded_field(entry, "userNote")?,
        Some(links::Source::Note),
    );
    let date = number::<tree::DateOrder>(entry, "createdAt")?;
    let entry = TimelineEntry {
        id: bounded_field(entry, "id")?.into_owned(),
        kind: display_text(&kind),
        title: value(entry, "title")?,
        text,
        note,
        text_links,
        note_links,
        created_at: date.map(tree::DateOrder::timestamp),
        edited_at: number::<tree::DateOrder>(entry, "editedAt")
            .ok()
            .flatten()
            .map(tree::DateOrder::timestamp),
        marker: if custom_marker.is_empty() {
            value(entry, "marker")?
        } else {
            custom_marker
        },
        marker_key,
        marker_name,
        marker_color,
        screenshot,
        log_part: number(entry, "logPart")?,
        log_start: number(entry, "logStartSeq")?,
        log_end: number(entry, "logEndSeq")?,
        input: excerpt(entry, "inputExcerpt")?,
        output: excerpt(entry, "outputExcerpt")?,
        extra_details,
        chat_details,
    };
    Ok((date, entry))
}

fn child<'a, 'input>(parent: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    parent.children().find(|node| node.has_tag_name(name))
}

fn field<'a>(parent: Node<'a, '_>, name: &str) -> io::Result<Option<Cow<'a, str>>> {
    let mut fields = parent.children().filter(|node| node.has_tag_name(name));
    let Some(field) = fields.next() else {
        return Ok(None);
    };
    if fields.next().is_some() {
        return Err(invalid("Invalid session journal scalar field"));
    }
    scalar_text(field).map(Some)
}

fn value(parent: Node<'_, '_>, name: &str) -> io::Result<String> {
    bounded_field(parent, name).map(|text| display_text(&text))
}

fn bounded_field<'a>(parent: Node<'a, '_>, name: &str) -> io::Result<Cow<'a, str>> {
    Ok(field(parent, name)?.unwrap_or(Cow::Borrowed("")))
}

fn scalar_text<'a>(node: Node<'a, '_>) -> io::Result<Cow<'a, str>> {
    if node.first_element_child().is_some() {
        return Err(invalid("Invalid session journal scalar field"));
    }
    let mut length = 0_usize;
    for text in node
        .children()
        .filter(Node::is_text)
        .filter_map(|child| child.text())
    {
        length = length
            .checked_add(text.len())
            .filter(|&length| length <= 256 * 1024)
            .ok_or_else(|| invalid("Session journal text field exceeds the size limit"))?;
    }
    let mut segments = node
        .children()
        .filter(Node::is_text)
        .filter_map(|child| child.text());
    let first = segments.next().unwrap_or("");
    let Some(second) = segments.next() else {
        return Ok(Cow::Borrowed(first));
    };
    let mut text = String::with_capacity(length);
    text.push_str(first);
    text.push_str(second);
    for segment in segments {
        text.push_str(segment);
    }
    Ok(Cow::Owned(text))
}

fn number<T: std::str::FromStr>(parent: Node<'_, '_>, name: &str) -> io::Result<Option<T>> {
    let value = field(parent, name)?;
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid("Invalid session journal field"))
        })
        .transpose()
}

fn excerpt(entry: Node<'_, '_>, name: &str) -> io::Result<Vec<String>> {
    child(entry, name)
        .into_iter()
        .flat_map(|wrapper| wrapper.children())
        .filter(|node| node.has_tag_name("line"))
        .map(|node| scalar_text(node).map(|text| display_text(&text)))
        .collect()
}

fn screenshot_reference(value: &str) -> Option<&str> {
    let filename = value.strip_prefix("screenshots/")?;
    (filename.ends_with(".png")
        && filename.len() > 4
        && filename.len() <= 128
        && filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)))
    .then_some(value)
}

fn display_text(value: &str) -> String {
    value
        .chars()
        .filter(|&character| is_display_character(character))
        .collect()
}

pub(crate) fn is_display_character(character: char) -> bool {
    matches!(character, '\n' | '\t')
        || (!character.is_control()
            && !matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}'))
}

fn check_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a safe absolute session journal directory",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn directory_names(path: &Path, limit: usize) -> io::Result<Vec<OsString>> {
    use rustix::fs::{Dir, Mode, OFlags, open, openat};
    use std::os::unix::ffi::OsStringExt as _;

    check_path(path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut folder = open("/", flags, Mode::empty())?;
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid("Invalid session journal directory component"));
        };
        folder = openat(folder, name, flags, Mode::empty())?;
    }
    let mut names = Vec::new();
    for entry in Dir::read_from(&folder)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if !matches!(name, b"." | b"..") {
            names.push(OsString::from_vec(name.to_vec()));
            if names.len() > limit {
                break;
            }
        }
    }
    Ok(names)
}

#[cfg(windows)]
fn directory_names(path: &Path, limit: usize) -> io::Result<Vec<OsString>> {
    check_path(path)?;
    kortty_files::tree::Directory::open_path(path)?.names(limit + 1)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
