//! Confirmed journal deletion using the catalog identity and a private staging directory.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::private_files::{Directory, EntryIdentity, FileStamp, nonce};
use crate::sftp_window::LocalSource;

use super::metadata::{LOCK, MetadataLock, lock_metadata_in, require_single_link};
use super::{
    DOCUMENT, MAX_DOCUMENT_BYTES, check_path, child, document_id, invalid, parse_document,
};

const MAX_ENTRIES: usize = 8192;
const MAX_DEPTH: usize = 32;

/// Snapshot of the directory and document actually read by the catalog worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeletionIdentity {
    root: EntryIdentity,
    directory: EntryIdentity,
    document: FileStamp,
}

impl DeletionIdentity {
    pub(super) fn location(&self) -> (EntryIdentity, EntryIdentity) {
        (self.root, self.directory)
    }

    pub(super) fn same_location(&self, other: &Self) -> bool {
        self.location() == other.location()
    }

    pub(crate) fn capture(root: &Directory, source: &LocalSource) -> io::Result<Self> {
        #[cfg(unix)]
        let directory = {
            use std::os::unix::fs::MetadataExt as _;

            let metadata = source.parent_file().metadata()?;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(windows)]
        let directory = source.parent_directory().identity()?;
        Ok(Self {
            root: root.identity()?,
            directory,
            document: FileStamp::read(source.file())?,
        })
    }

    pub(crate) fn require_document(&self, file: &File) -> io::Result<()> {
        self.document.require_unchanged(file)
    }
}

/// One prepared confirmation target. Preparation reads bounded metadata and leaves disk contents intact.
pub(crate) struct Deletion {
    root: Arc<Directory>,
    root_path: PathBuf,
    directory: PathBuf,
    name: OsString,
    expected_id: String,
    identity: DeletionIdentity,
}

impl fmt::Debug for Deletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalDeletion")
            .finish_non_exhaustive()
    }
}

impl Deletion {
    /// Resolves and checks a direct child of the confirmed catalog root. Call off the UI thread.
    pub(crate) fn prepare(
        root: Arc<Directory>,
        root_path: &Path,
        directory: &Path,
        expected_id: &str,
        expected: &DeletionIdentity,
    ) -> io::Result<Self> {
        check_path(root_path)?;
        check_path(directory)?;
        let name = directory
            .file_name()
            .filter(|_| directory.parent() == Some(root_path))
            .ok_or_else(|| invalid("Select a direct child of the journal folder"))?;
        let prepared = Self {
            root,
            root_path: root_path.to_owned(),
            directory: directory.to_owned(),
            name: name.to_owned(),
            expected_id: expected_id.into(),
            identity: expected.clone(),
        };
        let directory = prepared.current_directory()?;
        prepared.validate_document(&directory)?;
        Ok(prepared)
    }

    /// Executes this confirmed target off the UI thread. Active native writers hold the same lock.
    pub(crate) fn delete(&self) -> io::Result<()> {
        let directory = self.current_directory()?;
        self.validate_document(&directory)?;
        let lock = lock_metadata_in(&self.directory, directory)?;
        self.identity
            .require_document(&self.current_directory()?.open_file(DOCUMENT)?)?;
        lock.require_current(&self.directory)?;
        let stage = Stage::new(&self.root, &self.root_path)?;
        self.require_root()?;
        let lock = self.claim(&stage, lock)?;
        self.delete_claimed(&stage, lock)
    }

    fn claim(&self, stage: &Stage<'_>, lock: MetadataLock) -> io::Result<MetadataLock> {
        // NTFS refuses directory rename while an ordinary handle to a child is open.
        #[cfg(windows)]
        drop(lock);

        self.root.move_entry(&self.name, &stage.directory)?;

        #[cfg(unix)]
        {
            Ok(lock)
        }

        #[cfg(windows)]
        {
            let checked_lock = (|| {
                self.require_root()?;
                stage.require_current()?;
                let directory = self.require_directory(&stage.directory)?;
                self.identity
                    .require_document(&directory.open_file(DOCUMENT)?)?;
                lock_metadata_in(&stage.path.join(&self.name), directory)
            })();
            checked_lock.map_err(|error| stage.restore(&self.name, error))
        }
    }

    fn require_root(&self) -> io::Result<()> {
        if self.root.identity()? != self.identity.root
            || !self
                .root
                .same_as(&Directory::open(&self.root_path, false)?)?
        {
            return Err(invalid(
                "The journal root changed. Refresh the journal list.",
            ));
        }
        Ok(())
    }

    fn current_directory(&self) -> io::Result<Directory> {
        self.require_root()?;
        self.require_directory(&self.root)
    }

    fn require_directory(&self, parent: &Directory) -> io::Result<Directory> {
        let directory = parent.deletion_child(&self.name)?;
        if directory.identity()? != self.identity.directory {
            return Err(invalid(
                "The journal directory changed. Refresh the journal list.",
            ));
        }
        Ok(directory)
    }

    fn validate_document(&self, directory: &Directory) -> io::Result<()> {
        let file = directory.open_file(DOCUMENT)?;
        require_single_link(&file)?;
        self.identity.require_document(&file)?;
        if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("Session journal file exceeds the size limit"));
        }
        let mut xml = String::new();
        (&file)
            .take(MAX_DOCUMENT_BYTES + 1)
            .read_to_string(&mut xml)?;
        if xml.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(invalid("Session journal file exceeds the size limit"));
        }
        self.identity.require_document(&file)?;
        let document = parse_document(&xml)?;
        if document_id(&document)? != self.expected_id
            || child(document.root_element(), "meta").is_none()
        {
            return Err(invalid(
                "The selected journal document changed. Refresh the journal list.",
            ));
        }
        Ok(())
    }

    fn delete_claimed(&self, stage: &Stage<'_>, lock: MetadataLock) -> io::Result<()> {
        let prepared = (|| {
            self.require_root()?;
            let directory = self.require_directory(&stage.directory)?;
            lock.require_current(&stage.path.join(&self.name))?;
            self.validate_document(&directory)?;
            let mut remaining = MAX_ENTRIES;
            let mut entries = inventory(&directory, MAX_DEPTH, &mut remaining)?;
            // Preserve the journal's identifying document if an earlier removal fails.
            entries.sort_by_key(|entry| (is_lock(&entry.name), entry.name == OsStr::new(DOCUMENT)));
            self.require_root()?;
            stage.require_current()?;
            self.identity
                .require_document(&directory.open_file(DOCUMENT)?)?;
            lock.require_current(&stage.path.join(&self.name))?;
            Ok((directory, entries))
        })();
        let (directory, entries) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                #[cfg(windows)]
                drop(lock);
                return Err(stage.restore(&self.name, error));
            }
        };

        let lock_start = entries.partition_point(|entry| !is_lock(&entry.name));
        let removed = purge(&directory, &entries[..lock_start]);
        // Close the lock handle before removing its file and the enclosing folder on Windows.
        drop(lock);
        let removed = removed.and_then(|()| purge(&directory, &entries[lock_start..]));
        drop(directory);
        removed
            .and_then(|()| {
                stage
                    .directory
                    .remove_deletion_entry(&self.name, self.identity.directory, true)
            })
            .map_err(|error| stage.preserved(&self.name, error))?;
        stage.remove_empty()
    }
}

struct Stage<'a> {
    root: &'a Directory,
    name: OsString,
    directory: Directory,
    identity: EntryIdentity,
    path: PathBuf,
}

impl<'a> Stage<'a> {
    fn new(root: &'a Directory, path: &Path) -> io::Result<Self> {
        let name = OsString::from(format!(".kortty-journal-delete-{}", nonce()?));
        let directory = root.create_private_subdirectory(&name)?;
        Ok(Self {
            root,
            identity: directory.identity()?,
            directory,
            path: path.join(&name),
            name,
        })
    }

    fn require_current(&self) -> io::Result<()> {
        if self.root.deletion_child(&self.name)?.identity()? != self.identity {
            return Err(invalid("The journal staging directory changed"));
        }
        Ok(())
    }

    fn restore(&self, child: &OsStr, error: io::Error) -> io::Error {
        match self.directory.move_entry(child, self.root) {
            Ok(()) => error,
            Err(restore) => self.preserved(
                child,
                io::Error::new(error.kind(), format!("{error}. Restore failed ({restore})")),
            ),
        }
    }

    fn preserved(&self, child: &OsStr, error: io::Error) -> io::Error {
        io::Error::new(
            error.kind(),
            format!(
                "{error}. Staged files were retained. Original recovery path: {}",
                self.path.join(child).display()
            ),
        )
    }

    fn remove_empty(&self) -> io::Result<()> {
        self.root
            .remove_deletion_entry(&self.name, self.identity, true)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        // Only an unchanged, empty stage can be removed here.
        let _ = self.remove_empty();
    }
}

struct TreeEntry {
    name: OsString,
    identity: EntryIdentity,
    children: Option<Vec<TreeEntry>>,
}

fn inventory(
    directory: &Directory,
    depth: usize,
    remaining: &mut usize,
) -> io::Result<Vec<TreeEntry>> {
    let names = directory.names(*remaining)?;
    *remaining -= names.len();
    names
        .into_iter()
        .map(|name| {
            let (identity, is_directory) = directory.deletion_entry(&name)?;
            let children = if is_directory {
                let depth = depth.checked_sub(1).ok_or_else(|| {
                    invalid("Journal directory nesting exceeds the deletion limit")
                })?;
                let child = directory.deletion_child(&name)?;
                if child.identity()? != identity {
                    return Err(invalid("Journal entry changed during deletion preparation"));
                }
                Some(inventory(&child, depth, remaining)?)
            } else {
                None
            };
            Ok(TreeEntry {
                name,
                identity,
                children,
            })
        })
        .collect()
}

fn purge(directory: &Directory, entries: &[TreeEntry]) -> io::Result<()> {
    for entry in entries {
        if let Some(children) = &entry.children {
            let child = directory.deletion_child(&entry.name)?;
            if child.identity()? != entry.identity {
                return Err(invalid("Journal entry changed before deletion"));
            }
            purge(&child, children)?;
        }
        directory.remove_deletion_entry(&entry.name, entry.identity, entry.children.is_some())?;
    }
    Ok(())
}

fn is_lock(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(LOCK))
}

#[cfg(test)]
mod tests;
