use std::io::{self, Read as _, Write as _};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::ffi::OsStr;

use zeroize::Zeroizing;

use crate::logfile_window::zstandard::{ENCODE_CHUNK_BYTES, encode_chunk};
use crate::private_files::{Directory, EntryIdentity, FileStamp};

use super::super::super::capture::{LiveArchive, LiveArchives};
use super::super::super::metadata::require_single_link;
use super::super::storage::{Storage, file_identity};
use super::{LogFormat, invalid};

const FINISH_BUDGET: Duration = Duration::from_secs(4);

pub(super) struct SealedPart {
    pub(super) number: u32,
    pub(super) bytes: u64,
    pub(super) identity: EntryIdentity,
}

pub(super) fn rotate(
    storage: &Storage,
    format: LogFormat,
    part: &SealedPart,
    archives: &LiveArchives,
) -> bool {
    archive_checked(
        storage,
        format,
        part,
        archives,
        Instant::now() + FINISH_BUDGET,
    )
    .is_ok()
}

pub(super) fn archive(
    storage: &Storage,
    format: LogFormat,
    parts: Vec<SealedPart>,
    archives: &LiveArchives,
) {
    let deadline = Instant::now() + FINISH_BUDGET;
    for part in parts {
        if Instant::now() >= deadline {
            break;
        }
        // The readable plain file is the fallback when optional compaction fails.
        let _ = archive_checked(storage, format, &part, archives, deadline);
    }
}

fn archive_checked(
    storage: &Storage,
    format: LogFormat,
    part: &SealedPart,
    archives: &LiveArchives,
    deadline: Instant,
) -> io::Result<()> {
    archive_part(
        storage.locked_directory()?,
        &format.part_name(part.number),
        part,
        archives,
        deadline,
        || storage.require_metadata(),
    )
}

fn archive_part(
    directory: &Directory,
    name: &str,
    part: &SealedPart,
    archives: &LiveArchives,
    deadline: Instant,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    let source = directory.open_private_file(name)?;
    require_single_link(&source)?;
    if file_identity(&source)? != part.identity || source.metadata()?.len() != part.bytes {
        return Err(invalid("The closed session journal log part changed"));
    }
    let stamp = FileStamp::read(&source)?;
    let archive = format!("{name}.zst");
    let mut staged = None;
    let mut frame_ends = Vec::new();
    directory.publish_checked(
        &archive,
        |output| {
            let mut input = (&source).take(part.bytes + 1);
            let mut buffer = Zeroizing::new(vec![0; ENCODE_CHUNK_BYTES]);
            let mut remaining = part.bytes;
            let mut compressed = 0;
            while remaining != 0 {
                check_deadline(deadline)?;
                let count = remaining.min(ENCODE_CHUNK_BYTES as u64) as usize;
                input.read_exact(&mut buffer[..count])?;
                let frame = encode_chunk(&buffer[..count]);
                output.write_all(&frame)?;
                compressed += frame.len() as u64;
                frame_ends.push(compressed);
                remaining -= count as u64;
            }
            if input.read(&mut buffer[..1])? != 0 {
                return Err(invalid("The closed session journal log part grew"));
            }
            output.sync_all()?;
            // Retain the synchronized write handle during the publication checks.
            staged = Some((FileStamp::read(output)?, output.try_clone()?));
            Ok(())
        },
        || {
            check_deadline(deadline)?;
            verify()?;
            stamp.require_unchanged(&source)?;
            unchanged(directory, name, &stamp)
        },
    )?;

    // Commit a synced archive before removing the original. If anything fails after
    // publication, keep the recoverable files in place.
    directory.sync()?;
    let (staged, write_handle) = staged.expect("successful archive publication wrote a file");
    let file = directory.open_private_file(&archive)?;
    require_single_link(&file)?;
    #[cfg(unix)]
    staged.stamp_after_move(&file)?;
    #[cfg(windows)]
    staged.require_unchanged(&file)?;
    drop(write_handle);
    let published = FileStamp::read(&file)?;
    unchanged(directory, &archive, &published)?;
    stamp.require_unchanged(&source)?;
    drop(source);
    // Register first so a live poll can always open the sealed original or its equivalent.
    archives.record(
        part.number,
        LiveArchive {
            original: part.identity,
            stamp: published.clone(),
            length: part.bytes,
            frame_ends,
        },
    );
    let _ = remove_plain(directory, name, &stamp, || {
        verify()?;
        unchanged(directory, &archive, &published)
    });
    Ok(())
}

fn unchanged(directory: &Directory, name: &str, stamp: &FileStamp) -> io::Result<()> {
    let file = directory.open_private_file(name)?;
    require_single_link(&file)?;
    stamp.require_unchanged(&file)
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Journal compression reached its time limit",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn remove_plain(
    directory: &Directory,
    name: &str,
    stamp: &FileStamp,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    directory.remove_unchanged_checked(name, stamp, |file| {
        verify()?;
        require_single_link(file)
    })
}

#[cfg(unix)]
fn remove_plain(
    directory: &Directory,
    name: &str,
    stamp: &FileStamp,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    verify()?;
    unchanged(directory, name, stamp)?;
    let stage_name = format!(
        ".kortty-journal-compression-{}",
        crate::private_files::nonce()?
    );
    let stage_name = OsStr::new(&stage_name);
    let stage = directory.create_private_subdirectory(stage_name)?;
    let identity = stage.identity()?;
    let entry = OsStr::new(name);
    let result = directory.move_entry(entry, &stage).and_then(|()| {
        let remove = (|| {
            verify()?;
            let source = stage.open_private_file(name)?;
            require_single_link(&source)?;
            let claimed = stamp.stamp_after_move(&source)?;
            stage.remove_unchanged(name, &claimed)
        })();
        if remove.is_err() {
            stage.move_entry(entry, directory)?;
        }
        remove
    });
    let synced = if result.is_ok() { stage.sync() } else { Ok(()) };
    let clean = directory.remove_deletion_entry(stage_name, identity, true);
    result.and(synced).and(clean)?;
    directory.sync()
}
