//! Held native executable paths and bounded content fingerprints.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

use pgp::crypto::hash::HashAlgorithm;

use super::{FileStamp, invalid};

pub(crate) const MAX_EXECUTABLE: u64 = 512 * 1024 * 1024;

pub(crate) fn valid_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            || !path.extension().is_some_and(|suffix| suffix.eq_ignore_ascii_case("exe"))
            || path.components().any(|part| matches!(part, Component::Normal(name) if name.to_str().is_some_and(|name| name.contains(':'))))
        {
            return false;
        }
    }
    path.is_absolute()
        && !path.components().any(|part| part == Component::ParentDir)
        && path.components().count() <= 64
        && path
            .to_str()
            .is_some_and(|name| name.len() <= 4096 && !name.chars().any(char::is_control))
}

pub(crate) struct Opened {
    pub(crate) file: File,
    stamp: FileStamp,
    #[cfg(windows)]
    _parents: Vec<File>,
}

impl Opened {
    pub(crate) fn recheck(&self, path: &Path) -> io::Result<()> {
        self.stamp.require_unchanged(&self.file)?;
        self.stamp.require_unchanged(&open(path)?.file)
    }
}

pub(crate) fn open(path: &Path) -> io::Result<Opened> {
    if !valid_path(path) {
        return Err(invalid("the approved executable path is invalid"));
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::MetadataExt as _;

        let parent = path
            .parent()
            .ok_or_else(|| invalid("the executable has no parent directory"))?;
        let directory = crate::private_files::Directory::open_owner_controlled(parent)?;
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| invalid("the executable has no filename"))?;
        let file = directory.open_file(name)?;
        let metadata = file.metadata()?;
        let uid = metadata.uid();
        if (uid != 0 && uid != rustix::process::geteuid().as_raw())
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(invalid("the executable must be owner-controlled"));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&file)?;
        file
    };
    #[cfg(windows)]
    let (file, parents) = locked_windows_path(path)?;
    if path.canonicalize()? != path {
        return Err(invalid("the approved executable path changed"));
    }
    let stamp = FileStamp::read(&file)?;
    Ok(Opened {
        file,
        stamp,
        #[cfg(windows)]
        _parents: parents,
    })
}

#[cfg(windows)]
fn locked_windows_path(path: &Path) -> io::Result<(File, Vec<File>)> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;

    const SHARE_READ: u32 = 0x1;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let parents = lock_windows_ancestors(path)?;
    // Do not allow a replacement or a writable handle to the approved executable during launch.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || kortty_files::tree::is_reparse_point(&metadata) {
        return Err(invalid(
            "native executable must be an ordinary executable file",
        ));
    }
    Ok((file, parents))
}

#[cfg(windows)]
pub(crate) fn lock_windows_ancestors(path: &Path) -> io::Result<Vec<File>> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;

    const SHARE_READ: u32 = 0x1;
    const SHARE_WRITE: u32 = 0x2;
    const LIST_DIRECTORY: u32 = 0x1;
    const READ_ATTRIBUTES: u32 = 0x80;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let ancestors: Vec<_> = path.ancestors().skip(1).collect();
    let mut parents = Vec::with_capacity(ancestors.len());
    for parent in ancestors.into_iter().rev() {
        // Attribute-only handles do not participate in Windows share-access checks.
        let handle = OpenOptions::new()
            .access_mode(LIST_DIRECTORY | READ_ATTRIBUTES)
            .share_mode(SHARE_READ | SHARE_WRITE)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(parent)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || kortty_files::tree::is_reparse_point(&metadata) {
            return Err(invalid(
                "An ancestor directory changed or is a reparse point",
            ));
        }
        parents.push(handle);
    }
    Ok(parents)
}

pub(crate) fn digest(file: &mut File, canceled: Option<&AtomicBool>) -> io::Result<[u8; 32]> {
    if !(1..=MAX_EXECUTABLE).contains(&file.metadata()?.len()) {
        return Err(invalid(
            "native executable executable is empty or exceeds 512 MiB",
        ));
    }
    file.rewind()?;
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut read = 0;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if canceled.is_some_and(|canceled| canceled.load(Ordering::Acquire)) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "executable verification canceled",
            ));
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > MAX_EXECUTABLE {
            return Err(invalid("native executable exceeds 512 MiB"));
        }
        hash.update(&buffer[..count]);
    }
    hash.finalize()
        .as_ref()
        .try_into()
        .map_err(|_| invalid("invalid native executable fingerprint"))
}

pub(crate) fn require_native_image(file: &mut File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata()?.mode() & 0o6000 != 0 {
            return Err(invalid("the executable must not set user or group IDs"));
        }
    }
    file.rewind()?;
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)?;
    #[cfg(windows)]
    let native = bytes.starts_with(b"MZ");
    #[cfg(target_os = "linux")]
    let native = &bytes == b"\x7fELF";
    #[cfg(target_os = "macos")]
    let native = matches!(
        u32::from_be_bytes(bytes),
        0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe | 0xcafe_babf
    );
    if !native {
        return Err(invalid("select an ordinary native executable image"));
    }
    Ok(())
}
