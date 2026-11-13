//! Resolve copy sources to regular paths while preserving no-follow destinations.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kortty_ssh::sftp::StatusCode;

use super::{Attributes, Client, Error, FileKind, Result, files, parent, unchanged};
use crate::sftp_window::local_tree::Directory;

pub(super) fn local(path: &Path) -> Result<Option<(PathBuf, u64)>> {
    let source_folder = parent(path)?;
    let source_parent = Directory::open_path(source_folder)?;
    let Some(original) = local_link(&source_parent, path)? else {
        return Ok(None);
    };
    // Use native resolution for relative targets and link chains. The checks
    // below reject a link or parent that changed during resolution.
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if unavailable_local_target(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (Some(target_folder), Some(name)) = (target.parent(), target.file_name()) else {
        return Ok(None);
    };
    let target_parent = Directory::open_path(target_folder)?;
    let Some(regular) = regular_local(&target_parent, name)? else {
        return Ok(None);
    };
    if fs::canonicalize(path)? != target
        || local_link(&source_parent, path)?.as_ref() != Some(&original)
        || !source_parent.same_as(&Directory::open_path(source_folder)?)?
        || !target_parent.same_as(&Directory::open_path(target_folder)?)?
        || regular_local(&target_parent, name)?.as_ref() != Some(&regular)
    {
        return Err(io::Error::other("The local symbolic link or its target changed").into());
    }
    Ok(Some((target, regular.0)))
}

#[derive(PartialEq, Eq)]
struct LocalLink {
    target: PathBuf,
    #[cfg(unix)]
    stamp: UnixStamp,
    #[cfg(windows)]
    stamp: files::LocalStamp,
}

#[cfg(unix)]
fn local_link(directory: &Directory, path: &Path) -> io::Result<Option<LocalLink>> {
    use std::os::unix::ffi::OsStringExt as _;

    use rustix::fs::{AtFlags, FileType, readlinkat, statat};

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("Expected a symbolic link filename"))?;
    let metadata = match statat(directory.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Symlink {
        return Ok(None);
    }
    let target = readlinkat(directory.file(), name, Vec::new())?;
    Ok(Some(LocalLink {
        target: std::ffi::OsString::from_vec(target.into_bytes()).into(),
        stamp: unix_stamp(&metadata),
    }))
}

#[cfg(windows)]
fn local_link(_directory: &Directory, path: &Path) -> io::Result<Option<LocalLink>> {
    use std::os::windows::fs::FileTypeExt as _;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_symlink_file() {
        return Ok(None);
    }
    Ok(Some(LocalLink {
        target: fs::read_link(path)?,
        stamp: files::LocalStamp::capture(&metadata),
    }))
}

#[cfg(unix)]
type UnixStamp = [i128; 10];

#[cfg(unix)]
fn unix_stamp(metadata: &rustix::fs::Stat) -> UnixStamp {
    [
        metadata.st_dev.into(),
        metadata.st_ino.into(),
        metadata.st_mode.into(),
        metadata.st_uid.into(),
        metadata.st_gid.into(),
        metadata.st_size.into(),
        metadata.st_mtime.into(),
        metadata.st_mtime_nsec.into(),
        metadata.st_ctime.into(),
        metadata.st_ctime_nsec.into(),
    ]
}

#[cfg(unix)]
fn regular_local(directory: &Directory, name: &OsStr) -> io::Result<Option<(u64, UnixStamp)>> {
    use rustix::fs::{AtFlags, FileType, statat};

    let metadata = match statat(directory.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Ok(None);
    }
    let size = u64::try_from(metadata.st_size)
        .map_err(|_| io::Error::other("The linked file size is invalid"))?;
    Ok(Some((size, unix_stamp(&metadata))))
}

#[cfg(windows)]
fn regular_local(
    directory: &Directory,
    name: &OsStr,
) -> io::Result<Option<(u64, super::resume::persist::Identity, files::LocalStamp)>> {
    let file = match directory.open_entry_metadata(name) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || kortty_files::tree::is_reparse_point(&metadata) {
        return Ok(None);
    }
    Ok(Some((
        metadata.len(),
        super::resume::persist::Identity::capture(&file)?,
        files::LocalStamp::capture(&metadata),
    )))
}

fn unavailable_local_target(error: &io::Error) -> bool {
    #[cfg(unix)]
    let loop_error = rustix::io::Errno::LOOP.raw_os_error();
    #[cfg(windows)]
    let loop_error = 1921; // ERROR_CANT_RESOLVE_FILENAME

    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) || error.raw_os_error() == Some(loop_error)
}

pub(super) fn remote(
    client: &mut Client,
    path: &[u8],
    original: Attributes,
) -> Result<Option<(Vec<u8>, u64)>> {
    if original.kind() != FileKind::Symlink {
        return Ok(None);
    }
    let Some(target) = remote_target(client.canonicalize(path))? else {
        return Ok(None);
    };
    if target.len() > 16_384 {
        return Err(Error::Limit("resolved SFTP file link path").into());
    }
    if target == b"/" {
        return Ok(None);
    }
    if !target.strip_prefix(b"/").is_some_and(|tail| {
        tail.split(|byte| *byte == b'/')
            .all(|name| files::validate_remote_name(name).is_ok())
    }) {
        return Err(Error::Protocol("SFTP REALPATH returned a noncanonical link target").into());
    }
    let Some(regular) = remote_target(client.lstat(&target))? else {
        return Ok(None);
    };
    if regular.kind() != FileKind::File {
        return Ok(None);
    }
    let size = regular
        .size
        .ok_or_else(|| io::Error::other("The linked remote file size is missing"))?;
    if !unchanged(regular, client.stat(path)?)
        || client.canonicalize(path)? != target
        || !unchanged(original, client.lstat(path)?)
        || client.canonicalize(&target)? != target
        || !unchanged(regular, client.lstat(&target)?)
    {
        return Err(Error::SourceChanged.into());
    }
    Ok(Some((target, size)))
}

fn remote_target<T>(result: std::result::Result<T, Error>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        // Some SFTP v3 servers report dangling links and loops as generic Failure.
        Err(Error::Status(status))
            if matches!(status.code, StatusCode::NoSuchFile | StatusCode::Failure) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn packet(replies: &mut Vec<u8>, body: &[u8]) {
        replies.extend_from_slice(&(body.len() as u32).to_be_bytes());
        replies.extend_from_slice(body);
    }

    fn realpath(replies: &mut Vec<u8>, id: u32, path: &[u8]) {
        let mut body = vec![104];
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&1_u32.to_be_bytes());
        body.extend_from_slice(&(path.len() as u32).to_be_bytes());
        body.extend_from_slice(path);
        body.extend_from_slice(&0_u32.to_be_bytes()); // longname
        body.extend_from_slice(&0_u32.to_be_bytes()); // attrs
        packet(replies, &body);
    }

    fn file_attrs(replies: &mut Vec<u8>, id: u32) {
        let mut body = vec![105];
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&5_u32.to_be_bytes()); // size and permissions
        body.extend_from_slice(&3_u64.to_be_bytes());
        body.extend_from_slice(&0o100644_u32.to_be_bytes());
        packet(replies, &body);
    }

    fn resolve(replies: Vec<u8>) -> Result<Option<(Vec<u8>, u64)>> {
        let mut client = Client::new(Box::new(Cursor::new(replies)), Box::new(io::sink())).unwrap();
        remote(
            &mut client,
            b"/copy/alias",
            Attributes {
                permissions: Some(0o120777),
                ..Attributes::default()
            },
        )
    }

    #[test]
    fn remote_link_refuses_noncanonical_server_target() {
        let mut replies = Vec::new();
        packet(&mut replies, &[2, 0, 0, 0, 3]);
        realpath(&mut replies, 1, b"/copy/../outside/file");
        let error = resolve(replies).unwrap_err();
        assert!(error.message.contains("noncanonical link target"));
    }

    #[test]
    fn remote_link_refuses_retargeting_even_with_identical_file_metadata() {
        let mut replies = Vec::new();
        packet(&mut replies, &[2, 0, 0, 0, 3]);
        realpath(&mut replies, 1, b"/outside/original");
        file_attrs(&mut replies, 2); // LSTAT original target
        file_attrs(&mut replies, 3); // STAT link after it was retargeted
        realpath(&mut replies, 4, b"/outside/replacement");
        let error = resolve(replies).unwrap_err();
        assert_eq!(error.message, Error::SourceChanged.to_string());
    }
}
