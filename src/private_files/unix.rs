use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, mkdirat, open, openat, renameat,
    renameat_with, statat, unlinkat,
};

use super::{EntryIdentity, FileStamp, invalid, nonce};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const REGULAR: OFlags = OFlags::NOFOLLOW
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY)
    .union(OFlags::CLOEXEC);
const PRIVATE_FILE: Mode = Mode::RUSR.union(Mode::WUSR);
const PRIVATE_DIRECTORY: Mode = PRIVATE_FILE.union(Mode::XUSR);

pub(crate) struct Directory(File);

impl Directory {
    pub(crate) fn open(path: &Path, create: bool) -> io::Result<Self> {
        Self::open_checked(path, create, |_| Ok(()))
    }

    pub(crate) fn open_owner_controlled(path: &Path) -> io::Result<Self> {
        Self::owner_controlled(path, false)
    }

    pub(crate) fn open_administrator_controlled(path: &Path) -> io::Result<Self> {
        Self::open_checked(path, false, require_administrator_controlled)
    }

    pub(crate) fn create_owner_controlled(path: &Path) -> io::Result<Self> {
        Self::owner_controlled(path, true)
    }

    fn owner_controlled(path: &Path, create: bool) -> io::Result<Self> {
        Self::open_checked(path, create, |file| {
            let metadata = file.metadata()?;
            let uid = metadata.uid();
            let mode = metadata.mode();
            let trusted_owner = uid == 0 || uid == rustix::process::geteuid().as_raw();
            let protected_directory = mode & 0o022 == 0 || (uid == 0 && mode & 0o1000 != 0);
            if !trusted_owner || !protected_directory {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "The path requires owner-controlled directories",
                ));
            }
            #[cfg(target_os = "macos")]
            kortty_files::require_no_granting_extended_acl(file)?;
            Ok(())
        })
    }

    fn open_checked(
        path: &Path,
        create: bool,
        check: impl Fn(&File) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(invalid("expected an absolute profile directory"));
        }
        let mut directory = Self(open("/", DIRECTORY, Mode::empty())?.into());
        check(&directory.0)?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid("expected an ordinary profile directory"));
            };
            directory = directory.child(name, create)?;
            check(&directory.0)?;
        }
        Ok(directory)
    }

    fn child(&self, name: &OsStr, create: bool) -> io::Result<Self> {
        let child = || openat(&self.0, name, DIRECTORY, Mode::empty());
        let file = match child() {
            Err(error) if create && error == rustix::io::Errno::NOENT => {
                match mkdirat(&self.0, name, PRIVATE_DIRECTORY) {
                    Ok(()) => self.sync()?,
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(error) => return Err(error.into()),
                }
                child()?
            }
            result => result?,
        };
        Ok(Self(file.into()))
    }

    pub(crate) fn private_child(&self, name: &str, create: bool) -> io::Result<Self> {
        let child = self.child(OsStr::new(name), create)?;
        require_private(&child.0, false)?;
        Ok(child)
    }

    pub(crate) fn subdirectory(&self, name: &str, create: bool) -> io::Result<Self> {
        self.child(OsStr::new(name), create)
    }

    pub(crate) fn same_as(&self, other: &Self) -> io::Result<bool> {
        self.same_as_file(&other.0)
    }

    pub(crate) fn same_as_file(&self, other: &File) -> io::Result<bool> {
        let left = self.0.metadata()?;
        let right = other.metadata()?;
        Ok(right.is_dir() && (left.dev(), left.ino()) == (right.dev(), right.ino()))
    }

    pub(crate) fn move_subdirectory(&self, name: &str, destination: &Self) -> io::Result<()> {
        self.move_entry(OsStr::new(name), destination)
    }

    pub(crate) fn identity(&self) -> io::Result<EntryIdentity> {
        let metadata = self.0.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(crate) fn create_private_subdirectory(&self, name: &OsStr) -> io::Result<Self> {
        mkdirat(&self.0, name, PRIVATE_DIRECTORY)?;
        let child = self.deletion_child(name)?;
        if let Err(error) = require_private(&child.0, false) {
            if let Ok(identity) = child.identity() {
                let _ = self.remove_deletion_entry(name, identity, true);
            }
            return Err(error);
        }
        Ok(child)
    }

    pub(crate) fn move_entry(&self, name: &OsStr, destination: &Self) -> io::Result<()> {
        Ok(renameat_with(
            &self.0,
            name,
            &destination.0,
            name,
            RenameFlags::NOREPLACE,
        )?)
    }

    pub(crate) fn deletion_child(&self, name: &OsStr) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self(self.open_deletion_entry(name, DIRECTORY)?))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let child = self.child(name, false)?;
            if child.identity()?.0 != self.identity()?.0 {
                return Err(invalid(
                    "Journal deletion cannot cross a filesystem boundary",
                ));
            }
            Ok(child)
        }
    }

    /// Opens a private directory without following links or crossing a mount.
    pub(crate) fn private_deletion_child(&self, name: &OsStr) -> io::Result<Self> {
        if Path::new(name).file_name() != Some(name) {
            return Err(invalid("expected an ordinary profile directory name"));
        }
        let child = self.deletion_child(name)?;
        require_private(&child.0, false)?;
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    fn open_deletion_entry(&self, name: &OsStr, flags: OFlags) -> io::Result<File> {
        use rustix::fs::{ResolveFlags, openat2};

        Ok(openat2(
            &self.0,
            name,
            flags,
            Mode::empty(),
            ResolveFlags::NO_XDEV | ResolveFlags::NO_SYMLINKS,
        )?
        .into())
    }

    pub(crate) fn deletion_entry(&self, name: &OsStr) -> io::Result<(EntryIdentity, bool)> {
        #[cfg(target_os = "linux")]
        {
            let file =
                self.open_deletion_entry(name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC)?;
            let metadata = file.metadata()?;
            Ok(((metadata.dev(), metadata.ino()), metadata.is_dir()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let metadata = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
            #[allow(clippy::unnecessary_cast)] // stat field widths vary by target.
            let identity = (metadata.st_dev as u64, metadata.st_ino as u64);
            if identity.0 != self.identity()?.0 {
                return Err(invalid(
                    "Journal deletion cannot cross a filesystem boundary",
                ));
            }
            Ok((
                identity,
                FileType::from_raw_mode(metadata.st_mode) == FileType::Directory,
            ))
        }
    }

    pub(crate) fn remove_deletion_entry(
        &self,
        name: &OsStr,
        expected: EntryIdentity,
        directory: bool,
    ) -> io::Result<()> {
        if self.deletion_entry(name)? != (expected, directory) {
            return Err(invalid("Journal entry changed before deletion"));
        }
        let flags = if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        Ok(unlinkat(&self.0, name, flags)?)
    }

    pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in Dir::read_from(&self.0)? {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            if names.len() == limit {
                return Err(invalid("profile directory exceeds its file count limit"));
            }
            names.push(OsStr::from_bytes(bytes).to_owned());
        }
        Ok(names)
    }

    pub(crate) fn has_entry(&self, name: &str) -> io::Result<bool> {
        match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn open_file(&self, name: &str) -> io::Result<File> {
        let before = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
            return Err(invalid("profile entry is not a regular file"));
        }
        let file: File = openat(&self.0, name, REGULAR | OFlags::RDONLY, Mode::empty())?.into();
        let after = file.metadata()?;
        #[allow(clippy::unnecessary_cast)] // stat field widths differ across Unix targets.
        if !after.is_file()
            || before.st_dev as u64 != after.dev()
            || before.st_ino as u64 != after.ino()
        {
            return Err(invalid("profile entry changed while being opened"));
        }
        Ok(file)
    }

    pub(crate) fn open_private_file(&self, name: &str) -> io::Result<File> {
        let file = self.open_file(name)?;
        require_private(&file, true)?;
        Ok(file)
    }

    pub(crate) fn open_existing_lock(&self, name: &str) -> io::Result<File> {
        let file: File = openat(&self.0, name, REGULAR | OFlags::RDWR, Mode::empty())?.into();
        require_private(&file, true)?;
        Ok(file)
    }

    pub(crate) fn open_lock(&self, name: &str) -> io::Result<File> {
        let flags = REGULAR | OFlags::RDWR;
        let open = || openat(&self.0, name, flags, Mode::empty());
        let file: File = match open() {
            Err(rustix::io::Errno::NOENT) => match openat(
                &self.0,
                name,
                flags | OFlags::CREATE | OFlags::EXCL,
                PRIVATE_FILE,
            ) {
                Err(rustix::io::Errno::EXIST) => open(),
                result => result,
            },
            result => result,
        }?
        .into();
        require_private(&file, true)?;
        Ok(file)
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
        self.publish_checked_access(name, OFlags::WRONLY, write, check)
    }

    pub(crate) fn publish_checked_readable(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_owner_controlled_publication()?;
        self.publish_checked_access(name, OFlags::RDWR, write, || {
            self.require_owner_controlled_publication()?;
            check()
        })
    }

    fn require_owner_controlled_publication(&self) -> io::Result<()> {
        let metadata = self.0.metadata()?;
        let uid = metadata.uid();
        if (uid != 0 && uid != rustix::process::geteuid().as_raw()) || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The publication directory must be owner-controlled",
            ));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&self.0)?;
        Ok(())
    }

    fn publish_checked_access(
        &self,
        name: &str,
        access: OFlags,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_staged(access, write, |stage| {
            check()?;
            Ok(renameat_with(
                &self.0,
                stage,
                &self.0,
                name,
                RenameFlags::NOREPLACE,
            )?)
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
        self.write_staged(OFlags::WRONLY, write, |stage| {
            check()?;
            Ok(renameat(&self.0, stage, &self.0, name)?)
        })
    }

    pub(crate) fn replace_with_readable(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_owner_controlled_publication()?;
        self.write_staged(OFlags::RDWR, write, |stage| {
            self.require_owner_controlled_publication()?;
            check()?;
            Ok(renameat(&self.0, stage, &self.0, name)?)
        })
    }

    pub(crate) fn replace_staged(
        &self,
        stage: &Self,
        name: &str,
        staged: &super::FileStamp,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        check()?;
        staged.require_unchanged(&stage.open_private_file(name)?)?;
        Ok(renameat(&stage.0, name, &self.0, name)?)
    }

    fn write_staged(
        &self,
        access: OFlags,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        publish: impl FnOnce(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        let stage = format!(".stage-{}", nonce()?);
        let mut file: File = openat(
            &self.0,
            &stage,
            REGULAR | access | OFlags::CREATE | OFlags::EXCL,
            PRIVATE_FILE,
        )?
        .into();
        let result = require_private(&file, true)
            .and_then(|()| write(&mut file))
            .and_then(|()| file.sync_all())
            .and_then(|()| publish(&stage));
        if result.is_err() {
            let _ = unlinkat(&self.0, &stage, AtFlags::empty());
        }
        result
    }

    pub(crate) fn remove_copy(&self, name: &str) -> io::Result<()> {
        let _file = self.open_private_file(name)?;
        Ok(unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
        let _file = self.open_file(name)?;
        Ok(unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub(crate) fn remove_unchanged(&self, name: &str, expected: &FileStamp) -> io::Result<()> {
        expected.require_unchanged(&self.open_file(name)?)?;
        Ok(unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.0.sync_all()
    }
}

pub(crate) fn require_administrator_controlled(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "A policy model must use an administrator-controlled path",
        ));
    }
    #[cfg(target_os = "macos")]
    kortty_files::require_no_granting_extended_acl(file)?;
    Ok(())
}

fn require_private(file: &File, regular: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || (regular && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "profile entry is not private to the current user",
        ));
    }
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(file)?;
    Ok(())
}
