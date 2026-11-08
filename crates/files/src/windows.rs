use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, MoveFileExW,
    SECURITY_IDENTIFICATION,
};

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
