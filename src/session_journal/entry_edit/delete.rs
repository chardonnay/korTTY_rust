//! An exact, confirmed removal of one timeline entry, followed by verified image cleanup.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;

use roxmltree::{Document, Node};
use zeroize::Zeroizing;

use super::{
    Directory, Draft, EntryState, MetadataLock, Source, apply_patches, child, document_id, entries,
    field, find_entry, invalid, lock_source, normalize_marker, parse_document, patch_scalars,
    require_java_fields,
};

pub(in crate::session_journal) mod screenshot;

use screenshot::{Cleanup, Review};

pub(crate) struct Deletion {
    source: Source,
    id: String,
    original: EntryState,
    row_xml: Arc<Zeroizing<String>>,
    screenshot: Option<String>,
    reviewed_image: Result<Option<Review>, ()>,
}

impl fmt::Debug for Deletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntryDeletion")
            .finish_non_exhaustive()
    }
}

impl Deletion {
    pub(super) fn prepare(draft: &Draft) -> io::Result<Self> {
        draft.source().with_document(|_, document| {
            let entry = find_entry(document, draft.id())?;
            let current = EntryState::read(entry)?;
            if draft.original != current || draft.row_xml.as_str() != row_xml(document, entry) {
                return Err(changed());
            }
            let screenshot = screenshot_field(entry)?.map(Cow::into_owned);
            let reviewed_image = if let Some(reference) = screenshot.as_deref() {
                let directory = Directory::open(draft.source().directory(), false)?;
                draft
                    .source()
                    .require_location(draft.source().directory(), &directory)?;
                Review::read(&directory, reference).map_err(|_| ())
            } else {
                Ok(None)
            };
            Ok(Self {
                source: draft.source().clone(),
                id: draft.id().into(),
                original: current,
                row_xml: Arc::clone(&draft.row_xml),
                screenshot,
                reviewed_image,
            })
        })
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn require_current(
        &self,
        current: &EntryState,
        screenshot: Option<&str>,
        row_xml: &str,
    ) -> io::Result<()> {
        if self.original != *current
            || self.screenshot.as_deref() != screenshot
            || self.row_xml.as_str() != row_xml
        {
            return Err(changed());
        }
        Ok(())
    }

    pub(in crate::session_journal) fn image_cleanup<'a>(
        &self,
        journal: &Directory,
        remaining: impl Iterator<Item = &'a str>,
    ) -> io::Result<Option<Cleanup>> {
        let Some(reference) = self.screenshot.as_deref() else {
            return Ok(None);
        };
        let review = self
            .reviewed_image
            .as_ref()
            .map_err(|()| invalid("Could not verify the selected session journal screenshot"))?;
        Cleanup::prepare(journal, reference, remaining, review.as_ref())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) cleanup_warning: bool,
    pub(crate) durability_warning: bool,
}

/// Commits the XML first, then cleans up the unshared image under the same journal lock.
pub(crate) fn commit(deletion: &Deletion) -> io::Result<Outcome> {
    let source = deletion.source();
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected session journal identity changed"));
    }
    let entry = find_entry(&document, deletion.id())?;
    let current = EntryState::read(entry)?;
    deletion.require_current(
        &current,
        screenshot_field(entry)?.as_deref(),
        row_xml(&document, entry),
    )?;

    let updated = remove_entry(&xml, &document, entry)?;
    let cleanup = prepare_image_cleanup(deletion, lock.directory(), &document, entry);
    drop(document);
    drop(xml);
    lock.publish_document(&stamp, Some(&updated), |opened| {
        source.require_location(source.directory(), opened)
    })?;
    if lock.directory().sync().is_err() {
        return Ok(Outcome {
            durability_warning: true,
            ..Outcome::default()
        });
    }
    let cleanup_warning = match cleanup {
        Ok(None) => false,
        Ok(Some(cleanup)) => remove_published_image(cleanup, &lock, source, &updated).is_err(),
        Err(_) => true,
    };
    Ok(Outcome {
        cleanup_warning,
        durability_warning: false,
    })
}

fn remove_published_image(
    cleanup: Cleanup,
    lock: &MetadataLock,
    source: &Source,
    published: &str,
) -> io::Result<()> {
    let (stamp, current) = lock.read_document()?;
    if current != published {
        return Err(invalid("The session journal changed before image cleanup"));
    }
    cleanup.remove(lock.directory(), || {
        source.require_location(source.directory(), lock.directory())?;
        lock.require_current(source.directory())?;
        lock.require_document(&stamp)
    })
}

fn prepare_image_cleanup(
    deletion: &Deletion,
    directory: &Directory,
    document: &Document<'_>,
    selected: Node<'_, '_>,
) -> io::Result<Option<Cleanup>> {
    if deletion.screenshot.is_none() {
        return Ok(None);
    }
    let mut references = Vec::new();
    for entry in remaining_entries(document, selected) {
        if let Some(reference) = screenshot_field(entry)? {
            references.push(reference);
        }
    }
    deletion.image_cleanup(
        directory,
        references.iter().map(|reference| reference.as_ref()),
    )
}

fn row_xml<'input>(document: &Document<'input>, entry: Node<'_, '_>) -> &'input str {
    &document.input_text()[entry.range()]
}

fn screenshot_field<'a>(entry: Node<'a, '_>) -> io::Result<Option<Cow<'a, str>>> {
    require_java_fields(entry, &["screenshotFile"])?;
    Ok(field(entry, "screenshotFile")?.filter(|value| !value.is_empty()))
}

fn changed() -> io::Error {
    invalid("The session journal entry changed. Reopen the entry before deleting it.")
}

fn remaining_entries<'a, 'input>(
    document: &'a Document<'input>,
    selected: Node<'a, 'input>,
) -> impl Iterator<Item = Node<'a, 'input>> {
    entries(document)
        .filter(move |entry| *entry != selected && entry.tag_name().namespace().is_none())
}

fn remove_entry(xml: &str, document: &Document<'_>, selected: Node<'_, '_>) -> io::Result<String> {
    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let mut errors = 0;
    let mut used_markers = HashSet::new();
    for entry in remaining_entries(document, selected) {
        require_java_fields(entry, &["marker", "markerId"])?;
        if field(entry, "marker")?.as_deref() == Some("ERROR") {
            errors += 1;
        }
        if let Some(id) = field(entry, "markerId")?
            .as_deref()
            .and_then(normalize_marker)
        {
            used_markers.insert(id);
        }
    }

    let mut patches = vec![(selected.range(), String::new())];
    if let Some(wrapper) = child(document.root_element(), "markerDefinitions") {
        require_java_fields(wrapper, &[])?;
        for definition in wrapper
            .children()
            .filter(|node| node.has_tag_name("marker") && node.tag_name().namespace().is_none())
        {
            require_java_fields(definition, &["id"])?;
            let id = field(definition, "id")?;
            if id.as_deref().is_none_or(|id| !used_markers.contains(id)) {
                patches.push((definition.range(), String::new()));
            }
        }
    }
    patch_scalars(
        xml,
        meta,
        [("errorCount", Some(errors.to_string().as_str()))],
        &mut patches,
    )?;
    let updated = apply_patches(xml, patches)?;
    parse_document(&updated)?;
    Ok(updated)
}

#[cfg(test)]
mod tests;
