//! Bounded, local export for Java v1 session journals.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use roxmltree::{Document, Node};

use crate::private_files::Directory;
use crate::sftp_window::LocalSource;

use super::deletion::DeletionIdentity;
use super::export_branding::PdfBranding;
use super::metadata::java_whitespace;
use super::tree::DateOrder;
use super::{
    boolean, bounded_field, child, display_text, document_id, document_text, entries, invalid,
    number, open_document, optional_raw_value, parse_document, scalar_text,
};

mod bundle;
mod filter;
mod markdown;
pub(super) use markdown::screenshot_bytes;
mod pdf;
pub(crate) mod topic_ai;

pub(crate) use filter::{
    DEFAULT_TOLERANCE_MINUTES, Filter, MAX_TOLERANCE_MINUTES, MAX_TOPIC_BYTES, MAX_WINDOWS,
    MarkerMode, Preview, TimeWindow, parse_time_of_day,
};
pub(crate) use markdown::Markdown;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub include_screenshots: bool,
    pub include_chats: bool,
    pub filter: Filter,
    pub branding: PdfBranding,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            include_screenshots: true,
            include_chats: true,
            filter: Filter::default(),
            branding: PdfBranding::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Marker {
    pub id: String,
    pub name: String,
}

pub(crate) struct Snapshot {
    root: PathBuf,
    directory: PathBuf,
    identity: DeletionIdentity,
    xml: Option<String>,
    header: Header,
    entries: Vec<Entry>,
    markers: Vec<Marker>,
}

pub(crate) enum TopicSelection {
    Model {
        run: Box<topic_ai::Run>,
        redactor: crate::terminal_logging::Redactor,
    },
    LocalFallback(topic_ai::Fallback),
}

/// One selection bound to the exact source snapshot shared by every renderer.
pub(crate) struct Selection<'a> {
    snapshot: &'a Snapshot,
    preview: Preview,
    entries: Vec<&'a Entry>,
    ai_applied: bool,
    pub notice: Option<TopicNotice>,
}

#[derive(Clone, Copy)]
pub(crate) enum TopicNotice {
    Fallback(topic_ai::Fallback),
    LocalTail(usize),
}

impl TopicNotice {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Fallback(reason) => {
                format!("{}. The local text filter was used.", reason.message())
            }
            Self::LocalTail(count) => {
                format!("The local text filter checked {count} entries beyond the AI limit.")
            }
        }
    }
}

impl Selection<'_> {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn require_matches(&self, filter: &Filter) -> io::Result<()> {
        if filter.is_active() && self.is_empty() {
            let mut message = "The journal export filters select no entries".to_owned();
            if let Some(notice) = self.notice {
                message.push_str(". ");
                message.push_str(&notice.message());
            }
            return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
        }
        Ok(())
    }
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("entries", &self.entries.len())
            .field("markers", &self.markers.len())
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    pub(crate) fn select_for_export(
        &self,
        filter: &Filter,
        topic_ai: Option<&TopicSelection>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Selection<'_>> {
        let Some(ai) = topic_ai.filter(|_| !filter.topic().is_empty()) else {
            let (preview, entries) = filter.select(self)?;
            return Ok(Selection {
                snapshot: self,
                preview,
                entries,
                ai_applied: false,
                notice: None,
            });
        };
        if filter.topic_regex {
            return Err(invalid("AI selection requires a literal topic"));
        }
        let pattern = filter.compile()?.expect("nonempty validated topic");
        let (mut preview, mut entries) = filter.candidates(self)?;
        let total = entries.len();
        let mut notice = None;
        let mut ai_applied = false;
        if !entries.is_empty() {
            let outcome = match ai {
                TopicSelection::Model { run, redactor } => {
                    self.open_current_source()?;
                    run.choose(filter.topic(), &entries, redactor, cancelled)?
                }
                TopicSelection::LocalFallback(reason) => topic_ai::Outcome::Fallback(*reason),
            };
            match outcome {
                topic_ai::Outcome::Selected {
                    indices,
                    considered,
                } => {
                    let kept = indices
                        .into_iter()
                        .collect::<std::collections::HashSet<_>>();
                    let mut index = 0;
                    entries.retain(|entry| {
                        let include = if index < considered {
                            kept.contains(&index)
                        } else {
                            filter::matches_topic(&pattern, entry)
                        };
                        index += 1;
                        include
                    });
                    ai_applied = true;
                    if considered < total {
                        notice = Some(TopicNotice::LocalTail(total - considered));
                    }
                }
                topic_ai::Outcome::Fallback(reason) => {
                    entries.retain(|entry| filter::matches_topic(&pattern, entry));
                    notice = Some(TopicNotice::Fallback(reason));
                }
            }
        }
        preview.kept_entries = entries.len();
        preview.dropped_by_topic = total - entries.len();
        Ok(Selection {
            snapshot: self,
            preview,
            entries,
            ai_applied,
            notice,
        })
    }

    pub(crate) fn title(&self) -> &str {
        &self.header.title
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn used_markers(&self) -> &[Marker] {
        &self.markers
    }

    pub(crate) fn preview(&self, filter: &Filter) -> io::Result<Preview> {
        Ok(filter.select(self)?.0)
    }

    fn open_current_source(&self) -> io::Result<LocalSource> {
        let source = open_document(&self.directory)?;
        let root = Directory::open(&self.root, false)?;
        if DeletionIdentity::capture(&root, &source)? != self.identity {
            return Err(changed());
        }
        Ok(source)
    }
}

/// Reads the current version of the selected journal from its verified catalog location.
pub(crate) fn load(
    root: &Path,
    directory: &Path,
    expected_id: &str,
    expected: &DeletionIdentity,
) -> io::Result<Snapshot> {
    load_snapshot(root, directory, expected_id, expected, false)
}

/// Keeps the validated source XML only when the export will publish an HTML bundle.
pub(crate) fn load_for_bundle(
    root: &Path,
    directory: &Path,
    expected_id: &str,
    expected: &DeletionIdentity,
) -> io::Result<Snapshot> {
    load_snapshot(root, directory, expected_id, expected, true)
}

fn load_snapshot(
    root: &Path,
    directory: &Path,
    expected_id: &str,
    expected: &DeletionIdentity,
    retain_xml: bool,
) -> io::Result<Snapshot> {
    if directory.parent() != Some(root) {
        return Err(changed());
    }
    let source = open_document(directory)?;
    let root_handle = Directory::open(root, false)?;
    let identity = DeletionIdentity::capture(&root_handle, &source)?;
    if !identity.same_location(expected) {
        return Err(changed());
    }
    let xml = document_text(&source)?;
    identity.require_document(source.file())?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != expected_id {
        return Err(changed());
    }
    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let mut markers = Markers::read(&document)?;
    let mut selected = entries(&document)
        .enumerate()
        .map(|(xml_index, entry)| Entry::read(entry, xml_index, &mut markers))
        .collect::<io::Result<Vec<_>>>()?;
    // OffsetDateTime's natural order breaks equal-instant ties by local time.
    selected.sort_by_key(|entry| (entry.created_at.is_none(), entry.created_at));
    let header = Header::read(meta)?;
    drop(document);
    let snapshot = Snapshot {
        root: root.to_owned(),
        directory: directory.to_owned(),
        identity,
        xml: retain_xml.then_some(xml),
        header,
        entries: selected,
        markers: markers.used,
    };
    snapshot.open_current_source()?;
    Ok(snapshot)
}

fn changed() -> io::Error {
    invalid("The journal changed. Refresh the journal and reopen export options.")
}

struct Header {
    title: String,
    description: String,
    connection: String,
    started_at: Option<DateOrder>,
    ended_at: Option<Timestamp>,
    commands: u64,
    errors: u64,
    screenshots: u64,
}

impl Header {
    fn read(meta: Node<'_, '_>) -> io::Result<Self> {
        let title = raw(meta, "title")?;
        let name = raw(meta, "connectionName")?;
        let host = raw(meta, "host")?;
        let user = raw(meta, "username")?;
        let port: i32 = number(meta, "port")?.unwrap_or_default();
        Ok(Self {
            connection: connection(&title, strip(&name), strip(&host), strip(&user), port),
            title,
            description: raw(meta, "description")?,
            started_at: number(meta, "startedAt")?,
            ended_at: number(meta, "endedAt")?,
            commands: number(meta, "commandCount")?.unwrap_or_default(),
            errors: number(meta, "errorCount")?.unwrap_or_default(),
            screenshots: number(meta, "screenshotCount")?.unwrap_or_default(),
        })
    }
}

fn connection(title: &str, name: &str, host: &str, user: &str, port: i32) -> String {
    let title = strip(title).to_lowercase();
    let mut parts = Vec::new();
    let user_host = if user.is_empty() {
        host.to_owned()
    } else {
        format!("{user}@{host}")
    };
    let endpoint = format!("{user_host}:{port}");
    if !host.is_empty() && !title.contains(&user_host.to_lowercase()) {
        parts.push(endpoint.as_str());
    }
    if !name.is_empty() {
        let normalized = name.to_lowercase();
        let alias = !host.is_empty()
            && [host, &user_host, &endpoint]
                .iter()
                .any(|value| normalized == value.to_lowercase());
        if !alias && !title.contains(&normalized) {
            parts.push(name);
        }
    }
    parts.join(" · ")
}

struct Entry {
    id: String,
    xml_index: usize,
    kind: String,
    created_at: Option<DateOrder>,
    log_start: Option<u64>,
    log_end: Option<u64>,
    title: String,
    run_as_root: bool,
    text: String,
    note: String,
    marker: Option<usize>,
    marker_color: [u8; 3],
    input: Vec<String>,
    output: Vec<String>,
    screenshot: Option<Screenshot>,
    agent: Option<Agent>,
}

impl Entry {
    fn read(entry: Node<'_, '_>, xml_index: usize, markers: &mut Markers) -> io::Result<Self> {
        let kind = bounded_field(entry, "kind")?;
        let (marker, marker_color) = markers.resolve(entry)?;
        Ok(Self {
            id: raw(entry, "id")?,
            xml_index,
            kind: kind.as_ref().to_owned(),
            created_at: number(entry, "createdAt")?,
            log_start: number(entry, "logStartSeq")?,
            log_end: number(entry, "logEndSeq")?,
            title: raw(entry, "title")?,
            run_as_root: bounded_field(entry, "runAsUser")? == "root",
            text: raw(entry, "text")?,
            note: raw(entry, "userNote")?,
            marker,
            marker_color,
            input: sequence(entry, "inputExcerpt", "line")
                .map(|line| scalar_text(line).map(std::borrow::Cow::into_owned))
                .collect::<io::Result<_>>()?,
            output: sequence(entry, "outputExcerpt", "line")
                .map(|line| scalar_text(line).map(std::borrow::Cow::into_owned))
                .collect::<io::Result<_>>()?,
            screenshot: (kind == "SCREENSHOT")
                .then(|| Screenshot::read(entry))
                .transpose()?
                .flatten(),
            agent: (kind == "AGENT").then(|| Agent::read(entry)).transpose()?,
        })
    }
}

struct Screenshot {
    reference: String,
    ai_description: String,
    tags: Vec<String>,
}

impl Screenshot {
    fn read(entry: Node<'_, '_>) -> io::Result<Option<Self>> {
        let Some(reference) = optional_raw_value(entry, "screenshotFile")? else {
            return Ok(None);
        };
        Ok(Some(Self {
            reference,
            ai_description: raw(entry, "aiDescription")?,
            tags: sequence(entry, "aiTags", "tag")
                .map(|tag| scalar_text(tag).map(std::borrow::Cow::into_owned))
                .collect::<io::Result<_>>()?,
        }))
    }
}

struct Agent {
    model: String,
    duration_millis: u64,
    tokens: u64,
    chat: Option<Chat>,
}

impl Agent {
    fn read(entry: Node<'_, '_>) -> io::Result<Self> {
        Ok(Self {
            model: raw(entry, "agentModel")?,
            duration_millis: number(entry, "agentDurationMillis")?.unwrap_or_default(),
            tokens: number(entry, "agentTokens")?.unwrap_or_default(),
            chat: child(entry, "chatSnapshot").map(Chat::read).transpose()?,
        })
    }
}

struct Chat {
    title: String,
    captured_at: Option<Timestamp>,
    frozen: bool,
    messages: Vec<Message>,
}

impl Chat {
    fn read(node: Node<'_, '_>) -> io::Result<Self> {
        Ok(Self {
            title: raw(node, "title")?,
            captured_at: number(node, "capturedAt")?,
            frozen: boolean(node, "frozen")?,
            messages: sequence(node, "messages", "message")
                .map(Message::read)
                .collect::<io::Result<_>>()?,
        })
    }
}

struct Message {
    user: bool,
    profile: String,
    content: String,
    steps: Vec<Step>,
}

impl Message {
    fn read(node: Node<'_, '_>) -> io::Result<Self> {
        Ok(Self {
            user: bounded_field(node, "role")? == "user",
            profile: raw(node, "aiProfileName")?,
            content: raw(node, "content")?,
            steps: sequence(node, "steps", "step")
                .map(Step::read)
                .collect::<io::Result<_>>()?,
        })
    }
}

struct Step {
    status: String,
    title: String,
    summary: String,
    command: String,
    exit_code: Option<i32>,
    output: String,
    truncated: bool,
}

impl Step {
    fn read(node: Node<'_, '_>) -> io::Result<Self> {
        Ok(Self {
            status: raw(node, "status")?,
            title: raw(node, "title")?,
            summary: raw(node, "summary")?,
            command: raw(node, "command")?,
            exit_code: number(node, "exitCode")?,
            output: raw(node, "output")?,
            truncated: boolean(node, "truncated")?,
        })
    }
}

#[derive(Default)]
pub(super) struct Markers {
    definitions: HashMap<String, (String, [u8; 3])>,
    indices: HashMap<String, usize>,
    used: Vec<Marker>,
}

impl Markers {
    pub(super) fn read(document: &Document<'_>) -> io::Result<Self> {
        let mut definitions = HashMap::new();
        for marker in sequence(document.root_element(), "markerDefinitions", "marker") {
            if let Some(id) = normalize_marker(&bounded_field(marker, "id")?) {
                let name = raw(marker, "name")?;
                let name = if strip(&name).is_empty() {
                    builtin_name(&id).to_owned()
                } else {
                    display_text(&name)
                };
                let color = bounded_field(marker, "color")?;
                let color = marker_rgb(&color).unwrap_or(legacy_marker_color(
                    &bounded_field(marker, "legacyMarker")?,
                    "INFO",
                ));
                definitions.entry(id).or_insert((name, color));
            }
        }
        Ok(Self {
            definitions,
            indices: HashMap::new(),
            used: Vec::new(),
        })
    }

    fn resolve(&mut self, entry: Node<'_, '_>) -> io::Result<(Option<usize>, [u8; 3])> {
        let legacy = bounded_field(entry, "marker")?;
        let id = match normalize_marker(&bounded_field(entry, "markerId")?) {
            Some(id) => id,
            None => match legacy.as_ref() {
                "INFO" => "info".into(),
                "IMPORTANT" => "important".into(),
                "ERROR" => "error".into(),
                _ => return Ok((None, legacy_marker_color("NONE", "NONE"))),
            },
        };
        if id == "none" {
            return Ok((None, legacy_marker_color("NONE", "NONE")));
        }
        let definition = self.definitions.get(&id);
        let color = definition.map_or_else(
            || {
                legacy_marker_color(
                    match id.as_str() {
                        "error" => "ERROR",
                        "important" => "IMPORTANT",
                        "info" => "INFO",
                        _ => &legacy,
                    },
                    "NONE",
                )
            },
            |(_, color)| *color,
        );
        let index = *self.indices.entry(id.clone()).or_insert_with(|| {
            let name =
                definition.map_or_else(|| builtin_name(&id).to_owned(), |(name, _)| name.clone());
            self.used.push(Marker { id, name });
            self.used.len() - 1
        });
        Ok((Some(index), color))
    }

    pub(super) fn visible(
        &mut self,
        entry: Node<'_, '_>,
    ) -> io::Result<Option<(&str, &str, [u8; 3])>> {
        let (marker, color) = self.resolve(entry)?;
        Ok(marker.map(|index| {
            let marker = &self.used[index];
            (marker.id.as_str(), marker.name.as_str(), color)
        }))
    }
}

pub(super) fn marker_rgb(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let [_, red, green, blue] = u32::from_str_radix(value, 16).ok()?.to_be_bytes();
    Some([red, green, blue])
}

fn legacy_marker_color(value: &str, default: &str) -> [u8; 3] {
    match if value.is_empty() { default } else { value } {
        "ERROR" => [0xcf, 0x22, 0x2e],
        "IMPORTANT" => [0x9a, 0x67, 0x00],
        "INFO" => [0x09, 0x69, 0xda],
        _ => [0x6e, 0x77, 0x81],
    }
}

pub(super) fn normalize_marker(value: &str) -> Option<String> {
    let mut id = String::new();
    for character in value
        .trim_matches(|c: char| c <= ' ')
        .to_lowercase()
        .chars()
    {
        if id.len() == 32 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            id.push(character);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    if id.ends_with('-') {
        id.pop();
    }
    (!id.is_empty()).then_some(id)
}

fn builtin_name(id: &str) -> &str {
    match id {
        "info" => "Info",
        "important" => "Important",
        "error" => "Error",
        _ => id,
    }
}

fn raw(node: Node<'_, '_>, name: &str) -> io::Result<String> {
    bounded_field(node, name).map(std::borrow::Cow::into_owned)
}

fn sequence<'a, 'input>(
    node: Node<'a, 'input>,
    wrapper: &str,
    item: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    child(node, wrapper)
        .into_iter()
        .flat_map(|node| node.children())
        .filter(move |node| node.has_tag_name(item))
}

fn strip(value: &str) -> &str {
    value.trim_matches(java_whitespace)
}
