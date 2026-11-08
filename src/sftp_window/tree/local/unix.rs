use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, mkdirat, open, openat, renameat,
    renameat_with, statat, unlinkat,
};

use super::{Entry, limit};
use crate::sftp_window::files::LocalStamp;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const SOURCE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY);

pub(crate) struct Directory(File);

impl Directory {
    pub(crate) fn file(&self) -> &File {
        &self.0
    }

    pub(crate) fn open_path(path: &Path) -> io::Result<Self> {
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(io::Error::other("Expected an absolute local folder"));
        }
        let mut current = Self(File::from(open("/", DIRECTORY, Mode::empty())?));
        for component in components {
            let Component::Normal(name) = component else {
                return Err(io::Error::other("Expected a canonical local folder"));
            };
            current = current.open_child(name)?;
        }
        Ok(current)
    }

    fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        Ok(Self(File::from(openat(
            &self.0,
            name,
            DIRECTORY,
            Mode::empty(),
        )?)))
    }

    pub(super) fn entry(&self, name: &OsStr) -> io::Result<Entry> {
        let metadata = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => Ok(Entry::Directory(self.open_child(name)?)),
            FileType::RegularFile => {
                let file = File::from(openat(&self.0, name, SOURCE, Mode::empty())?);
                if !file.metadata()?.is_file() {
                    return Err(io::Error::other(
                        "Local upload source changed before opening",
                    ));
                }
                Ok(Entry::File(file))
            }
            _ => Ok(Entry::Skip),
        }
    }

    pub(crate) fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        mkdirat(&self.0, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
        self.open_child(name)
    }

    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<File> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(openat(
            &self.0,
            name,
            flags,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    pub(in crate::sftp_window::tree) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in Dir::read_from(&self.0)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if names.len() == limit {
                return Err(super::limit());
            }
            names.push(OsString::from_vec(name.to_vec()));
        }
        Ok(names)
    }

    pub(crate) fn same_as(&self, other: &Self) -> io::Result<bool> {
        Ok(self.identity()? == other.identity()?)
    }

    pub(crate) fn identity(&self) -> io::Result<(u64, u64)> {
        let metadata = self.0.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    fn require_child(&self, name: &OsStr, expected: &Self) -> io::Result<()> {
        if self.open_child(name)?.same_as(expected)? {
            Ok(())
        } else {
            Err(io::Error::other("Local staging folder changed"))
        }
    }

    pub(super) fn publish(&self, name: &OsStr, stage: &Self, target: &OsStr) -> io::Result<()> {
        self.require_child(name, stage)?;
        Ok(renameat_with(
            &self.0,
            name,
            &self.0,
            target,
            RenameFlags::NOREPLACE,
        )?)
    }

    pub(super) fn publish_file(
        &self,
        stage: &Self,
        name: &OsStr,
        file: &File,
        target: &OsStr,
    ) -> io::Result<()> {
        stage.require_file(name, file)?;
        Ok(renameat_with(
            &stage.0,
            name,
            &self.0,
            target,
            RenameFlags::NOREPLACE,
        )?)
    }

    pub(crate) fn replace_file(
        &self,
        stage: &Self,
        name: &OsStr,
        file: &File,
        target: &OsStr,
        expected: &LocalStamp,
    ) -> io::Result<()> {
        stage.require_file(name, file)?;
        if let Entry::File(current) = self.entry(target)?
            && LocalStamp::capture(&current.metadata()?) == *expected
        {
            return Ok(renameat(&stage.0, name, &self.0, target)?);
        }
        Err(io::Error::other("The local transfer destination changed"))
    }

    fn require_file(&self, name: &OsStr, expected: &File) -> io::Result<()> {
        let Entry::File(actual) = self.entry(name)? else {
            return Err(io::Error::other("The local staging file changed"));
        };
        let actual = actual.metadata()?;
        let expected = expected.metadata()?;
        if (actual.dev(), actual.ino()) == (expected.dev(), expected.ino()) {
            Ok(())
        } else {
            Err(io::Error::other("The local staging file changed"))
        }
    }

    pub(super) fn discard(
        &self,
        name: &OsStr,
        stage: Self,
        depth: usize,
        remaining: &mut usize,
    ) -> io::Result<()> {
        self.require_child(name, &stage)?;
        for child in stage.names(*remaining)? {
            *remaining = remaining.checked_sub(1).ok_or_else(limit)?;
            let attributes = statat(&stage.0, &child, AtFlags::SYMLINK_NOFOLLOW)?;
            if FileType::from_raw_mode(attributes.st_mode) == FileType::Directory {
                let depth = depth.checked_sub(1).ok_or_else(limit)?;
                stage.discard(&child, stage.open_child(&child)?, depth, remaining)?;
            } else {
                unlinkat(&stage.0, &child, AtFlags::empty())?;
            }
        }
        self.require_child(name, &stage)?;
        Ok(unlinkat(&self.0, name, AtFlags::REMOVEDIR)?)
    }
}
