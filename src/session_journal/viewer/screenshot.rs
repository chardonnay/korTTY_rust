//! A reviewed screenshot row and the exact PNG bytes it displayed.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io;

use crate::private_files::{Directory, EntryIdentity, FileStamp};

use super::Source;
use crate::session_journal::capture::bundle::ScreenshotRef;
use crate::session_journal::entry_edit::{find_entry, require_java_fields};
use crate::session_journal::export::screenshot_bytes;
use crate::session_journal::metadata::require_single_link;
use crate::session_journal::screenshot_edit::annotation_stage_name;
use crate::session_journal::{MAX_PNG_BYTES, bounded_field, field, invalid};

const SCREENSHOTS: &str = "screenshots";

pub(crate) struct Screenshot {
    source: Source,
    id: String,
    reference: ScreenshotRef,
    reviewed: ImageState,
    bytes: Vec<u8>,
}

impl fmt::Debug for Screenshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewedJournalScreenshot")
            .finish_non_exhaustive()
    }
}

#[derive(Eq, PartialEq)]
struct ImageState {
    created_at: Option<String>,
    folder: EntryIdentity,
    file: EntryIdentity,
    stamp: FileStamp,
}

impl Screenshot {
    pub(crate) fn is_visible_reference(reference: &str) -> bool {
        ScreenshotRef::visible(reference).is_some()
    }

    pub(crate) fn review(source: &Source, id: &str, reference: &str) -> io::Result<Self> {
        let reference = ScreenshotRef::parse(reference).ok_or_else(changed)?;
        let (reviewed, bytes) = read(source, id, &reference)?;
        Ok(Self {
            source: source.clone(),
            id: id.into(),
            reference,
            reviewed,
            bytes,
        })
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn reference(&self) -> &str {
        self.reference.as_str()
    }

    pub(crate) fn filename(&self) -> &str {
        self.reference
            .as_str()
            .split_once('/')
            .expect("verified screenshot path")
            .1
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn is_file(&self, file: &File) -> io::Result<bool> {
        Ok(file_identity(file)? == self.reviewed.file)
    }

    pub(crate) fn require_current(&self) -> io::Result<()> {
        let (current, bytes) = read(&self.source, &self.id, &self.reference)?;
        if current != self.reviewed || bytes != self.bytes {
            return Err(changed());
        }
        Ok(())
    }

    pub(in crate::session_journal) fn require_entry(
        &self,
        entry: roxmltree::Node<'_, '_>,
    ) -> io::Result<()> {
        require_java_fields(entry, &["kind", "screenshotFile", "createdAt"])?;
        if bounded_field(entry, "kind")? != "SCREENSHOT"
            || bounded_field(entry, "screenshotFile")? != self.reference()
            || field(entry, "createdAt")?.as_deref() != self.reviewed.created_at.as_deref()
        {
            return Err(changed());
        }
        Ok(())
    }

    pub(in crate::session_journal) fn created_at(&self) -> Option<&str> {
        self.reviewed.created_at.as_deref()
    }

    pub(in crate::session_journal) fn require_image(&self, folder: &Directory) -> io::Result<()> {
        let file = open_image(folder, self.filename())?;
        if folder.identity()? != self.reviewed.folder || file_identity(&file)? != self.reviewed.file
        {
            return Err(changed());
        }
        self.reviewed.stamp.require_unchanged(&file)
    }

    /// Rechecks the published PNG against a reviewed asset using the locked journal directory.
    pub(in crate::session_journal) fn require_image_current_in(
        &self,
        journal: &Directory,
    ) -> io::Result<()> {
        let (folder, file, stamp, bytes) = read_image_in(&self.source, journal, &self.reference)?;
        if folder != self.reviewed.folder
            || file != self.reviewed.file
            || stamp != self.reviewed.stamp
            || bytes != self.bytes
        {
            return Err(changed());
        }
        Ok(())
    }
}

fn read(source: &Source, id: &str, reference: &ScreenshotRef) -> io::Result<(ImageState, Vec<u8>)> {
    let read_once = || {
        source.with_document(|_, document| {
            let entry = find_entry(document, id)?;
            require_java_fields(entry, &["kind", "screenshotFile", "createdAt"])?;
            if bounded_field(entry, "kind")? != "SCREENSHOT"
                || bounded_field(entry, "screenshotFile")? != reference.as_str()
            {
                return Err(changed());
            }
            let created_at = field(entry, "createdAt")?.map(|value| value.into_owned());
            let (folder, file, stamp, bytes) = read_image(source, reference)?;
            Ok((
                ImageState {
                    created_at,
                    folder,
                    file,
                    stamp,
                },
                bytes,
            ))
        })
    };
    match read_once() {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => read_once(),
        result => result,
    }
}

fn read_image(
    source: &Source,
    reference: &ScreenshotRef,
) -> io::Result<(EntryIdentity, EntryIdentity, FileStamp, Vec<u8>)> {
    let journal = Directory::open(source.directory(), false)?;
    read_image_in(source, &journal, reference)
}

fn read_image_in(
    source: &Source,
    journal: &Directory,
    reference: &ScreenshotRef,
) -> io::Result<(EntryIdentity, EntryIdentity, FileStamp, Vec<u8>)> {
    source.require_location(source.directory(), journal)?;
    let folder = journal.deletion_child(OsStr::new(SCREENSHOTS))?;
    let folder_identity = folder.identity()?;
    let name = reference
        .as_str()
        .split_once('/')
        .expect("verified screenshot path")
        .1;
    let open = || open_published_image(&folder, name);
    let file = open()?;
    let identity = file_identity(&file)?;
    let stamp = FileStamp::read(&file)?;
    let bytes = screenshot_bytes(
        || {
            let opened = open()?;
            stamp.require_unchanged(&opened)?;
            Ok(opened)
        },
        MAX_PNG_BYTES,
    )?;
    source.require_location(source.directory(), journal)?;
    if !folder.same_as(&journal.deletion_child(OsStr::new(SCREENSHOTS))?)? {
        return Err(changed());
    }
    stamp.require_unchanged(&open()?)?;
    Ok((folder_identity, identity, stamp, bytes))
}

pub(in crate::session_journal) fn open_image(folder: &Directory, name: &str) -> io::Result<File> {
    let file = folder.open_file(name)?;
    require_single_link(&file)?;
    #[cfg(unix)]
    if folder.deletion_entry(OsStr::new(name))? != (file_identity(&file)?, false) {
        return Err(changed());
    }
    Ok(file)
}

pub(in crate::session_journal) fn open_published_image(
    folder: &Directory,
    name: &str,
) -> io::Result<File> {
    require_published_image(name, |stage| folder.has_entry(stage))?;
    open_image(folder, name)
}

// Call before each handle open, including the final file-stamp comparison after reading.
pub(in crate::session_journal) fn require_published_image(
    name: &str,
    has_entry: impl FnOnce(&str) -> io::Result<bool>,
) -> io::Result<()> {
    if has_entry(&annotation_stage_name(name))? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "The journal screenshot has a pending annotation edit",
        ));
    }
    Ok(())
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

fn changed() -> io::Error {
    invalid("The selected journal screenshot changed or is unavailable")
}
