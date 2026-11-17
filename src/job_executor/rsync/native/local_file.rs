//! Publish a reviewed local file after either rsync download transport verifies its bytes.

use kortty_jobs::RunId;
use kortty_server_archive::privileged_rsync::{Review, SourceLimits};

use crate::job_executor::rsync::VacantFileProof;
use crate::sftp_window::{
    ReviewedFilePublication, ReviewedFileReceiver, VacantLocalFilePublication,
    VacantLocalFileReceiver, scheduled::Client,
};

use super::local_existing::{
    MissingLocalFilePublication, MissingLocalFileReceiver, MissingLocalFileStage,
};
use super::recurring::LocalFile;
use super::*;

enum StagedReceiver {
    Existing(Box<ReviewedFileReceiver>, StagedDownload),
    Vacant(Box<VacantLocalFileReceiver>, StagedDownload),
    MissingParents(Box<MissingLocalFileReceiver>, MissingLocalFileStage),
}

enum Publication {
    Existing(ReviewedFilePublication),
    Vacant(VacantLocalFilePublication),
    MissingParents(MissingLocalFilePublication),
}

pub(super) fn run(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    protected: Option<&Review>,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let approval = action.approval.as_ref().ok_or_else(unavailable)?;
    let guard = context.guard.as_ref().ok_or_else(unavailable)?;
    let checked = Cell::new((Instant::now(), true));
    let stopped = &|| stopped() || authority_revoked(&checked, guard);
    let authorize = || {
        check_cancelled(context.cancellation)?;
        require_authority(Some(guard))
    };
    authorize()?;
    let create = action.uses_reviewed_file_creation(false);
    let (destination, reviewed) = match (
        approval.vacant_file_download.as_deref(),
        approval.existing_file_download.as_deref(),
    ) {
        (Some(review), None) if create => (&review.destination, LocalFile::Vacant(review.snapshot)),
        (None, Some(review)) => (&review.destination, LocalFile::Existing(review.snapshot)),
        _ => return Err(unavailable()),
    };
    if destination != &action.local {
        return Err(unavailable());
    }
    if create {
        action
            .require_safe_vacant_local_program()
            .map_err(Failure::blocked)?;
    } else {
        action
            .require_safe_existing_local_program()
            .map_err(Failure::blocked)?;
    }
    let snapshot = match (context.recurring, reviewed) {
        (Some(route), reviewed) => route.local_file(destination, reviewed)?,
        (None, reviewed) if action.standing.is_none() => reviewed,
        (None, reviewed @ LocalFile::Existing(_)) if !create => reviewed,
        _ => return Err(unavailable()),
    };
    let stage_failed = |_| Failure::failed("Could not create the private local rsync file stage.");
    let receiver = match snapshot {
        LocalFile::Existing(snapshot) => {
            let receiver = ReviewedFileReceiver::prepare(destination, snapshot, stopped)
                .map_err(|_| unavailable())?;
            let stage = receiver.stage().map_err(stage_failed)?;
            StagedReceiver::Existing(Box::new(receiver), stage)
        }
        LocalFile::Vacant(VacantFileProof::File(snapshot)) => {
            let receiver = VacantLocalFileReceiver::prepare(destination, snapshot, stopped)
                .map_err(|_| unavailable())?;
            let stage = receiver.stage().map_err(stage_failed)?;
            StagedReceiver::Vacant(Box::new(receiver), stage)
        }
        LocalFile::Vacant(VacantFileProof::Hierarchy(snapshot)) => {
            let parents = context.local_parents()?;
            let receiver =
                MissingLocalFileReceiver::prepare(destination, snapshot, &parents, stopped)
                    .map_err(|_| unavailable())?;
            let stage = receiver.stage().map_err(stage_failed)?;
            StagedReceiver::MissingParents(Box::new(receiver), stage)
        }
    };
    let (writer, parent_entries) = match &receiver {
        StagedReceiver::Existing(_, stage) | StagedReceiver::Vacant(_, stage) => {
            (stage.writer(), 0)
        }
        StagedReceiver::MissingParents(receiver, stage) => {
            (stage.writer(), receiver.missing_parent_count())
        }
    };
    let output = writer
        .and_then(File::try_clone)
        .map_err(|_| Failure::failed("Could not open the private local rsync file stage."))?;
    let limits = context
        .recurring
        .map(|route| route.source_limits(parent_entries))
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
                route.reserve([current.inventory.ok_or_else(unavailable)?], parent_entries)?;
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
                route.reserve([manifest.inventory()?], parent_entries)?;
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
    let publication = match receiver {
        StagedReceiver::Existing(receiver, stage) => receiver
            .publish(stage, inventory, stopped, &permit, arm)
            .map(Publication::Existing),
        StagedReceiver::Vacant(receiver, stage) => receiver
            .publish(stage, inventory, stopped, &permit, arm)
            .map(Publication::Vacant),
        StagedReceiver::MissingParents(receiver, stage) => receiver
            .publish(stage, inventory, stopped, &permit, arm)
            .map(Publication::MissingParents),
    };
    let publication = match publication {
        Ok(publication) => publication,
        Err(error) => {
            authorize()?;
            require_running(stopped).map_err(|_| transfer_stopped())?;
            return Ok(publication_failure(
                id,
                Failure::blocked("Could not publish the reviewed local rsync file."),
                &error,
            ));
        }
    };
    let (successor, recovery, parents, remaining_stage) = match publication {
        Publication::Vacant(VacantLocalFilePublication::Published(successor)) => {
            (successor, None, Vec::new(), None)
        }
        Publication::Existing(ReviewedFilePublication::Published {
            successor,
            recovery,
        }) => (successor, Some(recovery), Vec::new(), None),
        Publication::MissingParents(MissingLocalFilePublication::Published {
            successor,
            parents,
            remaining_stage,
        }) => (successor, None, parents, remaining_stage),
        Publication::Existing(ReviewedFilePublication::Restored { error }) => {
            return Ok(publication_failure(
                id,
                Failure::failed("The original rsync file was restored."),
                &error,
            ));
        }
        Publication::Existing(ReviewedFilePublication::Uncertain { error, recovery }) => {
            return Ok(uncertain(id, &error, recovery.as_deref()));
        }
        Publication::Vacant(VacantLocalFilePublication::Uncertain(error)) => {
            return Ok(uncertain(id, &error, None));
        }
        Publication::MissingParents(MissingLocalFilePublication::Uncertain { error, stage }) => {
            let mut completion = uncertain(id, &error, None);
            completion.append_detail(&format!(
                "Check the private staging folder {}.",
                stage.display()
            ));
            return Ok(completion);
        }
    };
    let mut completion = Completion::from_result(id, Ok(0));
    completion.append_detail(&format!("Published {} verified bytes.", inventory.bytes));
    describe_recovery(&mut completion, recovery.as_deref());
    if let Some(path) = remaining_stage {
        completion.append_detail(&format!(
            "Private staging folder {} may remain.",
            path.display()
        ));
    }
    Ok(match context.recurring {
        Some(route) => route.confirm_with_parents(
            completion,
            Some(rsync::standing::Receiver::LocalFile {
                destination: destination.clone(),
                snapshot: *successor,
            }),
            &parents,
        ),
        None => context.record_local_parents(completion, parents),
    })
}

fn uncertain(id: RunId, error: &io::Error, recovery: Option<&Path>) -> Completion {
    let mut completion = publication_failure(
        id,
        Failure::failed(
            "Could not confirm the local rsync publication. Review the destination before retrying.",
        ),
        error,
    );
    describe_recovery(&mut completion, recovery);
    completion
}

fn describe_recovery(completion: &mut Completion, recovery: Option<&Path>) {
    if let Some(recovery) = recovery {
        completion.append_detail(&format!(
            "The original is retained at {}.",
            recovery.display()
        ));
    }
}

fn publication_failure(id: RunId, failure: Failure, error: &io::Error) -> Completion {
    let mut completion = Completion::from_result(id, Err(failure));
    completion.append_detail(&error.to_string());
    completion
}

fn unavailable() -> Failure {
    Failure::blocked("The reviewed local rsync file changed or is unavailable.")
}
