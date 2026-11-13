//! Send one approved file to a private server stage, then publish by hard link.

use std::cell::Cell;
use std::io::Seek;

use kortty_ssh::sftp::Error;

use crate::scheduler_window::ConnectionRef;
use crate::sftp_window::LocalSource;
use crate::sftp_window::scheduled::{CheckedRemoteFolder, Client};

use super::*;

pub(crate) fn digest(path: &Path) -> io::Result<(u64, [u8; 32])> {
    copy_reviewed(&LocalSource::open(path)?, io::sink(), &|| false)
}

pub(super) fn apply<C: Fn() -> bool>(
    client: &mut Client,
    transfer: Transfer<'_, C>,
    listener: UnixListener,
    socket: &Path,
    reference: &ConnectionRef,
) -> Result<String, Failure> {
    let action = transfer.action;
    let stopped = transfer.stopped;
    let cancellation = transfer.cancellation.clone();
    let review = action
        .approval
        .as_ref()
        .and_then(|approval| approval.upload.as_ref())
        .ok_or_else(|| Failure::blocked("Review and approve the exact rsync upload first."))?;
    if !client.supports_hardlink() {
        return Err(Failure::blocked(
            "The SFTP server does not support no-replace publication.",
        ));
    }
    authorize(reference)?;
    let source = LocalSource::open(&action.local)
        .map_err(|_| Failure::blocked("The reviewed local rsync source is unavailable."))?;
    let local_error = || Failure::failed("Could not verify the private local rsync source.");
    let staging_file = transfer
        .temp
        .directory()
        .and_then(|directory| directory.create_file(OsStr::new(PAYLOAD)))
        .map_err(|_| local_error())?;
    let copied = copy_reviewed(&source, &staging_file, stopped).map_err(|_| {
        Failure::blocked("The reviewed local rsync source changed or exceeded the size limit.")
    })?;
    if copied != (review.bytes, review.digest) {
        return Err(Failure::blocked(
            "The reviewed local rsync source changed. Review it again.",
        ));
    }
    staging_file.sync_all().map_err(|_| local_error())?;
    drop(staging_file);
    let mut staged = transfer.temp.payload().map_err(|_| local_error())?;
    let stamp = FileStamp::read(&staged).map_err(|_| local_error())?;
    let parent =
        CheckedRemoteFolder::read(client, action.remote.parent().as_bytes().to_vec(), stopped)
            .map_err(|_| Failure::blocked("The rsync destination folder could not be verified."))?;
    if !parent.trusted(parent.owner()) {
        return Err(Failure::blocked(
            "Choose a protected server destination folder.",
        ));
    }
    let current_parent = |client: &mut Client| {
        parent
            .verify(client, false)
            .map_err(|_| Failure::failed("The rsync destination folder changed."))
    };
    let checked = Cell::new(None);
    authorize(reference)?;
    let cleaned = remote::transfer::publish_remote(
        client,
        &action.remote,
        review.bytes,
        &cancellation,
        |client, payload| {
            current_parent(client)?;
            let stage = payload
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .ok_or_else(|| Failure::failed("The private server stage is invalid."))?;
            if remote::verify_entry(client, stage)?.uid != Some(parent.owner()) {
                return Err(Failure::blocked(
                    "The upload stage and destination folder have different owners.",
                ));
            }
            authorize(reference)?;
            transfer.run(listener, socket, payload, true)?;
            current_parent(client)?;
            let attrs = remote::verify_entry(client, payload)?;
            if attrs.kind() != FileKind::File
                || attrs.size != Some(review.bytes)
                || attrs.permissions.map(|mode| mode & 0o7777) != Some(0o600)
                || attrs.uid != Some(parent.owner())
            {
                return Err(Failure::failed(
                    "The server rsync result is not the reviewed private file.",
                ));
            }
            let sink = VerifyAndCopy {
                source: &mut staged,
                output: io::sink(),
                cancelled: stopped,
            };
            let received = client
                .download_regular(payload.as_bytes(), attrs, sink)
                .map_err(|_| {
                    Failure::failed("The server rsync result differs from the reviewed file.")
                })?;
            if received != review.bytes {
                return Err(Failure::failed("The server rsync result is incomplete."));
            }
            stamp
                .require_unchanged(&staged)
                .map_err(|_| local_error())?;
            checked.set(Some(attrs));
            Ok(())
        },
        |client| {
            current_parent(client)?;
            source
                .verify()
                .map_err(|_| Failure::blocked("The reviewed local rsync source changed."))?;
            authorize(reference)?;
            require_running(stopped).map_err(|_| transfer_stopped())
        },
        |client, from, to| {
            let expected = checked
                .get()
                .ok_or(Error::Protocol("The reviewed rsync stage is unavailable"))?;
            if !same_metadata(expected, client.lstat(from)?) {
                return Err(Error::Protocol(
                    "The reviewed rsync stage changed before publication",
                ));
            }
            client.hardlink(from, to)
        },
    )?;
    let mut detail = format!(
        "Uploaded {} verified bytes via native rsync and SSH.",
        review.bytes
    );
    if !cleaned {
        detail.push_str(" A private server staging folder may remain.");
    }
    Ok(detail)
}

fn same_metadata(left: Attributes, right: Attributes) -> bool {
    Attributes {
        accessed: None,
        ..left
    } == Attributes {
        accessed: None,
        ..right
    }
}

fn copy_reviewed(
    source: &LocalSource,
    mut output: impl Write,
    stopped: &impl Fn() -> bool,
) -> io::Result<(u64, [u8; 32])> {
    let size = source.size();
    if size > MAX_FILE {
        return Err(io::Error::other(
            "Rsync uploads support regular files up to 256 MiB.",
        ));
    }
    let file = source.file();
    let stamp = FileStamp::read(file)?;
    let mut input = file;
    input.rewind()?;
    let mut digest = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = [0; 32 * 1024];
    let mut remaining = size;
    while remaining != 0 {
        require_running(stopped)?;
        let length = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..length])?;
        digest.update(&buffer[..length]);
        output.write_all(&buffer[..length])?;
        remaining -= length as u64;
    }
    require_running(stopped)?;
    if input.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::other("The reviewed rsync source changed."));
    }
    stamp.require_unchanged(file)?;
    source.verify()?;
    Ok((
        size,
        digest
            .finalize()
            .as_ref()
            .try_into()
            .expect("SHA-256 produces 32 bytes"),
    ))
}
