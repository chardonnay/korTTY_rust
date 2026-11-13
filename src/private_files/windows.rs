use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use kortty_files::tree::{self, Directory as Handle};

use super::{EntryIdentity, FileStamp, invalid, nonce};

pub(crate) struct Directory(Handle);

impl Directory {
    pub(crate) fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !create {
            return Handle::open_path(path).map(Self);
        }
        let mut components = path.components();
        let (Some(Component::Prefix(prefix)), Some(Component::RootDir)) =
            (components.next(), components.next())
        else {
            return Err(invalid("expected an absolute profile directory"));
        };
        let mut volume = PathBuf::from(prefix.as_os_str());
        volume.push(r"\");
        let mut directory = Self(Handle::open_path(&volume)?);
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid("expected an ordinary profile directory"));
            };
            directory = directory.child(name, true)?;
        }
        Ok(directory)
    }

    fn child(&self, name: &OsStr, create: bool) -> io::Result<Self> {
        let open = || self.read_child(name).and_then(Handle::from_file);
        let handle = match open() {
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match self.0.create_child(name) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open()?,
                    result => {
                        let created = result?;
                        let identity = created.identity()?;
                        // The exclusive creator handle would conflict with rename into the directory.
                        drop(created);
                        let opened = open()?;
                        if opened.identity()? != identity {
                            return Err(invalid("profile directory changed after creation"));
                        }
                        opened
                    }
                }
            }
            result => result?,
        };
        Ok(Self(handle))
    }

    fn read_child(&self, name: &OsStr) -> io::Result<File> {
        self.0
            .read_child(name)?
            .ok_or_else(|| invalid("profile entry is a reparse point"))
    }

    pub(crate) fn private_child(&self, name: &str, create: bool) -> io::Result<Self> {
        let child = self.child(OsStr::new(name), create)?;
        child.0.require_owner_only()?;
        Ok(child)
    }

    pub(crate) fn subdirectory(&self, name: &str, create: bool) -> io::Result<Self> {
        self.child(OsStr::new(name), create)
    }

    pub(crate) fn same_as(&self, other: &Self) -> io::Result<bool> {
        self.0.same_as(&other.0)
    }

    pub(crate) fn same_as_directory(&self, other: &Handle) -> io::Result<bool> {
        self.0.same_as(other)
    }

    pub(crate) fn move_subdirectory(&self, name: &str, destination: &Self) -> io::Result<()> {
        let name = OsStr::new(name);
        let opened = Handle::from_file(self.0.child_for_removal(name)?)?;
        destination.0.publish(&opened, name)
    }

    pub(crate) fn identity(&self) -> io::Result<EntryIdentity> {
        self.0.identity()
    }

    pub(crate) fn create_private_subdirectory(&self, name: &OsStr) -> io::Result<Self> {
        let created = self.0.create_child(name)?;
        let identity = created.identity()?;
        drop(created);
        let opened = self.deletion_child(name)?;
        if opened.identity()? != identity {
            return Err(invalid("Journal staging directory changed after creation"));
        }
        opened.0.require_owner_only()?;
        Ok(opened)
    }

    pub(crate) fn move_entry(&self, name: &OsStr, destination: &Self) -> io::Result<()> {
        let entry = self.0.child_for_removal(name)?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() && !tree::is_reparse_point(&metadata) {
            destination.0.publish(&Handle::from_file(entry)?, name)
        } else {
            destination.0.publish_file(&entry, name)
        }
    }

    pub(crate) fn deletion_child(&self, name: &OsStr) -> io::Result<Self> {
        let child = self.child(name, false)?;
        if child.identity()?.0 != self.identity()?.0 {
            return Err(invalid(
                "Journal deletion cannot cross a filesystem boundary",
            ));
        }
        Ok(child)
    }

    /// Opens a private directory without following reparse points or crossing a volume.
    pub(crate) fn private_deletion_child(&self, name: &OsStr) -> io::Result<Self> {
        let child = self.deletion_child(name)?;
        child.0.require_owner_only()?;
        Ok(child)
    }

    pub(crate) fn deletion_entry(&self, name: &OsStr) -> io::Result<(EntryIdentity, bool)> {
        deletion_entry(&self.0.child_for_removal(name)?)
    }

    pub(crate) fn remove_deletion_entry(
        &self,
        name: &OsStr,
        expected: EntryIdentity,
        directory: bool,
    ) -> io::Result<()> {
        let entry = self.0.child_for_removal(name)?;
        if deletion_entry(&entry)? != (expected, directory) {
            return Err(invalid("Journal entry changed before deletion"));
        }
        tree::remove(entry)
    }

    pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        self.0.names(limit)
    }

    /// Reads the hidden attribute through a no-follow child handle.
    pub(crate) fn is_hidden_child(&self, name: &str) -> io::Result<bool> {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        Ok(self
            .read_child(OsStr::new(name))?
            .metadata()?
            .file_attributes()
            & FILE_ATTRIBUTE_HIDDEN
            != 0)
    }

    pub(crate) fn has_entry(&self, name: &str) -> io::Result<bool> {
        match self.0.read_child(OsStr::new(name)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn open_file(&self, name: &str) -> io::Result<File> {
        let file = self.read_child(OsStr::new(name))?;
        if !file.metadata()?.is_file() {
            return Err(invalid("profile entry is not a regular file"));
        }
        Ok(file)
    }

    pub(crate) fn open_private_file(&self, name: &str) -> io::Result<File> {
        let file = self.open_file(name)?;
        Handle::require_owner_only_file(&file)?;
        Ok(file)
    }

    pub(crate) fn open_existing_lock(&self, name: &str) -> io::Result<File> {
        self.0.open_shared_lock_file(OsStr::new(name))
    }

    pub(crate) fn open_lock(&self, name: &str) -> io::Result<File> {
        let name = OsStr::new(name);
        match self.0.open_shared_lock_file(name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match self.0.create_shared_lock_file(name) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        self.0.open_shared_lock_file(name)
                    }
                    result => result,
                }
            }
            result => result,
        }
    }

    pub(crate) fn publish_copy(&self, name: &str, contents: &[u8]) -> io::Result<()> {
        self.publish_with(name, |file| file.write_all(contents))
    }

    pub(crate) fn publish_with(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> io::Result<()> {
        self.publish_checked(name, write, || Ok(()))
    }

    pub(crate) fn publish_checked(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.publish_checked_access(name, false, write, check)
    }

    pub(crate) fn publish_checked_readable(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.publish_checked_access(name, true, write, check)
    }

    fn publish_checked_access(
        &self,
        name: &str,
        readable: bool,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_staged(readable, write, |file| {
            check()?;
            self.0.publish_file(file, OsStr::new(name))
        })
    }

    pub(crate) fn replace_file(&self, name: &str, contents: &[u8]) -> io::Result<()> {
        self.replace_with(name, |file| file.write_all(contents), || Ok(()))
    }

    pub(crate) fn replace_with(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_staged(false, write, |file| {
            check()?;
            self.0.replace_file(file, OsStr::new(name))
        })
    }

    pub(crate) fn replace_with_readable(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_staged(true, write, |file| {
            check()?;
            self.0.replace_file(file, OsStr::new(name))
        })
    }

    /// Publishes while a caller-held lock protects a checked replacement.
    pub(crate) fn publish_bytes_with_retry(
        &self,
        name: &str,
        bytes: &[u8],
        check_replacement: Option<impl Fn() -> io::Result<()>>,
    ) -> io::Result<()> {
        const ACCESS_DENIED: i32 = 5;
        const SHARING_VIOLATION: i32 = 32;

        let name = OsStr::new(name);
        self.write_staged(
            false,
            |file| file.write_all(bytes),
            |file| {
                let publish = || {
                    if let Some(check) = &check_replacement {
                        check()?;
                        self.0.replace_file_with_shared_readers(file, name)
                    } else {
                        self.0.publish_file(file, name)
                    }
                };
                // Keep the same staged file and recheck the destination on each attempt.
                for delay in [10, 25, 50, 100, 200] {
                    match publish() {
                        Err(error)
                            if matches!(
                                error.raw_os_error(),
                                Some(ACCESS_DENIED | SHARING_VIOLATION)
                            ) =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }
                        result => return result,
                    }
                }
                publish()
            },
        )
    }

    pub(crate) fn replace_staged(
        &self,
        stage: &Self,
        name: &str,
        staged: &super::FileStamp,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let name = OsStr::new(name);
        let file = stage.0.child_for_removal(name)?;
        Handle::require_owner_only_file(&file)?;
        check()?;
        staged.require_unchanged(&file)?;
        self.0.replace_file(&file, name)
    }

    fn write_staged(
        &self,
        readable: bool,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        publish: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        let stage = format!(".stage-{}", nonce()?);
        let name = OsStr::new(&stage);
        let mut file = if readable {
            self.0.create_read_write_file(name)
        } else {
            self.0.create_file(name)
        }
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("creating protected file stage: {error}"),
            )
        })?;
        let result = write(&mut file)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                publish(&file).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("publishing protected file stage: {error}"),
                    )
                })
            });
        if result.is_err() {
            let _ = tree::remove(file);
        }
        result
    }

    pub(crate) fn remove_copy(&self, name: &str) -> io::Result<()> {
        let file = self.0.child_for_removal(OsStr::new(name))?;
        Handle::require_owner_only_file(&file)?;
        tree::remove(file)
    }

    pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
        use std::os::windows::fs::MetadataExt as _;

        const REPARSE_POINT: u32 = 0x400;
        let file = self.0.child_for_removal(OsStr::new(name))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
            return Err(invalid("profile entry is not a regular file"));
        }
        tree::remove(file)
    }

    pub(crate) fn remove_unchanged(&self, name: &str, expected: &FileStamp) -> io::Result<()> {
        self.remove_unchanged_checked(name, expected, |_| Ok(()))
    }

    pub(crate) fn remove_unchanged_checked(
        &self,
        name: &str,
        expected: &FileStamp,
        check: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        let file = self.0.child_for_removal(OsStr::new(name))?;
        expected.require_unchanged(&file)?;
        check(&file)?;
        tree::remove(file)
    }

    #[allow(clippy::unnecessary_wraps)] // Windows flushes staged files; the Unix interface also syncs directories.
    pub(crate) fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

fn deletion_entry(file: &File) -> io::Result<(EntryIdentity, bool)> {
    let metadata = file.metadata()?;
    Ok((
        tree::file_identity(file)?,
        metadata.is_dir() && !tree::is_reparse_point(&metadata),
    ))
}
