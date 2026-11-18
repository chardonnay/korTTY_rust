use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Storage::FileSystem::MAXIMUM_REPARSE_DATA_BUFFER_SIZE;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

use super::{invalid, is_reparse_point, ok};

/// Returns the exact metadata payload for a symbolic link or drive-path junction.
/// Volume mount points and all other reparse tags are outside reviewed removal.
pub fn reviewed_link_data(file: &File) -> io::Result<Option<Vec<u8>>> {
    if !is_reparse_point(&file.metadata()?) {
        return Ok(None);
    }
    let mut data = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
    let mut returned = 0;
    // SAFETY: this synchronous query uses a live handle and a writable buffer.
    ok(unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            null(),
            0,
            data.as_mut_ptr().cast(),
            MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
            &mut returned,
            null_mut(),
        )
    })?;
    if !(8..=MAXIMUM_REPARSE_DATA_BUFFER_SIZE).contains(&returned) {
        return Err(invalid("Invalid Windows reparse data size"));
    }
    data.truncate(returned as usize);
    let tag = u32::from_le_bytes(data[..4].try_into().unwrap());
    if !matches!(tag, IO_REPARSE_TAG_SYMLINK | IO_REPARSE_TAG_MOUNT_POINT) {
        return Ok(None);
    }
    let length = usize::from(u16::from_le_bytes([data[4], data[5]]));
    if length + 8 != data.len() {
        return Err(invalid("Invalid Windows reparse data length"));
    }
    if tag == IO_REPARSE_TAG_MOUNT_POINT && !drive_junction(&data[8..]) {
        return Ok(None);
    }
    Ok(Some(data))
}

fn drive_junction(payload: &[u8]) -> bool {
    // Offsets are relative to the UTF-16 path buffer following the four fields.
    let Some(header) = payload.get(..8) else {
        return false;
    };
    let offset = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let length = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let start = 8 + offset;
    if !offset.is_multiple_of(2) || !length.is_multiple_of(2) || length < 14 {
        return false;
    }
    let Some(path) = payload.get(start..start + length) else {
        return false;
    };
    let mut units = path
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_le_bytes);
    units.by_ref().take(4).eq(r"\??\".encode_utf16())
        && units.next().is_some_and(|letter| {
            u8::try_from(letter).is_ok_and(|letter| letter.is_ascii_alphabetic())
        })
        && units.take(2).eq(r":\".encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_junctions_allow_drive_paths_and_exclude_volume_mounts() {
        let payload = |target: &str| {
            let target = target
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            let mut payload = vec![0; 8];
            payload[2..4].copy_from_slice(&(target.len() as u16).to_le_bytes());
            payload.extend(target);
            payload
        };
        assert!(drive_junction(&payload(r"\??\C:\fixture")));
        assert!(!drive_junction(&payload(
            r"\??\Volume{12345678-1234-1234-1234-123456789abc}\"
        )));
        assert!(!drive_junction(&payload(r"\??\UNC\host\share")));
    }
}
