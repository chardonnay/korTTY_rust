//! Verify every mixed source and publish one combined tree per pinned route.

use std::cell::Cell;

use kortty_jobs::RunId;
use kortty_server_archive::privileged_rsync::{Operation, Review, SourceLimits, Spec};
use kortty_server_archive::rsync_tree::{
    self as protocol, DeleteScope, ExistingTree, RecurringReceiver, projection,
};

use crate::job_executor::rsync::{RecurringReceiverTarget, UploadCreation};
use crate::scheduler_window::RemotePath;
use crate::scheduler_window::rsync::{
    Elevation, MixedDestination, MixedPlan, MixedPrivilegedReview, MixedReceiver, MixedReview,
    MixedReviewTarget,
    mixed::{bare_source, source_contents},
};
use crate::sftp_window::{
    LocalSource, PrivateTreeDestination, PrivateTreePublishError, VerifiedTreeStage,
    scheduled::Client,
};

use super::recurring::LocalTree;
use super::*;

const INPUT_FILE: &str = "source-file";
const SOURCE_CHANGED: &str = "The mixed rsync inputs changed. Review the transfer again.";
const STAGE_FAILED: &str = "Could not verify the combined private rsync stage.";

enum LocalReceiver {
    Existing(Box<local_existing::Receiver>),
    Vacant(Box<local_existing::VacantLocalTreeReceiver>),
}

pub(in crate::job_executor::rsync) fn review(
    client: &mut Client,
    exec: &ExecOpener,
    target: &MixedReviewTarget,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    stopped: &dyn Fn() -> bool,
    authority: ExecutionGuard,
) -> Result<MixedReview, String> {
    let checked = Cell::new((Instant::now(), true));
    let stopped = || stopped() || authority_revoked(&checked, &authority);
    authority().map_err(|error| error.to_string())?;
    let elevated = target.elevation == Elevation::Passwordless;
    let (inputs, mut privileged) = match &target.destination {
        MixedDestination::Server(_) => (
            local_inputs(&target.sources, None, SourceLimits::MAXIMUM, &stopped)
                .map_err(|error| error.to_string())?,
            None,
        ),
        MixedDestination::Local(_) if elevated => {
            let (inputs, proofs) =
                protected_inputs(exec, &target.sources, &cancelled, &authority, &stopped)
                    .map_err(|error| error.detail)?;
            (inputs, Some(MixedPrivilegedReview::Download(proofs)))
        }
        MixedDestination::Local(_) => (
            remote_inputs(client, &target.sources, &stopped, None, 0)
                .map_err(|error| error.detail)?
                .1,
            None,
        ),
    };
    let projected =
        projection::project(&inputs, target.delete).map_err(|error| error.to_string())?;
    let mut ordinary_receiver = None;
    let receiver = match &target.destination {
        MixedDestination::Server(destination) if elevated => {
            let mut spec = Spec {
                path: destination.as_str().into(),
                operation: Operation::Upload {
                    directory: true,
                    replace: false,
                    delete: DeleteScope::Retain,
                },
            };
            let replacement = Operation::Upload {
                directory: true,
                replace: true,
                delete: projected.delete.clone(),
            };
            let proof = if target.review_vacant {
                spec.operation = replacement;
                privileged::review_destination(
                    exec,
                    &spec,
                    cancelled,
                    authority.clone(),
                    Some(UploadCreation::Tree),
                )
            } else {
                privileged::review(exec, &spec, cancelled.clone(), authority.clone()).or_else(
                    |_| {
                        require_running(&stopped).map_err(|_| transfer_stopped())?;
                        spec.operation = replacement;
                        privileged::review(exec, &spec, cancelled, authority.clone())
                    },
                )
            }
            .map_err(|error| error.detail)?;
            let snapshot = proof.inventory.map(|inventory| ExistingTree {
                inventory,
                fingerprint: proof.fingerprint,
            });
            privileged = Some(MixedPrivilegedReview::Upload(Box::new(proof)));
            MixedReceiver::Server {
                destination: destination.clone(),
                snapshot,
            }
        }
        MixedDestination::Server(destination) if target.review_vacant => {
            let proof = existing::review_recurring(
                exec,
                RecurringReceiverTarget {
                    destination,
                    directory: true,
                    delete: &projected.delete,
                },
                cancelled,
                authority.clone(),
            )
            .map_err(|error| error.detail)?;
            ordinary_receiver = Some(proof);
            MixedReceiver::Server {
                destination: destination.clone(),
                snapshot: match proof {
                    RecurringReceiver::Existing { snapshot } => Some(snapshot),
                    RecurringReceiver::Vacant { .. } => None,
                },
            }
        }
        MixedDestination::Server(destination) => {
            let snapshot = match client.lstat(destination.as_str()) {
                Ok(_) => Some(
                    existing::review_receiver(
                        exec,
                        destination,
                        cancelled,
                        authority.clone(),
                        true,
                    )
                    .map_err(|error| error.detail)?,
                ),
                Err(error) if error.is_not_found() => {
                    remote::transfer::verify_new_destination(client, destination)
                        .map_err(|error| error.detail)?;
                    None
                }
                Err(_) => return Err("The server rsync destination could not be checked.".into()),
            };
            MixedReceiver::Server {
                destination: destination.clone(),
                snapshot,
            }
        }
        MixedDestination::Local(destination) => {
            review_local_receiver(destination, target.review_vacant, elevated, &stopped)
                .map_err(|error| error.to_string())?
        }
    };
    require_running(&stopped).map_err(|error| error.to_string())?;
    let plan = MixedPlan::from_projection(target, &inputs, &projected)?;
    let review = MixedReview {
        plan,
        receiver,
        privileged,
        ordinary_receiver,
    };
    authority().map_err(|error| error.to_string())?;
    if !target.matches(&review) {
        return Err("The mixed rsync review changed or is inconsistent.".into());
    }
    Ok(review)
}

fn review_local_receiver(
    destination: &Path,
    review_vacant: bool,
    elevated: bool,
    stopped: &impl Fn() -> bool,
) -> io::Result<MixedReceiver> {
    let absent = if review_vacant {
        match std::fs::symlink_metadata(destination) {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        }
    } else if elevated {
        match PrivateTreeDestination::inspect(destination) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error),
        }
    } else {
        VerifiedTreeStage::review_vacancy(destination)?
    };
    let snapshot = (!absent)
        .then(|| local_existing::inspect(destination, stopped))
        .transpose()?;
    let vacant = (absent && review_vacant)
        .then(|| local_existing::VacantLocalTreeReceiver::inspect(destination, stopped))
        .transpose()?
        .map(Box::new);
    Ok(MixedReceiver::Local {
        destination: destination.to_owned(),
        snapshot,
        vacant,
    })
}

pub(super) fn run(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    uploading: bool,
    id: RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let approval = action
        .approval
        .as_ref()
        .and_then(|approval| approval.mixed.as_deref())
        .ok_or_else(|| Failure::blocked("Review the exact mixed rsync transfer first."))?;
    let target = action
        .mixed_target(context.reference, uploading)
        .map_err(Failure::blocked)?;
    if !target.matches(approval) {
        return Err(Failure::blocked(
            "Review the exact mixed rsync transfer first.",
        ));
    }
    let guard = context
        .guard
        .as_ref()
        .ok_or_else(|| Failure::blocked("The mixed rsync approval is unavailable."))?;
    let checked = Cell::new((Instant::now(), true));
    let stopped = || stopped() || authority_revoked(&checked, guard);
    let authorize = || {
        check_cancelled(context.cancellation)?;
        require_authority(Some(guard))
    };
    authorize()?;
    let pool = Scratch::new(&std::env::temp_dir()).map_err(|_| stage_failed())?;
    let inputs = pool.directory().map_err(|_| stage_failed())?;
    let mut completion = match &approval.receiver {
        MixedReceiver::Server {
            destination,
            snapshot,
        } => {
            let privileged = match &approval.privileged {
                Some(MixedPrivilegedReview::Upload(proof)) => Some(proof),
                _ => None,
            };
            let snapshot = match (privileged, approval.ordinary_receiver, context.recurring) {
                (None, None, Some(route)) => Some(route.remote(destination)?),
                _ => *snapshot,
            };
            let limits = context
                .recurring
                .map(|route| route.source_limits(0))
                .transpose()?
                .unwrap_or(SourceLimits::MAXIMUM);
            let scanned = local_inputs(&target.sources, Some(inputs), limits, &stopped)
                .map_err(|_| Failure::blocked(SOURCE_CHANGED))?;
            let projected =
                require_projection(&target, &approval.plan, &scanned, context.recurring, 0)?;
            let (temp, listener, endpoint) = stage_bridge(context, &stopped)?;
            let output = create_scratch_child(
                temp.directory().map_err(|_| stage_failed())?,
                OsStr::new(PAYLOAD),
            )
            .map_err(|_| stage_failed())?;
            arrange(&projected, inputs, &output, false, &stopped).map_err(|_| stage_failed())?;
            drop(output);
            let transfer = context.transfer(action, &temp, &stopped);
            if let Some(proof) = privileged {
                privileged::Control::from_context(context)?.upload_staged(
                    transfer,
                    listener,
                    &endpoint,
                    privileged::Upload {
                        approved: proof,
                        incoming: projected.inventory,
                        route: context.recurring,
                    },
                    id,
                )?
            } else if let Some(approved) = approval.ordinary_receiver {
                let route = context.recurring.ok_or_else(|| {
                    Failure::blocked("A recurring rsync run requires its current firing approval.")
                })?;
                existing::apply_ordinary_recurring(
                    transfer,
                    listener,
                    &endpoint,
                    id,
                    existing::Receiver {
                        destination,
                        approval: existing::ReceiverApproval::Recurring(approved),
                        scope: &projected.delete,
                        projected: Some(projected.inventory),
                    },
                    route,
                )?
            } else if let Some(snapshot) = snapshot {
                let receiver = existing::Receiver {
                    destination,
                    approval: existing::ReceiverApproval::Directory(snapshot),
                    scope: &projected.delete,
                    projected: Some(projected.inventory),
                };
                if let Some(route) = context.recurring {
                    let (completion, successor) = existing::apply_recurring_receiver(
                        transfer,
                        listener,
                        &endpoint,
                        id,
                        receiver,
                        |removed| route.arm(removed),
                    )?;
                    route.confirm(
                        completion,
                        successor.map(|snapshot| rsync::standing::Receiver::Remote {
                            destination: destination.clone(),
                            snapshot,
                        }),
                    )
                } else {
                    existing::apply_receiver(transfer, listener, &endpoint, id, receiver)?
                }
            } else {
                remote::transfer::verify_new_destination(client, destination)?;
                let detail = upload_tree::apply_staged(
                    transfer,
                    listener,
                    &endpoint,
                    authorize,
                    projected.inventory,
                )?;
                success(id, detail)
            }
        }
        MixedReceiver::Local {
            destination,
            snapshot,
            vacant,
        } => {
            action
                .require_safe_mixed_local_program()
                .map_err(Failure::blocked)?;
            let proof = match (context.recurring, snapshot, vacant) {
                (Some(route), Some(_), None) | (Some(route), None, Some(_)) => {
                    Some(route.local_tree(destination, vacant.as_deref())?)
                }
                (None, Some(snapshot), None) => Some(LocalTree::Existing(*snapshot)),
                (None, None, Some(vacant))
                    if action.standing.is_none() && action.allow_vacant_directory_download =>
                {
                    Some(LocalTree::Vacant(**vacant))
                }
                (None, None, None) => None,
                _ => return Err(vacant_local_changed()),
            };
            let parents = context.local_parents()?;
            let receiver = proof
                .map(|proof| match proof {
                    LocalTree::Existing(snapshot) => {
                        local_existing::Receiver::prepare(destination, snapshot, &stopped)
                            .map(Box::new)
                            .map(LocalReceiver::Existing)
                    }
                    LocalTree::Vacant(snapshot) => {
                        local_existing::VacantLocalTreeReceiver::prepare_with_parents(
                            destination,
                            snapshot,
                            &parents,
                            &stopped,
                        )
                        .map(Box::new)
                        .map(LocalReceiver::Vacant)
                    }
                })
                .transpose()
                .map_err(|_| Failure::blocked("The reviewed local rsync receiver changed."))?;
            let parent_entries = match &receiver {
                Some(LocalReceiver::Vacant(receiver)) => receiver.missing_parent_count(),
                _ => 0,
            };
            let projected =
                if let Some(MixedPrivilegedReview::Download(proofs)) = &approval.privileged {
                    let fresh;
                    let (proofs, expected) = if let Some(route) = context.recurring {
                        let scanned;
                        (fresh, scanned) = fresh_protected_inputs(
                            context,
                            &target.sources,
                            proofs,
                            route.source_limits(parent_entries)?,
                            &stopped,
                        )?;
                        let projected = require_projection(
                            &target,
                            &approval.plan,
                            &scanned,
                            Some(route),
                            parent_entries,
                        )?;
                        (fresh.as_slice(), Some(projected))
                    } else {
                        (proofs.as_slice(), None)
                    };
                    let scanned = receive_protected_sources(
                        client,
                        context,
                        action,
                        inputs,
                        target.sources.iter().zip(proofs),
                        &stopped,
                    )?;
                    if let Some(expected) = expected {
                        let current = projection::project(&scanned, target.delete)
                            .map_err(|_| Failure::blocked(SOURCE_CHANGED))?;
                        if current != expected {
                            return Err(Failure::blocked(SOURCE_CHANGED));
                        }
                        expected
                    } else {
                        require_projection(&target, &approval.plan, &scanned, None, 0)?
                    }
                } else {
                    let (sources, scanned) = remote_inputs(
                        client,
                        &target.sources,
                        &stopped,
                        context.recurring,
                        parent_entries,
                    )?;
                    let projected = require_projection(
                        &target,
                        &approval.plan,
                        &scanned,
                        context.recurring,
                        parent_entries,
                    )?;
                    for (index, input) in sources.into_iter().enumerate() {
                        receive_source(client, context, action, inputs, index, input, &stopped)?;
                    }
                    projected
                };
            authorize()?;
            match receiver {
                Some(receiver) => {
                    let output = create_scratch_child(inputs, OsStr::new("combined"))
                        .map_err(|_| stage_failed())?;
                    arrange(&projected, inputs, &output, true, &stopped)
                        .map_err(|_| stage_failed())?;
                    let incoming = local_existing::Incoming {
                        directory: &output,
                        inventory: projected.inventory,
                        delete: &projected.delete,
                    };
                    let arm =
                        |removed| context.recurring.map_or(Ok(()), |route| route.arm(removed));
                    let local_existing::Published {
                        completion,
                        successor,
                        parents,
                    } = match receiver {
                        LocalReceiver::Existing(receiver) => {
                            receiver.publish(incoming, &stopped, &authorize, id, arm)
                        }
                        LocalReceiver::Vacant(receiver) => {
                            receiver.publish(incoming, &stopped, &authorize, id, arm)
                        }
                    }?;
                    match context.recurring {
                        Some(route) => route.confirm_with_parents(
                            completion,
                            successor.map(|snapshot| rsync::standing::Receiver::Local {
                                destination: destination.clone(),
                                snapshot,
                            }),
                            &parents,
                        ),
                        None => context.record_local_parents(completion, parents),
                    }
                }
                None => {
                    publish_new_local(
                        destination,
                        &projected,
                        inputs,
                        approval.privileged.is_some(),
                        &stopped,
                        &authorize,
                    )?;
                    success(id, "Published the verified combined rsync tree.".into())
                }
            }
        }
    };
    if completion.outcome == RunOutcome::Success {
        completion.append_detail(&format!(
            "{} mixed sources verified for this route.",
            target.sources.len()
        ));
    }
    Ok(completion)
}

fn publish_new_local(
    path: &Path,
    projected: &projection::Projection,
    inputs: &local_tree::Directory,
    private: bool,
    stopped: &impl Fn() -> bool,
    authorize: &impl Fn() -> Result<(), Failure>,
) -> Result<(), Failure> {
    let fill = |root| {
        arrange(projected, inputs, root, private, stopped).map_err(|_| stage_failed())?;
        if upload_tree::checked_inventory(root, stopped).map_err(|_| stage_failed())?
            != projected.inventory
        {
            return Err(stage_failed());
        }
        authorize()
    };
    if private {
        let stage = PrivateTreeDestination::inspect(path)
            .and_then(PrivateTreeDestination::stage)
            .map_err(|_| vacant_local_changed())?;
        fill(stage.root().map_err(|_| stage_failed())?)?;
        return stage
            .publish(|| !stopped() && authorize().is_ok())
            .map_err(|error| match error {
                PrivateTreePublishError::Uncertain => Failure::failed(tree::UNCERTAIN),
                PrivateTreePublishError::Denied => transfer_stopped(),
                PrivateTreePublishError::Refused => vacant_local_changed(),
            });
    }
    let mut incomplete = false;
    let mut stage = VerifiedTreeStage::create(path, &mut incomplete).map_err(|_| {
        if incomplete {
            stage_failed()
        } else {
            vacant_local_changed()
        }
    })?;
    fill(stage.root())?;
    stage
        .publish(&|| stopped() || authorize().is_err())
        .map_err(|error| {
            if error.uncertain {
                Failure::failed(tree::UNCERTAIN)
            } else {
                vacant_local_changed()
            }
        })?;
    Ok(())
}

fn require_projection(
    target: &MixedReviewTarget,
    reviewed: &MixedPlan,
    sources: &[projection::Source],
    recurring: Option<recurring::Route<'_>>,
    parent_entries: u32,
) -> Result<projection::Projection, Failure> {
    let projected = projection::project(sources, target.delete)
        .map_err(|_| Failure::blocked(SOURCE_CHANGED))?;
    let current = MixedPlan::from_projection(target, sources, &projected)
        .map_err(|_| Failure::blocked(SOURCE_CHANGED))?;
    if let Some(route) = recurring {
        if current.delete != reviewed.delete
            || current.sources.len() != reviewed.sources.len()
            || current
                .sources
                .iter()
                .zip(&reviewed.sources)
                .any(|(current, reviewed)| {
                    current.raw != reviewed.raw || current.placement != reviewed.placement
                })
        {
            return Err(Failure::blocked(SOURCE_CHANGED));
        }
        route.reserve(
            current.sources.into_iter().map(|source| source.inventory),
            parent_entries,
        )?;
    } else if &current != reviewed {
        return Err(Failure::blocked(SOURCE_CHANGED));
    }
    Ok(projected)
}

fn local_inputs(
    sources: &[String],
    pool: Option<&local_tree::Directory>,
    limits: SourceLimits,
    stopped: &impl Fn() -> bool,
) -> io::Result<Vec<projection::Source>> {
    let mut budget = protocol::Manifest::bounded(limits.entries, limits.bytes);
    let mut scanned = Vec::with_capacity(sources.len());
    for (index, raw) in sources.iter().enumerate() {
        require_running(stopped)?;
        let bare = bare_source(raw, true);
        let path = Path::new(bare);
        let leaf = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(invalid_source)?;
        let metadata = std::fs::symlink_metadata(path)?;
        let layout = placement(leaf, source_contents(raw, true), metadata.is_dir())?;
        let staged = pool
            .map(|pool| pool.create_child(&source_name(index)))
            .transpose()?;
        let entries = if metadata.is_dir() {
            if matches!(layout, projection::Placement::Named(_)) {
                budget.add(leaf, None)?;
            }
            upload_tree::projected_entries(path, staged.as_ref(), &mut budget, stopped)?
        } else {
            let source = LocalSource::open(path)?;
            if budget.remaining_entries() == 0 || source.size() > budget.remaining_bytes() {
                return Err(io::Error::other(
                    "The mixed rsync sources exceed the combined transfer limit.",
                ));
            }
            let file = staged
                .as_ref()
                .map(|stage| stage.create_file(OsStr::new(INPUT_FILE)))
                .transpose()?;
            let (size, digest) = match &file {
                Some(file) => upload::copy_reviewed(&source, file, stopped)?,
                None => upload::copy_reviewed(&source, io::sink(), stopped)?,
            };
            if let Some(file) = file {
                file.sync_all()?;
            }
            budget.add(leaf, Some((size, digest)))?;
            vec![projection::Entry {
                relative: String::new(),
                file: Some((size, digest)),
            }]
        };
        scanned.push(projection::Source {
            placement: layout,
            entries,
        });
    }
    Ok(scanned)
}

type RemoteInput = (RemotePath, tree::Manifest);

fn fresh_protected_inputs(
    context: &TransferContext<'_>,
    sources: &[String],
    approved: &[Review],
    limits: SourceLimits,
    stopped: &impl Fn() -> bool,
) -> Result<(Vec<Review>, Vec<projection::Source>), Failure> {
    let mut budget = protocol::Manifest::bounded(limits.entries, limits.bytes);
    let mut proofs = Vec::with_capacity(sources.len());
    let mut scanned = Vec::with_capacity(sources.len());
    for (raw, approved) in sources.iter().zip(approved) {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let placement = protected_placement(raw, approved)?;
        // A named source also contributes its enclosing directory to the receiver.
        let available = budget
            .remaining_entries()
            .checked_sub(usize::from(matches!(
                &placement,
                projection::Placement::Named(_)
            )))
            .ok_or_else(exceeded_combined_limit)?;
        let (proof, entries) = privileged::fresh_source(
            context,
            approved,
            SourceLimits {
                entries: available as u32,
                bytes: budget.remaining_bytes(),
            },
            stopped,
        )?;
        track_input(&mut budget, &placement, &entries).map_err(|_| exceeded_combined_limit())?;
        scanned.push(projection::Source { placement, entries });
        proofs.push(proof);
    }
    Ok((proofs, scanned))
}

fn protected_inputs(
    exec: &ExecOpener,
    sources: &[String],
    cancelled: &(impl Fn() -> bool + Clone + Send + Sync + 'static),
    authority: &ExecutionGuard,
    stopped: &impl Fn() -> bool,
) -> Result<(Vec<projection::Source>, Vec<Review>), Failure> {
    let mut budget = protocol::Manifest::default();
    let mut proofs = Vec::with_capacity(sources.len());
    let mut scanned = Vec::with_capacity(sources.len());
    for raw in sources {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let path =
            RemotePath::try_from(bare_source(raw, false).to_owned()).map_err(Failure::blocked)?;
        let contents = source_contents(raw, false);
        let review = |directory| {
            privileged::review_source(
                exec,
                &Spec {
                    path: path.as_str().into(),
                    operation: Operation::Download { directory },
                },
                cancelled.clone(),
                authority.clone(),
                stopped,
            )
        };
        let (proof, entries) = match review(true) {
            Err(_) if !contents && !stopped() => review(false)?,
            result => result?,
        };
        let layout = protected_placement(raw, &proof)?;
        track_input(&mut budget, &layout, &entries).map_err(|_| exceeded_combined_limit())?;
        scanned.push(projection::Source {
            placement: layout,
            entries,
        });
        proofs.push(proof);
    }
    Ok((scanned, proofs))
}

fn protected_placement(raw: &str, proof: &Review) -> Result<projection::Placement, Failure> {
    let directory = matches!(
        proof.spec.operation,
        Operation::Download { directory: true }
    );
    let path = RemotePath::try_from(proof.spec.path.clone()).map_err(Failure::blocked)?;
    placement(path.basename(), source_contents(raw, false), directory)
        .map_err(|_| Failure::blocked("A mixed rsync source path or directory layout is invalid."))
}

fn receive_protected_sources<'a>(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    inputs: &local_tree::Directory,
    sources: impl Iterator<Item = (&'a String, &'a Review)>,
    stopped: &impl Fn() -> bool,
) -> Result<Vec<projection::Source>, Failure> {
    let mut budget = protocol::Manifest::default();
    let mut scanned = Vec::with_capacity(sources.size_hint().0);
    for (index, (raw, proof)) in sources.enumerate() {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let mut control = privileged::Control::from_context(context)?;
        let (path, manifest) = control.prepare_source(client, proof, stopped)?;
        let placement = protected_placement(raw, proof)?;
        let entries = manifest.projection_entries()?;
        track_input(&mut budget, &placement, &entries).map_err(|_| exceeded_combined_limit())?;
        let inventory = proof
            .inventory
            .ok_or_else(|| Failure::blocked(SOURCE_CHANGED))?;
        receive_source(
            client,
            context,
            action,
            inputs,
            index,
            (path, manifest),
            stopped,
        )?;
        control.finish_read(inventory, stopped)?;
        scanned.push(projection::Source { placement, entries });
    }
    Ok(scanned)
}

fn remote_inputs(
    client: &mut Client,
    sources: &[String],
    stopped: &impl Fn() -> bool,
    recurring: Option<recurring::Route<'_>>,
    parent_entries: u32,
) -> Result<(Vec<RemoteInput>, Vec<projection::Source>), Failure> {
    let mut budget = protocol::Manifest::default();
    let mut limits = match recurring {
        Some(recurring) => {
            let remaining = recurring.remaining(parent_entries)?;
            tree::Limits {
                entries: remaining.entries as usize,
                bytes: remaining.bytes,
            }
        }
        None => tree::Limits {
            entries: rsync::MAX_TREE_ENTRIES,
            bytes: MAX_FILE,
        },
    };
    let mut manifests = Vec::with_capacity(sources.len());
    let mut scanned = Vec::with_capacity(sources.len());
    for raw in sources {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let bare = bare_source(raw, false);
        let contents = source_contents(raw, false);
        let path = RemotePath::try_from(bare.to_owned()).map_err(Failure::blocked)?;
        let manifest = tree::Manifest::read_limited(
            client,
            &path,
            tree::Limits {
                entries: limits.entries + usize::from(contents),
                bytes: limits.bytes,
            },
            stopped,
        )?;
        let layout =
            placement(path.basename(), contents, manifest.is_directory()).map_err(|_| {
                Failure::blocked("A mixed rsync source path or directory layout is invalid.")
            })?;
        let entries = manifest.projection_entries()?;
        limits.entries -= manifest.entries() - usize::from(contents);
        limits.bytes -= manifest.bytes();
        track_input(&mut budget, &layout, &entries).map_err(|_| exceeded_combined_limit())?;
        scanned.push(projection::Source {
            placement: layout,
            entries,
        });
        manifests.push((path, manifest));
    }
    Ok((manifests, scanned))
}

fn track_input(
    budget: &mut protocol::Manifest,
    layout: &projection::Placement,
    entries: &[projection::Entry],
) -> io::Result<()> {
    if let projection::Placement::Named(name) = layout {
        budget.add(name, None)?;
    }
    for entry in entries {
        budget.add(&entry.relative, entry.file)?;
    }
    Ok(())
}

fn exceeded_combined_limit() -> Failure {
    Failure::blocked("The mixed rsync sources exceed the combined transfer limit.")
}

fn placement(leaf: &str, contents: bool, directory: bool) -> io::Result<projection::Placement> {
    match (contents, directory) {
        (true, true) => Ok(projection::Placement::Contents),
        (false, true) => Ok(projection::Placement::Named(leaf.into())),
        (false, false) => Ok(projection::Placement::File(leaf.into())),
        (true, false) => Err(invalid_source()),
    }
}

fn receive_source(
    client: &mut Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    inputs: &local_tree::Directory,
    index: usize,
    (path, manifest): RemoteInput,
    stopped: &impl Fn() -> bool,
) -> Result<(), Failure> {
    let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
    let mut source = action.clone();
    source.remote = path;
    source.directory = manifest.is_directory();
    let environment = context.transfer(&source, &temp, stopped);
    let input = inputs
        .create_child(&source_name(index))
        .map_err(|_| stage_failed())?;
    if source.directory {
        tree::receive(
            client,
            environment,
            listener,
            &endpoint,
            context.reference,
            tree::Output::inherited(&input),
            Some(manifest),
        )?;
    } else {
        let output = input
            .create_file(OsStr::new(INPUT_FILE))
            .map_err(|_| stage_failed())?;
        tree::receive_projected_file(client, environment, listener, &endpoint, &manifest, output)?;
    }
    Ok(())
}

fn arrange(
    projected: &projection::Projection,
    inputs: &local_tree::Directory,
    output: &local_tree::Directory,
    private: bool,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    for entry in &projected.entries {
        require_running(stopped)?;
        let (parent, name) = parts(&entry.relative);
        let target_parent = descend(output, parent)?;
        let target = target_parent.as_ref().unwrap_or(output);
        if let Some((size, expected)) = entry.file {
            let source = directory(inputs, &source_name(entry.source))?;
            let (parent, name_in_source) = if entry.original.is_empty() {
                ("", INPUT_FILE)
            } else {
                parts(&entry.original)
            };
            let source_parent = descend(&source, parent)?;
            let local_tree::Entry::File(file) = local_tree::entry(
                source_parent.as_ref().unwrap_or(&source),
                OsStr::new(name_in_source),
            )?
            else {
                return Err(invalid_source());
            };
            require_single_link(&file)?;
            let stamp = FileStamp::read(&file)?;
            let mut stage = if private {
                target.create_file(OsStr::new(name))?
            } else {
                target.create_ordinary_file(OsStr::new(name))?
            };
            let reader = CheckedCopy {
                file: &file,
                output: &mut stage,
                stopped,
            };
            if protocol::hash(reader, size)? != expected {
                return Err(invalid_source());
            }
            stamp.require_unchanged(&file)?;
            stage.sync_all()?;
        } else {
            if private {
                target.create_child(OsStr::new(name))?;
            } else {
                target.create_transfer_child(OsStr::new(name))?;
            }
        }
    }
    #[cfg(unix)]
    output.file().sync_all()?;
    Ok(())
}

fn source_name(index: usize) -> OsString {
    format!("source-{index}").into()
}

fn parts(relative: &str) -> (&str, &str) {
    relative.rsplit_once('/').unwrap_or(("", relative))
}

fn descend(root: &local_tree::Directory, path: &str) -> io::Result<Option<local_tree::Directory>> {
    let mut current = None;
    for component in path.split('/').filter(|component| !component.is_empty()) {
        current = Some(directory(
            current.as_ref().unwrap_or(root),
            OsStr::new(component),
        )?);
    }
    Ok(current)
}

fn directory(parent: &local_tree::Directory, name: &OsStr) -> io::Result<local_tree::Directory> {
    if let local_tree::Entry::Directory(child) = local_tree::entry(parent, name)? {
        Ok(child)
    } else {
        Err(invalid_source())
    }
}

struct CheckedCopy<'a, F> {
    file: &'a File,
    output: &'a mut File,
    stopped: &'a F,
}

impl<F: Fn() -> bool> Read for CheckedCopy<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        require_running(self.stopped)?;
        let count = self.file.read(bytes)?;
        self.output.write_all(&bytes[..count])?;
        Ok(count)
    }
}

fn invalid_source() -> io::Error {
    io::Error::other(SOURCE_CHANGED)
}

fn stage_failed() -> Failure {
    Failure::failed(STAGE_FAILED)
}

fn vacant_local_changed() -> Failure {
    Failure::blocked("The reviewed vacant local rsync destination changed or is unavailable.")
}

fn success(id: RunId, detail: String) -> Completion {
    let mut completion = Completion::from_result(id, Ok(0));
    completion.detail = Some(detail);
    completion
}
