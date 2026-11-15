//! Replace a reviewed local file after either rsync download transport verifies its bytes.

use kortty_jobs::RunId;
use kortty_server_archive::privileged_rsync::{Review, SourceLimits};

use crate::sftp_window::{ReviewedFilePublication, ReviewedFileReceiver, scheduled::Client};

use super::*;

pub(super) fn run(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    protected: Option<&Review>,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let approval = action
        .approval
        .as_ref()
        .and_then(|approval| approval.existing_file_download.as_deref())
        .filter(|approval| approval.destination == action.local)
        .ok_or_else(unavailable)?;
    let guard = context.guard.as_ref().ok_or_else(unavailable)?;
    let checked = Cell::new((Instant::now(), true));
    let stopped = &|| stopped() || authority_revoked(&checked, guard);
    let authorize = || {
        check_cancelled(context.cancellation)?;
        require_authority(Some(guard))
    };
    authorize()?;
    action
        .require_safe_existing_local_program()
        .map_err(Failure::blocked)?;
    let snapshot = match context.recurring {
        Some(route) => route.local_file(&approval.destination)?,
        None => approval.snapshot,
    };
    let receiver = ReviewedFileReceiver::prepare(&approval.destination, snapshot, stopped)
        .map_err(|_| unavailable())?;
    let stage = receiver
        .stage()
        .map_err(|_| Failure::failed("Could not create the private local rsync file stage."))?;
    let output = stage
        .writer()
        .and_then(File::try_clone)
        .map_err(|_| Failure::failed("Could not open the private local rsync file stage."))?;
    let limits = context
        .recurring
        .map(recurring::Route::source_limits)
        .transpose()?
        .unwrap_or(SourceLimits::MAXIMUM);
    if limits.entries == 0 {
        return Err(Failure::blocked(
            "The recurring rsync firing has no file entries remaining.",
        ));
    }
    let limits = SourceLimits {
        entries: 1,
        bytes: limits.bytes,
    };
    let (path, manifest, control) = match protected {
        Some(approved) => {
            let current;
            let review = if let Some(route) = context.recurring {
                current = privileged::fresh_source(context, approved, limits, stopped)?.0;
                route.reserve([current.inventory.ok_or_else(unavailable)?])?;
                &current
            } else {
                approved
            };
            let mut control = privileged::Control::from_context(context)?;
            let (path, manifest) = control.prepare_source(client, review, stopped)?;
            (path, manifest, Some(control))
        }
        None => {
            let manifest = tree::Manifest::read_limited(
                client,
                &action.remote,
                tree::Limits {
                    entries: 1,
                    bytes: limits.bytes,
                },
                stopped,
            )?;
            if manifest.is_directory() {
                return Err(Failure::blocked(
                    "Choose an ordinary server file for the rsync download.",
                ));
            }
            if let Some(route) = context.recurring {
                route.reserve([manifest.inventory()?])?;
            }
            (action.remote.clone(), manifest, None)
        }
    };
    let inventory = manifest.inventory()?;
    if manifest.is_directory() || !limits.contains(inventory) {
        return Err(Failure::blocked(
            "The checked rsync file exceeds the allowed transfer scope.",
        ));
    }
    let mut source = Cow::Borrowed(action);
    if source.remote != path {
        source.to_mut().remote = path;
    }
    let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
    tree::receive_projected_file(
        client,
        context.transfer(&source, &temp, stopped),
        listener,
        &endpoint,
        &manifest,
        output,
    )?;
    if let Some(mut control) = control {
        control.finish_read(inventory, stopped)?;
    }
    authorize()?;
    let permit = || {
        authorize().map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.detail))
    };
    let arm = |removed| {
        context
            .recurring
            .map_or(Ok(()), |route| route.arm(removed))
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.detail))
    };
    let publication = match receiver.publish(stage, inventory, stopped, &permit, arm) {
        Ok(publication) => publication,
        Err(error) => {
            authorize()?;
            require_running(stopped).map_err(|_| transfer_stopped())?;
            return Ok(publication_failure(
                id,
                Failure::blocked("Could not replace the reviewed local rsync file."),
                &error,
            ));
        }
    };
    Ok(match publication {
        ReviewedFilePublication::Published {
            successor,
            recovery,
        } => {
            let mut completion = Completion::from_result(id, Ok(0));
            completion.append_detail(&format!(
                "Published {} verified bytes. The original is retained at {}.",
                inventory.bytes,
                recovery.display()
            ));
            match context.recurring {
                Some(route) => route.confirm(
                    completion,
                    Some(rsync::standing::Receiver::LocalFile {
                        destination: approval.destination.clone(),
                        snapshot: successor,
                    }),
                ),
                None => completion,
            }
        }
        ReviewedFilePublication::Restored { error } => publication_failure(
            id,
            Failure::failed("The original rsync file was restored."),
            &error,
        ),
        ReviewedFilePublication::Uncertain { error, recovery } => {
            let mut completion = publication_failure(
                id,
                Failure::failed(
                    "Could not confirm the local rsync publication. Review the destination before retrying.",
                ),
                &error,
            );
            if let Some(recovery) = recovery {
                completion.append_detail(&format!(
                    "The original is retained at {}.",
                    recovery.display()
                ));
            }
            completion
        }
    })
}

fn publication_failure(id: RunId, failure: Failure, error: &io::Error) -> Completion {
    let mut completion = Completion::from_result(id, Err(failure));
    completion.append_detail(&error.to_string());
    completion
}

fn unavailable() -> Failure {
    Failure::blocked("The reviewed local rsync file changed or is unavailable.")
}
