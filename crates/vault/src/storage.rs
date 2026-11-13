use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::{Error, envelope::MAX_FILE_BYTES, hex, random};

pub(super) fn absolute_destination(path: &Path) -> Result<PathBuf, Error> {
    let name = path
        .file_name()
        .ok_or(Error::InvalidInput("vault path requires a file name"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent)?.join(name))
}

pub(super) fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if regular(&metadata) => Ok(true),
        Ok(_) => Err(Error::InvalidFormat),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // A concurrent fork can briefly retain the open file description
        // until exec closes its duplicate. Release the lock before closing.
        let _ = self.0.unlock();
    }
}

pub(super) fn lock(path: &Path) -> Result<Lock, Error> {
    let lock_path = sidecar(path, ".lock")?;
    let file = open_lock_file(&lock_path)?;
    if !regular(&file.metadata()?) {
        return Err(Error::InvalidFormat);
    }
    match file.try_lock() {
        Ok(()) => Ok(Lock(file)),
        Err(TryLockError::WouldBlock) => Err(Error::Busy),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(super) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    if !exists(path)? {
        return Err(Error::NotFound);
    }
    let file = open_snapshot(path)?;
    let metadata = file.metadata()?;
    if !regular(&metadata) {
        return Err(Error::InvalidFormat);
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(Error::Limit);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::Limit);
    }
    Ok(bytes)
}

pub(super) fn publish(path: &Path, contents: &[u8], replace: bool) -> Result<(), Error> {
    publish_checked(path, contents, replace, || Ok(()))
}

pub(super) fn publish_checked(
    path: &Path,
    contents: &[u8],
    replace: bool,
    check: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    let (mut temporary, mut file) = create_temporary(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    check()?;
    let result = if replace {
        fs::rename(&temporary.path, path)
    } else {
        rename_no_replace(&temporary.path, path)
    };
    match result {
        Ok(()) => {
            temporary.published = true;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(Error::AlreadyExists),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or(Error::InvalidInput("vault path requires a directory"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(Error::Durability)
}

#[cfg(windows)]
pub(super) fn sync_parent(_path: &Path) -> Result<(), Error> {
    // The file is synced before its same-volume rename. The standard Windows
    // filesystem interface does not expose the Unix directory fsync operation.
    Ok(())
}

fn create_temporary(path: &Path) -> Result<(Temporary, File), Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    for _ in 0..8 {
        let suffix = format!(".tmp-{}", hex(random::<16>()?));
        let candidate = sidecar(path, &suffix)?;
        match options.open(&candidate) {
            Ok(file) => {
                return Ok((
                    Temporary {
                        path: candidate,
                        published: false,
                    },
                    file,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::AlreadyExists)
}

fn sidecar(path: &Path, suffix: &str) -> Result<PathBuf, Error> {
    let mut name = OsString::from(
        path.file_name()
            .ok_or(Error::InvalidInput("vault path requires a file name"))?,
    );
    name.push(suffix);
    Ok(path.with_file_name(name))
}

struct Temporary {
    path: PathBuf,
    published: bool,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn regular(metadata: &Metadata) -> bool {
    metadata.is_file()
}

#[cfg(windows)]
fn regular(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let flags =
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(open(path, flags, Mode::RUSR | Mode::WUSR)?))
}

#[cfg(windows)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, SECURITY_IDENTIFICATION,
    };
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(path)
}

#[cfg(unix)]
fn open_snapshot(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(open(path, flags, Mode::empty())?))
}

#[cfg(windows)]
fn open_snapshot(path: &Path) -> io::Result<File> {
    kortty_files::open_regular_without_reparse(path)
}

#[cfg(unix)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    Ok(renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)?)
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    kortty_files::rename_no_replace(from, to)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::DirBuilderExt as _;

    use super::*;

    #[test]
    fn vault_lock_releases_while_a_duplicate_descriptor_is_open() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let nonce = hex(random::<16>().unwrap());
        let path = std::env::temp_dir().join(format!("kortty-vault-lock-{nonce}"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let scratch = Scratch(path);
        let path = scratch.0.join("vault.bin");

        let original = lock(&path).unwrap();
        // Test double for the descriptor inherited by a child before exec.
        let inherited = original.0.try_clone().unwrap();
        assert!(matches!(lock(&path), Err(Error::Busy)));
        drop(original);

        let successor = lock(&path).unwrap();
        assert!(matches!(lock(&path), Err(Error::Busy)));
        drop(inherited);
        assert!(matches!(lock(&path), Err(Error::Busy)));
        drop(successor);
    }
}
