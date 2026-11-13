use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use kortty_ssh::sftp::{FileKind, Sftp};

use super::files::{self, LocalStamp};
use super::worker::{Counted, Failure, Progress};
use super::{Location, parent};

pub(crate) mod local;
pub(super) mod ordinary_download;
#[cfg(unix)]
pub(crate) use ordinary_download::Stage as VerifiedTreeStage;
pub(super) mod privileged_download;
pub(super) mod single_file;
pub(super) mod snapshot;
#[cfg(test)]
mod tests;

pub(super) const MAX_DEPTH: usize = 32;
pub(super) const MAX_ENTRIES: usize = 20_000;
pub(super) const MAX_BYTES: u64 = 1 << 40;
const MAX_LISTING_PAYLOAD: usize = 2 * 1024 * 1024;

type Result<T> = std::result::Result<T, Failure>;

pub(crate) enum FolderTransfer<'a> {
    Upload { local: &'a Path, remote: &'a [u8] },
    Download { remote: &'a [u8], local: &'a Path },
}

pub(crate) enum FolderError {
    HardlinkRequired,
    IncompleteUpload,
    LocalStageRemains,
    LocalPublicationUncertain,
    TransferFailed,
}

/// Copies a folder for an unattended job. A remote upload claims a fresh
/// private directory and each complete file is published without replacement.
/// A local download becomes visible at its destination only after completion.
pub(crate) fn copy_scheduled(
    client: &mut Sftp<impl Read, impl Write>,
    transfer: FolderTransfer<'_>,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<String, FolderError> {
    if matches!(transfer, FolderTransfer::Upload { .. }) && !client.supports_hardlink() {
        return Err(FolderError::HardlinkRequired);
    }
    let progress = Progress::new(None);
    let mut tree = Tree::new(&progress, &cancelled, true);
    tree.transfer(client, transfer).map_err(|_| {
        if tree.remote_upload_started {
            FolderError::IncompleteUpload
        } else if tree.local_publication_uncertain {
            FolderError::LocalPublicationUncertain
        } else if tree.local_cleanup_failed {
            FolderError::LocalStageRemains
        } else {
            FolderError::TransferFailed
        }
    })
}

pub(super) fn copy(
    client: &mut Sftp<impl Read, impl Write>,
    source: &Location,
    target: &Location,
    progress: &Progress,
    cancelled: impl Fn() -> bool,
) -> Result<String> {
    let transfer = match (source, target) {
        (Location::Local(local), Location::Remote(remote)) => {
            FolderTransfer::Upload { local, remote }
        }
        (Location::Remote(remote), Location::Local(local)) => {
            FolderTransfer::Download { remote, local }
        }
        _ => return Err(io::Error::other("Invalid SFTP folder transfer direction").into()),
    };
    Tree::new(progress, &cancelled, false).transfer(client, transfer)
}

struct Tree<'a> {
    progress: &'a Progress,
    cancelled: &'a dyn Fn() -> bool,
    no_replace_publication: bool,
    remote_upload_started: bool,
    local_cleanup_failed: bool,
    local_cleanup_note: Option<String>,
    local_publication_uncertain: bool,
    entries: usize,
    files: usize,
    folders: usize,
    skipped: usize,
    bytes: u64,
}

impl<'a> Tree<'a> {
    fn new(
        progress: &'a Progress,
        cancelled: &'a dyn Fn() -> bool,
        no_replace_publication: bool,
    ) -> Self {
        Self {
            progress,
            cancelled,
            no_replace_publication,
            remote_upload_started: false,
            local_cleanup_failed: false,
            local_cleanup_note: None,
            local_publication_uncertain: false,
            entries: 0,
            files: 0,
            folders: 1,
            skipped: 0,
            bytes: 0,
        }
    }

    fn transfer(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        transfer: FolderTransfer<'_>,
    ) -> Result<String> {
        match transfer {
            FolderTransfer::Upload { local, remote } => self.upload(client, local, remote)?,
            FolderTransfer::Download { remote, local } => self.download(client, remote, local)?,
        }
        let skipped = if self.skipped == 0 {
            String::new()
        } else {
            format!(" · {} links or special entries skipped", self.skipped)
        };
        let cleanup = self.local_cleanup_note.as_deref().unwrap_or("");
        Ok(format!(
            "{} folders · {} files · {} bytes{skipped}{cleanup}",
            self.folders, self.files, self.bytes
        ))
    }

    fn check_cancelled(&self) -> Result<()> {
        if (self.cancelled)() {
            Err(Failure::disconnected(
                "Folder transfer cancelled or SFTP connection closed",
            ))
        } else {
            Ok(())
        }
    }

    fn reserve_entries(&mut self, count: usize) -> Result<()> {
        self.entries += count;
        if self.entries > MAX_ENTRIES {
            return Err(io::Error::other("SFTP folder exceeds 20,000 entries").into());
        }
        Ok(())
    }

    fn reserve_file(&mut self, size: u64) -> Result<()> {
        if size > MAX_BYTES - self.bytes {
            return Err(io::Error::other("SFTP folder exceeds 1 TiB").into());
        }
        self.bytes += size;
        self.files += 1;
        Ok(())
    }

    fn subfolder(&mut self, depth: usize) -> Result<usize> {
        if depth == MAX_DEPTH {
            return Err(io::Error::other("SFTP folder exceeds 32 nested levels").into());
        }
        self.folders += 1;
        Ok(depth + 1)
    }

    fn upload(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        source: &Path,
        target: &[u8],
    ) -> Result<()> {
        self.check_cancelled()?;
        let source_parent = local::Directory::open_path(parent(source)?)?;
        let local::Entry::Directory(source) = local::entry(&source_parent, basename(source)?)?
        else {
            return Err(
                io::Error::other("The local source is no longer an ordinary folder").into(),
            );
        };
        remote_directory(client, remote_parent(target)?, false)?;
        self.check_cancelled()?;
        if let Err(error) = client.mkdir_private(target) {
            let error = Failure::from(error);
            self.remote_upload_started = error.disconnected;
            return Err(if error.disconnected {
                partial_remote(error, target)
            } else {
                error
            });
        }
        self.remote_upload_started = true;
        self.upload_contents(client, &source, target, 0)
            .map_err(|error| partial_remote(error, target))
    }

    fn upload_contents(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        source: &local::Directory,
        target: &[u8],
        depth: usize,
    ) -> Result<()> {
        self.check_cancelled()?;
        remote_directory(client, target, true)?;
        let names = source.names(MAX_ENTRIES - self.entries)?;
        self.reserve_entries(names.len())?;
        for name in names {
            self.check_cancelled()?;
            let child = || files::remote_name(&name).map(|name| files::remote_join(target, &name));
            let result = match local::entry(source, &name)? {
                local::Entry::Directory(directory) => {
                    let depth = self.subfolder(depth)?;
                    let child = child()?;
                    remote_directory(client, target, true)?;
                    client.mkdir_private(&child)?;
                    self.upload_contents(client, &directory, &child, depth)
                }
                local::Entry::File(file) => {
                    let child = child()?;
                    remote_directory(client, target, true)?;
                    self.upload_file(client, &file, target, &child)
                }
                local::Entry::Skip => {
                    self.skipped += 1;
                    continue;
                }
            };
            result.map_err(|error| {
                error.context(format!(" · {}", files::display(&name.to_string_lossy())))
            })?;
        }
        remote_directory(client, target, true)
    }

    fn upload_file(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        file: &File,
        parent: &[u8],
        target: &[u8],
    ) -> Result<()> {
        let stamp = LocalStamp::capture(&file.metadata()?);
        self.reserve_file(stamp.size)?;
        let staging = files::remote_join(parent, files::nonce()?.as_bytes());
        let count = client.upload_new(&staging, Counted(self.progress, file.take(stamp.size)))?;
        if count != stamp.size || LocalStamp::capture(&file.metadata()?) != stamp {
            return Err(io::Error::other("Local source changed during folder upload").into());
        }
        self.check_cancelled()?;
        remote_directory(client, parent, true)?;
        if self.no_replace_publication {
            let attrs = client.lstat(&staging)?;
            if attrs.kind() != FileKind::File
                || attrs.size != Some(stamp.size)
                || attrs.permissions.is_none_or(|mode| mode & 0o7777 != 0o600)
                || client.canonicalize(&staging)? != staging
            {
                return Err(io::Error::other("Could not verify the private staged upload").into());
            }
            self.check_cancelled()?;
            client.hardlink(&staging, target)?;
            client.remove_file(&staging)?;
        } else {
            client.rename(&staging, target)?;
        }
        Ok(())
    }

    fn download(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        source: &[u8],
        target: &Path,
    ) -> Result<()> {
        if self.no_replace_publication {
            return self.download_ordinary(client, source, target);
        }
        self.check_cancelled()?;
        remote_directory(client, source, false)?;
        let path = parent(target)?;
        let parent = local::Directory::open_path(path)?;
        let name = files::nonce()?;
        let name = OsStr::new(&name);
        let stage = parent.create_child(name)?;
        let result = (|| {
            self.download_contents(client, source, &stage, 0, None)?;
            self.check_cancelled()?;
            if !parent.same_as(&local::Directory::open_path(path)?)? {
                return Err(io::Error::other("The local destination folder changed").into());
            }
            local::publish(&parent, name, &stage, basename(target)?)?;
            Ok(())
        })();
        if let Err(error) = result {
            return Err(match local::discard(&parent, name, stage) {
                Ok(()) => error,
                Err(cleanup) => {
                    self.local_cleanup_failed = true;
                    error.context(format!(
                        " · could not remove local staging folder {} ({cleanup})",
                        files::display(&path.join(name).to_string_lossy())
                    ))
                }
            });
        }
        Ok(())
    }

    fn download_contents(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        source: &[u8],
        target: &local::Directory,
        depth: usize,
        barrier: Option<&local::Directory>,
    ) -> Result<()> {
        self.check_cancelled()?;
        remote_directory(client, source, false)?;
        let entries =
            client.read_dir_limited(source, MAX_ENTRIES - self.entries + 2, MAX_LISTING_PAYLOAD)?;
        self.reserve_entries(entries.len())?;
        let mut names = HashSet::with_capacity(entries.len());
        for entry in &entries {
            if !names.insert(entry.name.as_slice()) {
                return Err(io::Error::other("Server returned a duplicate folder entry").into());
            }
        }
        for entry in entries {
            self.check_cancelled()?;
            files::validate_transfer_name(&entry.name)?;
            let child = files::remote_join(source, &entry.name);
            remote_directory(client, source, false)?;
            let attrs = client.lstat(&child)?;
            match attrs.kind() {
                FileKind::Directory => {
                    let depth = self.subfolder(depth)?;
                    let name = files::local_name(&entry.name)?;
                    let folder = if barrier.is_some() {
                        target.create_transfer_child(&name)?
                    } else {
                        target.create_child(&name)?
                    };
                    self.download_contents(client, &child, &folder, depth, barrier)?;
                }
                FileKind::File => {
                    let size = attrs
                        .size
                        .ok_or_else(|| io::Error::other("Server did not report a file size"))?;
                    self.reserve_file(size)?;
                    let name = files::local_name(&entry.name)?;
                    let file = if barrier.is_some() {
                        target.create_ordinary_file(&name)?
                    } else {
                        target.create_file(&name)?
                    };
                    client.download_regular(
                        &child,
                        attrs,
                        Counted(
                            self.progress,
                            LimitedOutput {
                                file: &file,
                                remaining: size,
                                cancelled: self.cancelled,
                                barrier,
                            },
                        ),
                    )?;
                    file.sync_all()?;
                }
                FileKind::Symlink | FileKind::Special => self.skipped += 1,
                FileKind::Unknown => {
                    return Err(
                        io::Error::other("Server did not report a folder entry type").into(),
                    );
                }
            }
        }
        remote_directory(client, source, false)
    }
}

struct LimitedOutput<'a> {
    file: &'a File,
    remaining: u64,
    cancelled: &'a dyn Fn() -> bool,
    barrier: Option<&'a local::Directory>,
}

impl Write for LimitedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Some(barrier) = self.barrier {
            ordinary_download::require_private(barrier)?;
        }
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("Remote file exceeded its reported size"));
        }
        let count = self.file.write(bytes)?;
        self.remaining -= count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn basename(path: &Path) -> io::Result<&OsStr> {
    path.file_name()
        .ok_or_else(|| io::Error::other("Expected a local folder name"))
}

fn remote_parent(path: &[u8]) -> io::Result<&[u8]> {
    let slash = path
        .iter()
        .rposition(|&byte| byte == b'/')
        .filter(|&slash| slash + 1 < path.len() && path.starts_with(b"/"))
        .ok_or_else(|| io::Error::other("Expected an absolute remote folder path"))?;
    Ok(&path[..slash.max(1)])
}

fn remote_directory(
    client: &mut Sftp<impl Read, impl Write>,
    path: &[u8],
    private: bool,
) -> Result<()> {
    let attrs = client.lstat(path)?;
    if attrs.kind() != FileKind::Directory
        || client.canonicalize(path)? != path
        || private && attrs.permissions.is_none_or(|mode| mode & 0o777 != 0o700)
    {
        return Err(
            io::Error::other("Could not verify the remote folder path or permissions").into(),
        );
    }
    Ok(())
}

fn partial_remote(error: Failure, target: &[u8]) -> Failure {
    let target = files::display(&String::from_utf8_lossy(target));
    error.context(format!(" · partial remote folder may remain at {target}"))
}
