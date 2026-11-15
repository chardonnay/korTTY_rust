//! Replace a reviewed receiver tree only through the pinned two-phase helper.

use std::{cell::Cell, sync::Arc};

use kortty_jobs::RunId;
use kortty_server_archive::rsync_tree::{
    self as protocol, DeleteScope, ExistingDecision, ExistingReply, ExistingRequest,
    ExistingRequestV2, ExistingTree, Inventory,
};

use crate::scheduler_window::RemotePath;

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
            snapshot,
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
            snapshot,
            scope: &scope,
            projected: None,
        },
        Some(Recurring { reserve, arm }),
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
    pub(super) snapshot: ExistingTree,
    pub(super) scope: &'a DeleteScope,
    pub(super) projected: Option<Inventory>,
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
        }),
    )
}

struct Recurring<R, A> {
    reserve: R,
    arm: A,
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
    let incoming = match review.projected {
        Some(incoming) => incoming,
        None if recurring.is_some() => upload_tree::stage_current_input(&transfer)?,
        None => upload_tree::stage_input(&transfer)?,
    };
    let arm = match recurring {
        Some(Recurring { reserve, arm }) => {
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
    if receipt_required || review.projected.is_some() {
        helper.send_existing_v2(
            &ExistingRequestV2::Prepare {
                destination,
                incoming,
                original: review.snapshot,
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
                original: review.snapshot,
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
    let payload = format!("{parent}/{stage}/{PAYLOAD}");
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
                    && removals_match(review.snapshot.inventory, successor, removed) =>
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
        previous_fingerprint: receipt_required.then_some(review.snapshot.fingerprint),
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
    Failure::blocked(
        "Review the exact source, existing rsync receiver, and pinned SSH route again.",
    )
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
    published_receiver: Option<ExistingTree>,
}

impl Publication<'_> {
    fn run(
        &mut self,
        helper: &mut Helper,
        stopped: &impl Fn() -> bool,
        permitted: &PhasePermission,
    ) -> Status {
        if helper
            .send_guarded(&ExistingDecision::Quarantine, stopped, permitted)
            .is_ok()
            && let Ok(reply) = helper.receive(stopped)
            && let Some(status) = self.observe(reply)
        {
            if status != Status::Quarantined {
                return status;
            }
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
                if self.recovery.is_none() && self.valid_recovery(&recovery) =>
            {
                self.recovery = Some(recovery);
                Some(Status::Quarantined)
            }
            ExistingReply::Published {
                destination,
                successor,
                recovery,
            } if self.previous_fingerprint.is_none()
                && self.valid_publication(&destination, successor, &recovery) =>
            {
                Some(Status::Published)
            }
            ExistingReply::PublishedWithReceiver {
                destination,
                receiver,
                recovery,
            } if self
                .previous_fingerprint
                .is_some_and(|previous| previous != receiver.fingerprint)
                && self.valid_publication(&destination, receiver.inventory, &recovery) =>
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
                if let Some(recovery) = recovery
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
            .is_some_and(|stage| stage != self.upload_stage && protocol::valid_stage(stage))
    }

    fn complete(self, id: RunId, status: Status) -> Completion {
        let outcome = if status == Status::Published {
            Ok(0)
        } else {
            Err(Failure::failed(
                "Could not publish the existing rsync receiver.",
            ))
        };
        let mut completion = Completion::from_result(id, outcome);
        completion.detail = Some(match status {
            Status::Published => format!(
                "Published the verified rsync tree. The original is retained at {}.",
                self.recovery.as_deref().expect("confirmed publication requires a recovery path"),
            ),
            Status::Restored => "The server confirmed that the original is at the reviewed destination.".into(),
            Status::Rejected => "The server refused publication before confirming quarantine. Check the receiver and review it again.".into(),
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
            published_receiver: None,
        }
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
