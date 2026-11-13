//! Read leases for external files resolved one component at a time.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Component, Components, Path, PathBuf, Prefix, PrefixComponent};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_REMOTE_PROTOCOL_INFO, FILE_TRAVERSE, FileRemoteProtocolInfo, GetFileInformationByHandleEx,
    GetVolumeNameForVolumeMountPointW, QueryDosDeviceW, SECURITY_IDENTIFICATION, SYNCHRONIZE,
};

use super::{
    DIRECTORY_READ, Directory, FILE_DIRECTORY_FILE, FILE_GENERIC_READ, FILE_NON_DIRECTORY_FILE,
    FILE_OPEN, FILE_SHARE_READ, FILE_SHARE_WRITE, invalid, is_reparse_point,
};

/// A regular file and its ancestor directories held against replacement.
pub struct ExternalLease {
    source: AnchoredRead,
    path: PathBuf,
}

impl ExternalLease {
    /// Holds a local-drive path without following reparse points or sharing write/delete access
    /// to the final file. Directory handles deny renames while a child process opens the path.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_checked(path, |_, _| Ok(()))
    }

    /// Reads a terminal-selected path on a native drive without contacting a network provider.
    pub fn open_terminal_path(path: &Path) -> io::Result<Self> {
        let (prefix, components) = split_root(path)?;
        let Prefix::Disk(letter) = prefix.kind() else {
            return Err(invalid("Terminal files must use a local drive path"));
        };
        let root = Directory::open_path(&terminal_volume(letter)?)?;
        Self::open_from(root, components, |_, _| Ok(()))
    }

    /// Holds a local path whose source and ancestors permit administrative changes only.
    pub fn open_administrator_controlled(path: &Path) -> io::Result<Self> {
        Self::open_checked(path, super::security::require_administrator_controlled)
    }

    fn open_checked(
        path: &Path,
        check: impl Fn(&File, bool) -> io::Result<()>,
    ) -> io::Result<Self> {
        let (prefix, components) = split_root(path)?;
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
            return Err(invalid("External files must use a local drive path"));
        }
        let root = Directory::open_path(&volume_root(prefix))?;
        Self::open_from(root, components, check)
    }

    fn open_from(
        root: Directory,
        components: Components<'_>,
        check: impl Fn(&File, bool) -> io::Result<()>,
    ) -> io::Result<Self> {
        let source = AnchoredRead::open(root, components, DIRECTORY_READ, check)?;
        let path = crate::windows::local_reference_path(&source.file)?
            .ok_or_else(|| invalid("External files must be on a supported local Windows volume"))?;
        Ok(Self { source, path })
    }

    /// The already checked read handle for metadata validation.
    pub fn file(&self) -> &File {
        &self.source.file
    }

    /// The volume-GUID path of the opened file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn terminal_volume(letter: u8) -> io::Result<PathBuf> {
    let drive = [u16::from(letter), u16::from(b':'), 0];
    let mut target = [0; 1024];
    // SAFETY: the drive and output buffer remain live for the duration of the call.
    let used = unsafe { QueryDosDeviceW(drive.as_ptr(), target.as_mut_ptr(), target.len() as u32) }
        as usize;
    let target = target
        .get(..used)
        .unwrap_or_default()
        .split(|&unit| unit == 0)
        .next()
        .and_then(|name| String::from_utf16(name).ok());
    if !target.as_deref().is_some_and(|target| {
        [r"\Device\HarddiskVolume", r"\Device\CdRom"]
            .into_iter()
            .filter_map(|prefix| target.strip_prefix(prefix))
            .any(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        return Err(invalid("Terminal files must be on a native local volume"));
    }

    let root = [drive[0], drive[1], u16::from(b'\\'), 0];
    let mut volume = [0; 64];
    // SAFETY: root is NUL-terminated and the output buffer remains live during the call.
    if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = volume
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(volume.len());
    let volume = String::from_utf16(&volume[..end])
        .map_err(|_| invalid("Invalid local Windows volume name"))?;
    let path = PathBuf::from(volume);
    let mut components = path.components();
    if !matches!(
        (components.next(), components.next(), components.next()),
        (Some(Component::Prefix(prefix)), Some(Component::RootDir), None)
            if matches!(prefix.kind(), Prefix::Verbatim(name) if name.to_string_lossy().starts_with("Volume{"))
    ) {
        return Err(invalid("Invalid local Windows volume name"));
    }
    Ok(path)
}

/// An SMB file held for read-only acquisition with the directory chain kept open.
pub struct SmbReadLease(AnchoredRead);

impl SmbReadLease {
    /// Anchors a validated verbatim UNC path to its SMB share and rejects reparses.
    /// The caller's guard runs between component opens.
    pub fn open(path: &Path, check: impl Fn() -> io::Result<()>) -> io::Result<Self> {
        const DIRECTORY_ACCESS: u32 = FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE;
        let (prefix, components) = split_root(path)?;
        if !matches!(prefix.kind(), Prefix::VerbatimUNC(..)) {
            return Err(invalid("Expected a verbatim UNC path"));
        }
        check()?;
        let root = Directory::from_file(
            OpenOptions::new()
                .access_mode(DIRECTORY_ACCESS)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(volume_root(prefix))?,
        )?;
        AnchoredRead::open(root, components, DIRECTORY_ACCESS, |file, _| {
            check()?;
            require_smb(file)?;
            check()
        })
        .map(Self)
    }

    /// The opened regular file with write and delete sharing denied.
    pub fn file(&self) -> &File {
        &self.0.file
    }
}

struct AnchoredRead {
    file: File,
    _ancestors: Vec<Directory>,
}

impl AnchoredRead {
    fn open(
        root: Directory,
        components: Components<'_>,
        directory_access: u32,
        check: impl Fn(&File, bool) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut components = components.peekable();
        let mut ancestors = vec![root];
        check(ancestors[0].file(), true)?;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid("Expected an ordinary external file path"));
            };
            let parent = ancestors.last().expect("the volume is already open");
            if components.peek().is_none() {
                let file = parent.open_with_share(
                    name,
                    FILE_GENERIC_READ,
                    FILE_OPEN,
                    FILE_NON_DIRECTORY_FILE,
                    None,
                    FILE_SHARE_READ,
                )?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || is_reparse_point(&metadata) {
                    return Err(invalid("External reference must be a regular file"));
                }
                check(&file, false)?;
                return Ok(Self {
                    file,
                    _ancestors: ancestors,
                });
            }
            if ancestors.len() == 128 {
                return Err(invalid(
                    "External file path exceeds the directory depth limit",
                ));
            }
            let child = Directory::from_file(parent.open_with_share(
                name,
                directory_access,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
                None,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )?)?;
            check(child.file(), true)?;
            ancestors.push(child);
        }
        Err(invalid("Expected a regular external file"))
    }
}

fn split_root(path: &Path) -> io::Result<(PrefixComponent<'_>, Components<'_>)> {
    let mut components = path.components();
    let (Some(Component::Prefix(prefix)), Some(Component::RootDir)) =
        (components.next(), components.next())
    else {
        return Err(invalid("Expected an absolute external file path"));
    };
    Ok((prefix, components))
}

fn volume_root(prefix: PrefixComponent<'_>) -> PathBuf {
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push(r"\");
    root
}

fn require_smb(file: &File) -> io::Result<()> {
    const WNNC_NET_SMB: u32 = 0x0002_0000;
    let mut info = FILE_REMOTE_PROTOCOL_INFO::default();
    // SAFETY: the file and writable output record remain live during the query.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileRemoteProtocolInfo,
            (&mut info as *mut FILE_REMOTE_PROTOCOL_INFO).cast(),
            size_of::<FILE_REMOTE_PROTOCOL_INFO>() as u32,
        )
    };
    if result == 0 || info.Protocol != WNNC_NET_SMB {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "A UNC model source must use the Windows SMB redirector",
        ));
    }
    Ok(())
}
