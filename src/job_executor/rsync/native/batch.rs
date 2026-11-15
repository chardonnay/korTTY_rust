//! Preflight all members on one pinned session and report confirmed publications.

use kortty_jobs::RunId;

use crate::sftp_window::scheduled::Client;

use super::*;

pub(super) enum Source {
    File(Attributes),
    Directory(tree::Manifest),
}

pub(super) fn preflight(
    client: &mut Client,
    members: &[Cow<'_, rsync::Transfer>],
    context: &TransferContext<'_>,
    uploading: bool,
    stopped: &impl Fn() -> bool,
) -> Result<Vec<Option<Source>>, Failure> {
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let mut sources = Vec::with_capacity(members.len());
    for member in members {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        require_authority(context.guard.as_ref())?;
        let source = if uploading {
            verify_upload(client, member, stopped)?;
            None
        } else if member.directory {
            let inventory = tree::Manifest::read(client, &member.remote, stopped)?;
            bytes = bytes.saturating_add(inventory.bytes());
            // The manifest includes the root folder, like the aggregate entry limit.
            entries = entries.saturating_add(inventory.entries());
            Some(Source::Directory(inventory))
        } else {
            let attrs = remote::verify_entry(client, member.remote.as_str())?;
            bytes = bytes.saturating_add(checked_size(attrs)?);
            entries += 1;
            Some(Source::File(attrs))
        };
        if bytes > MAX_FILE || entries > rsync::MAX_TREE_ENTRIES {
            return Err(Failure::blocked(
                "The rsync batch exceeds 256 MiB or 4,096 entries.",
            ));
        }
        sources.push(source);
    }
    Ok(sources)
}

fn verify_upload(
    client: &mut Client,
    member: &rsync::Transfer,
    stopped: &impl Fn() -> bool,
) -> Result<(), Failure> {
    remote::transfer::verify_new_destination(client, &member.remote)?;
    let approval = member
        .approval
        .as_ref()
        .ok_or_else(|| Failure::blocked("Review the rsync batch contents."))?;
    let changed = || Failure::blocked("An rsync batch source changed or is unavailable.");
    if let Some(review) = &approval.directory_upload {
        let inventory =
            upload_tree::read_tree(&member.local, None, stopped).map_err(|_| changed())?;
        if inventory != review.inventory {
            return Err(changed());
        }
    } else {
        if !client.supports_hardlink() {
            return Err(Failure::blocked(
                "The SFTP server does not support no-replace publication.",
            ));
        }
        let review = approval.upload.as_ref().ok_or_else(changed)?;
        let digest = upload::checked_digest(&member.local, stopped).map_err(|_| changed())?;
        if digest != (review.bytes, review.digest) {
            return Err(changed());
        }
    }
    Ok(())
}

pub(super) fn run(
    client: &mut Client,
    context: &TransferContext<'_>,
    members: Vec<Cow<'_, rsync::Transfer>>,
    destinations: Vec<Destination>,
    sources: Vec<Option<Source>>,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Completion {
    let total = members.len();
    let mut confirmed = Vec::with_capacity(total);
    for ((member, destination), source) in members.into_iter().zip(destinations).zip(sources) {
        match run_one(client, context, &member, destination, source, stopped) {
            Ok(detail) => confirmed.push(detail),
            Err(error) => {
                let count = confirmed.len();
                let cancelled = check_cancelled(context.cancellation);
                let outcome = if error.detail == upload_tree::UNCERTAIN {
                    Err(error)
                } else {
                    cancelled.and(Err(error))
                };
                let mut result = Completion::from_result(id, outcome);
                result.append_detail(&format!(
                    "Confirmed {count} of {total} rsync publications. Check destination {} before retrying.",
                    count + 1
                ));
                return result;
            }
        }
    }
    let mut result = Completion::from_result(id, Ok(0));
    result.append_detail(&format!("Published all {total} reviewed rsync items."));
    for detail in confirmed {
        result.append_detail(&detail);
    }
    result
}
