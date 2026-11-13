//! Verified, field-scoped edits of a session journal timeline entry.

use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use roxmltree::{Document, Node};
use zeroize::Zeroizing;

use crate::private_files::Directory;
use crate::terminal_logging::Redactor;

use super::export::{marker_rgb, normalize_marker};
use super::markers::{Definition, MAX_DEFINITIONS};
use super::metadata::{MetadataLock, escape, java_whitespace, lock_metadata_in, xml_character};
use super::viewer::Source;
use super::{
    MAX_DOCUMENT_BYTES, bounded_field, child, display_text, document_id, field, invalid,
    parse_document,
};

pub(crate) const MAX_FIELD_BYTES: usize = 256 * 1024;
const MAX_MARKER_CHOICES: usize = 128;
const MAX_MARKER_LABEL: usize = 256;

pub(crate) mod delete;
mod marker_rules;
pub(crate) use marker_rules::apply_rules;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MarkerOutcome {
    pub(crate) changed: usize,
    pub(crate) durability_warning: bool,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct Fields {
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) note: String,
}

impl fmt::Debug for Fields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntryFields")
            .finish_non_exhaustive()
    }
}

impl Fields {
    fn read(entry: Node<'_, '_>) -> io::Result<Self> {
        Ok(Self {
            title: bounded_field(entry, "title")?.into_owned(),
            text: bounded_field(entry, "text")?.into_owned(),
            note: bounded_field(entry, "userNote")?.into_owned(),
        })
    }

    fn normalized(self) -> io::Result<Self> {
        let normalize = |text: String| -> io::Result<String> {
            if text.len() > MAX_FIELD_BYTES {
                return Err(bad_field());
            }
            let trimmed = text.trim_matches(java_whitespace);
            validate_field(trimmed)?;
            Ok(if trimmed.len() == text.len() {
                text
            } else {
                trimmed.to_owned()
            })
        };
        Ok(Self {
            title: normalize(self.title)?,
            text: normalize(self.text)?,
            note: normalize(self.note)?,
        })
    }
}

fn validate_field(text: &str) -> io::Result<()> {
    if text.len() > MAX_FIELD_BYTES || !text.chars().all(xml_character) {
        return Err(bad_field());
    }
    Ok(())
}

fn bad_field() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "A session journal entry field exceeds its text limit or contains invalid XML characters",
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LegacyMarker {
    None,
    Info,
    Important,
    Error,
}

impl LegacyMarker {
    const ALL: [Self; 4] = [Self::None, Self::Info, Self::Important, Self::Error];

    fn parse(value: Option<&str>) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|marker| Some(marker.xml()) == value)
    }

    pub(crate) fn xml(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Info => "INFO",
            Self::Important => "IMPORTANT",
            Self::Error => "ERROR",
        }
    }

    fn builtin(self) -> MarkerChoice {
        let (id, name) = match self {
            Self::None => ("none", "None"),
            Self::Info => ("info", "Info"),
            Self::Important => ("important", "Important"),
            Self::Error => ("error", "Error"),
        };
        MarkerChoice {
            definition: Definition {
                id: id.into(),
                name: name.into(),
                legacy: self,
                color: None,
            },
            profile: false,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct MarkerChoice {
    definition: Definition,
    profile: bool,
}

impl MarkerChoice {
    pub(crate) fn from_definition(definition: &Definition) -> Self {
        if definition.is_builtin() {
            return definition.legacy.builtin();
        }
        Self {
            definition: definition.clone(),
            profile: true,
        }
    }

    pub(crate) fn definition(&self) -> Definition {
        self.definition.clone()
    }

    pub(crate) fn is_profile_definition(&self) -> bool {
        self.profile
    }

    pub(crate) fn id(&self) -> &str {
        &self.definition.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.definition.name
    }

    pub(crate) fn legacy(&self) -> LegacyMarker {
        self.definition.legacy
    }

    pub(crate) fn color(&self) -> Option<[u8; 3]> {
        self.definition.color
    }

    pub(crate) fn is_builtin(&self) -> bool {
        self.definition.is_builtin()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkerSource {
    User,
    Rule,
}

impl MarkerSource {
    pub(crate) fn xml(self) -> &'static str {
        match self {
            Self::User => "USER",
            Self::Rule => "RULE",
        }
    }
}

impl fmt::Display for MarkerChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl fmt::Debug for MarkerChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntryMarker")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct MarkerState {
    pub(crate) legacy: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) source: Option<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct EntryState {
    pub(crate) fields: Fields,
    pub(crate) marker: MarkerState,
}

impl EntryState {
    fn read(entry: Node<'_, '_>) -> io::Result<Self> {
        require_java_fields(
            entry,
            &[
                "title",
                "text",
                "userNote",
                "marker",
                "markerId",
                "markerSource",
            ],
        )?;
        Ok(Self {
            fields: Fields::read(entry)?,
            marker: MarkerState {
                legacy: field(entry, "marker")?.map(|value| value.into_owned()),
                id: field(entry, "markerId")?.map(|value| value.into_owned()),
                source: field(entry, "markerSource")?.map(|value| value.into_owned()),
            },
        })
    }
}

#[derive(Clone)]
pub(crate) struct Draft {
    source: Source,
    id: String,
    original: EntryState,
    row_xml: Arc<Zeroizing<String>>,
    marker: MarkerChoice,
    stored_markers: Arc<[MarkerChoice]>,
    markers: Arc<[MarkerChoice]>,
}

impl Draft {
    /// Reads a single uniquely identified entry from the bound, current source off the UI thread.
    pub(crate) fn read(source: &Source, id: &str) -> io::Result<Self> {
        if id.len() > MAX_FIELD_BYTES || id.chars().all(java_whitespace) {
            return Err(ambiguous());
        }
        source.with_document(|_, document| {
            let entry = find_entry(document, id)?;
            let original = EntryState::read(entry)?;
            let (marker, markers) = marker_choices(document, &original.marker)?;
            let markers: Arc<[_]> = markers.into();
            Ok(Self {
                source: source.clone(),
                id: id.to_owned(),
                original,
                row_xml: Arc::new(Zeroizing::new(
                    document.input_text()[entry.range()].to_owned(),
                )),
                marker,
                stored_markers: Arc::clone(&markers),
                markers,
            })
        })
    }

    #[cfg(test)]
    pub(super) fn read_with_markers(
        source: &Source,
        id: &str,
        definitions: &[Definition],
    ) -> io::Result<Self> {
        Ok(Self::read(source, id)?.with_markers(definitions))
    }

    /// Merges a refreshed profile registry without reopening the journal or losing its baseline.
    pub(crate) fn with_markers(mut self, definitions: &[Definition]) -> Self {
        let mut choices = self
            .stored_markers
            .iter()
            .filter(|marker| marker.is_builtin())
            .cloned()
            .collect::<Vec<_>>();
        for choice in definitions
            .iter()
            .filter(|definition| !definition.is_builtin())
            .take(MAX_DEFINITIONS)
            .map(MarkerChoice::from_definition)
            .chain(
                self.stored_markers
                    .iter()
                    .filter(|marker| !marker.is_builtin())
                    .cloned(),
            )
        {
            if !choices.iter().any(|existing| existing.id() == choice.id()) {
                choices.push(choice);
            }
        }
        let id = self.marker.id();
        let selected = choices.iter().find(|choice| choice.id() == id).cloned();
        self.marker = selected.unwrap_or_else(|| {
            let choice = self
                .stored_markers
                .iter()
                .find(|choice| choice.id() == id)
                .cloned()
                .unwrap_or_else(|| self.marker.clone());
            choices.push(choice.clone());
            choice
        });
        self.markers = choices.into();
        self
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn fields(&self) -> &Fields {
        &self.original.fields
    }

    pub(crate) fn require_translation_source(&self) -> io::Result<()> {
        let check = || {
            self.source.with_document(|_, document| {
                let entry = find_entry(document, &self.id)?;
                require_java_fields(entry, &["userNote"])?;
                if bounded_field(entry, "userNote")?.as_ref() != self.original.fields.note {
                    return Err(invalid("The saved session journal note changed"));
                }
                Ok(())
            })
        };
        match check() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => check(),
            result => result,
        }
    }

    pub(crate) fn marker(&self) -> &MarkerChoice {
        &self.marker
    }

    pub(crate) fn markers(&self) -> &[MarkerChoice] {
        &self.markers
    }

    /// Binds the exact stored entry and its associated asset before confirmation.
    pub(crate) fn prepare_delete(&self) -> io::Result<delete::Deletion> {
        delete::Deletion::prepare(self)
    }

    pub(crate) fn edit(&self, changes: Changes) -> io::Result<Edit> {
        if !self.markers.contains(changes.marker()) {
            return Err(stale_marker());
        }
        let changes = match changes {
            Changes::Fields { fields, marker } => Changes::Fields {
                fields: fields.normalized()?,
                marker,
            },
            Changes::Marker(marker) => Changes::Marker(marker),
        };
        Ok(Edit {
            source: self.source.clone(),
            id: self.id.clone(),
            original: self.original.clone(),
            changes,
        })
    }
}

impl fmt::Debug for Draft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntryEditDraft")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Changes {
    Fields {
        fields: Fields,
        marker: MarkerChoice,
    },
    Marker(MarkerChoice),
}

impl Changes {
    fn marker(&self) -> &MarkerChoice {
        match self {
            Self::Fields { marker, .. } | Self::Marker(marker) => marker,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Edit {
    source: Source,
    id: String,
    original: EntryState,
    changes: Changes,
}

impl Edit {
    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn changes(&self) -> &Changes {
        &self.changes
    }

    pub(crate) fn into_changes(self) -> Changes {
        self.changes
    }

    pub(crate) fn require_current(&self, current: &EntryState) -> io::Result<()> {
        if self.original != *current {
            return Err(invalid(
                "The session journal entry changed. Close this editor and reopen the entry.",
            ));
        }
        Ok(())
    }

    pub(crate) fn redact_fields(&mut self, redactor: &Redactor) -> io::Result<()> {
        let Changes::Fields { fields, .. } = &mut self.changes else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_millis(100);
        let redact = |text: &str| -> io::Result<String> {
            let redacted = redactor.journal_text(text, deadline).ok_or_else(|| {
                invalid("Could not safely apply the journal entry redaction rules")
            })?;
            validate_field(&redacted)?;
            Ok(redacted.to_string())
        };
        *fields = Fields {
            title: redact(&fields.title)?,
            text: redact(&fields.text)?,
            note: redact(&fields.note)?,
        };
        Ok(())
    }
}

impl fmt::Debug for Edit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("EntryEdit").finish_non_exhaustive()
    }
}

/// Applies a guarded edit while holding the shared native writer lock. Call off the UI thread.
pub(crate) fn save(edit: &Edit) -> io::Result<()> {
    let source = edit.source();
    let directory = source.directory();
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected session journal identity changed"));
    }
    let updated = patch(&xml, &document, edit, Timestamp::now())?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        source.require_location(directory, opened)
    })
}

pub(super) fn lock_source(source: &Source) -> io::Result<MetadataLock> {
    let directory = source.directory();
    let opened = Directory::open(directory, false)?;
    source.require_location(directory, &opened)?;
    lock_metadata_in(directory, opened)
}

pub(super) fn entries<'a, 'input>(
    document: &'a Document<'input>,
) -> impl Iterator<Item = Node<'a, 'input>> {
    child(document.root_element(), "entries")
        .into_iter()
        .flat_map(|wrapper| wrapper.children())
        .filter(|node| node.has_tag_name("entry"))
}

pub(super) fn find_entry<'a, 'input>(
    document: &'a Document<'input>,
    id: &str,
) -> io::Result<Node<'a, 'input>> {
    let wrapper = child(document.root_element(), "entries").ok_or_else(ambiguous)?;
    require_java_fields(wrapper, &[])?;
    let mut found = None;
    for entry in entries(document) {
        if field(entry, "id")?.as_deref() == Some(id) {
            require_java_fields(entry, &["id"])?;
            if found.replace(entry).is_some() {
                return Err(ambiguous());
            }
        }
    }
    found.ok_or_else(ambiguous)
}

fn ambiguous() -> io::Error {
    invalid("The selected session journal entry is missing or ambiguous")
}

fn stale_marker() -> io::Error {
    invalid("The selected session journal marker is unavailable or has changed")
}

pub(super) fn require_java_fields(parent: Node<'_, '_>, names: &[&str]) -> io::Result<()> {
    if parent.tag_name().namespace().is_some()
        || parent.children().any(|node| {
            node.is_element()
                && node.tag_name().namespace().is_some()
                && names.contains(&node.tag_name().name())
        })
    {
        return Err(invalid(
            "A session journal entry field collides with a namespaced extension",
        ));
    }
    Ok(())
}

fn marker_choices(
    document: &Document<'_>,
    marker: &MarkerState,
) -> io::Result<(MarkerChoice, Vec<MarkerChoice>)> {
    let legacy = LegacyMarker::parse(marker.legacy.as_deref()).unwrap_or(LegacyMarker::None);
    let selected_id = marker.id.as_deref().and_then(normalize_marker);
    let mut choices: Vec<_> = LegacyMarker::ALL
        .into_iter()
        .map(LegacyMarker::builtin)
        .collect();
    let mut preferred = None;
    if let Some(wrapper) = child(document.root_element(), "markerDefinitions") {
        require_java_fields(wrapper, &[])?;
        for definition in wrapper
            .children()
            .filter(|node| node.has_tag_name("marker") && node.tag_name().namespace().is_none())
        {
            require_java_fields(definition, &["id", "name", "legacyMarker", "color"])?;
            let Some(id) = normalize_marker(&bounded_field(definition, "id")?) else {
                continue;
            };
            let selected = Some(id.as_str()) == selected_id.as_deref();
            if (!selected && choices.len() >= MAX_MARKER_CHOICES - 1)
                || choices.iter().any(|choice| choice.id() == id)
                || (selected && preferred.is_some())
            {
                continue;
            }
            let choice = MarkerChoice {
                definition: read_snapshot(definition, id)?,
                profile: false,
            };
            if selected {
                preferred = Some(choice.clone());
            }
            if choices.len() < MAX_MARKER_CHOICES - 1 {
                choices.push(choice);
            }
        }
    }
    let selected = if let Some(id) = selected_id {
        choices
            .iter()
            .find(|choice| choice.id() == id)
            .cloned()
            .or(preferred)
            .unwrap_or_else(|| MarkerChoice {
                definition: Definition {
                    name: id.clone(),
                    id,
                    legacy,
                    color: None,
                },
                profile: false,
            })
    } else {
        legacy.builtin()
    };
    if !choices.contains(&selected) {
        choices.push(selected.clone());
    }
    Ok((selected, choices))
}

fn read_snapshot(node: Node<'_, '_>, id: String) -> io::Result<Definition> {
    let name = display_text(&bounded_field(node, "name")?);
    let name: String = name.chars().take(MAX_MARKER_LABEL).collect();
    Ok(Definition {
        name: if name.trim_matches(java_whitespace).is_empty() {
            id.clone()
        } else {
            name
        },
        legacy: LegacyMarker::parse(field(node, "legacyMarker")?.as_deref())
            .unwrap_or(LegacyMarker::Info),
        color: marker_rgb(&bounded_field(node, "color")?),
        id,
    })
}

fn marker_values(
    definition: &Definition,
    source: MarkerSource,
) -> [(&'static str, Option<&str>); 3] {
    [
        ("marker", Some(definition.legacy.xml())),
        (
            "markerId",
            (!definition.is_builtin()).then_some(definition.id.as_str()),
        ),
        ("markerSource", Some(source.xml())),
    ]
}

fn patch(xml: &str, document: &Document<'_>, edit: &Edit, time: Timestamp) -> io::Result<String> {
    let entry = find_entry(document, edit.id())?;
    let state = EntryState::read(entry)?;
    edit.require_current(&state)?;
    let marker = edit.changes().marker();
    if !marker.is_profile_definition() {
        let (_, choices) = marker_choices(document, &state.marker)?;
        if !choices.contains(marker) {
            return Err(stale_marker());
        }
    }

    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let mut errors = usize::from(marker.legacy() == LegacyMarker::Error);
    for other in
        entries(document).filter(|other| *other != entry && other.tag_name().namespace().is_none())
    {
        require_java_fields(other, &["marker"])?;
        if field(other, "marker")?.as_deref() == Some("ERROR") {
            errors += 1;
        }
    }
    let errors = errors.to_string();
    let edited_at = time.to_string();
    let mut values = marker_values(&marker.definition, MarkerSource::User).to_vec();
    values.push(("editedAt", Some(edited_at.as_str())));
    if let Changes::Fields { fields, .. } = edit.changes() {
        values.extend([
            ("title", nonempty(&fields.title)),
            ("text", nonempty(&fields.text)),
            ("userNote", nonempty(&fields.note)),
        ]);
    }
    let mut patches = Vec::new();
    patch_scalars(xml, entry, values, &mut patches)?;
    patch_scalars(
        xml,
        meta,
        [("errorCount", Some(errors.as_str()))],
        &mut patches,
    )?;
    if marker.is_profile_definition() {
        marker_rules::patch_snapshots(xml, document, &[&marker.definition], &mut patches)?;
    }
    let updated = apply_patches(xml, patches)?;
    parse_document(&updated)?;
    Ok(updated)
}

fn nonempty(text: &str) -> Option<&str> {
    (!text.is_empty()).then_some(text)
}

pub(super) fn patch_scalars<'a>(
    xml: &str,
    parent: Node<'_, '_>,
    values: impl IntoIterator<Item = (&'static str, Option<&'a str>)>,
    patches: &mut Vec<(Range<usize>, String)>,
) -> io::Result<()> {
    let mut additions = String::new();
    for (name, value) in values {
        require_java_fields(parent, &[name])?;
        let mut nodes = parent.children().filter(|node| node.has_tag_name(name));
        let node = nodes.next();
        if nodes.next().is_some() || node.is_some_and(|node| node.first_element_child().is_some()) {
            return Err(invalid("Invalid session journal entry scalar field"));
        }
        let element = value.map(|value| format!("<{name}>{}</{name}>", escape(value)));
        if let Some(node) = node {
            patches.push((node.range(), element.unwrap_or_default()));
        } else if let Some(element) = element {
            additions.push_str(&element);
        }
    }
    append_children(xml, parent, additions, patches)
}

pub(super) fn append_children(
    xml: &str,
    parent: Node<'_, '_>,
    mut additions: String,
    patches: &mut Vec<(Range<usize>, String)>,
) -> io::Result<()> {
    if additions.is_empty() {
        return Ok(());
    }
    let range = parent.range();
    let source = &xml[range.clone()];
    if source.ends_with("/>") {
        additions.insert(0, '>');
        additions.push_str(&format!("</{}>", parent.tag_name().name()));
        patches.push((range.end - 2..range.end, additions));
    } else {
        let close = source
            .rfind("</")
            .ok_or_else(|| invalid("Invalid session journal entry element"))?;
        let offset = range.start + close;
        patches.push((offset..offset, additions));
    }
    Ok(())
}

pub(super) fn apply_patches(
    xml: &str,
    mut patches: Vec<(Range<usize>, String)>,
) -> io::Result<String> {
    patches.sort_unstable_by_key(|(range, _)| (range.start, range.end));
    let mut length = xml.len();
    let mut previous = 0;
    for (range, text) in &patches {
        if range.start < previous || xml.get(range.clone()).is_none() {
            return Err(invalid("Invalid session journal edit range"));
        }
        length = length
            .checked_sub(range.len())
            .and_then(|length| length.checked_add(text.len()))
            .ok_or_else(|| invalid("Session journal file exceeds the size limit"))?;
        previous = range.end;
    }
    if length as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Session journal file exceeds the size limit"));
    }
    let mut updated = String::with_capacity(length);
    let mut previous = 0;
    for (range, text) in patches {
        updated.push_str(&xml[previous..range.start]);
        updated.push_str(&text);
        previous = range.end;
    }
    updated.push_str(&xml[previous..]);
    Ok(updated)
}

#[cfg(test)]
mod tests;
