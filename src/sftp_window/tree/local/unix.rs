use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, FileTimes};
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fstat, mkdirat, open, openat, renameat,
    renameat_with, statat, unlinkat,
};
use sha2::{Digest, Sha256};

use super::reviewed::{ReviewedStamp, ReviewedVolume, changed};
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
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    pub(crate) fn file(&self) -> &File {
        &self.0
    }

    pub(crate) fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.0.metadata()
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
        let child = self.open_child(name)?;
        #[cfg(target_os = "macos")]
        if let Err(error) = kortty_files::clear_extended_acl(child.file()) {
            self.require_child(name, &child)?;
            unlinkat(&self.0, name, AtFlags::REMOVEDIR)?;
            return Err(error);
        }
        Ok(child)
    }

    /// Creates a destination folder with ordinary umask and default ACL inheritance.
    pub(crate) fn create_transfer_child(&self, name: &OsStr) -> io::Result<Self> {
        mkdirat(&self.0, name, Mode::from_raw_mode(0o777))?;
        self.open_child(name)
    }

    /// The caller keeps the payload behind a private ancestor until publication.
    pub(crate) fn create_tree_payload(&self, name: &OsStr) -> io::Result<Self> {
        self.create_transfer_child(name)
    }

    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<File> {
        self.create_file_with_access(name, OFlags::WRONLY)
    }

    /// Creates ordinary output only beneath the caller's private tree barrier.
    pub(crate) fn create_ordinary_file(&self, name: &OsStr) -> io::Result<File> {
        Ok(File::from(openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )?))
    }

    pub(in crate::sftp_window::tree) fn create_read_write_file(
        &self,
        name: &OsStr,
    ) -> io::Result<File> {
        self.create_file_with_access(name, OFlags::RDWR)
    }

    fn create_file_with_access(&self, name: &OsStr, access: OFlags) -> io::Result<File> {
        let flags = access | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(openat(&self.0, name, flags, Mode::RUSR | Mode::WUSR)?);
        #[cfg(target_os = "macos")]
        if let Err(error) = kortty_files::clear_extended_acl(&file) {
            self.require_file(name, &file)?;
            unlinkat(&self.0, name, AtFlags::empty())?;
            return Err(error);
        }
        Ok(file)
    }

    pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
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
        let actual = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let expected = fstat(expected)?;
        if FileType::from_raw_mode(actual.st_mode) == FileType::RegularFile
            && (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
        {
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

pub(super) fn hold_reviewed_child(
    parent: &Directory,
    name: &OsStr,
    lock: rustix::fs::FlockOperation,
) -> io::Result<Directory> {
    let child = parent.open_child(name)?;
    rustix::fs::flock(&child.0, lock)?;
    parent.require_child(name, &child)?;
    Ok(child)
}

pub(super) fn hold_reviewed_file_ancestors(path: &Path) -> io::Result<Vec<Directory>> {
    use rustix::fs::{FlockOperation, flock};

    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(changed());
    }
    let mut held = vec![Directory::open_path(Path::new("/"))?];
    for component in components {
        let Component::Normal(name) = component else {
            return Err(changed());
        };
        let child = held
            .last()
            .expect("held filesystem root")
            .open_child(name)?;
        flock(child.file(), FlockOperation::NonBlockingLockShared)?;
        held.push(child);
    }
    Ok(held)
}

pub(super) fn move_directory(
    source: &Directory,
    name: &OsStr,
    destination: &Directory,
    target: &OsStr,
) -> io::Result<()> {
    Ok(renameat_with(
        &source.0,
        name,
        &destination.0,
        target,
        RenameFlags::NOREPLACE,
    )?)
}

pub(super) fn remove_empty_stage(
    parent: &Directory,
    name: &OsStr,
    stage: Directory,
) -> io::Result<()> {
    let metadata = stage.file().metadata()?;
    if metadata.mode() & 0o7777 != 0o700 || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(changed());
    }
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(stage.file())?;
    parent.require_child(name, &stage)?;
    Ok(unlinkat(&parent.0, name, AtFlags::REMOVEDIR)?)
}

pub(super) fn remove_reviewed_entry(
    parent: &Directory,
    name: &OsStr,
    expected: &ReviewedStamp,
    directory: bool,
) -> io::Result<()> {
    match parent.entry(name) {
        Ok(Entry::Directory(opened)) if directory => {
            let current = reviewed_stamp(opened.file(), true, false)?;
            if !expected.same_identity(&current) {
                return Err(changed());
            }
            parent.require_child(name, &opened)?;
            // This descriptor-relative removal refuses a nonempty directory.
            Ok(unlinkat(&parent.0, name, AtFlags::REMOVEDIR)?)
        }
        Ok(Entry::File(opened)) if !directory => {
            let current = reviewed_stamp(&opened, false, false)?;
            if !expected.same_identity(&current) {
                return Err(changed());
            }
            parent.require_file(name, &opened)?;
            Ok(unlinkat(&parent.0, name, AtFlags::empty())?)
        }
        Err(error) => Err(error),
        _ => Err(changed()),
    }
}

pub(super) fn reviewed_volume(file: &File) -> io::Result<ReviewedVolume> {
    #[cfg(target_os = "linux")]
    let mount = {
        use rustix::fs::{StatxFlags, statx};
        let info = statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
        if info.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
            return Err(changed());
        }
        info.stx_mnt_id
    };
    #[cfg(not(target_os = "linux"))]
    let mount = 0;
    Ok(ReviewedVolume(file.metadata()?.dev(), mount))
}

pub(super) fn reviewed_stamp(
    file: &File,
    directory: bool,
    parent: bool,
) -> io::Result<ReviewedStamp> {
    let before = file.metadata()?;
    let uid = rustix::process::geteuid().as_raw();
    let mode = before.mode();
    if (before.uid() != uid && !(parent && before.uid() == 0))
        || mode & 0o7022 != 0
        || if directory {
            !before.is_dir()
        } else {
            !before.is_file() || before.nlink() != 1
        }
    {
        return Err(changed());
    }
    #[cfg(target_os = "macos")]
    {
        kortty_files::require_no_extended_acl(file)?;
        if fstat(file)?.st_flags != 0 {
            return Err(changed());
        }
    }
    let mut attributes = Sha256::new();
    for (name, value) in reviewed_xattrs(file)? {
        attributes.update((name.len() as u32).to_be_bytes());
        attributes.update(name.as_bytes());
        attributes.update((value.len() as u32).to_be_bytes());
        attributes.update(value);
    }
    let mut identity = [0; 16];
    identity[..8].copy_from_slice(&before.ino().to_be_bytes());
    let stamp = ReviewedStamp {
        volume: reviewed_volume(file)?,
        identity,
        permissions: mode,
        owner: before.uid(),
        group: before.gid(),
        attributes: attributes.finalize().into(),
        size: before.len(),
        created: 0,
        modified: (before.mtime(), before.mtime_nsec()),
        changed: (before.ctime(), before.ctime_nsec()),
    };
    let after = file.metadata()?;
    if before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() {
        return Err(changed());
    }
    Ok(stamp)
}

pub(super) fn copy_reviewed_metadata(from: &File, to: &File, directory: bool) -> io::Result<()> {
    use rustix::fs::{XattrFlags, fchown, fremovexattr, fsetxattr};

    let expected = reviewed_stamp(from, directory, false)?;
    let desired = reviewed_xattrs(from)?;
    let current = reviewed_xattrs(to)?;
    if to.metadata()?.gid() != expected.group {
        fchown(
            to,
            None,
            Some(rustix::process::Gid::from_raw(expected.group)),
        )?;
    }
    for name in current.keys().filter(|name| !desired.contains_key(*name)) {
        fremovexattr(to, name.as_str())?;
    }
    for (name, value) in desired
        .iter()
        .filter(|(name, value)| current.get(*name) != Some(*value))
    {
        fsetxattr(to, name.as_str(), value, XattrFlags::empty())?;
    }
    let source_metadata = from.metadata()?;
    to.set_permissions(source_metadata.permissions())?;
    to.set_times(FileTimes::new().set_modified(source_metadata.modified()?))?;
    let actual = reviewed_stamp(to, directory, false)?;
    if actual.permissions != expected.permissions
        || actual.owner != expected.owner
        || actual.group != expected.group
        || actual.attributes != expected.attributes
        || actual.modified != expected.modified
        || reviewed_stamp(from, directory, false)? != expected
    {
        return Err(changed());
    }
    Ok(())
}

fn reviewed_xattrs(file: &File) -> io::Result<BTreeMap<String, Vec<u8>>> {
    use rustix::fs::{fgetxattr, fgetxattr_query, flistxattr};

    const MAX_ATTRIBUTES: usize = 128;
    const MAX_BYTES: usize = 128 * 1024;
    let mut names = [0; 4096];
    let length = match flistxattr(file, &mut names) {
        Ok(length) => length,
        Err(rustix::io::Errno::NOTSUP) => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let names = names[..length]
        .split(|name| *name == 0)
        .filter(|name| !name.is_empty());
    let mut result = BTreeMap::new();
    let mut available = MAX_BYTES;
    for name in names {
        let name = std::str::from_utf8(name).map_err(|_| changed())?;
        #[cfg(target_os = "linux")]
        if name != "security.selinux" && !name.starts_with("user.") {
            return Err(changed());
        }
        if result.len() == MAX_ATTRIBUTES {
            return Err(changed());
        }
        let size = fgetxattr_query(file, name)?;
        available = available
            .checked_sub(size + name.len())
            .ok_or_else(changed)?;
        let mut value = vec![0; size.max(1)];
        if fgetxattr(file, name, value.as_mut_slice())? != size {
            return Err(changed());
        }
        value.truncate(size);
        result.insert(name.to_owned(), value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn transfer_folder_uses_ordinary_creation_and_refuses_existing_names() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let reference = root.join("ordinary");
        fs::create_dir(&reference).unwrap();
        let child = parent
            .create_transfer_child(OsStr::new("transfer"))
            .unwrap();
        assert_eq!(
            child.file().metadata().unwrap().mode() & 0o7777,
            fs::metadata(&reference).unwrap().mode() & 0o7777
        );
        let Entry::Directory(listed) = parent.entry(OsStr::new("transfer")).unwrap() else {
            panic!("expected the created transfer directory");
        };
        assert!(child.same_as(&listed).unwrap());
        assert!(
            parent
                .create_transfer_child(OsStr::new("transfer"))
                .is_err()
        );
        fs::write(root.join("file"), b"existing contents").unwrap();
        assert!(parent.create_transfer_child(OsStr::new("file")).is_err());
        assert_eq!(fs::read(root.join("file")).unwrap(), b"existing contents");
        symlink(&reference, root.join("link")).unwrap();
        assert!(parent.create_transfer_child(OsStr::new("link")).is_err());
        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .is_symlink()
        );
        let private = parent.create_child(OsStr::new("private")).unwrap();
        assert_eq!(private.file().metadata().unwrap().mode() & 0o077, 0);
    }
}
