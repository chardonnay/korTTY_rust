//! Bounded ZIP, TAR, and 7z encoding from reviewed archive members.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Seek, SeekFrom, Write};

use bzip2::{Compression, write::BzEncoder};
use kortty_vault::valid_archive_password;
use zeroize::Zeroizing;
use zip::{AesMode, CompressionMethod, System, ZipWriter, write::SimpleFileOptions};

use crate::scheduler_window::ArchiveFormat;

mod sevenz;

pub(crate) const MAX_ENTRIES: usize = 4096;
pub(crate) const MAX_PATH_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Eq, PartialEq)]
pub(crate) struct Member {
    pub name: String,
    pub directory: bool,
    pub size: u64,
    pub modified: u64,
}

/// Encodes the given member order. The callback supplies exactly one payload for each file.
pub(crate) fn encode(
    members: &[Member],
    format: ArchiveFormat,
    level: u8,
    password: Option<&str>,
    stopped: &dyn Fn() -> bool,
    mut write_payload: impl FnMut(usize, &mut dyn Write) -> io::Result<()>,
) -> io::Result<Zeroizing<Vec<u8>>> {
    if stopped() {
        return Err(cancelled());
    }
    validate(members, format, level, password)?;
    let cancellation_seen = Cell::new(false);
    let stopped = || {
        if !cancellation_seen.get() && stopped() {
            cancellation_seen.set(true);
        }
        cancellation_seen.get()
    };
    let limit_hit = Cell::new(false);
    let output = BoundedArchive {
        bytes: Zeroizing::new(Vec::new()),
        position: 0,
        limit_hit: &limit_hit,
        stopped: &stopped,
    };
    let result = match format {
        ArchiveFormat::Zip | ArchiveFormat::ZipPassword => zip(
            members,
            output,
            level,
            password,
            &stopped,
            &mut write_payload,
        ),
        ArchiveFormat::Tar => tar(members, output, &stopped, &mut write_payload),
        ArchiveFormat::TarBz2 => {
            let compression = Compression::try_new(u32::from(level))
                .ok_or_else(|| invalid("Choose bzip2 compression level 1–9"))?;
            tar(
                members,
                BzEncoder::new(output, compression),
                &stopped,
                &mut write_payload,
            )
            .and_then(BzEncoder::finish)
        }
        ArchiveFormat::SevenZip | ArchiveFormat::SevenZipPassword => sevenz::encode(
            members,
            output,
            level,
            password,
            &stopped,
            &mut write_payload,
        ),
    };
    if stopped() {
        Err(cancelled())
    } else if limit_hit.get() {
        Err(io::Error::other(OutputLimit))
    } else {
        result.map(|output| output.bytes)
    }
}

fn validate(
    members: &[Member],
    format: ArchiveFormat,
    level: u8,
    password: Option<&str>,
) -> io::Result<()> {
    if members.is_empty() || members.len() > MAX_ENTRIES {
        return Err(invalid(
            "An archive must contain between 1 and 4096 entries",
        ));
    }
    if !format.valid_level(level) {
        return Err(invalid(format.level_hint()));
    }
    match format {
        ArchiveFormat::ZipPassword
            if password.is_none_or(str::is_empty)
                || members.iter().all(|member| member.directory) =>
        {
            return Err(invalid(
                "An encrypted ZIP requires a password and a regular file",
            ));
        }
        ArchiveFormat::SevenZipPassword if !password.is_some_and(valid_archive_password) => {
            return Err(invalid(
                "Enter a nonblank archive password of at most 1,024 bytes",
            ));
        }
        _ if password.is_some() && !format.is_password_protected() => {
            return Err(invalid(
                "Choose a password-protected archive format to use a password",
            ));
        }
        _ => {}
    }
    let mut names = BTreeSet::new();
    let mut path_bytes = 0;
    let mut input_bytes = 0;
    for member in members {
        if member.name.len() > MAX_PATH_BYTES - path_bytes {
            return Err(invalid("Archive member names exceed their path budget"));
        }
        path_bytes += member.name.len();
        if member.name.chars().any(char::is_control)
            || member.name.split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | "..")
                    || part.contains(['\\', ':'])
                    || part.ends_with([' ', '.'])
            })
            || !names.insert(member.name.as_str())
        {
            return Err(invalid(
                "Archive member names must be distinct, safe relative paths",
            ));
        }
        if member.directory && member.size != 0 {
            return Err(invalid(
                "Archive directory members must not contain payload bytes",
            ));
        }
        if member.size > MAX_INPUT_BYTES - input_bytes {
            return Err(invalid(
                "An archive may read at most 256 MiB of source files",
            ));
        }
        input_bytes += member.size;
    }
    if format.stores_uncompressed(level) && input_bytes > MAX_ARCHIVE_BYTES as u64 {
        return Err(io::Error::other(OutputLimit));
    }
    Ok(())
}

fn zip<W: Write>(
    members: &[Member],
    output: W,
    level: u8,
    password: Option<&str>,
    stopped: &dyn Fn() -> bool,
    write_payload: &mut impl FnMut(usize, &mut dyn Write) -> io::Result<()>,
) -> io::Result<W> {
    let mut writer = ZipWriter::new_stream(output);
    let options = SimpleFileOptions::default().system(System::Unix);
    let mut files = if level == 0 {
        options.compression_method(CompressionMethod::Stored)
    } else {
        options
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(i64::from(level)))
    }
    .unix_permissions(0o600);
    if let Some(password) = password {
        files = files.with_aes_encryption(AesMode::Aes256, password);
    }
    for (index, member) in members.iter().enumerate() {
        check_transfer(stopped)?;
        if member.directory {
            writer.add_directory(&member.name, options.unix_permissions(0o700))?;
        } else {
            writer.start_file(&member.name, files)?;
            write_member(index, member, &mut writer, stopped, write_payload)?;
        }
    }
    Ok(writer.finish()?.into_inner())
}

fn tar<W: Write>(
    members: &[Member],
    output: W,
    stopped: &dyn Fn() -> bool,
    write_payload: &mut impl FnMut(usize, &mut dyn Write) -> io::Result<()>,
) -> io::Result<W> {
    const BLOCK: usize = 512;
    let mut writer = tar::Builder::new(output);
    for (index, member) in members.iter().enumerate() {
        check_transfer(stopped)?;
        let mut header = tar::Header::new_ustar();
        if header.set_path(&member.name).is_err() {
            writer.append_pax_extensions([("path", member.name.as_bytes())])?;
            header = tar::Header::new_ustar();
            header.set_path(format!("PaxEntry-{index:04}"))?;
        }
        header.set_entry_type(if member.directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_mode(if member.directory { 0o700 } else { 0o600 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(member.modified);
        header.set_size(member.size);
        header.set_cksum();
        // The callback pushes checked bytes. Pad each payload to a POSIX TAR block boundary.
        let output = writer.get_mut();
        output.write_all(header.as_bytes())?;
        if !member.directory {
            write_member(index, member, output, stopped, write_payload)?;
            let padding = (BLOCK - (member.size % BLOCK as u64) as usize) % BLOCK;
            output.write_all(&[0; BLOCK][..padding])?;
        }
    }
    writer.into_inner()
}

fn write_member(
    index: usize,
    member: &Member,
    output: &mut dyn Write,
    stopped: &dyn Fn() -> bool,
    write_payload: &mut impl FnMut(usize, &mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut sink = CheckedEntry {
        output,
        remaining: member.size,
        stopped,
    };
    write_payload(index, &mut sink)?;
    if sink.remaining != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Archive source ended before its reported size",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Archive operation cancelled.")
}

fn check_transfer(stopped: &dyn Fn() -> bool) -> io::Result<()> {
    if stopped() {
        // Write::write_all retries Interrupted, so cancellation inside the codec is terminal.
        Err(io::Error::other("Archive transfer cancelled"))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct OutputLimit;

impl fmt::Display for OutputLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("An archive may be at most 64 MiB")
    }
}

impl std::error::Error for OutputLimit {}

pub(crate) fn is_output_limit(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<OutputLimit>())
}

struct BoundedArchive<'a> {
    bytes: Zeroizing<Vec<u8>>,
    position: usize,
    limit_hit: &'a Cell<bool>,
    stopped: &'a dyn Fn() -> bool,
}

impl Write for BoundedArchive<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        check_transfer(self.stopped)?;
        if input.is_empty() {
            return Ok(0);
        }
        let required = self
            .position
            .checked_add(input.len())
            .filter(|size| *size <= MAX_ARCHIVE_BYTES);
        let Some(required) = required else {
            self.limit_hit.set(true);
            return Err(io::Error::other(OutputLimit));
        };
        if required > self.bytes.capacity() {
            let capacity = (self.bytes.capacity() * 2)
                .max(required)
                .min(MAX_ARCHIVE_BYTES);
            let additional = capacity - self.bytes.len();
            self.bytes
                .try_reserve_exact(additional)
                .map_err(io::Error::other)?;
        }
        if required > self.bytes.len() {
            self.bytes.resize(required, 0);
        }
        self.bytes[self.position..required].copy_from_slice(input);
        self.position = required;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        check_transfer(self.stopped)
    }
}

impl Seek for BoundedArchive<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        check_transfer(self.stopped)?;
        let position = match position {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => (self.position as u64).checked_add_signed(offset),
            SeekFrom::End(offset) => (self.bytes.len() as u64).checked_add_signed(offset),
        }
        .ok_or_else(|| invalid("Invalid archive output position"))?;
        if position > MAX_ARCHIVE_BYTES as u64 {
            self.limit_hit.set(true);
            return Err(io::Error::other(OutputLimit));
        }
        self.position = position as usize;
        Ok(position)
    }
}

struct CheckedEntry<'a> {
    output: &'a mut dyn Write,
    remaining: u64,
    stopped: &'a dyn Fn() -> bool,
}

impl Write for CheckedEntry<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        check_transfer(self.stopped)?;
        if input.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive source exceeded its reported size",
            ));
        }
        let written = self.output.write(input)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        check_transfer(self.stopped)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::*;

    fn file(name: &str, size: u64) -> Member {
        Member {
            name: name.to_owned(),
            directory: false,
            size,
            modified: 123,
        }
    }

    #[test]
    fn archive_codec_encrypts_file_payloads_with_aes_and_keeps_directories() {
        let members = [
            Member {
                directory: true,
                ..file("folder", 0)
            },
            file("folder/source.txt", 7),
        ];
        let password = "fixture password";
        let bytes = encode(
            &members,
            ArchiveFormat::ZipPassword,
            6,
            Some(password),
            &|| false,
            |index, output| {
                assert_eq!(index, 1);
                output.write_all(b"payload")
            },
        )
        .unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).unwrap();
        assert!(archive.by_name("folder/").unwrap().is_dir());
        assert!(
            archive
                .by_name_decrypt("folder/source.txt", b"wrong")
                .is_err()
        );
        let mut payload = Vec::new();
        archive
            .by_name_decrypt("folder/source.txt", password.as_bytes())
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn archive_codec_checks_paths_sizes_and_output_limit() {
        for name in [
            "/absolute",
            "../outside",
            "dir/../../outside",
            "C:/drive",
            "dir\\outside",
            "directory/",
        ] {
            let error = encode(
                &[file(name, 1)],
                ArchiveFormat::Tar,
                0,
                None,
                &|| false,
                |_, _| unreachable!(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        for format in [ArchiveFormat::Zip, ArchiveFormat::SevenZip] {
            for (payload, kind) in [
                (b"ab".as_slice(), io::ErrorKind::UnexpectedEof),
                (b"abcd".as_slice(), io::ErrorKind::InvalidData),
            ] {
                let error = encode(
                    &[file("data", 3)],
                    format,
                    6,
                    None,
                    &|| false,
                    |_, output| output.write_all(payload),
                )
                .unwrap_err();
                assert_eq!(error.kind(), kind);
            }
        }
        for format in [ArchiveFormat::Tar, ArchiveFormat::SevenZip] {
            let limit = encode(
                &[file("large", MAX_ARCHIVE_BYTES as u64 + 1)],
                format,
                0,
                None,
                &|| false,
                |_, _| unreachable!(),
            )
            .unwrap_err();
            assert!(is_output_limit(&limit));
        }
    }

    #[test]
    fn archive_codec_reports_cancellation_and_keeps_payload_errors() {
        let members = [file("data", 1)];
        for format in [ArchiveFormat::TarBz2, ArchiveFormat::SevenZip] {
            let stopped = Cell::new(false);
            let error = encode(&members, format, 4, None, &|| stopped.get(), |_, output| {
                stopped.set(true);
                output.write_all(b"x")
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        }

        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarBz2,
            ArchiveFormat::SevenZip,
        ] {
            let error = encode(&members, format, 4, None, &|| false, |_, _| {
                Err(io::Error::other(fmt::Error))
            })
            .unwrap_err();
            assert!(
                error
                    .get_ref()
                    .is_some_and(|cause| cause.is::<fmt::Error>())
            );
        }
    }

    #[test]
    fn archive_codec_bounds_seek_and_keeps_payload_when_backfilling_header() {
        let limit_hit = Cell::new(false);
        let mut output = BoundedArchive {
            bytes: Zeroizing::new(Vec::new()),
            position: 0,
            limit_hit: &limit_hit,
            stopped: &|| false,
        };
        output.seek(SeekFrom::Start(4)).unwrap();
        output.write_all(b"payload").unwrap();
        output.rewind().unwrap();
        output.write_all(b"head").unwrap();
        assert_eq!(output.bytes.as_slice(), b"headpayload");
        assert_eq!(output.seek(SeekFrom::End(0)).unwrap(), 11);
        assert!(output.seek(SeekFrom::Current(-12)).is_err());
        assert!(!limit_hit.get());
        assert!(is_output_limit(
            &output
                .seek(SeekFrom::Start(MAX_ARCHIVE_BYTES as u64 + 1))
                .unwrap_err(),
        ));
        output
            .seek(SeekFrom::Start(MAX_ARCHIVE_BYTES as u64))
            .unwrap();
        assert!(is_output_limit(&output.write_all(b"overflow").unwrap_err()));
        assert!(limit_hit.get());
        assert_eq!(output.bytes.as_slice(), b"headpayload");
    }
}
