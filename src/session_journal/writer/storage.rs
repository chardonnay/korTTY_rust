use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::super::metadata::{MetadataLock, lock_metadata};
use super::super::{DOCUMENT, check_path, invalid};
use crate::private_files::FileStamp;

#[cfg(windows)]
use kortty_files::tree::Directory;

const STAGED_DOCUMENT: &str = ".journal.xml.pending";

#[cfg(unix)]
pub(super) type DirectoryIdentity = (u64, u64);
#[cfg(windows)]
pub(super) type DirectoryIdentity = (u64, [u8; 16]);

pub(super) fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    Directory::open_path(path)?.identity()
}

pub(super) struct Storage {
    directory: Directory,
    path: PathBuf,
    metadata_lock: Option<MetadataLock>,
    document_stamp: Option<FileStamp>,
    #[cfg(test)]
    pub(super) fail_next_metadata_sync: std::cell::Cell<bool>,
}

pub(super) struct MetadataError {
    pub(super) error: io::Error,
    pub(super) published: bool,
}

impl From<io::Error> for MetadataError {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            published: false,
        }
    }
}

impl From<MetadataError> for io::Error {
    fn from(error: MetadataError) -> Self {
        error.error
    }
}

impl Storage {
    pub(super) fn root(path: &Path) -> io::Result<Self> {
        check_path(path)?;
        let root = match Directory::open_path(path) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (parent, name) = path
                    .parent()
                    .zip(path.file_name())
                    .ok_or_else(|| invalid("Invalid session journal storage root"))?;
                let parent = Directory::open_path(parent)?;
                match create_child(&parent, name) {
                    Ok(root) => root,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Directory::open_path(path)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        root.require_owner_only()?;
        Ok(Self {
            directory: root,
            path: path.to_owned(),
            metadata_lock: None,
            document_stamp: None,
            #[cfg(test)]
            fail_next_metadata_sync: std::cell::Cell::new(false),
        })
    }

    pub(super) fn child(&self, name: &str) -> io::Result<Self> {
        Ok(Self {
            directory: create_child(&self.directory, OsStr::new(name))?,
            path: self.path.join(name),
            metadata_lock: None,
            document_stamp: None,
            #[cfg(test)]
            fail_next_metadata_sync: std::cell::Cell::new(false),
        })
    }

    pub(super) fn lock_metadata(&mut self) -> io::Result<()> {
        let lock = lock_metadata(&self.path)?;
        #[cfg(unix)]
        let same = lock.same_as_file(&self.directory.0)?;
        #[cfg(windows)]
        let same = lock.same_as_directory(&self.directory)?;
        if !same {
            return Err(invalid("Session journal directory changed before locking"));
        }
        self.metadata_lock = Some(lock);
        Ok(())
    }

    pub(super) fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.directory.identity()
    }

    pub(super) fn file(&self, name: &str) -> io::Result<File> {
        self.directory.create_file(OsStr::new(name))
    }

    pub(super) fn metadata_checked(
        &mut self,
        bytes: &[u8],
        verify: impl Fn() -> io::Result<()>,
    ) -> Result<(), MetadataError> {
        self.require_metadata()?;
        verify()?;
        let mut staged = self.file(STAGED_DOCUMENT)?;
        let staged_identity = file_identity(&staged)?;
        let result = staged
            .write_all(bytes)
            .and_then(|()| staged.sync_all())
            .map_err(MetadataError::from);
        let result = result.and_then(|()| {
            self.require_metadata()?;
            verify()?;
            FileStamp::read(&staged)?.require_unchanged(&self.open_file(STAGED_DOCUMENT)?)?;
            self.replace_document(&staged).map_err(MetadataError::from)
        });
        drop(staged);
        match result {
            Ok(()) => self.metadata_published(staged_identity),
            Err(error) => {
                if self
                    .open_file(STAGED_DOCUMENT)
                    .and_then(|file| file_identity(&file))
                    .is_ok_and(|identity| identity == staged_identity)
                {
                    self.remove_file(STAGED_DOCUMENT);
                }
                Err(error)
            }
        }
    }

    pub(super) fn save_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.file(name)?;
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        #[cfg(unix)]
        let result = result.and_then(|()| self.directory.0.sync_all());
        drop(file);
        if result.is_err() {
            self.remove_file(name);
        }
        result
    }

    pub(super) fn discard(mut self, parent: &Self, name: &str, log_name: &str) {
        for file in [STAGED_DOCUMENT, log_name] {
            self.remove_file(file);
        }
        if self.document_stamp.is_some() && self.require_document().is_ok() {
            self.remove_file(DOCUMENT);
        }
        if let Some(lock) = self.metadata_lock.take() {
            let _ = lock.discard_unpublished();
        }
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            use std::os::unix::fs::MetadataExt as _;

            if let Ok(expected) = self.directory.0.metadata()
                && let Ok(current) = parent
                    .directory
                    .open_child(OsStr::new(name))
                    .and_then(|directory| directory.0.metadata())
                && (expected.dev(), expected.ino()) == (current.dev(), current.ino())
            {
                let _ = unlinkat(&parent.directory.0, name, AtFlags::REMOVEDIR);
            }
        }
        #[cfg(windows)]
        {
            if let Ok(expected) = self.directory.identity()
                && let Ok(file) = parent.directory.child_for_removal(OsStr::new(name))
                && let Ok(directory) = Directory::from_file(file)
                && directory.identity().is_ok_and(|found| found == expected)
            {
                let _ = directory.remove();
            }
        }
    }

    #[cfg(unix)]
    fn replace_document(&self, _: &File) -> io::Result<()> {
        if self.document_stamp.is_none() {
            rustix::fs::renameat_with(
                &self.directory.0,
                STAGED_DOCUMENT,
                &self.directory.0,
                DOCUMENT,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)?;
        } else {
            rustix::fs::renameat(
                &self.directory.0,
                STAGED_DOCUMENT,
                &self.directory.0,
                DOCUMENT,
            )
            .map_err(io::Error::from)?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn replace_document(&self, staged: &File) -> io::Result<()> {
        if self.document_stamp.is_none() {
            self.directory.publish_file(staged, OsStr::new(DOCUMENT))?;
        } else {
            self.directory
                .replace_file_with_shared_readers(staged, OsStr::new(DOCUMENT))?;
        }
        Ok(())
    }

    fn metadata_published(&mut self, expected: DirectoryIdentity) -> Result<(), MetadataError> {
        let stamp = (|| {
            let installed = self.open_file(DOCUMENT)?;
            if file_identity(&installed)? != expected {
                return Err(invalid(
                    "Session journal document changed during publication",
                ));
            }
            FileStamp::read(&installed)
        })()
        .map_err(|error| MetadataError {
            error,
            published: true,
        })?;
        self.document_stamp = Some(stamp);
        #[cfg(test)]
        if self.fail_next_metadata_sync.replace(false) {
            // Test double for a durability failure after the real atomic replacement.
            return Err(MetadataError {
                error: io::Error::other("Injected journal directory sync failure"),
                published: true,
            });
        }
        #[cfg(unix)]
        self.directory.0.sync_all().map_err(|error| MetadataError {
            error,
            published: true,
        })?;
        Ok(())
    }

    pub(super) fn require_metadata(&self) -> io::Result<()> {
        self.locked_directory().map(|_| ())
    }

    pub(super) fn locked_directory(&self) -> io::Result<&crate::private_files::Directory> {
        let lock = self
            .metadata_lock
            .as_ref()
            .ok_or_else(|| invalid("Session journal metadata requires an exclusive writer lock"))?;
        lock.require_current(&self.path)?;
        self.require_document()?;
        Ok(lock.directory())
    }

    fn require_document(&self) -> io::Result<()> {
        match self.open_file(DOCUMENT) {
            Ok(file) => {
                let expected = self.document_stamp.as_ref().ok_or_else(|| {
                    invalid("Session journal document appeared before initial publication")
                })?;
                #[cfg(unix)]
                let links = {
                    use std::os::unix::fs::MetadataExt as _;
                    file.metadata()?.nlink()
                };
                #[cfg(windows)]
                let links = kortty_files::tree::file_link_count(&file)?;
                if links != 1 {
                    return Err(invalid("Session journal document has multiple hard links"));
                }
                expected.require_unchanged(&file)
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && self.document_stamp.is_none() =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn open_file(&self, name: &str) -> io::Result<File> {
        use rustix::fs::{Mode, OFlags, openat};
        Ok(File::from(openat(
            &self.directory.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    #[cfg(windows)]
    fn open_file(&self, name: &str) -> io::Result<File> {
        self.directory
            .read_child(OsStr::new(name))?
            .ok_or_else(|| invalid("Session journal document is a reparse point"))
    }

    #[cfg(unix)]
    fn remove_file(&self, name: &str) {
        let _ = rustix::fs::unlinkat(&self.directory.0, name, rustix::fs::AtFlags::empty());
    }

    #[cfg(windows)]
    fn remove_file(&self, name: &str) {
        if let Ok(file) = self.directory.child_for_removal(OsStr::new(name)) {
            let _ = kortty_files::tree::remove(file);
        }
    }
}

pub(in crate::session_journal) fn file_identity(
    file: &File,
) -> io::Result<crate::private_files::EntryIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    kortty_files::tree::file_identity(file)
}

#[cfg(unix)]
fn create_child(parent: &Directory, name: &OsStr) -> io::Result<Directory> {
    parent.create_child(name)
}

#[cfg(windows)]
fn create_child(parent: &Directory, name: &OsStr) -> io::Result<Directory> {
    let created = parent.create_child(name)?;
    let expected = created.identity()?;
    // The exclusive creator handle would conflict with publishing child files.
    drop(created);
    let opened = parent
        .read_child(name)?
        .ok_or_else(|| invalid("Session journal directory is a reparse point"))?;
    let opened = Directory::from_file(opened)?;
    if opened.identity()? != expected {
        return Err(invalid("Session journal directory changed after creation"));
    }
    opened.require_owner_only()?;
    Ok(opened)
}

#[cfg(unix)]
struct Directory(File);

#[cfg(unix)]
impl Directory {
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
        .union(rustix::fs::OFlags::DIRECTORY)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);

    fn identity(&self) -> io::Result<DirectoryIdentity> {
        file_identity(&self.0)
    }

    fn open_path(path: &Path) -> io::Result<Self> {
        use rustix::fs::{Mode, open};
        use std::path::Component;

        check_path(path)?;
        let mut directory = Self(File::from(open("/", Self::FLAGS, Mode::empty())?));
        for component in path.components().skip(1) {
            let Component::Normal(name) = component else {
                return Err(invalid("Invalid session journal directory component"));
            };
            directory = directory.open_child(name)?;
        }
        Ok(directory)
    }

    fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        use rustix::fs::{Mode, openat};
        Ok(Self(File::from(openat(
            &self.0,
            name,
            Self::FLAGS,
            Mode::empty(),
        )?)))
    }

    fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        use rustix::fs::{AtFlags, Mode, mkdirat, unlinkat};
        mkdirat(&self.0, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
        let result = self.open_child(name).and_then(|child| {
            child.require_owner_only()?;
            self.0.sync_all()?;
            Ok(child)
        });
        if result.is_err() {
            let _ = unlinkat(&self.0, name, AtFlags::REMOVEDIR);
        }
        result
    }

    fn create_file(&self, name: &OsStr) -> io::Result<File> {
        use rustix::fs::{Mode, OFlags, openat};

        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(openat(
            &self.0,
            name,
            flags,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    fn require_owner_only(&self) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        if self.0.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Session journal storage must be private to the current user",
            ));
        }
        Ok(())
    }
}
