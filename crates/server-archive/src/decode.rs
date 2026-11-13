//! Decode only checked members, with separate limits on input and expansion.

use std::io::{self, Cursor, Read};

use sevenz_rust2::{ArchiveReader, EntryDecision, Password};

use crate::manifest::{MAX_MEMBERS, MAX_PATH_BYTES, Manifest, ManifestBuilder, Member, MemberKind};
use crate::{Format, MAX_ARCHIVE_BYTES};

mod tar;
mod zip;

const LZMA_MEMORY_KIB: usize = 96 * 1024;

/// Visits ordinary files and explicit directories. The returned manifest also
/// includes implicit directories and checked links. A caller should publish
/// only after decoding succeeds and it has created the checked links.
pub fn visit(
    format: Format,
    bytes: &[u8],
    mut sink: impl FnMut(&Member, &mut dyn Read) -> io::Result<()>,
) -> io::Result<Manifest> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(invalid("Choose a nonempty archive no larger than 64 MiB"));
    }
    let mut builder = ManifestBuilder::new();
    let mut output = Output {
        builder: &mut builder,
        sink: &mut sink,
    };
    match format {
        Format::Zip => zip::visit(bytes, &mut output)?,
        Format::Tar => tar::visit(bytes, &mut output)?,
        Format::TarGzip => tar::visit(flate2::read::MultiGzDecoder::new(bytes), &mut output)?,
        Format::TarBzip2 => tar::visit(bzip2::read::MultiBzDecoder::new(bytes), &mut output)?,
        Format::TarXz => tar::visit(
            lzma_rust2::XzReader::new_mem_limit(bytes, true, LZMA_MEMORY_KIB as u32),
            &mut output,
        )?,
        Format::SevenZip => seven_zip(bytes, &mut output)?,
    }
    builder.finish()
}

type Sink<'a> = dyn FnMut(&Member, &mut dyn Read) -> io::Result<()> + 'a;

struct Output<'a> {
    builder: &'a mut ManifestBuilder,
    sink: &'a mut Sink<'a>,
}

impl Output<'_> {
    fn content(
        &mut self,
        path: &str,
        kind: MemberKind,
        size: u64,
        reader: &mut dyn Read,
    ) -> io::Result<()> {
        let member = self.builder.add(path, kind, size)?;
        let mut bounded = reader.take(size);
        if let Some(member) = member {
            (self.sink)(&member, &mut bounded)?;
        }
        io::copy(&mut bounded, &mut io::sink())?;
        if bounded.limit() != 0 || reader.read(&mut [0])? != 0 {
            return Err(invalid(
                "Archive member bytes do not match the declared size",
            ));
        }
        Ok(())
    }

    fn link(&mut self, path: &str, kind: MemberKind) -> io::Result<()> {
        self.builder.add(path, kind, 0)?;
        Ok(())
    }
}

fn seven_zip(bytes: &[u8], output: &mut Output<'_>) -> io::Result<()> {
    let mut reader = ArchiveReader::new_with_memory_limit(
        Cursor::new(bytes),
        Password::empty(),
        LZMA_MEMORY_KIB,
    )
    .map_err(seven_zip_error)?;
    if reader.archive().files.len() > MAX_MEMBERS {
        return Err(invalid("The archive has too many entries"));
    }
    if (0..reader.archive().files.len()).any(|index| reader.archive().is_file_encrypted(index)) {
        return Err(encrypted());
    }
    reader
        .for_each_entries_with_decision_allowing_surplus(|entry, input| {
            if entry.is_anti_item() {
                return Err(invalid("7z removal entries cannot be extracted").into());
            }
            let attr = entry.windows_attributes;
            let posix_type = if attr & 0x8000 != 0 {
                (attr >> 16) & 0o170000
            } else {
                0
            };
            match (posix_type, entry.is_directory()) {
                (0o120000, false) => {
                    let target = link_bytes(input, entry.size)?;
                    output.link(entry.name(), MemberKind::Symlink { target })?;
                }
                _ if attr & 0x400 != 0 => return Err(unsupported_entry().into()),
                (0 | 0o040000, true) => {
                    output.content(entry.name(), MemberKind::Directory, entry.size, input)?;
                }
                (0 | 0o100000, false) => {
                    output.content(entry.name(), MemberKind::File, entry.size, input)?;
                }
                _ => return Err(unsupported_entry().into()),
            }
            Ok(EntryDecision::Continue)
        })
        .map_err(seven_zip_error)
}

fn seven_zip_error(error: sevenz_rust2::Error) -> io::Error {
    match error {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            encrypted()
        }
        sevenz_rust2::Error::Io(error, _) => error,
        _ => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

fn link_bytes(reader: &mut dyn Read, size: u64) -> io::Result<String> {
    if size == 0 || size > MAX_PATH_BYTES as u64 {
        return Err(invalid("Archive link target exceeds the path limit"));
    }
    let mut bytes = vec![0; size as usize];
    reader.read_exact(&mut bytes)?;
    if reader.read(&mut [0])? != 0 {
        return Err(invalid("Archive link bytes do not match the declared size"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("Archive link targets must use UTF-8"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn encrypted() -> io::Error {
    invalid("Encrypted archives cannot be extracted on the server")
}

fn unsupported_entry() -> io::Error {
    invalid("The archive contains an unsupported member type")
}

#[cfg(test)]
mod tests;
