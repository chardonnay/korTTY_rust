//! Validate privileged path reviews while rsync transfers an ordinary login's handoff.

use std::sync::Arc;

use kortty_jobs::RunId;
use kortty_server_archive::privileged_rsync::{
    Decision, Operation, Reply, Request, Review, SourceLimits, Spec,
};
use kortty_server_archive::rsync_tree::{
    self as tree_protocol, DeleteScope, ExistingTree, Inventory, projection,
};

use crate::scheduler_window::RemotePath;
use crate::sftp_window::{
    LocalSource, PrivateTreeDestination, PrivateTreePublishError, scheduled::Client,
};

use super::{upload_tree::Helper, upload_tree::PhasePermission, *};

const REVIEW_REQUIRED: &str =
    "Review the protected server path and configured passwordless rsync helper.";
const CHECK_FAILED: &str = "The privileged rsync handoff differs from the reviewed content.";

pub(in crate::job_executor::rsync) fn review(
    exec: &ExecOpener,
    spec: &Spec,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    guard: ExecutionGuard,
) -> Result<Review, Failure> {
    if !spec.valid() {
        return Err(review_required());
    }
    let mut control = Control::open(exec, cancelled.clone(), guard)?;
    control.request(&Request::Review { spec: spec.clone() }, &cancelled)?;
    control.reviewed(spec, &cancelled)
}

pub(super) fn review_source(
    exec: &ExecOpener,
    spec: &Spec,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    guard: ExecutionGuard,
    stopped: &impl Fn() -> bool,
) -> Result<(Review, Vec<projection::Entry>), Failure> {
    Control::open(exec, cancelled, guard)?.source_manifest(spec, None, stopped)
}

pub(super) fn fresh_source(
    context: &TransferContext<'_>,
    approved: &Review,
    limits: SourceLimits,
    stopped: &impl Fn() -> bool,
) -> Result<(Review, Vec<projection::Entry>), Failure> {
    Control::from_context(context)?.source_manifest(
        &approved.spec,
        Some((approved, limits)),
        stopped,
    )
}

pub(super) fn run(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let approved = action
        .approval
        .as_ref()
        .and_then(|approval| approval.privileged.as_deref())
        .filter(|review| review.valid())
        .ok_or_else(review_required)?;
    let upload = matches!(approved.spec.operation, Operation::Upload { .. });
    if action.privileged_spec(upload).as_ref().ok() != Some(&approved.spec) {
        return Err(review_required());
    }
    let guard = context.guard.clone().ok_or_else(review_required)?;
    let checked = Cell::new((Instant::now(), true));
    let stopped = &|| stopped() || authority_revoked(&checked, &guard);
    if upload {
        let limits = context
            .recurring
            .map(recurring::Route::source_limits)
            .transpose()?;
        let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
        let transfer = context.transfer(action, &temp, stopped);
        let incoming = match (action.directory, limits) {
            (true, Some(limits)) => upload_tree::stage_bounded_input(
                &transfer,
                tree_protocol::Manifest::bounded(limits.entries, limits.bytes),
            )?,
            (true, None) => upload_tree::stage_input(&transfer)?,
            (false, _) => stage_file(&transfer, limits)?,
        };
        if let Some(route) = context.recurring {
            route.reserve([incoming])?;
        }
        Control::from_context(context)?.upload_staged(
            transfer,
            listener,
            &endpoint,
            Upload {
                approved,
                incoming,
                route: context.recurring,
            },
            id,
        )
    } else if !action.directory && action.existing_download.is_some() {
        local_file::run(client, context, action, Some(approved), id, stopped)
    } else {
        download(client, context, action, approved, id, stopped)
    }
}

fn stage_file<C: Fn() -> bool>(
    transfer: &Transfer<'_, C>,
    limits: Option<SourceLimits>,
) -> Result<Inventory, Failure> {
    let action = transfer.action;
    let review = action
        .approval
        .as_ref()
        .and_then(|approval| approval.upload.as_ref())
        .ok_or_else(review_required)?;
    let source = LocalSource::open(&action.local).map_err(|_| review_required())?;
    if limits.is_some_and(|limits| limits.entries == 0 || source.size() > limits.bytes) {
        return Err(Failure::blocked(
            "The recurring rsync source exceeds the firing limits.",
        ));
    }
    let staged = transfer
        .temp
        .directory()
        .and_then(|stage| stage.create_file(OsStr::new(PAYLOAD)))
        .map_err(|_| Failure::failed(CHECK_FAILED))?;
    let (bytes, digest) = upload::copy_reviewed(&source, &staged, transfer.stopped)
        .map_err(|_| Failure::failed(CHECK_FAILED))?;
    if limits.is_none() && (bytes, digest) != (review.bytes, review.digest) {
        return Err(review_required());
    }
    staged
        .sync_all()
        .map_err(|_| Failure::failed(CHECK_FAILED))?;
    let mut manifest = tree_protocol::Manifest::default();
    manifest
        .add("", Some((bytes, digest)))
        .map_err(|_| Failure::failed(CHECK_FAILED))?;
    Ok(manifest.finish())
}

fn download(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    approved: &Review,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let existing = action
        .approval
        .as_ref()
        .and_then(|approval| approval.existing_download.as_deref());
    let receiver = existing
        .map(|existing| {
            action
                .require_safe_existing_local_program()
                .map_err(Failure::blocked)?;
            let snapshot = match context.recurring {
                Some(route) => route.local(&existing.destination)?,
                None => existing.snapshot,
            };
            local_existing::Receiver::prepare(&existing.destination, snapshot, stopped)
                .map_err(|_| unavailable_destination())
        })
        .transpose()?;
    let fresh;
    let approved = if let Some(route) = context.recurring {
        if receiver.is_none() {
            return Err(unavailable_destination());
        }
        (fresh, _) = fresh_source(context, approved, route.source_limits()?, stopped)?;
        route.reserve([fresh.inventory.ok_or_else(review_required)?])?;
        &fresh
    } else {
        approved
    };
    let vacant = (action.directory && existing.is_none())
        .then(|| PrivateTreeDestination::inspect(&action.local))
        .transpose()
        .map_err(|_| unavailable_destination())?;
    let expected = approved.inventory.ok_or_else(review_required)?;
    let mut control = Control::from_context(context)?;
    let (path, scanned) = control.prepare_source(client, approved, stopped)?;
    let mut handoff = action.clone();
    handoff.remote = path;
    let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
    let transfer = context.transfer(&handoff, &temp, stopped);
    let authorize = || {
        check_cancelled(context.cancellation)?;
        require_authority(context.guard.as_ref())
    };
    if !action.directory {
        let stage = StagedDownload::create(&action.local).map_err(|_| unavailable_destination())?;
        let output = stage
            .writer()
            .and_then(File::try_clone)
            .map_err(|_| unavailable_destination())?;
        tree::receive_projected_file(client, transfer, listener, &endpoint, &scanned, output)?;
        control.finish_read(expected, stopped)?;
        stage
            .publish(|| stopped() || authorize().is_err())
            .map_err(|_| Failure::failed("Could not publish the privileged rsync download."))?;
    } else if let (Some(existing), Some(receiver)) = (existing, receiver) {
        let (incoming, _) = tree::receive_existing(
            client,
            transfer,
            listener,
            &endpoint,
            context.reference,
            Some(scanned),
        )?;
        control.finish_read(expected, stopped)?;
        let published = receiver.publish(
            local_existing::Incoming {
                directory: &incoming,
                inventory: expected,
                delete: &if existing.settings.delete {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                },
            },
            stopped,
            &authorize,
            id,
            |removed| context.recurring.map_or(Ok(()), |route| route.arm(removed)),
        )?;
        return Ok(match context.recurring {
            Some(route) => route.confirm(
                published.completion,
                published
                    .successor
                    .map(|snapshot| rsync::standing::Receiver::Local {
                        destination: existing.destination.clone(),
                        snapshot,
                    }),
            ),
            None => published.completion,
        });
    } else {
        let stage = vacant
            .ok_or_else(unavailable_destination)?
            .stage()
            .map_err(|_| unavailable_destination())?;
        tree::receive(
            client,
            transfer,
            listener,
            &endpoint,
            context.reference,
            tree::Output {
                directory: stage.root().map_err(|_| unavailable_destination())?,
                owner_only: true,
            },
            Some(scanned),
        )?;
        control.finish_read(expected, stopped)?;
        authorize()?;
        stage
            .publish(|| !stopped() && authorize().is_ok())
            .map_err(|error| match error {
                PrivateTreePublishError::Uncertain => Failure::failed(tree::UNCERTAIN),
                PrivateTreePublishError::Denied => {
                    authorize().err().unwrap_or_else(transfer_stopped)
                }
                PrivateTreePublishError::Refused => unavailable_destination(),
            })?;
    }
    Ok(success(
        id,
        format!(
            "Downloaded {} verified bytes through the privileged rsync helper.",
            expected.bytes
        ),
    ))
}

pub(super) struct Control {
    helper: Helper,
    permitted: PhasePermission,
}

pub(super) struct Upload<'a> {
    pub(super) approved: &'a Review,
    pub(super) incoming: Inventory,
    pub(super) route: Option<recurring::Route<'a>>,
}

impl Control {
    pub(super) fn from_context(context: &TransferContext<'_>) -> Result<Self, Failure> {
        let cancellation = context.cancellation.clone();
        let guard = context.guard.clone().ok_or_else(review_required)?;
        Self::open(&context.exec, move || cancellation.is_requested(), guard)
    }

    fn open(
        exec: &ExecOpener,
        cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
        guard: ExecutionGuard,
    ) -> Result<Self, Failure> {
        require_authority(Some(&guard))?;
        let helper = Helper::open_privileged(exec, cancelled.clone(), guard.clone())
            .map_err(|_| review_required())?;
        let permitted: PhasePermission = Arc::new(move || !cancelled() && guard().is_ok());
        Ok(Self { helper, permitted })
    }

    fn request(&mut self, request: &Request, stopped: &impl Fn() -> bool) -> Result<(), Failure> {
        self.helper
            .send_privileged(request, stopped, &self.permitted)
            .map_err(|_| review_required())
    }

    fn reviewed(&mut self, spec: &Spec, stopped: &impl Fn() -> bool) -> Result<Review, Failure> {
        match self.helper.receive_privileged(stopped) {
            Ok(Reply::Reviewed { review })
                if &review.spec == spec && review.valid() && (self.permitted)() =>
            {
                Ok(review)
            }
            _ => Err(review_required()),
        }
    }

    fn source_manifest(
        &mut self,
        spec: &Spec,
        recurring: Option<(&Review, SourceLimits)>,
        stopped: &impl Fn() -> bool,
    ) -> Result<(Review, Vec<projection::Entry>), Failure> {
        let (request, limits) = match recurring {
            Some((approved, limits)) => (
                Request::ReviewSourceLimited {
                    approved: approved.clone(),
                    limits,
                },
                limits,
            ),
            None => (
                Request::ReviewSource { spec: spec.clone() },
                SourceLimits::MAXIMUM,
            ),
        };
        self.request(&request, stopped)?;
        let review = self.reviewed(spec, stopped)?;
        let expected = review.inventory.ok_or_else(review_required)?;
        if !limits.contains(expected)
            || recurring.is_some_and(|(approved, _)| {
                review.policy != approved.policy || review.handoff != approved.handoff
            })
        {
            return Err(review_required());
        }
        let mut manifest = tree_protocol::Manifest::bounded(limits.entries, limits.bytes);
        let mut entries = Vec::with_capacity(expected.entries as usize);
        for _ in 0..expected.entries {
            let Ok(Reply::SourceEntry { entry }) = self.helper.receive_privileged(stopped) else {
                return Err(review_required());
            };
            manifest
                .add(&entry.relative, entry.file)
                .map_err(|_| review_required())?;
            entries.push(entry);
        }
        if manifest.finish() != expected
            || !matches!(self.helper.receive_privileged(stopped), Ok(Reply::SourceComplete { inventory }) if inventory == expected)
            || !(self.permitted)()
        {
            return Err(review_required());
        }
        Ok((review, entries))
    }

    fn prepare(
        &mut self,
        review: &Review,
        request: Request,
        stopped: &impl Fn() -> bool,
    ) -> Result<RemotePath, Failure> {
        self.request(&request, stopped)?;
        match self.helper.receive_privileged(stopped) {
            Ok(Reply::Prepared { path }) if review.contains_payload(&path) => {
                RemotePath::try_from(path).map_err(|_| review_required())
            }
            _ => Err(review_required()),
        }
    }

    fn phase(&mut self, decision: &Decision, stopped: &impl Fn() -> bool) -> io::Result<Reply> {
        self.helper
            .send_guarded(decision, stopped, &self.permitted)?;
        self.helper.receive_privileged(stopped)
    }

    pub(super) fn prepare_source(
        &mut self,
        client: &mut Client,
        review: &Review,
        stopped: &impl Fn() -> bool,
    ) -> Result<(RemotePath, tree::Manifest), Failure> {
        let Operation::Download { directory } = review.spec.operation else {
            return Err(review_required());
        };
        let expected = review.inventory.ok_or_else(review_required)?;
        let path = self.prepare(
            review,
            Request::Prepare {
                review: review.clone(),
                incoming: None,
            },
            stopped,
        )?;
        let scanned = tree::Manifest::read_limited(
            client,
            &path,
            tree::Limits {
                bytes: expected.bytes,
                entries: expected.entries as usize + usize::from(directory),
            },
            stopped,
        )?;
        if scanned.is_directory() != directory || scanned.inventory()? != expected {
            return Err(Failure::failed(CHECK_FAILED));
        }
        Ok((path, scanned))
    }

    pub(super) fn finish_read(
        &mut self,
        expected: Inventory,
        stopped: &impl Fn() -> bool,
    ) -> Result<(), Failure> {
        if matches!(self.phase(&Decision::Verify, stopped), Ok(Reply::Finished { inventory }) if inventory == expected)
            && (self.permitted)()
        {
            Ok(())
        } else {
            Err(Failure::failed(CHECK_FAILED))
        }
    }

    pub(super) fn upload_staged<C: Fn() -> bool>(
        &mut self,
        transfer: Transfer<'_, C>,
        listener: Listener,
        endpoint: &str,
        Upload {
            approved,
            incoming,
            route,
        }: Upload<'_>,
        id: RunId,
    ) -> Result<Completion, Failure> {
        let stopped = transfer.stopped;
        let destination =
            RemotePath::try_from(approved.spec.path.clone()).map_err(Failure::blocked)?;
        let mut review = Cow::Borrowed(approved);
        let request = if let Some(route) = route {
            let snapshot = route.remote(&destination)?;
            let review = review.to_mut();
            review.inventory = Some(snapshot.inventory);
            review.fingerprint = snapshot.fingerprint;
            Request::PrepareRecurring {
                review: review.clone(),
                incoming,
            }
        } else {
            Request::Prepare {
                review: approved.clone(),
                incoming: Some(incoming),
            }
        };
        let path = self.prepare(&review, request, stopped)?;
        transfer.run_checked(
            listener,
            endpoint,
            path.as_str(),
            true,
            ReceiveCheck {
                protocol_limit: incoming.bytes + PROTOCOL_OVERHEAD,
                check_output: || Ok(()),
            },
        )?;
        let (candidate, removed) = match self.phase(&Decision::Verify, stopped) {
            Ok(Reply::Verified { inventory, removed })
                if checked_candidate(&review, incoming, inventory, removed) =>
            {
                (inventory, removed)
            }
            _ => return Err(Failure::failed(CHECK_FAILED)),
        };
        if let Some(route) = route {
            // Persist the removal charge and pending publication before any rename.
            route.arm(removed)?;
        }
        let (completion, successor) =
            self.publish(&review, candidate, id, route.is_some(), stopped);
        Ok(match route {
            Some(route) => route.confirm(
                completion,
                successor.map(|snapshot| rsync::standing::Receiver::Remote {
                    destination,
                    snapshot,
                }),
            ),
            None => completion,
        })
    }

    fn publish(
        &mut self,
        review: &Review,
        candidate: Inventory,
        id: RunId,
        recurring: bool,
        stopped: &impl Fn() -> bool,
    ) -> (Completion, Option<ExistingTree>) {
        let destination = &review.spec.path;
        let mut receipt = Receipt {
            destination,
            candidate,
            recovery: None,
            published: false,
            recurring,
            successor: None,
        };
        let attempt = (|| {
            if matches!(
                review.spec.operation,
                Operation::Upload { replace: true, .. }
            ) {
                match self.phase(&Decision::Quarantine, stopped)? {
                    Reply::Quarantined { recovery } if valid_recovery(destination, &recovery) => {
                        receipt.recovery = Some(recovery)
                    }
                    other => return Ok(other),
                }
            }
            self.helper
                .send_guarded(&Decision::Publish, stopped, &self.permitted)?;
            receipt.published = true;
            self.helper.receive_privileged(stopped)
        })();
        if let Ok(reply) = attempt
            && let Some(completion) = receipt.observe(reply, id)
        {
            return (completion, receipt.successor);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        let giving_up = || Instant::now() >= deadline;
        let _ = self.helper.send(&Decision::Abort, &giving_up);
        while let Ok(reply) = self.helper.receive_privileged(&giving_up) {
            if let Some(completion) = receipt.observe(reply, id) {
                return (completion, receipt.successor);
            }
        }
        (receipt.uncertain(id), None)
    }
}

struct Receipt<'a> {
    destination: &'a str,
    candidate: Inventory,
    recovery: Option<String>,
    published: bool,
    recurring: bool,
    successor: Option<ExistingTree>,
}

impl Receipt<'_> {
    fn observe(&mut self, reply: Reply, id: RunId) -> Option<Completion> {
        match reply {
            Reply::Quarantined { recovery }
                if self.recovery.is_none() && valid_recovery(self.destination, &recovery) =>
            {
                self.recovery = Some(recovery);
                None
            }
            Reply::Published {
                destination,
                inventory,
                recovery,
            } if !self.recurring
                && self.published
                && destination == self.destination
                && inventory == self.candidate
                && recovery == self.recovery =>
            {
                Some(self.complete(id))
            }
            Reply::PublishedWithReceiver {
                destination,
                receiver,
                recovery,
            } if self.recurring
                && self.published
                && destination == self.destination
                && receiver.inventory == self.candidate
                && self.recovery.as_deref() == Some(recovery.as_str()) =>
            {
                self.successor = Some(receiver);
                Some(self.complete(id))
            }
            Reply::Aborted
            | Reply::Failed {
                outcome_unknown: false,
                recovery: None,
                ..
            } => Some(Completion::from_result(
                id,
                Err(Failure::failed(
                    "The privileged rsync helper stopped before confirming publication.",
                )),
            )),
            Reply::Failed {
                outcome_unknown: true,
                recovery,
                ..
            } => {
                if let Some(recovery) = recovery
                    && self.recovery.is_none()
                    && valid_recovery(self.destination, &recovery)
                {
                    self.recovery = Some(recovery);
                }
                Some(self.uncertain(id))
            }
            _ => None,
        }
    }

    fn complete(&self, id: RunId) -> Completion {
        let mut detail = format!(
            "Published {} verified bytes through the privileged rsync helper.",
            self.candidate.bytes
        );
        if let Some(recovery) = &self.recovery {
            detail.push_str(&format!(" The original is retained at {recovery}."));
        }
        success(id, detail)
    }

    fn uncertain(&self, id: RunId) -> Completion {
        let mut completion = Completion::from_result(
            id,
            Err(Failure::failed(
                "Privileged rsync publication could not be confirmed. Check the reviewed destination before retrying.",
            )),
        );
        if let Some(recovery) = &self.recovery {
            completion.append_detail(&format!("Last reported recovery path: {recovery}"));
        }
        completion
    }
}

fn checked_candidate(
    review: &Review,
    incoming: Inventory,
    candidate: Inventory,
    removed: u32,
) -> bool {
    match review.spec.operation {
        Operation::Upload {
            directory: true,
            replace: true,
            ..
        } => review.inventory.is_some_and(|original| {
            candidate.valid()
                && removed <= original.entries
                && candidate.entries >= original.entries - removed
        }),
        Operation::Upload { .. } => incoming == candidate && removed == 0,
        Operation::Download { .. } => false,
    }
}

fn valid_recovery(destination: &str, recovery: &str) -> bool {
    destination.rsplit_once('/').is_some_and(|(parent, _)| {
        recovery
            .strip_prefix(parent)
            .and_then(|path| path.strip_prefix('/'))
            .and_then(|path| path.strip_suffix("/original"))
            .is_some_and(tree_protocol::valid_stage)
    })
}

fn review_required() -> Failure {
    Failure::blocked(REVIEW_REQUIRED)
}

fn unavailable_destination() -> Failure {
    Failure::blocked("The reviewed local rsync destination is unavailable.")
}

fn success(id: RunId, detail: String) -> Completion {
    let mut completion = Completion::from_result(id, Ok(0));
    completion.detail = Some(detail);
    completion
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_privileged_file_candidate_binds_the_exact_new_content() {
        let inventory = |byte| {
            let mut manifest = tree_protocol::Manifest::default();
            manifest.add("", Some((8, [byte; 32]))).unwrap();
            manifest.finish()
        };
        let original = inventory(1);
        let incoming = inventory(2);
        let review = Review {
            spec: Spec {
                path: "/protected/file".into(),
                operation: Operation::Upload {
                    directory: false,
                    replace: true,
                    delete: DeleteScope::Retain,
                },
            },
            policy: [0; 32],
            fingerprint: [0; 32],
            inventory: Some(original),
            handoff: "/handoff/login".into(),
        };
        assert!(checked_candidate(&review, incoming, incoming, 0));
        assert!(!checked_candidate(&review, incoming, original, 0));
        assert!(!checked_candidate(&review, incoming, incoming, 1));
    }

    #[test]
    fn privileged_rsync_publication_requires_the_guarded_decision_and_exact_receipt() {
        let id: RunId = serde_json::from_str("1").unwrap();
        let candidate = tree_protocol::Manifest::default().finish();
        let destination = "/protected/receiver";
        let recovery = "/protected/.kortty-00000000000000000000000000000000.kortty-part/original";
        let mut receipt = Receipt {
            destination,
            candidate,
            recovery: None,
            published: false,
            recurring: false,
            successor: None,
        };
        let reply = |name: &str, recovery: Option<&str>| Reply::Published {
            destination: name.into(),
            inventory: candidate,
            recovery: recovery.map(str::to_owned),
        };
        assert!(receipt.observe(reply(destination, None), id).is_none());
        let foreign = "/other/.kortty-00000000000000000000000000000000.kortty-part/original";
        assert!(
            receipt
                .observe(
                    Reply::Quarantined {
                        recovery: foreign.into()
                    },
                    id
                )
                .is_none()
        );
        assert!(receipt.recovery.is_none());
        assert!(
            receipt
                .observe(
                    Reply::Quarantined {
                        recovery: recovery.into()
                    },
                    id
                )
                .is_none()
        );
        receipt.published = true;
        assert!(
            receipt
                .observe(reply(destination, Some(foreign)), id)
                .is_none()
        );
        assert!(
            receipt
                .observe(reply("/protected/other", Some(recovery)), id)
                .is_none()
        );
        assert_eq!(
            receipt
                .observe(reply(destination, Some(recovery)), id)
                .unwrap()
                .outcome,
            RunOutcome::Success
        );
    }

    #[test]
    fn recurring_privileged_receipt_requires_the_exact_held_successor() {
        let id: RunId = serde_json::from_str("1").unwrap();
        let candidate = tree_protocol::Manifest::default().finish();
        let destination = "/protected/receiver";
        let recovery = "/protected/.kortty-00000000000000000000000000000000.kortty-part/original";
        let mut receipt = Receipt {
            destination,
            candidate,
            recovery: Some(recovery.into()),
            published: false,
            recurring: true,
            successor: None,
        };
        let receiver = ExistingTree {
            inventory: candidate,
            fingerprint: [9; 32],
        };
        let reply = || Reply::PublishedWithReceiver {
            destination: destination.into(),
            receiver,
            recovery: recovery.into(),
        };
        assert!(receipt.observe(reply(), id).is_none());
        receipt.published = true;
        assert!(
            receipt
                .observe(
                    Reply::Published {
                        destination: destination.into(),
                        inventory: candidate,
                        recovery: Some(recovery.into())
                    },
                    id
                )
                .is_none()
        );
        assert_eq!(
            receipt.observe(reply(), id).unwrap().outcome,
            RunOutcome::Success
        );
        assert_eq!(receipt.successor, Some(receiver));
    }
}
