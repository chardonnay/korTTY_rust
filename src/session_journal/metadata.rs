//! Bounded edits that preserve imported journal XML outside the selected metadata field.

use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use roxmltree::Node;

use super::viewer::Source;
use super::{
    DOCUMENT, Journal, MAX_DOCUMENT_BYTES, boolean, check_path, child, document_id, invalid,
    journal, parse_document,
};
pub(super) use crate::private_files::require_single_link;
use crate::private_files::{Directory, FileStamp};

pub(super) const LOCK: &str = ".kortty-journal.lock";
const MAX_FIELD_BYTES: usize = 256 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum MetadataEdit {
    Title(String),
    Description(String),
    Pinned(bool),
}

impl fmt::Debug for MetadataEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Title(_) => formatter.write_str("Title([redacted])"),
            Self::Description(_) => formatter.write_str("Description([redacted])"),
            Self::Pinned(pinned) => formatter.debug_tuple("Pinned").field(pinned).finish(),
        }
    }
}

impl MetadataEdit {
    /// Validates XML 1.0 text and applies the Java title/description whitespace rules.
    pub(crate) fn normalized(&self) -> io::Result<Self> {
        if self.field().1.len() > MAX_FIELD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Session journal metadata exceeds the text limit",
            ));
        }
        let (text, title) = match self {
            Self::Title(text) => (text.trim_matches(java_whitespace), true),
            Self::Description(text) if text.chars().all(java_whitespace) => ("", false),
            Self::Description(text) => (text.as_str(), false),
            Self::Pinned(pinned) => return Ok(Self::Pinned(*pinned)),
        };
        if !text.chars().all(xml_character) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Session journal metadata exceeds the text limit or contains invalid XML characters",
            ));
        }
        if title && text.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A session journal title must contain text",
            ));
        }
        Ok(if title {
            Self::Title(text.into())
        } else {
            Self::Description(text.into())
        })
    }

    fn field(&self) -> (&'static str, &str) {
        match self {
            Self::Title(text) => ("title", text),
            Self::Description(text) => ("description", text),
            Self::Pinned(true) => ("pinned", "true"),
            Self::Pinned(false) => ("pinned", "false"),
        }
    }
}

/// A cooperating process lock held through the writer's final save or an offline edit.
/// The lock file stays in place so another opener always locks the same file identity.
pub(crate) struct MetadataLock {
    directory: Directory,
    path: PathBuf,
    file: File,
    stamp: FileStamp,
}

impl Drop for MetadataLock {
    fn drop(&mut self) {
        // A forked worker can retain the descriptor until it execs.
        let _ = self.file.unlock();
    }
}

impl MetadataLock {
    pub(super) fn directory(&self) -> &Directory {
        &self.directory
    }

    pub(super) fn read_document(&self) -> io::Result<(FileStamp, String)> {
        let file = self.directory.open_file(DOCUMENT)?;
        Self::read_open_document(&file)
    }

    pub(super) fn read_writable_private_document(&self) -> io::Result<Option<(FileStamp, String)>> {
        let file = self.directory.open_private_file(DOCUMENT)?;
        if file.metadata()?.permissions().readonly() {
            return Ok(None);
        }
        Self::read_open_document(&file).map(Some)
    }

    fn read_open_document(file: &File) -> io::Result<(FileStamp, String)> {
        require_single_link(file)?;
        let stamp = FileStamp::read(file)?;
        if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("Session journal file exceeds the size limit"));
        }
        let mut xml = String::new();
        file.take(MAX_DOCUMENT_BYTES + 1).read_to_string(&mut xml)?;
        if xml.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(invalid("Session journal file exceeds the size limit"));
        }
        stamp.require_unchanged(file)?;
        Ok((stamp, xml))
    }

    pub(super) fn publish_document(
        &self,
        stamp: &FileStamp,
        xml: Option<&str>,
        validate: impl Fn(&Directory) -> io::Result<()>,
    ) -> io::Result<()> {
        let require_current = || {
            validate(&self.directory)?;
            self.require_current(&self.path)?;
            self.require_document(stamp)
        };
        if let Some(xml) = xml {
            self.directory.replace_with(
                DOCUMENT,
                |file| file.write_all(xml.as_bytes()),
                require_current,
            )?;
        } else {
            require_current()?;
        }
        Ok(())
    }

    pub(super) fn require_document(&self, stamp: &FileStamp) -> io::Result<()> {
        let current = self.directory.open_file(DOCUMENT)?;
        require_single_link(&current)?;
        stamp.require_unchanged(&current)
    }

    /// Removes an owned startup lock only after all unpublished document data was discarded.
    pub(crate) fn discard_unpublished(self) -> io::Result<()> {
        self.require_current(&self.path)?;
        if self.directory.has_entry(DOCUMENT)? {
            return Err(invalid(
                "A published session journal lock must remain in place",
            ));
        }
        self.directory.remove_unchanged(LOCK, &self.stamp)
    }

    pub(crate) fn require_current(&self, path: &Path) -> io::Result<()> {
        check_path(path)?;
        if !self.directory.same_as(&Directory::open(path, false)?)? {
            return Err(invalid("The session journal directory changed"));
        }
        self.stamp.require_unchanged(&self.file)?;
        let current = self.directory.open_private_file(LOCK)?;
        require_single_link(&current)?;
        self.stamp.require_unchanged(&current)
    }

    #[cfg(unix)]
    pub(crate) fn same_as_file(&self, file: &File) -> io::Result<bool> {
        self.directory.same_as_file(file)
    }

    #[cfg(windows)]
    pub(crate) fn same_as_directory(
        &self,
        directory: &kortty_files::tree::Directory,
    ) -> io::Result<bool> {
        self.directory.same_as_directory(directory)
    }
}

/// Acquires the journal's nonblocking writer/edit lock without following symlinks.
pub(crate) fn lock_metadata(directory: &Path) -> io::Result<MetadataLock> {
    check_path(directory)?;
    lock_metadata_in(directory, Directory::open(directory, false)?)
}

pub(super) fn lock_metadata_in(directory: &Path, opened: Directory) -> io::Result<MetadataLock> {
    let file = opened.open_lock(LOCK)?;
    lock_opened_metadata(directory, opened, file)
}

pub(super) fn lock_existing_metadata_in(
    directory: &Path,
    opened: Directory,
) -> io::Result<MetadataLock> {
    let file = opened.open_existing_lock(LOCK)?;
    lock_opened_metadata(directory, opened, file)
}

fn lock_opened_metadata(
    directory: &Path,
    opened: Directory,
    file: File,
) -> io::Result<MetadataLock> {
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    file.try_lock().map_err(io::Error::from)?;
    let lock = MetadataLock {
        stamp,
        directory: opened,
        path: directory.into(),
        file,
    };
    lock.require_current(directory)?;
    Ok(lock)
}

/// Performs blocking disk work. Call from a worker, never from the UI thread.
/// An active native writer excludes this operation through the same journal lock.
pub(crate) fn edit_metadata(
    directory: &Path,
    expected_id: &str,
    edit: &MetadataEdit,
) -> io::Result<Journal> {
    PendingEdit::prepare(directory, expected_id, edit)?.publish()
}

/// Edits through an already verified directory handle so a replacement path cannot acquire a lock.
pub(crate) fn edit_metadata_in(
    directory: &Path,
    opened: Directory,
    source: &Source,
    edit: &MetadataEdit,
) -> io::Result<Journal> {
    source.require_location(directory, &opened)?;
    PendingEdit::prepare_in(directory, opened, source.id(), edit)?
        .publish_checked(|opened| source.require_location(directory, opened))
}

struct PendingEdit {
    lock: MetadataLock,
    stamp: FileStamp,
    xml: Option<String>,
    journal: Journal,
}

impl PendingEdit {
    fn prepare(directory: &Path, expected_id: &str, edit: &MetadataEdit) -> io::Result<Self> {
        check_path(directory)?;
        Self::prepare_in(
            directory,
            Directory::open(directory, false)?,
            expected_id,
            edit,
        )
    }

    fn prepare_in(
        directory: &Path,
        opened: Directory,
        expected_id: &str,
        edit: &MetadataEdit,
    ) -> io::Result<Self> {
        let edit = edit.normalized()?;
        let lock = lock_metadata_in(directory, opened)?;
        let (stamp, xml) = lock.read_document()?;
        let document = parse_document(&xml)?;
        if document_id(&document)? != expected_id {
            return Err(invalid("The selected session journal identity changed"));
        }
        let meta = child(document.root_element(), "meta")
            .ok_or_else(|| invalid("Missing session journal metadata"))?;
        let (xml, journal) = if matches!(edit, MetadataEdit::Pinned(pinned) if boolean(meta, "pinned")? == pinned)
        {
            (None, journal(directory, &document)?)
        } else {
            let (name, text) = edit.field();
            let mut updated = patch_field(&xml, meta, name, text)?;
            if matches!(edit, MetadataEdit::Title(_))
                && super::summary_store::is_native_source(directory, expected_id)
            {
                let parsed = parse_document(&updated)?;
                let updated_meta = child(parsed.root_element(), "meta")
                    .ok_or_else(|| invalid("Missing session journal metadata"))?;
                updated = patch_field(
                    &updated,
                    updated_meta,
                    super::summary_store::MANUAL_TITLE,
                    "true",
                )?;
            }
            if updated.len() as u64 > MAX_DOCUMENT_BYTES {
                return Err(invalid("Session journal file exceeds the size limit"));
            }
            let updated_document = parse_document(&updated)?;
            let journal = journal(directory, &updated_document)?;
            (Some(updated), journal)
        };
        Ok(Self {
            lock,
            stamp,
            xml,
            journal,
        })
    }

    fn publish(self) -> io::Result<Journal> {
        self.publish_checked(|_| Ok(()))
    }

    fn publish_checked(
        self,
        validate: impl Fn(&Directory) -> io::Result<()>,
    ) -> io::Result<Journal> {
        self.lock
            .publish_document(&self.stamp, self.xml.as_deref(), validate)?;
        Ok(self.journal)
    }
}

fn patch_field(xml: &str, meta: Node<'_, '_>, name: &str, text: &str) -> io::Result<String> {
    let mut fields = meta.children().filter(|node| node.has_tag_name(name));
    let field = fields.next();
    if fields.next().is_some() || field.is_some_and(|field| field.first_element_child().is_some()) {
        return Err(invalid("Invalid session journal metadata field"));
    }
    let element = format!("<{name}>{}</{name}>", escape(text));
    let mut updated = xml.to_owned();
    if let Some(field) = field {
        updated.replace_range(field.range(), &element);
    } else {
        let range = meta.range();
        let source = &xml[range.clone()];
        if source.ends_with("/>") {
            updated.replace_range(range.end - 2..range.end, &format!(">{element}</meta>"));
        } else {
            let close = source
                .rfind("</")
                .ok_or_else(|| invalid("Invalid session journal metadata element"))?;
            updated.insert_str(range.start + close, &element);
        }
    }
    Ok(updated)
}

pub(super) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        escaped.push_str(match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '\r' => "&#13;",
            _ => {
                escaped.push(character);
                continue;
            }
        });
    }
    escaped
}

pub(super) fn xml_character(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}'
        | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

// Java String.strip/isBlank use Character.isWhitespace, which preserves NBSP, FIGURE SPACE,
// NARROW NBSP and NEXT LINE even though Rust's Unicode whitespace predicate includes them.
pub(super) fn java_whitespace(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000d}' | '\u{001c}'..='\u{0020}'
        | '\u{1680}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}')
}

#[cfg(test)]
mod tests;
