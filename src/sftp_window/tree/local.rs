use std::ffi::OsStr;
use std::fs::File;
use std::io;

use super::{MAX_DEPTH, MAX_ENTRIES};
#[cfg(windows)]
use crate::sftp_window::{EntryKind, files};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::Directory;

#[cfg(windows)]
pub(crate) use kortty_files::tree::Directory;

pub(crate) enum Entry {
    Directory(Directory),
    File(File),
    Skip,
}

#[cfg(unix)]
pub(crate) fn entry(directory: &Directory, name: &OsStr) -> io::Result<Entry> {
    directory.entry(name)
}

#[cfg(windows)]
pub(crate) fn entry(directory: &Directory, name: &OsStr) -> io::Result<Entry> {
    let Some(file) = directory.read_child(name)? else {
        return Ok(Entry::Skip);
    };
    match files::local_kind(&file.metadata()?) {
        EntryKind::Directory => Ok(Entry::Directory(Directory::from_file(file)?)),
        EntryKind::File => Ok(Entry::File(file)),
        EntryKind::Link | EntryKind::Other => Ok(Entry::Skip),
    }
}

pub(super) fn publish(
    parent: &Directory,
    stage_name: &OsStr,
    stage: &Directory,
    destination: &OsStr,
) -> io::Result<()> {
    #[cfg(unix)]
    return parent.publish(stage_name, stage, destination);
    #[cfg(windows)]
    {
        let _ = stage_name;
        parent.publish(stage, destination)
    }
}

pub(crate) fn publish_file(
    parent: &Directory,
    stage: &Directory,
    name: &OsStr,
    file: &File,
    destination: &OsStr,
) -> io::Result<()> {
    #[cfg(unix)]
    return parent.publish_file(stage, name, file, destination);
    #[cfg(windows)]
    {
        let _ = (stage, name);
        parent.publish_file(file, destination)
    }
}

pub(crate) fn discard(parent: &Directory, name: &OsStr, stage: Directory) -> io::Result<()> {
    #[cfg(unix)]
    return parent.discard(name, stage, MAX_DEPTH, &mut (MAX_ENTRIES + 1));
    #[cfg(windows)]
    {
        let _ = (parent, name);
        discard_windows(stage, MAX_DEPTH, &mut (MAX_ENTRIES + 1))
    }
}

#[cfg(windows)]
fn discard_windows(directory: Directory, depth: usize, remaining: &mut usize) -> io::Result<()> {
    for name in directory.names(*remaining)? {
        *remaining = remaining.checked_sub(1).ok_or_else(limit)?;
        let file = directory.child_for_removal(&name)?;
        let metadata = file.metadata()?;
        if metadata.is_dir() && !kortty_files::tree::is_reparse_point(&metadata) {
            let depth = depth.checked_sub(1).ok_or_else(limit)?;
            discard_windows(Directory::from_file(file)?, depth, remaining)?;
        } else {
            kortty_files::tree::remove(file)?;
        }
    }
    directory.remove()
}

fn limit() -> io::Error {
    io::Error::other("SFTP folder entry or depth limit exceeded")
}
