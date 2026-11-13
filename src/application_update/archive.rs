//! Validate the staged release's native desktop executable without extracting it.

use std::collections::BTreeSet;
use std::io::{self, Read, Seek};
use std::sync::atomic::AtomicBool;

use flate2::read::MultiGzDecoder;
use zip::read::{ArchiveOffset, Config};
use zip::unstable::read::ZipIterable;
use zip::{CompressionMethod, ZipReadOptions};

use crate::native_executable::Platform;

use super::{Offer, active};

const MAX_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;

pub(super) fn verify(
    file: &mut (impl Read + Seek),
    offer: &Offer,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    active(cancelled)?;
    file.rewind()?;
    let (suffix, executable) = match offer.target.platform {
        Platform::Linux => (".tar.gz", "bin/kortty"),
        Platform::Windows => (".zip", "kortty.exe"),
        Platform::Macos => (".zip", "kortty.app/Contents/MacOS/kortty"),
    };
    let root = offer.name.strip_suffix(suffix).ok_or_else(invalid)?;
    let mut layout = Layout {
        root,
        executable: format!("{root}/{executable}"),
        paths: BTreeSet::new(),
        found: false,
    };
    if offer.target.platform == Platform::Linux {
        inspect_tar(file, offer, cancelled, &mut layout)?;
    } else {
        inspect_zip(file, offer, cancelled, &mut layout)?;
    }
    if !layout.found {
        return Err(io::Error::other(
            "The update archive does not contain the expected desktop executable",
        ));
    }
    active(cancelled)
}

struct Layout<'a> {
    root: &'a str,
    executable: String,
    paths: BTreeSet<String>,
    found: bool,
}

impl Layout<'_> {
    fn add(&mut self, name: &str) -> io::Result<bool> {
        let Some((root, path)) = name.split_once('/') else {
            return Err(invalid());
        };
        if name.len() > MAX_PATH_BYTES
            || root != self.root
            || path.split('/').any(|part| {
                part.is_empty()
                    || part.ends_with('.')
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            })
            || !self.paths.insert(name.to_ascii_lowercase())
        {
            return Err(invalid());
        }
        // The packager emits regular files only. A file cannot occupy the executable's
        // parent directory or appear below its path, including case-insensitive aliases.
        let expected = self.executable.to_ascii_lowercase();
        let path = name.to_ascii_lowercase();
        let ancestor = |left: &str, right: &str| {
            right
                .strip_prefix(left)
                .is_some_and(|rest| rest.starts_with('/'))
        };
        if ancestor(&path, &expected) || ancestor(&expected, &path) {
            return Err(invalid());
        }
        Ok(name == self.executable)
    }

    fn check_executable(&mut self, offer: &Offer, file: &mut impl Read) -> io::Result<()> {
        if !offer.target.matches(file).unwrap_or(false) {
            return Err(io::Error::other(
                "The update desktop executable has the wrong native architecture or format",
            ));
        }
        self.found = true;
        Ok(())
    }
}

fn inspect_zip(
    file: impl Read + Seek,
    offer: &Offer,
    cancelled: &AtomicBool,
    layout: &mut Layout<'_>,
) -> io::Result<()> {
    let mut zip = ZipIterable::try_new(
        file,
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
    )
    .map_err(|_| invalid())?
    .enforce_header_consistency(true);
    if !(1..=MAX_ENTRIES).contains(&zip.len()) {
        return Err(invalid());
    }
    let mut expanded = 0u64;
    let mut ranges = Vec::with_capacity(zip.len());
    let mut central = offer.size;
    let entries = zip.files().map_err(|_| invalid())?;
    while let Some(entry) = entries.next() {
        active(cancelled)?;
        let entry = entry.map_err(|_| invalid())?;
        let mode = entry.unix_mode().unwrap_or_default() & 0o170000;
        if !entry.is_file()
            || entry.is_symlink()
            || entry.encrypted()
            || entry.external_attributes() & 0x400 != 0
            || !matches!(mode, 0 | 0o100000)
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(invalid());
        }
        expanded = expanded
            .checked_add(entry.size())
            .filter(|size| *size <= MAX_EXPANDED)
            .ok_or_else(invalid)?;
        let name = entry.name().map_err(|_| invalid())?;
        if name.as_bytes() != entry.name_raw() {
            return Err(invalid());
        }
        let desktop = layout.add(&name)?;
        let header = entry.header_start();
        central = central.min(entry.central_header_start());
        let compressed = entry.compressed_size();
        let mut body = entry
            .with_reader(entries.reader_mut(), ZipReadOptions::new())
            .map_err(|_| invalid())?;
        let end = body
            .data_start()
            .filter(|start| *start > header)
            .and_then(|start| start.checked_add(compressed))
            .ok_or_else(invalid)?;
        ranges.push((header, end));
        if desktop {
            layout.check_executable(offer, &mut body)?;
        }
    }
    ranges.sort_unstable();
    if ranges.last().is_some_and(|&(_, end)| end > central)
        || ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
    {
        return Err(invalid());
    }
    Ok(())
}

fn inspect_tar(
    file: impl Read,
    offer: &Offer,
    cancelled: &AtomicBool,
    layout: &mut Layout<'_>,
) -> io::Result<()> {
    let reader = CancelRead {
        reader: MultiGzDecoder::new(file),
        cancelled,
    };
    let mut archive = tar::Archive::new(reader.take(MAX_EXPANDED));
    // Raw entries prevent extended TAR headers from replacing the path or allocating a large
    // metadata body. The packager emits only regular files with their full paths in the headers.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        let mut entry = entry?;
        if index >= MAX_ENTRIES || !entry.header().entry_type().is_file() {
            return Err(invalid());
        }
        let desktop = {
            let path = entry.header().path_bytes();
            layout.add(std::str::from_utf8(&path).map_err(|_| invalid())?)?
        };
        if desktop {
            layout.check_executable(offer, &mut entry)?;
        }
    }
    // Accept only standard zero padding after the first TAR end marker. This also exhausts
    // the gzip decoder within the cap, catching an appended archive or invalid gzip trailer.
    let mut reader = archive.into_inner();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(invalid());
        }
    }
    if reader.limit() == 0 {
        return Err(invalid());
    }
    Ok(())
}

struct CancelRead<'a, R> {
    reader: R,
    cancelled: &'a AtomicBool,
}

impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // Read::read_exact retries Interrupted, so use a terminal error for cancellation here.
        active(self.cancelled).map_err(|_| io::Error::other("Application update canceled"))?;
        self.reader.read(buffer)
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The update archive has an invalid release layout",
    )
}
