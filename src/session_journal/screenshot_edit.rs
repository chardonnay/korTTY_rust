//! Reviewed edits of a journal screenshot and its annotation XML.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::sync::Arc;

use jiff::Timestamp;
use roxmltree::Node;
use tiny_skia::Pixmap;

use crate::private_files::{Directory, FileStamp};

use super::annotation::{self, Annotation};
use super::entry_edit::{
    MAX_FIELD_BYTES, append_children, apply_patches, entries, find_entry, lock_source,
    patch_scalars, require_java_fields,
};
use super::export::screenshot_bytes;
use super::metadata::{java_whitespace, xml_character};
use super::viewer::{Screenshot, Source, screenshot::open_image};
use super::{MAX_PNG_BYTES, bounded_field, child, document_id, invalid, parse_document};

mod transaction;
pub(crate) use transaction::XmlWrite;

const SCREENSHOTS: &str = "screenshots";

pub(super) fn annotation_stage_name(filename: &str) -> String {
    format!(".kortty-annotation-{}.stage", filename.to_ascii_lowercase())
}

struct Asset {
    stamp: FileStamp,
    bytes: Vec<u8>,
}

enum Original {
    Missing,
    Available(Asset),
    Unavailable,
}

/// The current row, visible image and original as observed together off the UI thread.
pub(crate) struct Draft {
    screenshot: Arc<Screenshot>,
    original: Original,
    annotations: Vec<Annotation>,
    note: String,
    exclusive: bool,
    pending: bool,
}

impl Draft {
    pub(crate) fn read(source: &Source, id: &str, reference: &str) -> io::Result<Self> {
        Self::from_reviewed(Arc::new(Screenshot::review(source, id, reference)?))
    }

    pub(crate) fn from_reviewed(screenshot: Arc<Screenshot>) -> io::Result<Self> {
        let source = screenshot.source();
        let original_name = original_name(screenshot.filename());
        let read = || {
            source.with_document(|_, xml| {
                let entry = find_entry(xml, screenshot.id())?;
                screenshot.require_entry(entry)?;
                let annotations = annotation::read(entry)?;
                let note = read_note(entry)?;
                let directory = Directory::open(source.directory(), false)?;
                source.require_location(source.directory(), &directory)?;
                let folder = screenshot_folder(&directory)?;
                screenshot.require_image(&folder)?;
                let pending = folder.has_entry(&annotation_stage_name(screenshot.filename()))?;
                let original = match read_asset(&folder, &original_name) {
                    Ok(Some(asset)) => Original::Available(asset),
                    Ok(None) => Original::Missing,
                    Err(_) => Original::Unavailable,
                };
                let exclusive =
                    entries(xml)
                        .filter(|row| *row != entry)
                        .try_fold(true, |exclusive, row| {
                            let reference = bounded_field(row, "screenshotFile")?;
                            Ok::<_, io::Error>(
                                exclusive && !is_alias(&screenshot, &original_name, &reference),
                            )
                        })?;
                source.require_location(source.directory(), &directory)?;
                if !folder.same_as(&screenshot_folder(&directory)?)? {
                    return Err(changed());
                }
                screenshot.require_image(&folder)?;
                Ok((original, annotations, note, exclusive, pending))
            })
        };
        let (original, annotations, note, exclusive, pending) = match read() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => read(),
            result => result,
        }?;
        Ok(Self {
            screenshot,
            original,
            annotations,
            note,
            exclusive,
            pending,
        })
    }

    pub(crate) fn source(&self) -> &Source {
        self.screenshot.source()
    }

    pub(crate) fn id(&self) -> &str {
        self.screenshot.id()
    }

    pub(crate) fn reference(&self) -> &str {
        self.screenshot.reference()
    }

    pub(crate) fn visible(&self) -> &[u8] {
        self.screenshot.bytes()
    }

    pub(crate) fn original(&self) -> Option<&[u8]> {
        if self.pending || !self.exclusive {
            return None;
        }
        match &self.original {
            Original::Available(asset) if asset.bytes[16..24] == self.visible()[16..24] => {
                Some(&asset.bytes)
            }
            Original::Missing if self.annotations.is_empty() => Some(self.visible()),
            _ => None,
        }
    }

    pub(crate) fn can_annotate(&self) -> bool {
        self.original().is_some()
    }

    pub(crate) fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub(crate) fn note(&self) -> &str {
        &self.note
    }

    /// Prepares an immutable edit. PNG decoding and drawing run on the caller's worker.
    pub(crate) fn edit(
        self: &Arc<Self>,
        annotations: Vec<Annotation>,
        mut note: String,
    ) -> io::Result<Edit> {
        if note != self.note {
            if note.len() > MAX_FIELD_BYTES || !note.chars().all(xml_character) {
                return Err(invalid(
                    "The screenshot note exceeds its limit or contains invalid XML",
                ));
            }
            let trimmed = note.trim_matches(java_whitespace);
            if trimmed.len() != note.len() {
                note = trimmed.to_owned();
            }
        }
        let image = if annotations == self.annotations {
            None
        } else {
            let original = self.original().ok_or_else(|| {
                if self.pending {
                    transaction::recovery_required(self.screenshot.filename())
                } else {
                    invalid("The untouched screenshot is unavailable or shared by another entry")
                }
            })?;
            if annotations.is_empty() {
                Some(ImageEdit::Restore)
            } else {
                let original = Pixmap::decode_png(original)
                    .map_err(|_| invalid("Could not decode the untouched screenshot PNG"))?;
                let image = annotation::render(&original, &annotations)?
                    .encode_png()
                    .map_err(|_| invalid("Could not encode the annotated screenshot PNG"))?;
                if image.len() as u64 > MAX_PNG_BYTES {
                    return Err(invalid(
                        "The annotated screenshot exceeds its PNG size limit",
                    ));
                }
                Some(ImageEdit::Render(image))
            }
        };
        Ok(Edit {
            draft: Arc::clone(self),
            annotations,
            note,
            image,
        })
    }
}

impl fmt::Debug for Draft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScreenshotEditDraft")
            .finish_non_exhaustive()
    }
}

enum ImageEdit {
    Render(Vec<u8>),
    Restore,
}

/// Immutable edit prepared from a reviewed row and image pair.
pub(crate) struct Edit {
    draft: Arc<Draft>,
    annotations: Vec<Annotation>,
    note: String,
    image: Option<ImageEdit>,
}

impl Edit {
    pub(crate) fn source(&self) -> &Source {
        self.draft.source()
    }

    pub(crate) fn id(&self) -> &str {
        self.draft.id()
    }

    pub(crate) fn reference(&self) -> &str {
        self.draft.reference()
    }

    pub(crate) fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub(crate) fn note(&self) -> &str {
        &self.note
    }

    pub(crate) fn annotations_changed(&self) -> bool {
        self.image.is_some()
    }

    pub(crate) fn note_changed(&self) -> bool {
        self.note != self.draft.note
    }

    pub(crate) fn require_row(
        &self,
        created_at: Option<&str>,
        annotations: &[Annotation],
        note: &str,
    ) -> io::Result<()> {
        if created_at != self.draft.screenshot.created_at()
            || annotations != self.draft.annotations
            || note != self.draft.note
        {
            return Err(changed());
        }
        Ok(())
    }

    /// Shared by native writer and lossless XML editor. Call with the other rows' references.
    pub(crate) fn require_exclusive<'a>(
        &self,
        mut references: impl Iterator<Item = &'a str>,
    ) -> io::Result<()> {
        if self.annotations_changed() {
            let screenshot = &self.draft.screenshot;
            let original_name = original_name(screenshot.filename());
            if references.any(|reference| is_alias(screenshot, &original_name, reference)) {
                return Err(invalid(
                    "Another journal entry shares the reviewed screenshot",
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Edit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScreenshotEdit")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) durability_warning: bool,
    pub(crate) cleanup_warning: bool,
}

/// Commits a stopped journal edit under the same lock used by the recording actor.
pub(crate) fn apply(edit: &Edit) -> io::Result<Outcome> {
    let source = edit.source();
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(changed());
    }
    let entry = find_entry(&document, edit.id())?;
    let screenshot = &edit.draft.screenshot;
    screenshot.require_entry(entry)?;
    edit.require_row(
        screenshot.created_at(),
        &annotation::read(entry)?,
        &read_note(entry)?,
    )?;
    if edit.annotations_changed() {
        let references = entries(&document)
            .filter(|row| *row != entry)
            .map(|row| bounded_field(row, "screenshotFile"))
            .collect::<io::Result<Vec<_>>>()?;
        edit.require_exclusive(references.iter().map(|reference| reference.as_ref()))?;
    }
    if !edit.annotations_changed() && !edit.note_changed() {
        return Ok(Outcome::default());
    }
    let updated = patch(&xml, entry, edit)?;
    let mut published = None;
    let directory = lock.directory();
    edit.publish_with(directory, |operation| {
        lock.require_current(source.directory())?;
        source.require_location(source.directory(), directory)?;
        match operation {
            XmlWrite::Check => {
                lock.require_document(published.as_ref().unwrap_or(&stamp))?;
                Ok(false)
            }
            XmlWrite::Commit(verify) => {
                lock.publish_document(&stamp, Some(&updated), |opened| {
                    source.require_location(source.directory(), opened)?;
                    verify()
                })?;
                published = lock
                    .read_document()
                    .ok()
                    .filter(|(_, current)| current == &updated)
                    .map(|(stamp, _)| stamp);
                Ok(directory.sync().is_err())
            }
        }
    })
}

fn patch(xml: &str, entry: Node<'_, '_>, edit: &Edit) -> io::Result<String> {
    let time = Timestamp::now().to_string();
    let mut values = vec![("editedAt", Some(time.as_str()))];
    if edit.note_changed() {
        values.push(("userNote", (!edit.note().is_empty()).then_some(edit.note())));
    }
    let mut patches = Vec::new();
    patch_scalars(xml, entry, values, &mut patches)?;
    if edit.annotations_changed() {
        let annotations = annotation::xml(edit.annotations())?;
        if let Some(wrapper) = child(entry, "annotations") {
            patches.push((wrapper.range(), annotations));
        } else {
            append_children(xml, entry, annotations, &mut patches)?;
        }
    }
    let updated = apply_patches(xml, patches)?;
    parse_document(&updated)?;
    Ok(updated)
}

fn read_note(entry: Node<'_, '_>) -> io::Result<String> {
    require_java_fields(entry, &["userNote"])?;
    Ok(bounded_field(entry, "userNote")?.into_owned())
}

fn is_alias(screenshot: &Screenshot, original: &str, reference: &str) -> bool {
    reference.eq_ignore_ascii_case(screenshot.reference())
        || reference.split_once('/').is_some_and(|(dir, name)| {
            dir.eq_ignore_ascii_case(SCREENSHOTS) && name.eq_ignore_ascii_case(original)
        })
}

fn original_name(filename: &str) -> String {
    format!(
        "{}.orig.png",
        filename.strip_suffix(".png").unwrap_or(filename)
    )
}

fn screenshot_folder(journal: &Directory) -> io::Result<Directory> {
    journal.deletion_child(OsStr::new(SCREENSHOTS))
}

fn read_asset(folder: &Directory, name: &str) -> io::Result<Option<Asset>> {
    let opened = match open_image(folder, name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !folder.has_entry(name)? => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let stamp = FileStamp::read(&opened)?;
    let bytes = screenshot_bytes(
        || {
            let file = open_image(folder, name)?;
            stamp.require_unchanged(&file)?;
            Ok(file)
        },
        MAX_PNG_BYTES,
    )?;
    stamp.require_unchanged(&open_image(folder, name)?)?;
    Ok(Some(Asset { stamp, bytes }))
}

fn changed() -> io::Error {
    invalid("The reviewed journal screenshot or its entry changed")
}

#[cfg(test)]
mod tests;
