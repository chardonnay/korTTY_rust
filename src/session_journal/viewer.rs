//! A viewer is bound to the selected journal's physical location and document ID.

use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use roxmltree::Document;

use crate::private_files::{Directory, EntryIdentity};
use crate::sftp_window::LocalSource;

use super::capture::capture_page_in;
use super::deletion::DeletionIdentity;
use super::metadata::require_single_link;
use super::{
    Capture, CapturePage, Cursor, Journal, Summary, check_path, child, document_id, document_text,
    invalid, journal, number, open_document, parse_document,
};

pub(super) mod screenshot;
pub(crate) use screenshot::Screenshot;

#[derive(Clone, Debug)]
pub(crate) struct Source {
    directory: PathBuf,
    id: String,
    location: (EntryIdentity, EntryIdentity),
}

impl Source {
    /// Uses the catalog's verified location without opening files on the UI thread.
    pub(crate) fn catalog(summary: &Summary) -> io::Result<Self> {
        check_path(&summary.directory)?;
        if summary.directory.parent().is_none() || summary.directory.file_name().is_none() {
            return Err(changed());
        }
        let identity = summary.deletion_identity.as_ref().ok_or_else(changed)?;
        Ok(Self {
            directory: summary.directory.clone(),
            id: summary.id.clone(),
            location: identity.location(),
        })
    }

    /// Retains the original writer location without owning the writer, channels or locks.
    pub(crate) fn capture(capture: &Capture) -> Self {
        Self {
            directory: capture.directory().to_owned(),
            id: capture.id().to_owned(),
            location: capture.location(),
        }
    }

    /// Binds a metadata worker's already opened journal directory before it can publish.
    pub(crate) fn from_open_directory(
        path: &Path,
        id: &str,
        opened: &Directory,
    ) -> io::Result<Self> {
        Ok(Self {
            directory: path.to_owned(),
            id: id.to_owned(),
            location: directory_location(path, opened)?,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Case aliases are equivalent only when both physical directory identities agree.
    pub(crate) fn same_journal(&self, other: &Self) -> bool {
        self.id == other.id && self.location == other.location
    }

    pub(crate) fn matches_location(&self, identity: &DeletionIdentity) -> bool {
        self.location == identity.location()
    }

    pub(crate) fn require_location(&self, path: &Path, opened: &Directory) -> io::Result<()> {
        if self.location != directory_location(path, opened)? {
            return Err(changed());
        }
        Ok(())
    }

    /// `previous` is the stamp from a previously returned snapshot of this source.
    /// An unchanged stamp skips decoding, but still verifies the held and current paths.
    pub(crate) fn read(&self, previous: Option<&DeletionIdentity>) -> io::Result<Option<Journal>> {
        for _ in 0..2 {
            let (source, identity) = self.open_verified()?;
            let snapshot = if previous == Some(&identity) {
                Ok(None)
            } else {
                self.read_document(&source, &identity).map(Some)
            };
            if self.is_current(&source, &identity)? {
                return snapshot;
            }
        }
        Err(republished())
    }

    pub(crate) fn capture_page(
        &self,
        after: Option<Cursor>,
        range: Option<RangeInclusive<u64>>,
        query: Option<&str>,
    ) -> io::Result<CapturePage> {
        self.with_document(|source, document| {
            let meta = child(document.root_element(), "meta")
                .ok_or_else(|| invalid("Missing session journal metadata"))?;
            let parts = number::<u32>(meta, "logParts")?.unwrap_or(1);
            capture_page_in(source, self.id(), parts, after, range, query)
        })
    }

    fn open_verified(&self) -> io::Result<(LocalSource, DeletionIdentity)> {
        let parent = self.directory.parent().ok_or_else(changed)?;
        let root = Directory::open(parent, false)?;
        let source = open_document(&self.directory)?;
        require_single_link(source.file())?;
        let identity = DeletionIdentity::capture(&root, &source)?;
        if !self.matches_location(&identity) {
            return Err(changed());
        }
        Ok((source, identity))
    }

    fn is_current(&self, source: &LocalSource, identity: &DeletionIdentity) -> io::Result<bool> {
        let (_, reopened) = self.open_verified()?;
        Ok(reopened == *identity && identity.require_document(source.file()).is_ok())
    }

    fn parse<'a>(&self, xml: &'a str) -> io::Result<Document<'a>> {
        let document = parse_document(xml)?;
        if document_id(&document)? != self.id() {
            return Err(changed());
        }
        Ok(document)
    }

    fn read_document(
        &self,
        source: &LocalSource,
        identity: &DeletionIdentity,
    ) -> io::Result<Journal> {
        let xml = document_text(source)?;
        let document = self.parse(&xml)?;
        let mut snapshot = journal(self.directory(), &document)?;
        snapshot.summary.deletion_identity = Some(identity.clone());
        Ok(snapshot)
    }

    pub(super) fn with_document<T>(
        &self,
        read: impl FnOnce(&LocalSource, &Document<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        let (source, identity) = self.open_verified()?;
        let xml = document_text(&source)?;
        let document = self.parse(&xml)?;
        identity.require_document(source.file())?;
        let result = read(&source, &document);
        if self.is_current(&source, &identity)? {
            result
        } else {
            Err(republished())
        }
    }
}

fn directory_location(
    path: &Path,
    opened: &Directory,
) -> io::Result<(EntryIdentity, EntryIdentity)> {
    check_path(path)?;
    let parent = path.parent().ok_or_else(changed)?;
    let root = Directory::open(parent, false)?;
    if !opened.same_as(&Directory::open(path, false)?)?
        || !root.same_as(&Directory::open(parent, false)?)?
    {
        return Err(changed());
    }
    Ok((root.identity()?, opened.identity()?))
}

fn changed() -> io::Error {
    invalid("The selected session journal identity changed")
}

fn republished() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "The session journal was updated while being read",
    )
}

#[cfg(test)]
mod tests;
