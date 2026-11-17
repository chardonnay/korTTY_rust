//! The TAR.ZST layout used by the earlier signed MLX releases.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::logfile_window::zstandard::Decoder;
use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{
    FilesOnDisk, Inventory, Layout, PAYLOAD, authenticate_archive, check_cancelled,
    expansion_budget, invalid, open_parent, path_components,
};

const TAR_BLOCK: u64 = 512;
const MAX_TRAILING_ZEROS: u64 = 1024 * 1024;

pub(in crate::local_ai) fn apply_tar_zst(
    mut archive: File,
    installation: &Directory,
    layout: Layout<'_>,
    cancelled: &AtomicBool,
    create: bool,
) -> io::Result<()> {
    let stamp = authenticate_archive(&mut archive, &layout, cancelled)?;
    let payload = if create {
        installation.create_private_subdirectory(OsStr::new(PAYLOAD))?
    } else {
        installation.private_child(PAYLOAD, false)?
    };
    let decoder = Decoder::new(&mut archive, || cancelled.load(Ordering::Acquire));
    let decoded = TerminalRead(decoder);
    let maximum = expansion_budget(layout.size)
        .saturating_add(2 * TAR_BLOCK * layout.entries as u64)
        .saturating_add(TAR_BLOCK + MAX_TRAILING_ZEROS + 1);
    let result = apply_tar(decoded.take(maximum), &payload, &layout, cancelled, create);
    check_cancelled(cancelled)?;
    result?;
    require_single_link(&archive)?;
    stamp.require_unchanged(&archive)?;
    if create {
        installation.sync()?;
    }
    check_cancelled(cancelled)
}

fn apply_tar(
    reader: io::Take<impl Read>,
    payload: &Directory,
    layout: &Layout<'_>,
    cancelled: &AtomicBool,
    create: bool,
) -> io::Result<()> {
    let wrapper = layout
        .wrapper
        .ok_or_else(|| invalid("Missing signed TAR wrapper"))?;
    let mut archive = tar::Archive::new(reader);
    let mut inventory = Inventory::default();
    let mut node_count = 0;
    let mut budget = expansion_budget(layout.size);
    let mut files = BTreeMap::new();
    let mut aliases = Vec::new();
    let mut files_on_disk = FilesOnDisk::new(create, cancelled);

    // Raw headers exclude PAX overrides, GNU long paths, and sparse entries.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        check_cancelled(cancelled)?;
        let mut entry = entry?;
        if index >= layout.entries {
            return Err(invalid("Runtime TAR has too many entries"));
        }
        let kind = entry.header().entry_type();
        let size = entry.size();
        if !(kind.is_file() || (kind.is_dir() || kind.is_symlink()) && size == 0) {
            return Err(invalid("Runtime TAR contains an unsupported file entry"));
        }
        let raw = entry.header().path_bytes().into_owned();
        let path = std::str::from_utf8(&raw).map_err(|_| invalid("Invalid runtime TAR path"))?;
        let path = if kind.is_dir() {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        let components = path_components(path)?;
        if components.first().copied() != Some(wrapper) || components.len() == 1 && !kind.is_dir() {
            return Err(invalid("Runtime TAR entry is outside the signed wrapper"));
        }
        inventory.record(&components, kind.is_dir(), &mut node_count, layout.nodes)?;
        if kind.is_symlink() {
            let raw_target = entry.header().link_name_bytes();
            let target = raw_target
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .ok_or_else(|| invalid("Invalid runtime TAR alias"))?;
            if path_components(target)?.len() != 1 || layout.companion == Some(path) {
                return Err(invalid("Runtime TAR alias must point to a sibling file"));
            }
            aliases.push((path.to_owned(), target.to_owned()));
            continue;
        }

        let (&name, parents) = components.split_last().expect("validated path");
        let parent = open_parent(payload, parents, create)?;
        let parent = parent.as_ref().unwrap_or(payload);
        if kind.is_dir() {
            parent.private_child(name, create)?;
        } else {
            charge(&mut budget, size)?;
            let mut signed = HashRead::new(&mut entry);
            files_on_disk.apply(parent, name, &mut signed, size, path == layout.executable)?;
            files.insert(path.to_owned(), (size, signed.finish()));
        }
    }
    verify_padding(archive.into_inner(), cancelled)?;

    let mut has_entrypoint = files
        .get(layout.executable)
        .is_some_and(|(size, _)| *size > 0);
    for (path, target) in aliases {
        check_cancelled(cancelled)?;
        let (base, _) = path.rsplit_once('/').expect("validated wrapped file");
        let signed_target = format!("{base}/{target}");
        let &(size, digest) = files
            .get(&signed_target)
            .ok_or_else(|| invalid("Runtime TAR alias target is not a signed regular file"))?;
        charge(&mut budget, size)?;
        let executable = path == layout.executable;
        has_entrypoint |= executable && size > 0;
        let parts = path_components(&path)?;
        let (&name, parents) = parts.split_last().expect("validated path");
        let parent = open_parent(payload, parents, false)?.expect("validated wrapped file");
        let file = parent.open_private_file(&target)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let mut source = HashRead::new(file);
        files_on_disk.apply(&parent, name, &mut source, size, executable)?;
        stamp.require_unchanged(&source.reader)?;
        stamp.require_unchanged(&parent.open_private_file(&target)?)?;
        if source.finish() != digest {
            return Err(invalid(
                "Runtime TAR alias target differs from the signed file",
            ));
        }
    }
    if !has_entrypoint
        || layout
            .companion
            .is_some_and(|name| files.get(name).is_none_or(|(size, _)| *size == 0))
    {
        return Err(invalid(
            "Runtime TAR is missing a signed entrypoint or companion",
        ));
    }
    inventory.verify(payload, create, cancelled)
}

fn charge(budget: &mut u64, size: u64) -> io::Result<()> {
    *budget = budget
        .checked_sub(size)
        .ok_or_else(|| invalid("Runtime TAR exceeds its expansion limit"))?;
    Ok(())
}

fn verify_padding(mut reader: io::Take<impl Read>, cancelled: &AtomicBool) -> io::Result<()> {
    let mut buffer = [0; 8192];
    let mut padding = 0;
    loop {
        check_cancelled(cancelled)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        padding += read as u64;
        if padding > MAX_TRAILING_ZEROS || buffer[..read].iter().any(|byte| *byte != 0) {
            return Err(invalid("Runtime TAR contains invalid trailing data"));
        }
    }
    if reader.limit() == 0 || padding < TAR_BLOCK || padding % TAR_BLOCK != 0 {
        return Err(invalid("Runtime TAR has an invalid end marker"));
    }
    Ok(())
}

struct HashRead<R> {
    reader: R,
    hash: Sha256,
}

impl<R> HashRead<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            hash: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.hash.finalize().into()
    }
}

impl<R: Read> Read for HashRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

struct TerminalRead<R>(R);

impl<R: Read> Read for TerminalRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes).map_err(|error| {
            // TAR's read_exact would retry an Interrupted decoder instead of stopping.
            if error.kind() == io::ErrorKind::Interrupted {
                io::Error::other(error)
            } else {
                error
            }
        })
    }
}
