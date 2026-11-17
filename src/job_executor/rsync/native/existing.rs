//! Publish reviewed upload receivers through the pinned two-phase helper.

use std::{cell::Cell, sync::Arc};

use kortty_jobs::RunId;
use kortty_server_archive::privileged_rsync::SourceLimits;
use kortty_server_archive::rsync_tree::{
    self as protocol, DeleteScope, ExistingDecision, ExistingReply, ExistingRequest,
    ExistingRequestV2, ExistingTree, Inventory, RecurringReceiver,
};

use crate::job_executor::rsync::RecurringReceiverTarget;
use crate::scheduler_window::RemotePath;
use crate::scheduler_window::rsync::standing::{Receiver as StandingReceiver, RemoteVacancy};

use super::{
    upload_tree::{Helper, PhasePermission},
    *,
};

const ABORT_TIME: Duration = Duration::from_secs(1);
const AUTHORITY_POLL: Duration = Duration::from_millis(250);

pub(in crate::job_executor::rsync) fn review(
    exec: &ExecOpener,
    destination: &RemotePath,
    cancellation: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authority: ExecutionGuard,
) -> Result<ExistingTree, Failure> {
    review_receiver(exec, destination, cancellation, authority, false)
}

pub(super) fn review_receiver(
    exec: &ExecOpener,
    destination: &RemotePath,
    cancellation: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authority: ExecutionGuard,
    scoped: bool,
) -> Result<ExistingTree, Failure> {
    if !rsync::valid_remote(destination.as_str()) {
        return Err(review_required());
    }
    require_authority(Some(&authority))?;
    require_running(&cancellation).map_err(|_| transfer_stopped())?;
    let open = if scoped {
        Helper::open_existing_v2
    } else {
        Helper::open_existing
    };
    let mut helper = open(exec, cancellation.clone(), authority.clone())?;
    let permitted = permission(cancellation, authority);
    let stopped = || !permitted();
    let destination = destination.as_str().to_owned();
    if scoped {
        helper.send_existing_v2(
            &ExistingRequestV2::Review { destination },
            &stopped,
            &permitted,
        )
    } else {
        helper.send_guarded(
            &ExistingRequest::Review { destination },
            &stopped,
            &permitted,
        )
    }
    .map_err(|_| review_required())?;
    match helper.receive(&stopped) {
        Ok(ExistingReply::Reviewed { original }) if original.inventory.valid() && !stopped() => {
            Ok(original)
        }
        _ => Err(Failure::blocked(
            "Could not review the existing rsync destination. Check the installed helper and the receiver's ownership and filesystem.",
        )),
    }
}

pub(in crate::job_executor::rsync) fn review_recurring(
    exec: &ExecOpener,
    target: RecurringReceiverTarget<'_>,
    cancellation: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authority: ExecutionGuard,
) -> Result<RecurringReceiver, Failure> {
    let RecurringReceiverTarget {
        destination,
        directory,
        delete,
    } = target;
    if !rsync::valid_remote(destination.as_str())
        || !delete.valid()
        || (!directory && *delete != DeleteScope::Retain)
    {
        return Err(review_required());
    }
    require_authority(Some(&authority))?;
    require_running(&cancellation).map_err(|_| transfer_stopped())?;
    let mut helper = Helper::open_existing_v3(exec, cancellation.clone(), authority.clone())?;
    let permitted = permission(cancellation, authority);
    let stopped = || !permitted();
    helper
        .send_existing_v2(
            &ExistingRequestV2::ReviewRecurring {
                destination: destination.as_str().to_owned(),
                directory,
                delete: delete.clone(),
            },
            &stopped,
            &permitted,
        )
        .map_err(|_| review_required())?;
    match helper.receive(&stopped) {
        Ok(ExistingReply::ReviewedRecurring {
            destination: reviewed,
            receiver,
        }) if reviewed == destination.as_str() && receiver.valid(directory) && !stopped() => {
            Ok(receiver)
        }
        _ => Err(Failure::blocked(
            "Could not review the recurring rsync destination. Check the current Linux helper and the receiver's ownership and filesystem.",
        )),
    }
}

pub(super) fn apply<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
) -> Result<Completion, Failure> {
    let (destination, snapshot, scope) = action_receiver(transfer.action)?;
    apply_receiver(
        transfer,
        listener,
        bridge_endpoint,
        id,
        Receiver {
            destination,
            approval: ReceiverApproval::Directory(snapshot),
            scope: &scope,
            projected: None,
        },
    )
}

pub(super) fn apply_recurring<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
    snapshot: ExistingTree,
    reserve: impl FnOnce(Inventory) -> Result<(), Failure>,
    arm: impl FnOnce(u32) -> Result<(), Failure>,
) -> Result<(Completion, Option<ExistingTree>), Failure> {
    let (destination, _, scope) = action_receiver(transfer.action)?;
    apply_checked(
        transfer,
        listener,
        bridge_endpoint,
        id,
        Receiver {
            destination,
            approval: ReceiverApproval::Directory(snapshot),
            scope: &scope,
            projected: None,
        },
        Some(Recurring {
            reserve,
            arm,
            limits: None,
        }),
    )
}

fn action_receiver(
    action: &rsync::Transfer,
) -> Result<(&RemotePath, ExistingTree, DeleteScope), Failure> {
    let review = action
        .approval
        .as_ref()
        .and_then(|approval| approval.existing.as_deref())
        .ok_or_else(review_required)?;
    if Some(review.settings) != action.existing
        || action.existing_destination().as_ref().ok() != Some(&review.destination)
    {
        return Err(review_required());
    }
    let scope = if review.settings.delete {
        DeleteScope::All
    } else {
        DeleteScope::Retain
    };
    Ok((&review.destination, review.snapshot, scope))
}

pub(super) struct Receiver<'a> {
    pub(super) destination: &'a RemotePath,
    pub(super) approval: ReceiverApproval,
    pub(super) scope: &'a DeleteScope,
    pub(super) projected: Option<Inventory>,
}

#[derive(Clone, Copy)]
pub(super) enum ReceiverApproval {
    Directory(ExistingTree),
    Recurring(RecurringReceiver),
}

pub(super) fn apply_receiver<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
    review: Receiver<'_>,
) -> Result<Completion, Failure> {
    apply_checked(
        transfer,
        listener,
        bridge_endpoint,
        id,
        review,
        None::<NoRecurring>,
    )
    .map(|(completion, _)| completion)
}

pub(super) fn apply_recurring_receiver<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
    review: Receiver<'_>,
    arm: impl FnOnce(u32) -> Result<(), Failure>,
) -> Result<(Completion, Option<ExistingTree>), Failure> {
    apply_checked(
        transfer,
        listener,
        bridge_endpoint,
        id,
        review,
        Some(Recurring {
            reserve: |_| Ok(()),
            arm,
            limits: None,
        }),
    )
}

pub(super) fn apply_ordinary_recurring<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
    mut review: Receiver<'_>,
    route: recurring::Route<'_>,
) -> Result<Completion, Failure> {
    let ReceiverApproval::Recurring(approved) = review.approval else {
        return Err(review_required());
    };
    let destination = review.destination;
    let directory = transfer.action.directory;
    if !approved.valid(directory) {
        return Err(review_required());
    }
    let initial_vacancy = match approved {
        RecurringReceiver::Vacant { fingerprint } if directory => {
            Some(RemoteVacancy::Tree(fingerprint))
        }
        RecurringReceiver::Vacant { fingerprint } => Some(RemoteVacancy::File(fingerprint)),
        RecurringReceiver::Existing { .. } => None,
    };
    let (receiver, vacancy) = match route.remote_upload(destination, initial_vacancy)? {
        recurring::RemoteUpload::Existing(snapshot) => {
            (RecurringReceiver::Existing { snapshot }, None)
        }
        recurring::RemoteUpload::VacantFile | recurring::RemoteUpload::VacantTree => {
            (approved, initial_vacancy)
        }
    };
    if !receiver.valid(directory) {
        return Err(review_required());
    }
    review.approval = ReceiverApproval::Recurring(receiver);
    let limits = review
        .projected
        .is_none()
        .then(|| route.source_limits(0))
        .transpose()?;
    let (completion, successor) = apply_checked(
        transfer,
        listener,
        bridge_endpoint,
        id,
        review,
        Some(Recurring {
            reserve: |incoming| limits.map_or(Ok(()), |_| route.reserve([incoming], 0)),
            arm: |removed| route.arm(removed),
            limits,
        }),
    )?;
    let successor = successor.map(|snapshot| StandingReceiver::Remote {
        destination: destination.clone(),
        snapshot,
    });
    Ok(match vacancy {
        Some(vacancy) => route.confirm_first_remote(completion, successor, vacancy),
        None => route.confirm(completion, successor),
    })
}

struct Recurring<R, A> {
    reserve: R,
    arm: A,
    limits: Option<SourceLimits>,
}

type NoRecurring = Recurring<fn(Inventory) -> Result<(), Failure>, fn(u32) -> Result<(), Failure>>;

fn apply_checked<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    id: RunId,
    review: Receiver<'_>,
    recurring: Option<
        Recurring<
            impl FnOnce(Inventory) -> Result<(), Failure>,
            impl FnOnce(u32) -> Result<(), Failure>,
        >,
    >,
) -> Result<(Completion, Option<ExistingTree>), Failure> {
    let Transfer {
        action,
        exec,
        cancellation,
        temp,
        stopped,
        execution_guard,
        executable,
    } = transfer;
    let authority = execution_guard
        .as_ref()
        .ok_or_else(review_required)?
        .clone();
    let cancelled = cancellation.clone();
    let permitted = permission(move || cancelled.is_requested(), authority.clone());
    require_authority(Some(&authority))?;
    let authority_check = Cell::new((Instant::now(), true));
    let stopped = || {
        if stopped() {
            return true;
        }
        let mut state = authority_check.get();
        if state.1 && state.0.elapsed() >= AUTHORITY_POLL {
            state = (Instant::now(), permitted());
            authority_check.set(state);
        }
        !state.1
    };
    let transfer = Transfer {
        action,
        exec,
        cancellation: cancellation.clone(),
        temp,
        stopped: &stopped,
        execution_guard,
        executable,
    };
    require_running(&stopped).map_err(|_| transfer_stopped())?;
    let directory = action.directory;
    let (typed, original, vacancy) = match review.approval {
        ReceiverApproval::Directory(original) if directory => (None, Some(original), None),
        ReceiverApproval::Recurring(receiver)
            if recurring.is_some() && receiver.valid(directory) =>
        {
            match receiver {
                RecurringReceiver::Existing { snapshot } => (Some(receiver), Some(snapshot), None),
                RecurringReceiver::Vacant { fingerprint } => {
                    (Some(receiver), None, Some(fingerprint))
                }
            }
        }
        _ => return Err(review_required()),
    };
    let limits = recurring.as_ref().and_then(|recurring| recurring.limits);
    let incoming = match (review.projected, directory, limits) {
        (Some(incoming), _, _) => incoming,
        (None, false, limits) => upload::stage_file(&transfer, limits)?,
        (None, true, Some(limits)) => upload_tree::stage_bounded_input(
            &transfer,
            protocol::Manifest::bounded(limits.entries, limits.bytes),
        )?,
        (None, true, None) if recurring.is_some() => upload_tree::stage_current_input(&transfer)?,
        (None, true, None) => upload_tree::stage_input(&transfer)?,
    };
    let arm = match recurring {
        Some(Recurring { reserve, arm, .. }) => {
            reserve(incoming)?;
            Some(arm)
        }
        None => None,
    };
    let receipt_required = arm.is_some();
    require_running(&stopped).map_err(|_| transfer_stopped())?;
    let cancelled = cancellation.clone();
    let open = if receipt_required {
        Helper::open_existing_v3
    } else if review.projected.is_some() {
        Helper::open_existing_v2
    } else {
        Helper::open_existing
    };
    let mut helper = open(
        &transfer.exec,
        move || cancelled.is_requested(),
        authority.clone(),
    )?;
    let destination = review.destination.as_str().to_owned();
    if let Some(receiver) = typed {
        helper.send_existing_v2(
            &ExistingRequestV2::PrepareRecurring {
                destination,
                incoming,
                directory,
                receiver,
                delete: review.scope.clone(),
            },
            &stopped,
            &permitted,
        )
    } else if receipt_required || review.projected.is_some() {
        helper.send_existing_v2(
            &ExistingRequestV2::Prepare {
                destination,
                incoming,
                original: original.ok_or_else(review_required)?,
                delete: review.scope.clone(),
            },
            &stopped,
            &permitted,
        )
    } else {
        helper.send_guarded(
            &ExistingRequest::Prepare {
                destination,
                incoming,
                original: original.ok_or_else(review_required)?,
                delete: *review.scope == DeleteScope::All,
            },
            &stopped,
            &permitted,
        )
    }
    .map_err(|_| review_required())?;
    let stage = match helper.receive(&stopped) {
        Ok(ExistingReply::Prepared { stage }) if protocol::valid_stage(&stage) => stage,
        _ => return Err(review_required()),
    };
    let parent = review.destination.parent().trim_end_matches('/');
    let leaf = if directory { "" } else { "/file" };
    let payload = format!("{parent}/{stage}/{PAYLOAD}{leaf}");
    let verified = (|| {
        require_running(&stopped).map_err(|_| transfer_stopped())?;
        transfer.run_checked(
            listener,
            bridge_endpoint,
            &payload,
            true,
            ReceiveCheck {
                protocol_limit: incoming.bytes + PROTOCOL_OVERHEAD,
                check_output: || Ok(()),
            },
        )?;
        let unverified = || Failure::failed("Could not verify the staged rsync successor.");
        helper
            .send_guarded(&ExistingDecision::Verify, &stopped, &permitted)
            .map_err(|_| unverified())?;
        let (successor, removed) = match helper.receive(&stopped) {
            Ok(ExistingReply::Verified { successor }) if !receipt_required => (successor, 0),
            Ok(ExistingReply::VerifiedWithRemovals { successor, removed })
                if receipt_required
                    && if vacancy.is_some() || !directory {
                        removed == 0 && successor == incoming
                    } else {
                        original.is_some_and(|original| {
                            removals_match(original.inventory, successor, removed)
                        })
                    } =>
            {
                (successor, removed)
            }
            _ => return Err(unverified()),
        };
        if !successor_matches(successor, incoming, *review.scope == DeleteScope::All) {
            return Err(unverified());
        }
        require_running(&stopped).map_err(|_| transfer_stopped())?;
        if let Some(arm) = arm {
            // The reviewed receiver remains named until Quarantine is sent.
            arm(removed)?;
        }
        Ok(successor)
    })();
    let successor = match verified {
        Ok(successor) => successor,
        Err(error) => {
            let deadline = Instant::now() + ABORT_TIME;
            let stopped = || Instant::now() >= deadline;
            let _ = helper.send(&ExistingDecision::Abort, &stopped);
            return Err(error);
        }
    };
    let mut publication = Publication {
        destination: review.destination,
        successor,
        upload_stage: &stage,
        recovery: None,
        publishing: false,
        previous_fingerprint: original
            .filter(|_| receipt_required)
            .map(|original| original.fingerprint),
        vacancy,
        directory,
        published_receiver: None,
    };
    let status = publication.run(&mut helper, &stopped, &permitted);
    let receiver = publication.published_receiver;
    let mut completion = publication.complete(id, status);
    if matches!(status, Status::Restored | Status::Rejected) {
        if cancellation.is_requested() {
            completion.outcome = RunOutcome::Cancelled;
        } else if authority().is_err() {
            completion.outcome = RunOutcome::Blocked;
        }
    }
    Ok((completion, receiver))
}

fn permission(
    cancelled: impl Fn() -> bool + Send + Sync + 'static,
    authority: ExecutionGuard,
) -> PhasePermission {
    Arc::new(move || !cancelled() && authority().is_ok())
}

fn successor_matches(successor: Inventory, incoming: Inventory, delete: bool) -> bool {
    successor.valid()
        && if delete {
            successor == incoming
        } else {
            successor.entries >= incoming.entries
                && successor.files >= incoming.files
                && successor.bytes >= incoming.bytes
        }
}

fn removals_match(original: Inventory, successor: Inventory, removed: u32) -> bool {
    original
        .entries
        .checked_sub(removed)
        .is_some_and(|retained| retained <= successor.entries)
}

fn review_required() -> Failure {
    Failure::blocked("Review the exact source, rsync receiver, and pinned SSH route again.")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Quarantined,
    Published,
    Restored,
    Rejected,
    Uncertain,
}

struct Publication<'a> {
    destination: &'a RemotePath,
    successor: Inventory,
    upload_stage: &'a str,
    recovery: Option<String>,
    publishing: bool,
    previous_fingerprint: Option<[u8; 32]>,
    vacancy: Option<[u8; 32]>,
    directory: bool,
    published_receiver: Option<ExistingTree>,
}

impl Publication<'_> {
    fn run(
        &mut self,
        helper: &mut Helper,
        stopped: &impl Fn() -> bool,
        permitted: &PhasePermission,
    ) -> Status {
        let mut ready = self.vacancy.is_some();
        if !ready
            && helper
                .send_guarded(&ExistingDecision::Quarantine, stopped, permitted)
                .is_ok()
            && let Ok(reply) = helper.receive(stopped)
            && let Some(status) = self.observe(reply)
        {
            if status != Status::Quarantined {
                return status;
            }
            ready = true;
        }
        if ready {
            self.publishing = helper
                .send_guarded(&ExistingDecision::Publish, stopped, permitted)
                .is_ok();
            if self.publishing
                && let Ok(reply) = helper.receive(stopped)
                && let Some(status) = self.observe(reply)
            {
                return status;
            }
        }
        // Abort remains available after policy or user cancellation, and may race
        // a pending quarantine reply. Acknowledged restoration is decisive.
        let deadline = Instant::now() + ABORT_TIME;
        let stopped = || Instant::now() >= deadline;
        let _ = helper.send(&ExistingDecision::Abort, &stopped);
        while let Ok(reply) = helper.receive(&stopped) {
            match self.observe(reply) {
                Some(Status::Quarantined) => {}
                Some(status) => return status,
                None => break,
            }
        }
        Status::Uncertain
    }

    fn observe(&mut self, reply: ExistingReply) -> Option<Status> {
        match reply {
            ExistingReply::Quarantined { recovery }
                if self.vacancy.is_none()
                    && self.recovery.is_none()
                    && self.valid_recovery(&recovery) =>
            {
                self.recovery = Some(recovery);
                Some(Status::Quarantined)
            }
            ExistingReply::Published {
                destination,
                successor,
                recovery,
            } if self.vacancy.is_none()
                && self.previous_fingerprint.is_none()
                && self.valid_publication(&destination, successor, &recovery) =>
            {
                Some(Status::Published)
            }
            ExistingReply::PublishedWithReceiver {
                destination,
                receiver,
                recovery,
            } if self.previous_fingerprint.is_some_and(|previous| {
                previous != receiver.fingerprint && receiver.fingerprint != [0; 32]
            }) && self.valid_publication(&destination, receiver.inventory, &recovery) =>
            {
                self.published_receiver = Some(receiver);
                Some(Status::Published)
            }
            ExistingReply::FirstRecurringPublished {
                destination,
                vacancy,
                receiver,
            } if self.publishing
                && self.vacancy == Some(vacancy)
                && destination == self.destination.as_str()
                && self.recovery.is_none()
                && receiver.inventory == self.successor
                && receiver.fingerprint != [0; 32] =>
            {
                self.published_receiver = Some(receiver);
                Some(Status::Published)
            }
            ExistingReply::Aborted
            | ExistingReply::Failed {
                outcome_unknown: false,
                recovery: None,
                ..
            } => {
                if self.recovery.take().is_some() {
                    Some(Status::Restored)
                } else {
                    Some(Status::Rejected)
                }
            }
            ExistingReply::Failed {
                outcome_unknown: true,
                recovery,
                ..
            } => {
                if self.vacancy.is_none()
                    && let Some(recovery) = recovery
                    && self.recovery.is_none()
                    && self.valid_recovery(&recovery)
                {
                    self.recovery = Some(recovery);
                }
                Some(Status::Uncertain)
            }
            _ => None,
        }
    }

    fn valid_publication(&self, destination: &str, successor: Inventory, recovery: &str) -> bool {
        self.publishing
            && destination == self.destination.as_str()
            && successor == self.successor
            && self.recovery.as_deref() == Some(recovery)
    }

    fn valid_recovery(&self, path: &str) -> bool {
        let parent = self.destination.parent().trim_end_matches('/');
        path.strip_prefix(parent)
            .and_then(|path| path.strip_prefix('/'))
            .and_then(|path| path.strip_suffix("/original"))
            .is_some_and(|stage| {
                protocol::valid_stage(stage) && (stage != self.upload_stage) == self.directory
            })
    }

    fn complete(self, id: RunId, status: Status) -> Completion {
        let outcome = if status == Status::Published {
            Ok(0)
        } else {
            Err(Failure::failed(
                "Could not publish the reviewed rsync receiver.",
            ))
        };
        let mut completion = Completion::from_result(id, outcome);
        completion.detail = Some(match status {
            Status::Published => match &self.recovery {
                Some(recovery) => format!(
                    "Published the verified rsync receiver. The original is retained at {recovery}."
                ),
                None => "Published the verified rsync receiver at the reviewed vacant path.".into(),
            },
            Status::Restored => "The server confirmed that the original is at the reviewed destination.".into(),
            Status::Rejected => "The server refused publication. Check the receiver and review it again.".into(),
            Status::Quarantined | Status::Uncertain => match self.recovery.as_deref() {
                Some(recovery) => format!(
                    "Rsync publication or restoration is uncertain. Check the reviewed destination and the last reported recovery path {recovery} before retrying.",
                ),
                None => "Rsync publication or restoration is uncertain. Check the reviewed destination and its private recovery folders before retrying.".into(),
            },
        });
        completion
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: &str = ".kortty-0123456789abcdef0123456789abcdef.kortty-part";
    const RECOVERY: &str =
        "/srv/project/.kortty-1123456789abcdef0123456789abcdef.kortty-part/original";

    fn inventory() -> Inventory {
        Inventory {
            entries: 2,
            files: 1,
            bytes: 7,
            digest: [1; 32],
        }
    }

    fn publication(destination: &RemotePath) -> Publication<'_> {
        Publication {
            destination,
            successor: inventory(),
            upload_stage: STAGE,
            recovery: None,
            publishing: false,
            previous_fingerprint: None,
            vacancy: None,
            directory: true,
            published_receiver: None,
        }
    }

    #[test]
    fn ordinary_recurring_rsync_requires_the_exact_vacancy_receipt() {
        let destination = RemotePath::try_from("/srv/project/receiver".to_owned()).unwrap();
        let mut transaction = publication(&destination);
        let vacancy = [2; 32];
        transaction.vacancy = Some(vacancy);
        let receiver = ExistingTree {
            inventory: inventory(),
            fingerprint: [3; 32],
        };
        let receipt = |path: &str, vacancy, receiver| ExistingReply::FirstRecurringPublished {
            destination: path.to_owned(),
            vacancy,
            receiver,
        };
        assert_eq!(
            transaction.observe(receipt(destination.as_str(), vacancy, receiver)),
            None
        );
        transaction.publishing = true;
        for wrong in [
            receipt("/srv/project/wrong", vacancy, receiver),
            receipt(destination.as_str(), [4; 32], receiver),
            receipt(
                destination.as_str(),
                vacancy,
                ExistingTree {
                    fingerprint: [0; 32],
                    ..receiver
                },
            ),
            ExistingReply::Quarantined {
                recovery: RECOVERY.to_owned(),
            },
            ExistingReply::Published {
                destination: destination.as_str().to_owned(),
                successor: inventory(),
                recovery: RECOVERY.to_owned(),
            },
        ] {
            assert_eq!(transaction.observe(wrong), None);
        }
        assert_eq!(
            transaction.observe(receipt(destination.as_str(), vacancy, receiver)),
            Some(Status::Published),
        );
        assert_eq!(transaction.published_receiver, Some(receiver));
        assert!(transaction.recovery.is_none());
    }

    #[test]
    fn recurring_rsync_file_requires_recovery_in_its_private_stage() {
        let destination = RemotePath::try_from("/srv/project/receiver".to_owned()).unwrap();
        let mut transaction = publication(&destination);
        transaction.directory = false;
        assert!(transaction.valid_recovery(&format!("/srv/project/{STAGE}/original")));
        assert!(!transaction.valid_recovery(RECOVERY));
    }

    #[test]
    fn existing_rsync_recovery_detail_keeps_the_complete_stage_for_long_receivers() {
        let parent = format!("/{}/{}", "a".repeat(160), "b".repeat(160));
        let destination = RemotePath::try_from(format!("{parent}/receiver")).unwrap();
        let mut transaction = publication(&destination);
        let stage = ".kortty-1123456789abcdef0123456789abcdef.kortty-part/original";
        let recovery = format!("{parent}/{stage}");
        assert_eq!(
            transaction.observe(ExistingReply::Quarantined {
                recovery: recovery.clone()
            }),
            Some(Status::Quarantined)
        );
        let id = serde_json::from_value(serde_json::json!(1)).unwrap();
        let detail = transaction.complete(id, Status::Uncertain).detail.unwrap();
        assert!(detail.contains("last reported recovery path"));
        assert!(detail.contains(&recovery));
    }

    #[test]
    fn existing_rsync_requires_quarantine_and_the_exact_publication_receipt() {
        let destination = RemotePath::try_from("/srv/project/data".to_owned()).unwrap();
        let mut transaction = publication(&destination);
        let published = |recovery: &str| ExistingReply::Published {
            destination: destination.as_str().to_owned(),
            successor: inventory(),
            recovery: recovery.to_owned(),
        };
        assert_eq!(transaction.observe(published(RECOVERY)), None);
        assert!(!transaction.valid_recovery(&format!("/srv/project/{STAGE}/original")));
        assert!(!transaction.valid_recovery(&format!("/srv/project/./{STAGE}/original")));
        assert!(!transaction.valid_recovery(&format!("/srv/project-other/{STAGE}/original")));
        assert_eq!(
            transaction.observe(ExistingReply::Quarantined {
                recovery: RECOVERY.to_owned(),
            }),
            Some(Status::Quarantined)
        );
        assert_eq!(transaction.observe(published(RECOVERY)), None);
        transaction.publishing = true;
        assert_eq!(
            transaction.observe(published("/tmp/unreviewed/original")),
            None
        );
        assert_eq!(transaction.recovery.as_deref(), Some(RECOVERY));
        assert_eq!(
            transaction.observe(published(RECOVERY)),
            Some(Status::Published)
        );
    }

    #[test]
    fn recurring_rsync_requires_the_v3_receipt_for_the_exact_successor() {
        let destination = RemotePath::try_from("/srv/project/data".to_owned()).unwrap();
        let receiver = ExistingTree {
            inventory: inventory(),
            fingerprint: [2; 32],
        };
        let receipt = |destination: &str, recovery: &str, receiver: ExistingTree| {
            ExistingReply::PublishedWithReceiver {
                destination: destination.into(),
                receiver,
                recovery: recovery.into(),
            }
        };
        let mut transaction = publication(&destination);
        transaction.previous_fingerprint = Some([1; 32]);
        transaction.observe(ExistingReply::Quarantined {
            recovery: RECOVERY.into(),
        });
        assert_eq!(
            transaction.observe(receipt(destination.as_str(), RECOVERY, receiver)),
            None
        );
        transaction.publishing = true;
        assert_eq!(
            transaction.observe(ExistingReply::Published {
                destination: destination.as_str().into(),
                successor: receiver.inventory,
                recovery: RECOVERY.into(),
            }),
            None
        );
        let previous = ExistingTree {
            fingerprint: [1; 32],
            ..receiver
        };
        assert_eq!(
            transaction.observe(receipt(destination.as_str(), RECOVERY, previous)),
            None
        );
        assert_eq!(
            transaction.observe(receipt("/srv/project/other", RECOVERY, receiver)),
            None
        );
        assert_eq!(
            transaction.observe(receipt(
                destination.as_str(),
                "/tmp/wrong/original",
                receiver
            )),
            None
        );
        assert!(transaction.published_receiver.is_none());
        assert_eq!(
            transaction.observe(receipt(destination.as_str(), RECOVERY, receiver)),
            Some(Status::Published)
        );
        assert_eq!(transaction.published_receiver, Some(receiver));
    }

    #[test]
    fn recurring_rsync_rejects_inconsistent_removed_entry_counts() {
        let original = Inventory {
            entries: 10,
            ..inventory()
        };
        assert!(!removals_match(original, inventory(), 11));
        assert!(!removals_match(original, inventory(), 7));
        assert!(removals_match(original, inventory(), 8));
        assert!(removals_match(original, inventory(), 10));
    }

    #[test]
    fn existing_rsync_retains_known_recovery_until_restoration_is_confirmed() {
        let destination = RemotePath::try_from("/srv/project/data".to_owned()).unwrap();
        let mut transaction = publication(&destination);
        assert_eq!(
            transaction.observe(ExistingReply::Failed {
                message: "receiver locked".into(),
                outcome_unknown: false,
                recovery: None,
            }),
            Some(Status::Rejected)
        );
        assert_eq!(
            transaction.observe(ExistingReply::Aborted),
            Some(Status::Rejected)
        );
        transaction.observe(ExistingReply::Quarantined {
            recovery: RECOVERY.to_owned(),
        });
        assert_eq!(
            transaction.observe(ExistingReply::Failed {
                message: "inconsistent".into(),
                outcome_unknown: false,
                recovery: Some("/tmp/unreviewed/original".into()),
            }),
            None
        );
        assert_eq!(
            transaction.observe(ExistingReply::Failed {
                message: "lost".into(),
                outcome_unknown: true,
                recovery: Some("/tmp/unreviewed/original".into()),
            }),
            Some(Status::Uncertain)
        );
        assert_eq!(transaction.recovery.as_deref(), Some(RECOVERY));
        assert_eq!(
            transaction.observe(ExistingReply::Aborted),
            Some(Status::Restored)
        );
        assert!(transaction.recovery.is_none());
    }

    #[test]
    fn existing_rsync_mirror_requires_the_unchanged_source_inventory() {
        let incoming = inventory();
        let merged = Inventory {
            bytes: incoming.bytes + 1,
            digest: [2; 32],
            ..incoming
        };
        assert!(successor_matches(incoming, incoming, true));
        assert!(!successor_matches(merged, incoming, true));
        assert!(successor_matches(merged, incoming, false));
        assert!(!successor_matches(incoming, merged, false));
    }
}
