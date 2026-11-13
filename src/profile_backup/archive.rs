//! The portable kortty backup envelope has one authenticated WinZip AES-256 entry.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use kortty_vault::{Secret, derive_backup_password};
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, System, ZipArchive, ZipWriter};

use super::Snapshot;

pub(super) mod java;
#[cfg(test)]
mod tests;

const MAX_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 1024;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;

const PAYLOAD_MAGIC: &[u8; 24] = b"kortty-profile-snapshot\x01";
const PAYLOAD_HEADER_SIZE: usize = PAYLOAD_MAGIC.len() + 4;
const ENTRY_HEADER_SIZE: usize = 2 + 4;
const COMMENT_MAGIC: &[u8] = b"kortty-profile-backup\x01";
const SALT_SIZE: usize = 16;
const ZIP_COMMENT_SIZE: usize = COMMENT_MAGIC.len() + SALT_SIZE;
const ZIP_NAME: &str = "profile.snapshot";
// ZIP extra field 0x9901, seven-byte body, AE-2, WinZip AES, 256 bits, stored.
const AES_EXTRA: [u8; 11] = [1, 0x99, 7, 0, 2, 0, b'A', b'E', 3, 0, 0];
const AES_OVERHEAD: usize = 16 + 2 + 10;
const LOCAL_SIZE: usize = 30;
const DESCRIPTOR_SIZE: usize = 16;
const CENTRAL_SIZE: usize = 46;
const EOCD_SIZE: usize = 22;
const ZIP_LOCAL_PREFIX: usize = LOCAL_SIZE + ZIP_NAME.len() + AES_EXTRA.len();
const ZIP_CENTRAL_SIZE: usize = CENTRAL_SIZE + ZIP_NAME.len() + AES_EXTRA.len();
const ZIP_FIXED_SIZE: usize = ZIP_LOCAL_PREFIX
    + AES_OVERHEAD
    + DESCRIPTOR_SIZE
    + ZIP_CENTRAL_SIZE
    + EOCD_SIZE
    + ZIP_COMMENT_SIZE;

pub(super) fn write(destination: &Path, snapshot: &Snapshot, password: &Secret) -> io::Result<()> {
    crate::profile::create_new_atomic(destination, |output| write_to(output, snapshot, password))
}

pub(super) fn write_to(
    output: impl Write,
    snapshot: &Snapshot,
    password: &Secret,
) -> io::Result<()> {
    payload_size(snapshot)?;
    let mut salt = [0; SALT_SIZE];
    getrandom::fill(&mut salt).map_err(io::Error::other)?;
    let password = derive_backup_password(password, &salt).map_err(io::Error::other)?;
    let mut archive = ZipWriter::new_stream(output);
    let mut comment = COMMENT_MAGIC.to_vec();
    comment.extend_from_slice(&salt);
    archive.set_raw_comment(comment.into_boxed_slice())?;
    let options = SimpleFileOptions::default()
        .system(System::Unix)
        .unix_permissions(0o600)
        .compression_method(CompressionMethod::Stored)
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_bytes());
    // The stream writer uses AE-2, which keeps the plaintext CRC out of ZIP metadata.
    archive.start_file(ZIP_NAME, options)?;
    write_payload(&mut archive, snapshot)?;
    archive.finish()?;
    Ok(())
}

pub(super) fn read(source: &Path, password: &Secret) -> io::Result<Snapshot> {
    // Keep the structural check and cryptographic reader on the same immutable ciphertext.
    let archive_bytes = read_ciphertext(&crate::profile::open_preference(source)?)?;
    let (payload_size, salt) = validate_zip(&archive_bytes)?;
    let password = derive_backup_password(password, &salt).map_err(io::Error::other)?;
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))?;
    if archive.len() != 1 {
        return Err(invalid("invalid profile backup ZIP entry count"));
    }
    let mut member = archive.by_index_decrypt(0, password.as_bytes())?;
    if !member.encrypted()
        || member.name_raw() != ZIP_NAME.as_bytes()
        || member.size() != payload_size as u64
        || member.data_start() != Some(ZIP_LOCAL_PREFIX as u64)
    {
        return Err(invalid("invalid encrypted profile backup member"));
    }
    // Every successful decode reaches the end of the AES reader and verifies its HMAC.
    read_payload(&mut member, payload_size)
}

pub(super) fn verify_rotation_candidate(mut file: &File) -> io::Result<()> {
    let length = archive_length(file)?;
    inspect_zip(&mut file, length).map(|_| ())
}

fn read_ciphertext(mut file: &File) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; archive_length(file)?];
    file.read_exact(&mut bytes)?;
    require_eof(&mut file)?;
    Ok(bytes)
}

fn archive_length(file: &File) -> io::Result<usize> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || !(ZIP_FIXED_SIZE as u64..=(ZIP_FIXED_SIZE + MAX_PAYLOAD_BYTES) as u64)
            .contains(&metadata.len())
    {
        return Err(invalid("invalid profile backup file size or type"));
    }
    Ok(metadata.len() as usize)
}

pub(super) fn encode_snapshot(snapshot: &Snapshot) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(payload_size(snapshot)?));
    write_payload(&mut *bytes, snapshot)?;
    Ok(bytes)
}

pub(super) fn decode_snapshot(bytes: &[u8]) -> io::Result<Snapshot> {
    let mut input = bytes;
    read_payload(&mut input, bytes.len())
}

/// Reserves encoded space for validated members appended to a converted archive.
pub(super) fn remaining_capacity(
    snapshot: &Snapshot,
) -> io::Result<impl FnMut(&str, usize) -> bool> {
    let mut entries = MAX_ENTRIES - snapshot.len().min(MAX_ENTRIES);
    let mut remaining = MAX_PAYLOAD_BYTES - payload_size(snapshot)?;
    Ok(move |path: &str, bytes: usize| {
        let size = ENTRY_HEADER_SIZE + path.len() + bytes;
        if entries == 0 || size > remaining {
            return false;
        }
        entries -= 1;
        remaining -= size;
        true
    })
}

fn payload_size(snapshot: &Snapshot) -> io::Result<usize> {
    if snapshot.len() > MAX_ENTRIES {
        return Err(invalid("too many files in profile backup"));
    }
    let mut paths = Paths::default();
    let mut size = PAYLOAD_HEADER_SIZE;
    for (path, bytes) in snapshot {
        paths.insert(path)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(invalid("profile backup file exceeds the size limit"));
        }
        size += ENTRY_HEADER_SIZE + path.len() + bytes.len();
        if size > MAX_PAYLOAD_BYTES {
            return Err(invalid("profile backup exceeds the total size limit"));
        }
    }
    paths.require_separate_files()?;
    Ok(size)
}

fn write_payload(output: &mut impl Write, snapshot: &Snapshot) -> io::Result<()> {
    output.write_all(PAYLOAD_MAGIC)?;
    output.write_all(&(snapshot.len() as u32).to_le_bytes())?;
    for (path, contents) in snapshot {
        output.write_all(&(path.len() as u16).to_le_bytes())?;
        output.write_all(&(contents.len() as u32).to_le_bytes())?;
        output.write_all(path.as_bytes())?;
        for chunk in contents.chunks(64 * 1024) {
            output.write_all(chunk)?;
        }
    }
    Ok(())
}

pub(super) fn read_payload(input: &mut impl Read, size: usize) -> io::Result<Snapshot> {
    if !(PAYLOAD_HEADER_SIZE..=MAX_PAYLOAD_BYTES).contains(&size) {
        return Err(invalid("invalid profile backup payload size"));
    }
    let mut reader = PayloadReader {
        input,
        remaining: size,
    };
    if reader.array::<{ PAYLOAD_MAGIC.len() }>()? != *PAYLOAD_MAGIC {
        return Err(invalid("unsupported profile backup payload version"));
    }
    let count = u32::from_le_bytes(reader.array()?) as usize;
    if count > MAX_ENTRIES || count > reader.remaining / (ENTRY_HEADER_SIZE + 1) {
        return Err(invalid("invalid profile backup file count"));
    }

    let mut result = Snapshot::new();
    let mut paths = Paths::default();
    for _ in 0..count {
        let path_size = u16::from_le_bytes(reader.array()?) as usize;
        let file_size = u32::from_le_bytes(reader.array()?) as usize;
        if !(1..=MAX_PATH_BYTES).contains(&path_size)
            || file_size > MAX_FILE_BYTES
            || path_size + file_size > reader.remaining
        {
            return Err(invalid("invalid profile backup path or file length"));
        }
        let mut path = vec![0; path_size];
        reader.read_exact(&mut path)?;
        let path =
            String::from_utf8(path).map_err(|_| invalid("invalid profile backup path encoding"))?;
        paths.insert(&path)?;
        let mut contents = Zeroizing::new(vec![0; file_size]);
        reader.read_exact(&mut contents)?;
        result.insert(path, contents);
    }
    if reader.remaining != 0 {
        return Err(invalid("trailing data in profile backup payload"));
    }
    require_eof(reader.input)?;
    paths.require_separate_files()?;
    Ok(result)
}

struct PayloadReader<'a, R> {
    input: &'a mut R,
    remaining: usize,
}

impl<R: Read> PayloadReader<'_, R> {
    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut bytes = [0; N];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| invalid("truncated profile backup payload"))?;
        self.input.read_exact(bytes)
    }
}

fn require_eof(input: &mut impl Read) -> io::Result<()> {
    let mut extra = Zeroizing::new([0; 1]);
    loop {
        match input.read(&mut *extra) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
            Ok(0) => return Ok(()),
            Ok(_) => return Err(invalid("trailing data after profile backup")),
        }
    }
}

#[derive(Default)]
struct Paths(HashSet<String>);

impl Paths {
    fn insert(&mut self, path: &str) -> io::Result<()> {
        if path.len() > MAX_PATH_BYTES || path.split('/').any(invalid_component) {
            return Err(invalid("unsafe relative path in profile backup"));
        }
        let portable = path.nfd().flat_map(char::to_lowercase).collect();
        if !self.0.insert(portable) {
            return Err(invalid("duplicate or aliased path in profile backup"));
        }
        Ok(())
    }

    fn require_separate_files(&self) -> io::Result<()> {
        for path in &self.0 {
            for (offset, _) in path.match_indices('/') {
                if self.0.contains(&path[..offset]) {
                    return Err(invalid("profile backup path conflicts with a parent file"));
                }
            }
        }
        Ok(())
    }
}

pub(super) fn invalid_component(component: &str) -> bool {
    if component.is_empty()
        || component.ends_with([' ', '.'])
        || component.len() > 255
        || component.chars().any(|character| {
            character.is_control()
                || matches!(character, '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
    {
        return true;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn validate_zip(bytes: &[u8]) -> io::Result<(usize, [u8; SALT_SIZE])> {
    inspect_zip(&mut Cursor::new(bytes), bytes.len())
}

fn inspect_zip(
    input: &mut (impl Read + Seek),
    length: usize,
) -> io::Result<(usize, [u8; SALT_SIZE])> {
    let malformed = || invalid("invalid or unencrypted native profile backup ZIP");
    let size = length.checked_sub(ZIP_FIXED_SIZE).ok_or_else(malformed)?;
    if !(PAYLOAD_HEADER_SIZE..=MAX_PAYLOAD_BYTES).contains(&size) {
        return Err(malformed());
    }
    let compressed_size = size + AES_OVERHEAD;
    let descriptor_offset = ZIP_LOCAL_PREFIX + compressed_size;
    let central_offset = descriptor_offset + DESCRIPTOR_SIZE;
    let mut prefix = [0; ZIP_LOCAL_PREFIX];
    input.seek(SeekFrom::Start(0))?;
    input.read_exact(&mut prefix)?;
    let mut trailer = [0; DESCRIPTOR_SIZE + ZIP_CENTRAL_SIZE + EOCD_SIZE + ZIP_COMMENT_SIZE];
    input.seek(SeekFrom::Start(descriptor_offset as u64))?;
    input.read_exact(&mut trailer)?;
    let local = &prefix[..LOCAL_SIZE];
    let (descriptor, rest) = trailer.split_at(DESCRIPTOR_SIZE);
    let (directory, rest) = rest.split_at(ZIP_CENTRAL_SIZE);
    let (central, variable) = directory.split_at(CENTRAL_SIZE);
    let (eocd, comment) = rest.split_at(EOCD_SIZE);
    if !local.starts_with(b"PK\x03\x04")
        || u16_at(local, 6) != 9 // Encryption and signed data descriptor.
        || u16_at(local, 8) != 99
        || local[14..26] != [0; 12]
        || !matches_zip_member(local, 26, &prefix[LOCAL_SIZE..])
        || !descriptor.starts_with(b"PK\x07\x08")
        || u32_at(descriptor, 4) != 0
        || u32_at(descriptor, 8) != compressed_size
        || u32_at(descriptor, 12) != size
        || !central.starts_with(b"PK\x01\x02")
        || central[6..16] != local[4..14]
        || u32_at(central, 16) != 0
        || u32_at(central, 20) != compressed_size
        || u32_at(central, 24) != size
        || !matches_zip_member(central, 28, variable)
        || central[32..38] != [0; 6] // No file comment, split disk, or text metadata.
        || u16_at(central, 4) >> 8 != 3 // UNIX creator and ordinary owner-only mode.
        || u32_at(central, 38) != 0o100600 << 16
        || u32_at(central, 42) != 0
        || !eocd.starts_with(b"PK\x05\x06")
        || eocd[4..8] != [0; 4]
        || u16_at(eocd, 8) != 1
        || u16_at(eocd, 10) != 1
        || u32_at(eocd, 12) != ZIP_CENTRAL_SIZE
        || u32_at(eocd, 16) != central_offset
        || usize::from(u16_at(eocd, 20)) != ZIP_COMMENT_SIZE
        || !comment.starts_with(COMMENT_MAGIC)
    {
        return Err(malformed());
    }
    let salt = comment[COMMENT_MAGIC.len()..]
        .try_into()
        .map_err(|_| malformed())?;
    Ok((size, salt))
}

fn matches_zip_member(header: &[u8], size_offset: usize, variable: &[u8]) -> bool {
    usize::from(u16_at(header, size_offset)) == ZIP_NAME.len()
        && usize::from(u16_at(header, size_offset + 2)) == AES_EXTRA.len()
        && variable.starts_with(ZIP_NAME.as_bytes())
        && variable[ZIP_NAME.len()..] == AES_EXTRA
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]) as usize
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
