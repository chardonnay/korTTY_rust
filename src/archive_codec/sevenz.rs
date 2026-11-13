use std::io::{self, Read, Seek, Write};
use std::time::{Duration, UNIX_EPOCH};

use sevenz_rust2::{
    ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, NtTime,
    encoder_options::{ArchiveAes, Lzma2Options},
};
use zeroize::Zeroizing;

use super::{Member, check_transfer, invalid, write_member};

const MIB: u32 = 1024 * 1024;

pub(super) fn encode<W: Write + Seek>(
    members: &[Member],
    output: W,
    level: u8,
    password: Option<&str>,
    stopped: &dyn Fn() -> bool,
    write_payload: &mut impl FnMut(usize, &mut dyn Write) -> io::Result<()>,
) -> io::Result<W> {
    let encryption = password.map(ArchiveAes::new).transpose()?;
    check_transfer(stopped)?;
    let mut writer = ArchiveWriter::new(output).map_err(writer_error)?;
    if let Some(encryption) = &encryption {
        writer.set_header_encryption(encryption.stream()?);
    } else {
        writer.set_encrypt_header(false);
    }
    for (index, member) in members.iter().enumerate() {
        check_transfer(stopped)?;
        let entry = archive_entry(member)?;
        let mut spool = Zeroizing::new(Vec::new());
        if !member.directory {
            let size = usize::try_from(member.size)
                .map_err(|_| invalid("The archive member exceeds the input size limit"))?;
            spool.try_reserve_exact(size).map_err(io::Error::other)?;
            // Keep the checked source callback outside the library so its errors retain their type.
            write_member(index, member, &mut *spool, stopped, write_payload)?;
        }
        check_transfer(stopped)?;
        let source = if spool.is_empty() {
            None
        } else {
            let compression = content_method(level, member.size);
            let methods = match &encryption {
                Some(encryption) => vec![encryption.stream()?.into(), compression],
                None => vec![compression],
            };
            writer.set_content_methods(methods);
            Some(SpoolReader {
                bytes: spool.as_slice(),
                stopped,
            })
        };
        writer
            .push_archive_entry(entry, source)
            .map_err(writer_error)?;
    }
    check_transfer(stopped)?;
    writer.finish()
}

fn archive_entry(member: &Member) -> io::Result<ArchiveEntry> {
    const UNIX_EXTENSION: u32 = 0x8000;
    let (mut entry, attributes) = if member.directory {
        (
            ArchiveEntry::new_directory(&member.name),
            (0o040700 << 16) | 0x10,
        )
    } else {
        (
            ArchiveEntry::new_file(&member.name),
            (0o100600 << 16) | 0x20,
        )
    };
    entry.windows_attributes = attributes | UNIX_EXTENSION;
    entry.has_windows_attributes = true;
    entry.last_modified_date = UNIX_EPOCH
        .checked_add(Duration::from_secs(member.modified))
        .and_then(|time| NtTime::try_from(time).ok())
        .ok_or_else(|| invalid("An archive member timestamp exceeds the 7z time range"))?;
    entry.has_last_modified_date = true;
    Ok(entry)
}

fn content_method(level: u8, size: u64) -> EncoderConfiguration {
    if level == 0 {
        return EncoderConfiguration::new(EncoderMethod::COPY);
    }
    let maximum = match level {
        1 => MIB,
        2 => 2 * MIB,
        3 | 4 => 4 * MIB,
        _ => 8 * MIB,
    };
    let dictionary = size.min(u64::from(maximum)).next_power_of_two().max(4096) as u32;
    let mut options = Lzma2Options::from_level(u32::from(level));
    options.set_dictionary_size(dictionary);
    options.into()
}

fn writer_error(error: sevenz_rust2::Error) -> io::Error {
    match error {
        sevenz_rust2::Error::Io(error, _) => error,
        _ => io::Error::other("Could not encode a 7z archive"),
    }
}

struct SpoolReader<'a> {
    bytes: &'a [u8],
    stopped: &'a dyn Fn() -> bool,
}

impl Read for SpoolReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_transfer(self.stopped)?;
        self.bytes.read(output)
    }
}
