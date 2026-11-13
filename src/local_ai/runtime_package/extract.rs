//! Bounded installation and verification of the exact payload from a signed archive.

use std::collections::{BTreeMap, btree_map::Entry};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::sync::atomic::AtomicBool;

use zip::read::{ArchiveOffset, Config};
use zip::unstable::read::ZipIterable;
use zip::{CompressionMethod, ZipReadOptions};

use crate::local_ai::runtime_index::Package;
use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{MAX_ARCHIVE_BYTES, check_cancelled, copy_archive};

pub(in crate::local_ai) const PAYLOAD: &str = "payload";
const MAX_ENTRIES: usize = 20_000;
// Leave room within the installation cleanup limit for its root and metadata.
const MAX_NODES: usize = 24_990;
const MAX_PATH_BYTES: usize = 1024;
const MAX_PATH_DEPTH: usize = 32;
const MAX_EXPANDED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MIN_EXPANSION_LIMIT: u64 = 512 * 1024 * 1024;
const CHUNK: usize = 128 * 1024;

pub(in crate::local_ai) struct Layout<'a> {
    pub(in crate::local_ai) size: u64,
    pub(in crate::local_ai) sha256: &'a str,
    pub(in crate::local_ai) executable: &'a str,
    pub(in crate::local_ai) companion: Option<&'a str>,
    pub(in crate::local_ai) wrapper: Option<&'a str>,
    pub(in crate::local_ai) entries: usize,
    pub(in crate::local_ai) nodes: usize,
}

impl<'a> From<&'a Package> for Layout<'a> {
    fn from(package: &'a Package) -> Self {
        Self {
            size: package.size(),
            sha256: package.sha256(),
            executable: package.entrypoint(),
            companion: None,
            wrapper: None,
            entries: MAX_ENTRIES,
            nodes: MAX_NODES,
        }
    }
}

pub(super) fn unpack(
    archive: File,
    stage: &Directory,
    package: &Package,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    apply_archive(archive, stage, package.into(), cancelled, true)
}

pub(super) fn verify(
    archive: File,
    installation: &Directory,
    package: &Package,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    apply_archive(archive, installation, package.into(), cancelled, false)
}

pub(in crate::local_ai) fn apply_archive(
    mut archive: File,
    installation: &Directory,
    layout: Layout<'_>,
    cancelled: &AtomicBool,
    create: bool,
) -> io::Result<()> {
    let stamp = authenticate_archive(&mut archive, &layout, cancelled)?;
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    // The iterator checks the central directory without allocating an entry table.
    let mut zip = ZipIterable::try_new(&mut archive, config)
        .map_err(io::Error::other)?
        .enforce_header_consistency(true);
    if !(1..=layout.entries).contains(&zip.len()) {
        return Err(invalid("Runtime ZIP has an invalid entry count"));
    }
    let payload = if create {
        installation.create_private_subdirectory(OsStr::new(PAYLOAD))?
    } else {
        installation.private_child(PAYLOAD, false)?
    };
    let files = zip.files().map_err(io::Error::other)?;
    let mut inventory = Inventory::default();
    let mut node_count = 0;
    let mut has_entrypoint = false;
    let mut has_companion = layout.companion.is_none();
    let mut ranges = Vec::new();
    let mut central_start = layout.size;
    let mut remaining = layout
        .size
        .saturating_mul(30)
        .clamp(MIN_EXPANSION_LIMIT, MAX_EXPANDED_BYTES);
    let mut buffer = vec![0; CHUNK];
    let mut compare_buffer = if create { Vec::new() } else { vec![0; CHUNK] };

    while let Some(entry) = files.next() {
        check_cancelled(cancelled)?;
        let entry = entry.map_err(io::Error::other)?;
        let is_directory = entry.is_dir();
        let expected_kind = if is_directory { 0o040000 } else { 0o100000 };
        let kind = entry.unix_mode().unwrap_or_default() & 0o170000;
        if entry.encrypted()
            || entry.is_symlink()
            || entry.external_attributes() & 0x400 != 0
            || (kind != 0 && kind != expected_kind)
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(invalid("Runtime ZIP contains an unsupported file entry"));
        }
        let size = entry.size();
        if is_directory && size != 0 {
            return Err(invalid("Runtime ZIP directory contains data"));
        }
        remaining = remaining
            .checked_sub(size)
            .ok_or_else(|| invalid("Runtime ZIP exceeds its expansion limit"))?;

        let path = entry.name().map_err(io::Error::other)?.into_owned();
        if path.as_bytes() != entry.name_raw() {
            return Err(invalid("Runtime ZIP entry has conflicting names"));
        }
        let path = if is_directory {
            path.strip_suffix('/').unwrap_or(&path)
        } else {
            &path
        };
        let components = path_components(path)?;
        if let Some(wrapper) = layout.wrapper
            && (components.first().copied() != Some(wrapper)
                || (components.len() == 1 && !is_directory))
        {
            return Err(invalid(
                "Runtime ZIP contains a path outside the signed installation wrapper",
            ));
        }
        let executable = path == layout.executable;
        if layout.companion == Some(path) {
            if is_directory || size == 0 {
                return Err(invalid("Runtime ZIP companion is not a regular file"));
            }
            has_companion = true;
        }
        if executable {
            if is_directory || size == 0 {
                return Err(invalid(
                    "Signed runtime entrypoint is not an executable file",
                ));
            }
            has_entrypoint = true;
        }
        inventory.record(&components, is_directory, &mut node_count, layout.nodes)?;
        let (name, parents) = components.split_last().expect("validated path");
        let mut child = None;
        for part in parents {
            child = Some(
                child
                    .as_ref()
                    .unwrap_or(&payload)
                    .private_child(part, create)?,
            );
        }
        let parent = child.as_ref().unwrap_or(&payload);
        let name = *name;

        let header = entry.header_start();
        central_start = central_start.min(entry.central_header_start());
        let compressed_size = entry.compressed_size();
        let mut input = entry
            .with_reader(files.reader_mut(), ZipReadOptions::new())
            .map_err(io::Error::other)?;
        let end = input
            .data_start()
            .filter(|start| *start > header)
            .and_then(|start| start.checked_add(compressed_size))
            .filter(|end| *end <= central_start)
            .ok_or_else(|| invalid("Runtime ZIP entry extends beyond its data area"))?;
        ranges.push((header, end));

        if is_directory {
            stream_entry(&mut input, size, &mut buffer, cancelled, |_| Ok(()))?;
            parent.private_child(name, create)?;
        } else if create {
            parent.publish_with(name, |file| {
                stream_entry(&mut input, size, &mut buffer, cancelled, |bytes| {
                    file.write_all(bytes)
                })?;
                #[cfg(unix)]
                if executable {
                    mark_executable(file)?;
                }
                Ok(())
            })?;
        } else {
            let mut file = parent.open_private_file(name)?;
            require_single_link(&file)?;
            let file_stamp = FileStamp::read(&file)?;
            if file.metadata()?.len() != size {
                return Err(invalid("Installed runtime file has the wrong size"));
            }
            #[cfg(unix)]
            if executable {
                require_executable(&file)?;
            }
            stream_entry(&mut input, size, &mut buffer, cancelled, |expected| {
                let actual = &mut compare_buffer[..expected.len()];
                file.read_exact(actual)?;
                if actual != expected {
                    return Err(invalid(
                        "Installed runtime file differs from the signed ZIP",
                    ));
                }
                Ok(())
            })?;
            file_stamp.require_unchanged(&file)?;
            file_stamp.require_unchanged(&parent.open_private_file(name)?)?;
        }
    }

    ranges.sort_unstable();
    if ranges.last().is_some_and(|&(_, end)| end > central_start)
        || ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
    {
        return Err(invalid("Runtime ZIP contains overlapping entries"));
    }
    if !has_entrypoint {
        return Err(invalid(
            "Runtime ZIP does not contain its signed entrypoint",
        ));
    }
    if !has_companion {
        return Err(invalid("Runtime ZIP is missing its signed companion"));
    }
    inventory.verify(&payload, create, cancelled)?;
    if create {
        installation.sync()?;
    }
    require_single_link(&archive)?;
    stamp.require_unchanged(&archive)?;
    check_cancelled(cancelled)
}

fn authenticate_archive(
    archive: &mut File,
    layout: &Layout<'_>,
    cancelled: &AtomicBool,
) -> io::Result<FileStamp> {
    require_single_link(archive)?;
    let stamp = FileStamp::read(archive)?;
    let size = layout.size;
    if !(1..=MAX_ARCHIVE_BYTES).contains(&size) || archive.metadata()?.len() != size {
        return Err(invalid("Runtime ZIP size differs from the signed index"));
    }
    archive.rewind()?;
    if copy_archive(archive, &mut io::sink(), size, cancelled)? != layout.sha256 {
        return Err(invalid("Runtime ZIP digest differs from the signed index"));
    }
    stamp.require_unchanged(archive)?;
    archive.rewind()?;
    Ok(stamp)
}

fn path_components(path: &str) -> io::Result<Vec<&str>> {
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid("Runtime ZIP entry path is too long"));
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() > MAX_PATH_DEPTH
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 255
                || part.ends_with(['.', ' '])
                || part.bytes().any(|byte| {
                    !(b' '..=b'~').contains(&byte)
                        || matches!(
                            byte,
                            b'\\' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|' | b'~'
                        )
                })
                || is_device_name(part)
        })
    {
        return Err(invalid("Runtime ZIP contains an unsafe portable path"));
    }
    Ok(parts)
}

fn is_device_name(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or(part)
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
}

fn stream_entry(
    input: &mut impl Read,
    size: u64,
    buffer: &mut [u8],
    cancelled: &AtomicBool,
    mut consume: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut remaining = size;
    while remaining != 0 {
        check_cancelled(cancelled)?;
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(invalid(
                "Runtime ZIP entry is smaller than its declared size",
            ));
        }
        consume(&buffer[..count])?;
        remaining -= count as u64;
    }
    // Reading to EOF lets the ZIP reader also validate the entry's CRC.
    if input.read(&mut buffer[..1])? != 0 {
        return Err(invalid(
            "Runtime ZIP entry is larger than its declared size",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Inventory(BTreeMap<String, Node>);

struct Node {
    name: String,
    children: Option<Inventory>,
    explicit: bool,
}

impl Inventory {
    fn record(
        &mut self,
        parts: &[&str],
        directory: bool,
        count: &mut usize,
        max_nodes: usize,
    ) -> io::Result<()> {
        let (&name, rest) = parts.split_first().expect("validated path");
        let explicit = rest.is_empty();
        let has_children = !explicit || directory;
        let node = match self.0.entry(name.to_ascii_lowercase()) {
            Entry::Vacant(slot) => {
                *count += 1;
                if *count > max_nodes {
                    return Err(invalid("Runtime ZIP creates too many payload nodes"));
                }
                slot.insert(Node {
                    name: name.to_owned(),
                    children: has_children.then(Inventory::default),
                    explicit,
                })
            }
            Entry::Occupied(slot) => {
                let node = slot.into_mut();
                if node.name != name
                    || node.children.is_some() != has_children
                    || (node.explicit && explicit)
                {
                    return Err(invalid(
                        "Runtime ZIP contains a duplicate or conflicting path",
                    ));
                }
                node.explicit |= explicit;
                node
            }
        };
        if !rest.is_empty() {
            node.children
                .as_mut()
                .expect("parent directory")
                .record(rest, directory, count, max_nodes)?;
        }
        Ok(())
    }

    fn verify(&self, actual: &Directory, sync: bool, cancelled: &AtomicBool) -> io::Result<()> {
        check_cancelled(cancelled)?;
        let names = actual.names(self.0.len().saturating_add(1))?;
        if names.len() != self.0.len() {
            return Err(invalid(
                "Runtime payload contains a missing or unexpected path",
            ));
        }
        for name in &names {
            check_cancelled(cancelled)?;
            let node = name.to_str().and_then(|name| {
                self.0
                    .get(&name.to_ascii_lowercase())
                    .filter(|node| node.name == name)
            });
            let node =
                node.ok_or_else(|| invalid("Runtime payload contains an unexpected path"))?;
            if let Some(children) = &node.children {
                children.verify(&actual.private_child(&node.name, false)?, sync, cancelled)?;
            } else {
                require_single_link(&actual.open_private_file(&node.name)?)?;
            }
        }
        if sync {
            actual.sync()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn mark_executable(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
fn require_executable(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if file.metadata()?.permissions().mode() & 0o7777 != 0o700 {
        return Err(invalid(
            "Installed runtime entrypoint permissions are invalid",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
