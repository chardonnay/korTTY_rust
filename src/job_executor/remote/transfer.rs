use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use kortty_jobs::Cancellation;
use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};

use crate::scheduler_window::RemotePath;
use crate::sftp_window::{
    FolderError, FolderTransfer, LocalSource, OrdinaryDownload, copy_scheduled_folder, staging_name,
};

use super::{Failure, before_change, verify_directory, verify_entry};

const MAX_FILE_BYTES: u64 = 1 << 30;
pub(super) const LOCAL_PUBLICATION_UNCERTAIN: &str =
    "Local folder publication could not be confirmed. Inspect the destination before retrying.";

mod copy_tree;

pub(super) enum FolderRemains {
    RemotePartial,
    RemoteCopyPartial,
    LocalStage,
    LocalPublished,
    LocalPublicationUncertain,
}

pub(super) fn folder(
    client: &mut Sftp<impl Read, impl Write>,
    transfer: FolderTransfer<'_>,
    cancellation: &Cancellation,
    remains: &mut Option<FolderRemains>,
) -> Result<String, Failure> {
    let local_download = matches!(transfer, FolderTransfer::Download { .. });
    let result = copy_scheduled_folder(client, transfer, || cancellation.is_requested());
    *remains = match result {
        Err(FolderError::IncompleteUpload) => Some(FolderRemains::RemotePartial),
        Err(FolderError::LocalStageRemains) => Some(FolderRemains::LocalStage),
        Err(FolderError::LocalPublicationUncertain) => {
            Some(FolderRemains::LocalPublicationUncertain)
        }
        Ok(_) if local_download => Some(FolderRemains::LocalPublished),
        _ => None,
    };
    match result {
        Ok(detail) => Ok(detail),
        Err(FolderError::LocalPublicationUncertain) => {
            Err(Failure::failed(LOCAL_PUBLICATION_UNCERTAIN))
        }
        Err(error) => {
            before_change(cancellation)?;
            Err(match error {
                FolderError::HardlinkRequired => Failure::blocked(
                    "The SFTP server does not advertise hard-link support for scheduled folder uploads",
                ),
                _ => Failure::failed("The scheduled SFTP folder transfer failed"),
            })
        }
    }
}

pub(super) fn upload(
    client: &mut Sftp<impl Read, impl Write>,
    local: &Path,
    remote: &RemotePath,
    cancellation: &Cancellation,
) -> Result<String, Failure> {
    before_change(cancellation)?;
    let source = LocalSource::open(local)
        .map_err(|_| Failure::failed("Could not open the ordinary local upload source"))?;
    let size = checked_size(Some(source.size()))?;
    let cleaned = publish_remote(
        client,
        remote,
        size,
        cancellation,
        |client, payload| {
            let count = client.upload_new(
                payload,
                CheckedInput(source.file().take(size), cancellation),
            );
            before_change(cancellation)?;
            let count =
                count.map_err(|_| Failure::failed("SFTP could not write the staged upload"))?;
            if count != size {
                return Err(Failure::failed(
                    "The local upload source changed during transfer",
                ));
            }
            Ok(())
        },
        |_| {
            source
                .verify()
                .map_err(|_| Failure::failed("The local upload source changed during transfer"))
        },
        |client, from, to| client.rename(from, to),
    )?;
    Ok(published_detail(
        format!("Uploaded {size} bytes via SFTP"),
        cleaned,
    ))
}

pub(super) fn copy_remote(
    client: &mut Sftp<impl Read, impl Write>,
    from: &RemotePath,
    to: &RemotePath,
    cancellation: &Cancellation,
    remains: &mut Option<FolderRemains>,
) -> Result<String, Failure> {
    if from.contains(to) {
        return Err(Failure::blocked(
            "Choose a remote copy destination outside the source path",
        ));
    }
    if !client.supports_copy_data() {
        return Err(Failure::blocked(
            "The SFTP server does not advertise server-side copy-data",
        ));
    }
    if !client.supports_hardlink() {
        return Err(Failure::blocked(
            "The SFTP server does not advertise no-replace hard-link publication",
        ));
    }
    before_change(cancellation)?;
    let attributes = verify_entry(client, from.as_str())?;
    if attributes.kind() == FileKind::Directory {
        return copy_tree::run(client, from, to, attributes, cancellation, remains);
    }
    let size = checked_size(attributes.size)?;
    let cleaned = copy_remote_file(client, from, to, attributes, size, cancellation)?;
    Ok(published_detail(
        format!("Copied {size} bytes on the server via SFTP"),
        cleaned,
    ))
}

fn copy_remote_file(
    client: &mut Sftp<impl Read, impl Write>,
    from: &RemotePath,
    to: &RemotePath,
    attributes: Attributes,
    size: u64,
    cancellation: &Cancellation,
) -> Result<bool, Failure> {
    publish_remote(
        client,
        to,
        size,
        cancellation,
        |client, payload| {
            let copied = client.copy_regular_new(
                from.as_str().as_bytes(),
                attributes,
                payload.as_bytes(),
                || cancellation.is_requested(),
            );
            before_change(cancellation)?;
            copied.map_err(|_| {
                Failure::failed("SFTP could not complete the checked server-side copy")
            })?;
            Ok(())
        },
        |client| verify_unchanged(client, from, attributes),
        // OpenSSH's standard RENAME can fall back to a racy stat+rename
        // on filesystems without link support. A hard link fails closed there.
        |client, from, to| client.hardlink(from, to),
    )
}

pub(super) fn published_detail(mut detail: String, cleaned: bool) -> String {
    if !cleaned {
        detail.push_str(". A private staging folder may remain.");
    }
    detail
}

pub(in crate::job_executor) fn publish_remote<R: Read, W: Write>(
    client: &mut Sftp<R, W>,
    remote: &RemotePath,
    size: u64,
    cancellation: &Cancellation,
    write_stage: impl FnOnce(&mut Sftp<R, W>, &str) -> Result<(), Failure>,
    verify_source: impl FnOnce(&mut Sftp<R, W>) -> Result<(), Failure>,
    publish: impl FnOnce(&mut Sftp<R, W>, &str, &str) -> Result<(), Error>,
) -> Result<bool, Failure> {
    let parent = remote.parent();
    verify_new_destination(client, remote)?;
    let nonce =
        staging_name().map_err(|_| Failure::failed("Could not choose a private staging path"))?;
    let stage = format!("{}/{nonce}", parent.trim_end_matches('/'));
    let payload = format!("{stage}/payload");
    before_change(cancellation)?;
    client
        .mkdir_private(&stage)
        .map_err(|_| Failure::failed("Could not create a private remote staging folder"))?;
    let original = private_directory(client, &stage)
        .map_err(|_| Failure::failed("Could not verify the new remote staging folder"))?;
    let result = (|| {
        before_change(cancellation)?;
        write_stage(client, &payload)?;
        verify_directory(client, parent)?;
        let current = private_directory(client, &stage)?;
        let file = verify_entry(client, &payload)?;
        if !same_owner(current, original)
            || !private_file(file, original)
            || file.size != Some(size)
        {
            return Err(Failure::failed("The staged remote file changed"));
        }
        verify_source(client)?;
        before_change(cancellation)?;
        publish(client, &payload, remote.as_str())
            .map_err(|_| Failure::failed("Could not publish the remote transfer to a new file"))
    })();
    let cleaned = !cancellation.is_requested() && cleanup(client, &stage, &payload, original);
    match result {
        Ok(()) => Ok(cleaned),
        Err(error) if cleaned || cancellation.is_requested() => Err(error),
        Err(_) => Err(Failure::failed(
            "SFTP transfer failed. A private staging folder may remain.",
        )),
    }
}

pub(super) fn publish_bytes(
    client: &mut Sftp<impl Read, impl Write>,
    remote: &RemotePath,
    bytes: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, Failure> {
    publish_remote(
        client,
        remote,
        bytes.len() as u64,
        cancellation,
        |client, payload| {
            let count = client.upload_new(payload, CheckedInput(bytes, cancellation));
            before_change(cancellation)?;
            if count.ok() != Some(bytes.len() as u64) {
                return Err(Failure::failed(
                    "Could not write the private staged archive",
                ));
            }
            Ok(())
        },
        |_| Ok(()),
        |client, from, to| client.hardlink(from, to),
    )
}

pub(super) fn verify_new_destination(
    client: &mut Sftp<impl Read, impl Write>,
    remote: &RemotePath,
) -> Result<(), Failure> {
    verify_directory(client, remote.parent())?;
    match client.lstat(remote.as_str()) {
        Ok(_) => Err(Failure::failed(
            "The remote transfer destination already exists",
        )),
        Err(error) if error.is_not_found() => Ok(()),
        Err(_) => Err(Failure::failed(
            "Could not check the remote transfer destination",
        )),
    }
}

pub(super) fn download(
    client: &mut Sftp<impl Read, impl Write>,
    remote: &RemotePath,
    local: &Path,
    cancellation: &Cancellation,
) -> Result<String, Failure> {
    before_change(cancellation)?;
    let attributes = verify_entry(client, remote.as_str())?;
    if attributes.kind() != FileKind::File {
        return Err(Failure::failed(
            "The remote download source must be a regular file",
        ));
    }
    let size = checked_size(attributes.size)?;
    let stage = OrdinaryDownload::create(local).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            Failure::failed("The local download destination already exists")
        } else {
            Failure::failed("Could not create a private local download stage")
        }
    })?;
    let file = stage
        .writer()
        .map_err(|_| Failure::failed("The local download stage is unavailable"))?;
    let count = client.download_regular(
        remote.as_str().as_bytes(),
        attributes,
        BoundedOutput {
            file,
            remaining: size,
            cancellation,
        },
    );
    before_change(cancellation)?;
    let count = count.map_err(|_| Failure::failed("Could not download the checked remote file"))?;
    if count != size {
        return Err(Failure::failed(
            "The remote file size changed during download",
        ));
    }
    verify_unchanged(client, remote, attributes)?;
    if let Err(error) = stage.publish(|| cancellation.is_requested()) {
        before_change(cancellation)?;
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            Failure::failed("The local download destination already exists")
        } else {
            Failure::failed("Could not publish the local download to a new file")
        });
    }
    Ok(format!("Downloaded {count} bytes via SFTP"))
}

pub(super) fn verify_unchanged(
    client: &mut Sftp<impl Read, impl Write>,
    path: &RemotePath,
    expected: Attributes,
) -> Result<(), Failure> {
    let current = verify_entry(client, path.as_str())?;
    if (Attributes {
        accessed: None,
        ..current
    }) != (Attributes {
        accessed: None,
        ..expected
    }) {
        return Err(Failure::failed(
            "The source file changed during the SFTP transfer",
        ));
    }
    Ok(())
}

fn checked_size(size: Option<u64>) -> Result<u64, Failure> {
    let size = size.ok_or_else(|| Failure::failed("The SFTP file size is unavailable"))?;
    if size > MAX_FILE_BYTES {
        return Err(Failure::blocked(
            "A scheduled SFTP file may be at most 1 GiB",
        ));
    }
    Ok(size)
}

fn private_directory(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
) -> Result<Attributes, Failure> {
    let attrs = verify_entry(client, path)?;
    if attrs.kind() == FileKind::Directory
        && attrs.permissions.map(|mode| mode & 0o7777) == Some(0o700)
        && attrs.uid.is_some()
        && attrs.gid.is_some()
    {
        Ok(attrs)
    } else {
        Err(Failure::failed("The remote staging folder must be private"))
    }
}

fn same_owner(current: Attributes, original: Attributes) -> bool {
    current.uid == original.uid && current.gid == original.gid
}

fn private_file(file: Attributes, owner: Attributes) -> bool {
    file.kind() == FileKind::File
        && same_owner(file, owner)
        && file.permissions.map(|mode| mode & 0o7777) == Some(0o600)
}

fn cleanup(
    client: &mut Sftp<impl Read, impl Write>,
    stage: &str,
    payload: &str,
    original: Attributes,
) -> bool {
    if !private_directory(client, stage).is_ok_and(|actual| same_owner(actual, original)) {
        return false;
    }
    match client.lstat(payload) {
        Ok(attrs) if private_file(attrs, original) => {
            if client.remove_file(payload).is_err() {
                return false;
            }
        }
        Err(error) if error.is_not_found() => {}
        _ => return false,
    }
    client.remove_dir(stage).is_ok()
}

struct CheckedInput<'a, T>(T, &'a Cancellation);

impl<T: Read> Read for CheckedInput<'_, T> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_transfer(self.1)?;
        self.0.read(output)
    }
}

struct BoundedOutput<'a> {
    file: &'a File,
    remaining: u64,
    cancellation: &'a Cancellation,
}

impl Write for BoundedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check_transfer(self.cancellation)?;
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "Remote download exceeded its reported size",
            ));
        }
        let count = self.file.write(bytes)?;
        self.remaining -= count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(super) fn check_transfer(cancellation: &Cancellation) -> io::Result<()> {
    if cancellation.is_requested() {
        Err(io::Error::other("Scheduled SFTP transfer cancelled"))
    } else {
        Ok(())
    }
}
