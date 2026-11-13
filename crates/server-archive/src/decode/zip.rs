use std::io::{self, Cursor};

use zip::ZipArchive;
use zip::read::{ArchiveOffset, Config};

use crate::manifest::{MAX_MEMBERS, MemberKind};

use super::{Output, encrypted, invalid, link_bytes, unsupported_entry};

pub(super) fn visit(bytes: &[u8], output: &mut Output<'_>) -> io::Result<()> {
    let count = preflight_directory(bytes)?;
    let mut archive = ZipArchive::with_config(
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
        Cursor::new(bytes),
    )
    .map_err(zip_error)?;
    if archive.len() != count {
        return Err(invalid(
            "ZIP central directory has duplicate or inconsistent entries",
        ));
    }
    for index in 0..count {
        if archive.by_index_data(index).map_err(zip_error)?.encrypted() {
            return Err(encrypted());
        }
    }
    archive.validate_header_consistency().map_err(zip_error)?;
    for index in 0..count {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().map_err(zip_error)?.into_owned();
        let size = entry.size();
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        match (mode, entry.is_dir()) {
            (0o120000, false) => {
                let target = link_bytes(&mut entry, size)?;
                output.link(&name, MemberKind::Symlink { target })?;
            }
            (0 | 0o040000, true) => {
                output.content(&name, MemberKind::Directory, size, &mut entry)?
            }
            (0 | 0o100000, false) => output.content(&name, MemberKind::File, size, &mut entry)?,
            _ => return Err(unsupported_entry()),
        }
    }
    Ok(())
}

// Bound the number of records before ZipArchive allocates the directory map.
// Require one contiguous central directory with a footer at the end of the file.
fn preflight_directory(bytes: &[u8]) -> io::Result<usize> {
    let end = bytes.len().checked_sub(22).ok_or_else(bad_directory)?;
    let start = end.saturating_sub(u16::MAX as usize);
    let end = (start..=end)
        .rev()
        .find(|&index| {
            bytes[index..].starts_with(b"PK\x05\x06")
                && index + 22 + usize::from(u16_at(bytes, index + 20)) == bytes.len()
        })
        .ok_or_else(bad_directory)?;
    let footer = &bytes[end..end + 22];
    if footer[4..8] != [0; 4] || u16_at(footer, 8) != u16_at(footer, 10) {
        return Err(invalid("Multi-disk ZIP archives are unsupported"));
    }
    let zip32 = Directory {
        count: u16_at(footer, 10).into(),
        size: u32_at(footer, 12).into(),
        offset: u32_at(footer, 16).into(),
    };
    let locator = end
        .checked_sub(20)
        .filter(|&offset| bytes[offset..end].starts_with(b"PK\x06\x07"));
    let (dir, footer_start) = match locator {
        Some(locator) => zip64_directory(bytes, locator, zip32)?,
        None => (zip32, end),
    };
    if dir.count > MAX_MEMBERS as u64 {
        return Err(invalid("The ZIP has too many entries"));
    }
    if dir.offset.checked_add(dir.size) != Some(footer_start as u64) {
        return Err(bad_directory());
    }
    let mut directory = &bytes[dir.offset as usize..footer_start];
    for _ in 0..dir.count {
        let record = directory.get(..46).ok_or_else(bad_directory)?;
        if !record.starts_with(b"PK\x01\x02") || u16_at(record, 34) != 0 {
            return Err(bad_directory());
        }
        let length = 46
            + [28, 30, 32]
                .into_iter()
                .map(|index| usize::from(u16_at(record, index)))
                .sum::<usize>();
        directory = directory.get(length..).ok_or_else(bad_directory)?;
    }
    if !directory.is_empty() {
        return Err(bad_directory());
    }
    Ok(dir.count as usize)
}

#[derive(Clone, Copy)]
struct Directory {
    count: u64,
    size: u64,
    offset: u64,
}

fn zip64_directory(
    bytes: &[u8],
    locator: usize,
    zip32: Directory,
) -> io::Result<(Directory, usize)> {
    let locator_record = &bytes[locator..locator + 20];
    if u32_at(locator_record, 4) != 0 || u32_at(locator_record, 16) != 1 {
        return Err(invalid("Multi-disk ZIP64 archives are unsupported"));
    }
    let start = usize::try_from(u64_at(locator_record, 8)).map_err(|_| bad_directory())?;
    let record = bytes
        .get(start..locator)
        .filter(|record| record.len() >= 56 && record.len() <= 64 * 1024)
        .ok_or_else(bad_directory)?;
    if !record.starts_with(b"PK\x06\x06")
        || u64_at(record, 4) != record.len() as u64 - 12
        || record[16..24] != [0; 8]
        || u64_at(record, 24) != u64_at(record, 32)
    {
        return Err(bad_directory());
    }
    let dir = Directory {
        count: u64_at(record, 32),
        size: u64_at(record, 40),
        offset: u64_at(record, 48),
    };
    if zip32.count != dir.count && zip32.count != u64::from(u16::MAX)
        || zip32.size != dir.size && zip32.size != u64::from(u32::MAX)
        || zip32.offset != dir.offset && zip32.offset != u64::from(u32::MAX)
    {
        return Err(bad_directory());
    }
    Ok((dir, start))
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn bad_directory() -> io::Error {
    invalid("Invalid or unsupported ZIP central directory")
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
