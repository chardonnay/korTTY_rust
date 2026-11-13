use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_OPENED,
    GetDiskFreeSpaceExW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, MoveFileExW,
    SECURITY_IDENTIFICATION, VOLUME_NAME_GUID,
};

/// Returns the space available to the caller on the directory's volume.
pub fn available_space(directory: &Path) -> io::Result<u64> {
    let directory = wide_path(directory.as_os_str())?;
    let mut available = 0;
    // SAFETY: The NUL-terminated path and available-space output live for the call.
    if unsafe { GetDiskFreeSpaceExW(directory.as_ptr(), &mut available, null_mut(), null_mut()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

/// Renames a file or directory on the same volume, failing if the target already exists.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide_path(from.as_os_str())?;
    let to = wide_path(to.as_os_str())?;
    // Both paths are live NUL-terminated UTF-16 buffers. Zero flags request a
    // same-volume rename without replacing or copying the destination.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Opens a regular file without resolving a reparse point in the final component.
pub fn open_regular_without_reparse(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transfer source must be a regular file without a reparse point",
        ));
    }
    Ok(file)
}

/// Proves the opened file is on a native Windows volume rather than an SMB or unknown provider.
pub fn is_local_reference_file(file: &File) -> io::Result<bool> {
    local_reference_path(file).map(|path| path.is_some())
}

pub(crate) fn local_reference_path(file: &File) -> io::Result<Option<PathBuf>> {
    let mut path = vec![0_u16; 32_768];
    // SAFETY: The file handle and UTF-16 output buffer remain live for the call.
    let used = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            FILE_NAME_OPENED | VOLUME_NAME_GUID,
        )
    } as usize;
    if used == 0 {
        return Err(io::Error::last_os_error());
    }
    if used >= path.len() {
        return Ok(None);
    }
    let Ok(path) = String::from_utf16(&path[..used]) else {
        return Ok(None);
    };
    let volume = path
        .strip_prefix(r"\\?\Volume{")
        .and_then(|path| path.split_once("}\\"));
    if !volume.is_some_and(|(id, _)| {
        id.len() == 36
            && id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }) {
        return Ok(None);
    }

    let mut format = [0_u16; 32];
    // SAFETY: Only the file-system name is requested, into a writable UTF-16 buffer.
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            format.as_mut_ptr(),
            format.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let Some(end) = format.iter().position(|&unit| unit == 0) else {
        return Ok(None);
    };
    let local = String::from_utf16(&format[..end])
        .is_ok_and(|format| matches!(format.as_str(), "NTFS" | "ReFS" | "FAT" | "FAT32" | "exFAT"));
    Ok(local.then(|| PathBuf::from(path)))
}

fn wide_path(path: &OsStr) -> io::Result<Vec<u16>> {
    let mut path: Vec<_> = path.encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file path contains a NUL character",
        ));
    }
    path.push(0);
    Ok(path)
}
