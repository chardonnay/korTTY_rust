use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::{Directory, Entry, EntryKind, EntryName, Location};

const MAX_LOCAL_ENTRIES: usize = 100_000;

pub(crate) fn list_local(path: &Path) -> io::Result<Directory> {
    let path = fs::canonicalize(path)?;
    let mut entries = Vec::new();
    for result in fs::read_dir(&path)? {
        if entries.len() >= MAX_LOCAL_ENTRIES {
            return Err(io::Error::other("directory has more than 100,000 entries"));
        }
        let entry = result?;
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let name = entry.file_name();
        let display = display(&name.to_string_lossy());
        entries.push(Entry {
            name: EntryName::Local(name),
            hidden: display.starts_with('.') || windows_hidden(&metadata),
            display,
            kind: local_kind(&metadata),
            size: metadata.is_file().then_some(metadata.len()),
            modified: None,
            mode: local_mode(&metadata),
        });
    }
    Ok(Directory::sorted(Location::Local(path), entries))
}

pub(super) fn local_kind(metadata: &Metadata) -> EntryKind {
    if metadata.file_type().is_symlink() {
        EntryKind::Link
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

#[cfg(windows)]
fn windows_hidden(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn windows_hidden(_: &Metadata) -> bool {
    false
}

#[cfg(unix)]
fn local_mode(metadata: &Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn local_mode(_: &Metadata) -> Option<u32> {
    None
}

pub(super) fn display(value: &str) -> String {
    let mut shown = String::new();
    for ch in value.chars() {
        if shown.len() >= 1024 {
            break;
        }
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            shown.extend(ch.escape_default());
        } else {
            shown.push(ch);
        }
    }
    shown
}

pub(super) fn remote_name(name: &OsStr) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes()
    };
    #[cfg(not(unix))]
    let bytes = name.to_str().ok_or_else(invalid_name)?.as_bytes();
    validate_transfer_name(bytes)?;
    Ok(bytes.to_vec())
}

pub(super) fn local_name(bytes: &[u8]) -> io::Result<OsString> {
    validate_remote_name(bytes)?;
    #[cfg(unix)]
    let name = {
        use std::os::unix::ffi::OsStringExt as _;
        OsString::from_vec(bytes.to_vec())
    };
    #[cfg(not(unix))]
    let name = {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid_name())?;
        if !valid_windows_name(text) {
            return Err(invalid_name());
        }
        OsString::from(text)
    };
    let mut components = Path::new(&name).components();
    if !matches!(components.next(), Some(Component::Normal(part)) if part == name)
        || components.next().is_some()
    {
        return Err(invalid_name());
    }
    Ok(name)
}

#[cfg(windows)]
fn valid_windows_name(name: &str) -> bool {
    if name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
    {
        return false;
    }
    !windows_device_name(name)
}

fn windows_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|tail| {
            matches!(
                tail,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

pub(crate) fn validate_transfer_name(name: &[u8]) -> io::Result<()> {
    validate_remote_name(name)?;
    // A Windows SFTP server can reinterpret a literal Unix backslash, colon,
    // reserved device name or trailing space/dot while resolving the child.
    if name.iter().any(|&byte| matches!(byte, b'\\' | b':'))
        || matches!(name.last(), Some(b'.' | b' '))
        || std::str::from_utf8(name).is_ok_and(windows_device_name)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Name has an ambiguous meaning on some SFTP servers",
        ));
    }
    Ok(())
}

pub(super) fn validate_remote_name(name: &[u8]) -> io::Result<()> {
    if name.is_empty()
        || matches!(name, b"." | b"..")
        || name.iter().any(|&byte| matches!(byte, 0 | b'/'))
    {
        return Err(invalid_name());
    }
    Ok(())
}

pub(super) fn remote_join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.is_empty() && !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

fn invalid_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "name cannot be represented as one file-system component",
    )
}

pub(crate) fn nonce() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut name = String::from(".kortty-");
    for byte in bytes {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".kortty-part");
    Ok(name)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct LocalStamp {
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    created: u64,
    #[cfg(windows)]
    attributes: u32,
}

impl LocalStamp {
    pub(crate) fn capture(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt as _;
        Self {
            kind: local_kind(metadata),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(unix)]
            uid: metadata.uid(),
            #[cfg(unix)]
            gid: metadata.gid(),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            created: metadata.creation_time(),
            #[cfg(windows)]
            attributes: metadata.file_attributes(),
        }
    }
}

pub(super) fn local_existing(path: &Path) -> io::Result<Option<LocalStamp>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(LocalStamp::capture(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub(super) fn open_without_links(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(open(path, flags, Mode::empty())?))
}

#[cfg(windows)]
pub(super) fn open_without_links(path: &Path) -> io::Result<File> {
    kortty_files::open_regular_without_reparse(path)
}

#[cfg(unix)]
pub(super) fn rename_local(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    Ok(renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)?)
}

#[cfg(windows)]
pub(super) fn rename_local(from: &Path, to: &Path) -> io::Result<()> {
    kortty_files::rename_no_replace(from, to)
}

#[cfg(unix)]
pub(super) fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let file = open_without_links(path)?;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub(super) fn set_permissions(_: &Path, _: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "POSIX file permissions are unavailable",
    ))
}

pub(super) struct StagedFile {
    path: Option<PathBuf>,
    file: File,
}

impl StagedFile {
    pub(super) fn create(parent: &Path) -> io::Result<Self> {
        let path = parent.join(nonce()?);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self {
            path: Some(path),
            file,
        })
    }

    pub(super) fn writer(&mut self) -> &mut File {
        &mut self.file
    }

    pub(super) fn publish(
        mut self,
        destination: &Path,
        replace: Option<&LocalStamp>,
    ) -> io::Result<()> {
        let file = &self.file;
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::other("staged download has no path"))?;
        if let Some(expected) = replace {
            let current = check_replacement(destination, expected)?;
            preserve_permissions(file, &current)?;
        }
        file.sync_all()?;
        let stamp = LocalStamp::capture(&file.metadata()?);
        if !owns_staged_path(path, &stamp)? {
            return Err(io::Error::other("staging file changed during download"));
        }
        if let Some(expected) = replace {
            check_replacement(destination, expected)?;
            fs::rename(path, destination)?;
        } else {
            rename_local(path, destination)?;
        }
        self.path = None;
        Ok(())
    }
}

fn owns_staged_path(path: &Path, stamp: &LocalStamp) -> io::Result<bool> {
    Ok(stamp.kind == EntryKind::File && local_existing(path)?.as_ref() == Some(stamp))
}

fn check_replacement(destination: &Path, expected: &LocalStamp) -> io::Result<Metadata> {
    let current = fs::symlink_metadata(destination)?;
    if expected.kind != EntryKind::File || LocalStamp::capture(&current) != *expected {
        return Err(io::Error::other("destination changed during download"));
    }
    Ok(current)
}

fn preserve_permissions(file: &File, previous: &Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let stage = file.metadata()?;
        if stage.uid() != previous.uid()
            || stage.gid() != previous.gid()
            || previous.mode() & 0o7000 != 0
        {
            return Err(io::Error::other(
                "replacement cannot retain owner, group and permissions",
            ));
        }
        file.set_permissions(previous.permissions())
    }
    #[cfg(not(unix))]
    {
        let _ = (file, previous);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic replacement preserving permissions is unavailable",
        ))
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = &self.path
            && let Ok(metadata) = self.file.metadata()
            && owns_staged_path(path, &LocalStamp::capture(&metadata)).unwrap_or(false)
        {
            let _ = fs::remove_file(path);
        }
    }
}

pub(super) fn numbered(name: &[u8], number: usize) -> Vec<u8> {
    let lowercase = name.to_ascii_lowercase();
    let combined = [b".tar.gz".as_slice(), b".tar.bz2", b".tar.xz", b".tar.zst"]
        .into_iter()
        .find(|suffix| lowercase.ends_with(suffix) && suffix.len() < name.len());
    let at = combined
        .map(|suffix| name.len() - suffix.len())
        .or_else(|| {
            name.iter()
                .rposition(|&byte| byte == b'.')
                .filter(|&offset| offset != 0)
        })
        .unwrap_or(name.len());
    let mut result = name[..at].to_vec();
    result.extend_from_slice(format!(" ({number})").as_bytes());
    result.extend_from_slice(&name[at..]);
    result
}

#[cfg(test)]
mod tests;
