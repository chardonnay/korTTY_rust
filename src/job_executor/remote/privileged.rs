use std::cell::Cell;
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use kortty::ssh_runtime::password_file::{DEADLINE, TRANSFER_DEADLINE};
use kortty_jobs::{Cancellation, RunOutcome, RunRequest, RunTrigger};
use kortty_server_archive::rsync_tree::Manifest;
use kortty_ssh::{
    ConnectOptions, Endpoint, ExecConnection, HostKey, SensitiveLine, SshSession, TrustStore,
};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store as ConnectionStore;
use crate::job_executor::{Credentials, connection};
use crate::profile;
use crate::scheduler_window::{ConnectionRef, SshCommand, SudoMode, privileged, sync};
use crate::sftp_window::{
    ArchiveUpdateFailure, LocalSource, PrivilegedArchiveFailure, ReviewedFilePublication,
    ReviewedFileReceiver, StagedDownload, exchange_privileged_archive_update,
    exchange_privileged_server_archive, privileged_file as file,
    privileged_file_replace as upload_replace,
};
use crate::ssh_connection::ExecOpener;

use super::server_archive::{
    LocalCopy, publish_verified_copy, update_completion, update_uncertain_completion,
};
use super::{Completion, ExecutionPolicy, Failure, SavedAuth, check_cancelled};

const APPROVAL_REQUIRED: &str = "Review the privileged transfer and its pinned SSH route first.";
const DENIED: &str = "Managed policy forbids this privileged file transfer.";
const DESTINATION_CHANGED: &str =
    "Review the local replacement destination again. Its file, path or metadata changed.";
const TREE_REVIEW: &str =
    "Review the exact privileged folder transfer and its pinned SSH route first.";
const FILE_ACTION_REVIEW: &str =
    "Review the privileged file action and its pinned SSH route first.";
const FILE_ACTION_DENIED: &str = "Managed policy forbids this privileged file action.";
const ARCHIVE_REVIEW: &str = "Review the privileged archive and its pinned SSH route first.";
const ARCHIVE_DENIED: &str = "Managed policy forbids this privileged archive action.";
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(1800);

enum LocalFile {
    Download {
        stage: StagedDownload,
        replacement: Option<Box<ReviewedFileReceiver>>,
    },
    Upload(LocalSource),
    ReplaceUpload(upload_replace::Source),
}

pub(super) fn run_archive(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    archive: &privileged::PrivilegedArchive,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
    password: Option<&str>,
) -> Result<Completion, Failure> {
    let spec = archive.archive.server_spec().map_err(Failure::blocked)?;
    let copies_archive = spec.report_payload;
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(|| Failure::blocked(ARCHIVE_REVIEW))?;
    let context = policy
        .privileged
        .clone()
        .ok_or_else(|| Failure::blocked(ARCHIVE_REVIEW))?;
    if !policy.sudo_allowed
        || (copies_archive && !policy.file_transfer_allowed)
        || !privileged_allowed_now(reference, copies_archive)
    {
        return Err(Failure::blocked(ARCHIVE_DENIED));
    }
    let trust = trust_store().map_err(|_| Failure::blocked(ARCHIVE_REVIEW))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(ARCHIVE_REVIEW))?;
    let review_action = Arc::clone(&request.action);
    let review_policy = policy.clone();
    let review_reference = reference.clone();
    let review_archive = archive.clone();
    let review_pins = pins.clone();
    let review_current = move || {
        review_policy.require_target_groups(&review_action).is_ok()
            && profile::connections_path()
                .and_then(ConnectionStore::open)
                .is_ok_and(|store| {
                    store
                        .get(review_reference.id)
                        .map(ConnectionRef::from)
                        .as_ref()
                        == Some(&review_reference)
                })
            && sync::pins(&review_reference, &trust).is_ok_and(|current| current == review_pins)
            && context
                .authorize_archive(
                    &review_archive,
                    &review_reference,
                    review_action.sudo,
                    &review_pins,
                )
                .is_ok()
    };
    if !review_current() {
        return Err(Failure::blocked(ARCHIVE_REVIEW));
    }
    check_cancelled(&request.cancellation)?;
    let mut saved_auth = saved_auth.map_err(Failure::blocked)?;
    let sudo_password = match authorization(request.action.sudo, saved_auth.as_mut())? {
        file::Auth::Passwordless => None,
        file::Auth::Password(line) => Some(line),
    };
    let deadline = Instant::now() + ARCHIVE_TIMEOUT;
    let checked_reference = reference.clone();
    let cancellation = request.cancellation.clone();
    let stopped = move || {
        cancellation.is_requested()
            || Instant::now() >= deadline
            || !privileged_allowed_now(&checked_reference, copies_archive)
    };
    let reuse = archive.archive.reuse_output;
    let local_update = archive
        .archive
        .download
        .as_deref()
        .filter(|_| reuse)
        .map(|path| LocalCopy::prepare(path, true, &stopped))
        .transpose()?;
    let local = archive
        .archive
        .download
        .as_deref()
        .filter(|_| !reuse)
        .map(StagedDownload::create)
        .transpose()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Failure::failed("The local archive destination already exists.")
            } else {
                Failure::failed("Could not create a private local archive stage.")
            }
        })?;
    let writer = local
        .as_ref()
        .map(StagedDownload::writer)
        .transpose()
        .map_err(|_| Failure::failed("The private local archive stage was unavailable."))?;
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let reviewed = review_current.clone();
    let authorized_reference = reference.clone();
    let authorize = move || {
        if reviewed() && privileged_allowed_now(&authorized_reference, copies_archive) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                ARCHIVE_REVIEW,
            ))
        }
    };
    let pinned =
        TrustStore::from_pins(pins.clone()).map_err(|_| Failure::blocked(ARCHIVE_REVIEW))?;
    let tcp = crate::ssh_connection::connect_transport(
        &jumps.first().unwrap_or(&target).endpoint,
        &stopped,
    )
    .map_err(|error| super::connect_failure(error.into()))?;
    let connection = ExecConnection::connect_preconnected(
        tcp,
        (target, jumps),
        pinned,
        ConnectOptions::default(),
        &stopped,
        &authorize,
    )
    .map_err(super::connect_failure)?;
    if reuse {
        let writer = local_update.as_ref().map(LocalCopy::writer).transpose()?;
        let result = connection
            .exec_opener()
            .ok_or(ArchiveUpdateFailure::BeforePublication)
            .and_then(|opener| {
                exchange_privileged_archive_update(
                    &opener,
                    &spec,
                    sudo_password,
                    password,
                    writer,
                    stopped.clone(),
                    authorize.clone(),
                )
            });
        drop(connection);
        return match result {
            Ok(publication) => {
                let copy = local_update.map(|local| {
                    if publication.copy_verified {
                        publish_verified_copy(
                            local,
                            publication.payload,
                            &request.cancellation,
                            &stopped,
                            &authorize,
                        )
                    } else {
                        Err(copy_failure(
                            &request.cancellation,
                            &authorize,
                            "The server did not return a complete verified local copy.",
                        )
                        .into())
                    }
                });
                Ok(update_completion(
                    request,
                    copy,
                    publication.recovery.as_deref(),
                ))
            }
            Err(ArchiveUpdateFailure::Uncertain { recovery }) => {
                Ok(update_uncertain_completion(request, recovery.as_deref()))
            }
            Err(error) => {
                check_cancelled(&request.cancellation)?;
                if !privileged_allowed_now(reference, copies_archive) {
                    return Err(Failure::blocked(ARCHIVE_DENIED));
                }
                authorize().map_err(|_| Failure::blocked(ARCHIVE_REVIEW))?;
                Err(match error {
                    ArchiveUpdateFailure::Refused => Failure::failed(
                        "The privileged archive helper refused the update. Check the source, output and configured roots.",
                    ),
                    _ => Failure::blocked(
                        "Could not review the privileged archive update. Check the installed helper and sudo rule for the selected mode.",
                    ),
                })
            }
        };
    }
    let result = connection
        .exec_opener()
        .ok_or(PrivilegedArchiveFailure::BeforeCommit)
        .and_then(|opener| {
            exchange_privileged_server_archive(
                &opener,
                &spec,
                sudo_password,
                password,
                writer,
                stopped.clone(),
                authorize.clone(),
            )
        });
    drop(connection);
    match result {
        Ok(()) => {
            if let Some(stage) = local {
                let result = stage.publish_checked(|_| {
                    if stopped() {
                        return Err(io::Error::other("The archive copy stopped."));
                    }
                    authorize()
                });
                let result = result.map_err(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        Failure::failed("The local archive destination already exists.")
                    } else {
                        copy_failure(
                            &request.cancellation,
                            &authorize,
                            "The verified copy could not be published. Check the local destination before retrying.",
                        )
                    }
                });
                return Ok(super::server_archive::copy_completion(request, result));
            }
            let mut completion = Completion::from_result(request.id, Ok(0));
            completion.detail =
                Some("Created the reviewed privileged archive on the server.".into());
            Ok(completion)
        }
        Err(PrivilegedArchiveFailure::CopyFailed) => Ok(super::server_archive::copy_completion(
            request,
            Err(copy_failure(
                &request.cancellation,
                &authorize,
                "The server did not return a complete verified local copy.",
            )),
        )),
        Err(PrivilegedArchiveFailure::Uncertain) => Err(Failure::failed(
            "The server may have published the privileged archive. Check the destination before retrying.",
        )),
        Err(error) => {
            check_cancelled(&request.cancellation)?;
            if !privileged_allowed_now(reference, copies_archive) {
                return Err(Failure::blocked(ARCHIVE_DENIED));
            }
            if !review_current() {
                return Err(Failure::blocked(ARCHIVE_REVIEW));
            }
            if Instant::now() >= deadline {
                return Err(Failure::failed("The privileged archive review timed out."));
            }
            Err(match error {
                PrivilegedArchiveFailure::Refused => Failure::failed(
                    "The privileged helper refused the archive. Check the configured roots, source and destination.",
                ),
                _ => Failure::blocked(
                    "Could not review the privileged archive. Check the installed helper and sudo rule for the selected mode.",
                ),
            })
        }
    }
}

fn copy_failure(
    cancellation: &Cancellation,
    authorize: &impl Fn() -> io::Result<()>,
    detail: &'static str,
) -> Failure {
    if let Err(error) = check_cancelled(cancellation) {
        return error;
    }
    if authorize().is_err() {
        return Failure::blocked("The local archive copy is no longer authorized.");
    }
    Failure::failed(detail)
}

pub(super) fn run_file_action(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    action: &privileged::PrivilegedFileAction,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(|| Failure::blocked(FILE_ACTION_REVIEW))?;
    let context = policy
        .privileged
        .as_ref()
        .ok_or_else(|| Failure::blocked(FILE_ACTION_REVIEW))?;
    if !policy.sudo_allowed || !privileged_allowed_now(reference, false) {
        return Err(Failure::blocked(FILE_ACTION_DENIED));
    }
    let mut saved_auth = saved_auth.map_err(Failure::blocked)?;
    let auth = authorization(request.action.sudo, saved_auth.as_mut())?;
    let trust = trust_store().map_err(|_| Failure::blocked(FILE_ACTION_REVIEW))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(FILE_ACTION_REVIEW))?;
    let review_current = || {
        policy.require_target_groups(&request.action).is_ok()
            && profile::connections_path()
                .and_then(ConnectionStore::open)
                .is_ok_and(|store| {
                    store.get(reference.id).map(ConnectionRef::from).as_ref() == Some(reference)
                })
            && sync::pins(reference, &trust).is_ok_and(|current| current == pins)
            && context
                .authorize_file_action(action, reference, request.action.sudo, &pins)
                .is_ok()
    };
    if !review_current() {
        return Err(Failure::blocked(FILE_ACTION_REVIEW));
    }
    let cancellation = &request.cancellation;
    check_cancelled(cancellation)?;
    let deadline = Instant::now() + DEADLINE;
    let checked_reference = reference.clone();
    let stopped = cancellation.clone();
    let allowed = move || {
        !stopped.is_requested()
            && Instant::now() < deadline
            && privileged_allowed_now(&checked_reference, false)
    };
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let mut session =
        super::sync::open_pinned_for_job(target, jumps, &pins, || !allowed(), request, policy)?;
    let result = tree_opener(&session).and_then(|opener| {
        file::apply_file_action(
            opener,
            action.path.as_str(),
            &action.operation,
            auth,
            allowed,
            review_current,
        )
        .map_err(|error| {
            if error == file::Failure::Uncertain {
                return Failure::failed(
                    "The server may have applied the privileged action. Verify the path before retrying.",
                );
            }
            if let Err(cancelled) = check_cancelled(cancellation) {
                return cancelled;
            }
            if !privileged_allowed_now(reference, false) {
                return Failure::blocked(FILE_ACTION_DENIED);
            }
            if !review_current() {
                return Failure::blocked(FILE_ACTION_REVIEW);
            }
            match error {
                file::Failure::Conflict => {
                    Failure::failed("The reviewed path changed or the destination already exists.")
                }
                file::Failure::Blocked => {
                    Failure::blocked("The sudo credential or protected Linux path was rejected.")
                }
                _ => Failure::failed("The privileged file action did not complete."),
            }
        })
    });
    let _ = session.close();
    result?;
    let mut completion = Completion::from_result(request.id, Ok(0));
    completion.detail = Some("Applied the reviewed privileged file action.".into());
    Ok(completion)
}

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    transfer: &privileged::PrivilegedTransfer,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(|| Failure::blocked(APPROVAL_REQUIRED))?;
    let context = policy
        .privileged
        .as_ref()
        .ok_or_else(|| Failure::blocked(APPROVAL_REQUIRED))?;
    if !policy.file_transfer_allowed || !policy.sudo_allowed || !allowed_now(reference) {
        return Err(Failure::blocked(DENIED));
    }
    let cancellation = &request.cancellation;
    let deadline = Instant::now() + TRANSFER_DEADLINE;
    let mut saved_auth = saved_auth.map_err(Failure::blocked)?;
    let auth = authorization(request.action.sudo, saved_auth.as_mut())?;
    let trust = trust_store().map_err(|_| Failure::blocked(APPROVAL_REQUIRED))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(APPROVAL_REQUIRED))?;
    let receipt = context
        .authorize(transfer, reference, request.action.sudo, &pins)
        .map_err(|_| Failure::blocked(APPROVAL_REQUIRED))?;
    let claimed = Arc::new(AtomicBool::new(false));
    let review_current = {
        let policy = policy.clone();
        let action = Arc::clone(&request.action);
        let transfer = transfer.clone();
        let reference = reference.clone();
        let context = context.clone();
        let pins = pins.clone();
        let receipt = receipt.clone();
        let claimed = Arc::clone(&claimed);
        move || {
            policy.require_target_groups(&action).is_ok()
                && sync::pins(&reference, &trust).is_ok_and(|current| current == pins)
                && (!transfer.replace_existing || context.saved_route_current(&reference))
                && if claimed.load(Ordering::Acquire) {
                    context.require_claimed(&receipt).is_ok()
                } else {
                    context
                        .authorize(&transfer, &reference, action.sudo, &pins)
                        .is_ok_and(|current| current == receipt)
                }
        }
    };
    let gate_error = || gate_failure(cancellation, reference, deadline);
    let review_error = || {
        gate_error().or_else(|| (!review_current()).then(|| Failure::blocked(APPROVAL_REQUIRED)))
    };
    if !review_current() {
        return Err(Failure::blocked(APPROVAL_REQUIRED));
    }
    if !file::valid_path(transfer.remote.as_str()) {
        return Err(Failure::blocked(
            "Choose a canonical absolute Linux file path.",
        ));
    }
    check_cancelled(cancellation)?;
    let checked_reference = reference.clone();
    let stopped = cancellation.clone();
    let allowed = move || {
        !stopped.is_requested() && Instant::now() < deadline && allowed_now(&checked_reference)
    };
    let local = match transfer.direction {
        privileged::PrivilegedDirection::Download if transfer.replace_existing => {
            let receiver = ReviewedFileReceiver::open_transfer(&transfer.local, &|| !allowed())
                .map_err(|_| {
                    gate_error().unwrap_or_else(|| Failure::blocked(DESTINATION_CHANGED))
                })?;
            if receiver.fingerprint() != receipt.proof() {
                return Err(Failure::blocked(DESTINATION_CHANGED));
            }
            let stage = receiver
                .stage()
                .map_err(|_| Failure::blocked(DESTINATION_CHANGED))?;
            LocalFile::Download {
                stage,
                replacement: Some(Box::new(receiver)),
            }
        }
        privileged::PrivilegedDirection::Download => LocalFile::Download {
            stage: StagedDownload::create(&transfer.local)
                .map_err(|_| Failure::blocked("Choose an absent local file destination."))?,
            replacement: None,
        },
        privileged::PrivilegedDirection::Upload if transfer.replace_existing => {
            let source =
                upload_replace::Source::open(&transfer.local, receipt.max_bytes(), &allowed)
                    .map_err(|_| {
                        gate_error().unwrap_or_else(|| {
                        Failure::blocked("Choose a single-link regular upload source within the approved size limit.")
                        })
                    })?;
            LocalFile::ReplaceUpload(source)
        }
        privileged::PrivilegedDirection::Upload => {
            let source = LocalSource::open(&transfer.local)
                .map_err(|_| Failure::blocked("Choose a regular local upload source."))?;
            if source.size() > file::MAX_TRANSFER_BYTES {
                return Err(Failure::blocked(
                    "Privileged transfers are limited to 1 GiB.",
                ));
            }
            crate::private_files::require_single_link(source.file()).map_err(|_| {
                Failure::blocked("The local upload source must have a single link.")
            })?;
            LocalFile::Upload(source)
        }
    };
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    if let Some(failure) = review_error() {
        return Err(failure);
    }
    let opened =
        super::sync::open_pinned_for_job(target, jumps, &pins, || !allowed(), request, policy);
    if let Some(failure) = review_error() {
        return Err(failure);
    }
    let mut session = opened?;
    let opener = session.exec_opener().ok_or_else(|| {
        Failure::blocked("The authenticated SSH route cannot open the fixed file helper.")
    })?;
    let succeeded = |detail| {
        let mut completion = Completion::from_result(request.id, Ok(0));
        completion.detail = Some(detail);
        completion
    };
    let claim = |bytes| {
        context.claim_replacement(&receipt, bytes, &|| !allowed())?;
        claimed.store(true, Ordering::Release);
        Ok(())
    };
    let finish = |successor| {
        if claimed.load(Ordering::Acquire) {
            context.finish_replacement(&receipt, successor)
        } else {
            Ok(())
        }
    };
    let outcome = match local {
        LocalFile::Download { stage, replacement } => (|| {
            let output = stage
                .writer()
                .map_err(|_| Failure::failed("The private download stage is unavailable."))?;
            let (bytes, digest) = file::download(
                ExecOpener::Direct(opener),
                transfer.remote.as_str(),
                output,
                receipt.max_bytes(),
                auth,
                allowed.clone(),
            )
            .map_err(|error| transfer_failure(error, cancellation, reference, deadline))?;
            if let Some(receiver) = replacement {
                let incoming = Manifest::regular_file(bytes, digest);
                let stopped = || !allowed();
                let authorize = || {
                    if review_current() {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            APPROVAL_REQUIRED,
                        ))
                    }
                };
                let publication =
                    receiver.publish(stage, incoming, &stopped, &authorize, |_| claim(bytes));
                return replacement_completion(request, bytes, publication, review_error, finish);
            }
            stage
                .publish(|| !allowed() || !review_current())
                .map_err(|_| {
                    review_error().unwrap_or_else(|| {
                        Failure::failed(
                            "Could not publish the verified download to a new local file.",
                        )
                    })
                })?;
            Ok(succeeded(format!(
                "Privileged download published {bytes} bytes."
            )))
        })(),
        LocalFile::Upload(source) => file::upload(
            ExecOpener::Direct(opener),
            transfer.remote.as_str(),
            &source,
            auth,
            allowed.clone(),
            &review_current,
        )
        .map(|bytes| succeeded(format!("Privileged upload published {bytes} bytes.")))
        .map_err(|error| {
            if error == file::Failure::Blocked && !review_current() {
                Failure::blocked(APPROVAL_REQUIRED)
            } else {
                transfer_failure(error, cancellation, reference, deadline)
            }
        }),
        LocalFile::ReplaceUpload(source) => {
            let publication_allowed = {
                let allowed = allowed.clone();
                let review_current = review_current.clone();
                Arc::new(move || allowed() && review_current())
            };
            let upload = upload_replace::Upload {
                path: transfer.remote.as_str(),
                source: &source,
                proof: receipt.proof(),
                recurring: transfer.standing.is_some(),
                transfer_allowed: Arc::new(allowed.clone()),
                publication_allowed,
            };
            match upload_replace::upload(&opener, sudo_password(auth), upload, || {
                claim(source.payload.bytes)
            }) {
                Ok(published) => Ok(confirmed_replacement(
                    request,
                    format!(
                        "Replaced the reviewed server file with {} verified bytes. Original retained at {:?}.",
                        source.payload.bytes, published.recovery,
                    ),
                    finish(published.successor),
                )),
                Err(upload_replace::Failure::Uncertain(recovery)) => {
                    let mut completion = Completion::from_result(
                        request.id,
                        Err(Failure::failed(
                            "The protected upload outcome is uncertain. Check the server destination before retrying.",
                        )),
                    );
                    if let Some(recovery) = recovery {
                        completion.append_detail(&format!("Remote recovery path {recovery:?}"));
                    }
                    Ok(completion)
                }
                Err(failure) => {
                    let reason = review_error().unwrap_or_else(|| {
                        Failure::blocked(match failure {
                            upload_replace::Failure::Changed => "Review the server destination and local upload source again. The approved transfer changed.",
                            _ => "The protected upload was refused. Check sudo authorization, the installed helper and the server replacement roots.",
                        })
                    });
                    if matches!(failure, upload_replace::Failure::Refused) {
                        // A clean, confirmed refusal cannot advance the reviewed receiver.
                        let _ = finish(None);
                    }
                    Err(reason)
                }
            }
        }
    };
    let _ = session.close();
    outcome
}

pub(crate) fn preview_file_upload(
    request: &privileged::UploadPreview,
    mut credentials: Credentials,
    cancelled: Arc<AtomicBool>,
    trust: TrustStore,
) -> Result<privileged::ReviewedDestination, String> {
    let pins = sync::pins(&request.reference, &trust)
        .map_err(|_| "Verify the SSH host keys for the complete saved route before review.")?;
    if sync::fingerprints(&pins) != request.pins {
        return Err("The verified SSH host keys changed. Review the route again.".into());
    }
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    let reference = request.reference.clone();
    let checked_pins = pins.clone();
    let connections = request.connections.clone();
    let allowed = move || {
        !cancelled.load(Ordering::Acquire) && Instant::now() < deadline && allowed_now(&reference)
    };
    let limit = request
        .standing
        .map_or(file::MAX_TRANSFER_BYTES, |settings| {
            settings.limits.max_bytes
        });
    let source = upload_replace::Source::open(&request.local, limit, &allowed)
        .map_err(|error| format!("Could not review the local upload source. {error}"))?;
    let password = sudo_password(
        authorization(request.sudo, Some(&mut credentials)).map_err(|error| error.detail)?,
    );
    let permitted = {
        let reference = request.reference.clone();
        Arc::new(move || {
            allowed()
                && sync::pins(&reference, &trust).is_ok_and(|current| current == checked_pins)
                && privileged::saved_route_current(&connections, &reference)
        })
    };
    let (target, jumps) = credentials.into_route().map_err(|error| error.detail)?;
    let mut session = super::sync::open_pinned(target, jumps, &pins, || !permitted())
        .map_err(|error| error.detail)?;
    let result = session
        .exec_opener()
        .ok_or_else(|| io::Error::other("Missing SSH exec channel"))
        .and_then(|opener| {
            upload_replace::review(
                &opener,
                request.path.as_str(),
                &source,
                request.standing.is_some(),
                password,
                permitted,
            )
        });
    let _ = session.close();
    let review = result.map_err(|_| "Could not review the protected server file. Check sudo authorization, the installed helper and the server replacement roots.")?;
    Ok(privileged::ReviewedDestination {
        fingerprint: review.proof,
        bytes: review.bytes,
        source_bytes: Some(source.payload.bytes),
    })
}

fn sudo_password(auth: file::Auth) -> Option<SensitiveLine> {
    match auth {
        file::Auth::Passwordless => None,
        file::Auth::Password(line) => Some(line),
    }
}

fn replacement_completion(
    request: &RunRequest<SshCommand>,
    bytes: u64,
    publication: io::Result<ReviewedFilePublication>,
    review_error: impl FnOnce() -> Option<Failure>,
    finish: impl FnOnce(Option<[u8; 32]>) -> io::Result<()>,
) -> Result<Completion, Failure> {
    match publication {
        Ok(ReviewedFilePublication::Published {
            recovery,
            successor,
        }) => {
            let detail = format!(
                "Replaced the reviewed local file with {bytes} verified bytes. Original retained at {recovery:?}."
            );
            Ok(confirmed_replacement(
                request,
                detail,
                finish(Some(successor.fingerprint)),
            ))
        }
        Ok(ReviewedFilePublication::Uncertain { error, recovery }) => {
            let mut completion = Completion::from_result(
                request.id,
                Err(Failure::failed(
                    "The replacement outcome is uncertain. Check the destination before retrying.",
                )),
            );
            completion.append_detail(&error.to_string());
            if let Some(recovery) = recovery {
                completion.append_detail(&format!("Local recovery path {recovery:?}"));
            }
            Ok(completion)
        }
        result => {
            let failure = review_error().unwrap_or_else(|| {
                Failure::failed("Could not publish the reviewed local replacement.")
            });
            let error = match result {
                Ok(ReviewedFilePublication::Restored { error }) => {
                    let _ = finish(None);
                    error
                }
                Err(error) => error,
                _ => unreachable!("published and uncertain replacements were handled"),
            };
            let mut completion = Completion::from_result(request.id, Err(failure));
            completion.append_detail(&error.to_string());
            Ok(completion)
        }
    }
}

fn confirmed_replacement(
    request: &RunRequest<SshCommand>,
    detail: String,
    saved: io::Result<()>,
) -> Completion {
    if saved.is_err() {
        let mut completion = Completion::from_result(
            request.id,
            Err(Failure::failed(
                "The replacement was published but its successor could not be saved. Review the destination again.",
            )),
        );
        completion.append_detail(&detail);
        completion
    } else {
        let mut completion = Completion::from_result(request.id, Ok(0));
        completion.detail = Some(detail);
        completion
    }
}

pub(super) fn preview_tree(
    request: sync::PreviewRequest,
    mut credentials: Credentials,
    pins: &[(Endpoint, HostKey)],
) -> Result<sync::Review, String> {
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    let reference = request.connection;
    let cancelled = request.cancelled;
    let allowed = move || {
        !cancelled.load(Ordering::Acquire) && Instant::now() < deadline && allowed_now(&reference)
    };
    let auth = authorization(request.sudo, Some(&mut credentials)).map_err(|error| error.detail)?;
    let copy = request.copy;
    let mut plan = (copy.direction == sync::Direction::Upload)
        .then(|| {
            file::TreeUpload::scan(&copy.local, copy.remote.as_str(), &copy.excludes, &allowed)
        })
        .transpose()
        .map_err(tree_detail)?;
    if copy.standing.is_some()
        && let Some(plan) = &mut plan
    {
        plan.request_receiver_receipt();
        if !copy.replace_changed {
            plan.request_root_creation();
        }
    }
    let (target, jumps) = credentials.into_route().map_err(|error| error.detail)?;
    let mut session = super::sync::open_pinned(target, jumps, pins, || !allowed())
        .map_err(|error| error.detail)?;
    let opener = tree_opener(&session).map_err(|error| error.detail)?;
    let preview = if let Some(plan) = plan {
        if copy.replace_changed || copy.standing.is_some() {
            file::preview_tree_upload_merge(opener, copy.remote.as_str(), &plan, auth, allowed)
        } else {
            file::inspect_tree(opener, copy.remote.as_str(), auth, allowed)
                .and_then(|witness| plan.preview(&witness))
                .map(|preview| (preview, None))
        }
    } else {
        file::preview_tree_download(
            opener,
            copy.remote.as_str(),
            &copy.local,
            auth,
            allowed,
            copy.replace_changed,
            copy.standing.is_some(),
        )
    };
    let _ = session.close();
    let (preview, receiver) = preview.map_err(tree_detail)?;
    Ok(sync::Review {
        preview,
        pins: sync::fingerprints(pins),
        standing_roots: copy.standing.is_some().then_some(receiver).flatten(),
    })
}

pub(super) fn run_tree(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    copy: &sync::Copy,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    if !matches!(
        request.trigger,
        RunTrigger::Manual | RunTrigger::Scheduled | RunTrigger::Dependency
    ) {
        return Err(Failure::blocked(TREE_REVIEW));
    }
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(|| Failure::blocked(TREE_REVIEW))?;
    let context = policy
        .sync
        .as_ref()
        .filter(|context| context.permits(request))
        .ok_or_else(|| Failure::blocked(TREE_REVIEW))?;
    if !policy.file_transfer_allowed || !policy.sudo_allowed || !allowed_now(reference) {
        return Err(Failure::blocked(DENIED));
    }
    let standing = context.standing_approval();
    if standing.is_some() != copy.standing.is_some()
        || (request.trigger == RunTrigger::Dependency && standing.is_none())
    {
        return Err(Failure::blocked(TREE_REVIEW));
    }
    let tree_receipt = standing
        .map(|(captured, admission)| {
            let receipt = context.store.load_tree(
                &request.job,
                copy.approval.as_deref(),
                context.scope,
                std::time::SystemTime::now(),
                Some(admission),
            )?;
            if receipt.approval != *captured {
                return Err(io::Error::other(TREE_REVIEW));
            }
            Ok(receipt)
        })
        .transpose()
        .map_err(|_| Failure::blocked(TREE_REVIEW))?;
    let receipt = if tree_receipt.is_some() {
        None
    } else {
        Some(
            context
                .load_receipt(copy.approval.as_deref())
                .map_err(|_| Failure::blocked(TREE_REVIEW))?,
        )
    };
    let trust = trust_store().map_err(|_| Failure::blocked(TREE_REVIEW))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(TREE_REVIEW))?;
    let reviewed_pins = tree_receipt
        .as_ref()
        .map(|receipt| &receipt.approval.pins)
        .or_else(|| receipt.as_ref().map(|receipt| &receipt.pins))
        .ok_or_else(|| Failure::blocked(TREE_REVIEW))?;
    if &sync::fingerprints(&pins) != reviewed_pins {
        return Err(Failure::blocked(TREE_REVIEW));
    }
    let deadline = Instant::now() + TRANSFER_DEADLINE;
    let cancellation = &request.cancellation;
    let mut saved_auth = saved_auth.map_err(Failure::blocked)?;
    let auth = authorization(request.action.sudo, saved_auth.as_mut())?;
    let checked_reference = reference.clone();
    let stopped = cancellation.clone();
    let allowed = move || {
        !stopped.is_requested() && Instant::now() < deadline && allowed_now(&checked_reference)
    };
    let gate_error = || gate_failure(cancellation, reference, deadline);
    let mut plan = (copy.direction == sync::Direction::Upload)
        .then(|| {
            file::TreeUpload::scan(&copy.local, copy.remote.as_str(), &copy.excludes, &allowed)
        })
        .transpose()
        .map_err(|error| gate_error().unwrap_or_else(|| Failure::blocked(tree_detail(error))))?;
    if copy.standing.is_some()
        && let Some(plan) = &mut plan
    {
        plan.request_receiver_receipt();
        if tree_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.create_root)
        {
            plan.request_root_creation();
        }
    }
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let mut session =
        super::sync::open_pinned_for_job(target, jumps, &pins, || !allowed(), request, policy)?;
    let claimed = Cell::new(false);
    let pins_current = || {
        policy.require_target_groups(&request.action).is_ok()
            && sync::pins(reference, &trust).is_ok_and(|current| current == pins)
            && (!(copy.replace_changed || standing.is_some())
                || privileged::saved_route_current(
                    &crate::profile::connections_path_at(context.store.profile()),
                    reference,
                ))
    };
    let claim = |proof| {
        let Some(receipt) = receipt.as_ref() else {
            return Err(file::Failure::Blocked);
        };
        if proof != receipt.proof || !pins_current() {
            return Err(file::Failure::Conflict);
        }
        context
            .store
            .claim(&request.job, receipt)
            .map_err(|_| file::Failure::Blocked)?;
        claimed.set(true);
        Ok(())
    };
    let publication_allowed = || {
        pins_current()
            && if let (Some(receipt), Some((_, admission))) = (&tree_receipt, standing) {
                context
                    .store
                    .require_tree_claimed(&request.job, receipt, admission.firing())
                    .is_ok()
            } else {
                receipt.as_ref().is_some_and(|receipt| {
                    context.store.require_claimed(&request.job, receipt).is_ok()
                })
            }
    };
    let opener = tree_opener(&session)?;
    let failure = |error| {
        if error == file::Failure::Uncertain {
            Failure::failed(tree_detail(error))
        } else if let Some(failure) = gate_error() {
            failure
        } else if claimed.get() {
            Failure::failed(tree_detail(error))
        } else {
            Failure::blocked(tree_detail(error))
        }
    };
    if copy.replace_changed || standing.is_some() {
        let claim_merge = |review: file::MergeClaim| {
            if let (Some(receipt), Some((_, admission))) = (&tree_receipt, standing) {
                if review.receiver != Some(receipt.receiver) || !pins_current() {
                    return Err(file::Failure::Conflict);
                }
                context
                    .store
                    .arm_tree(
                        &request.job,
                        receipt,
                        admission,
                        (review.actions, review.bytes),
                        &|| {
                            cancellation.is_requested()
                                || Instant::now() >= deadline
                                || !allowed_now(reference)
                        },
                    )
                    .map_err(|_| file::Failure::Blocked)?;
                claimed.set(true);
                Ok(())
            } else {
                claim(review.proof)
            }
        };
        let merged = if let Some(plan) = plan {
            file::upload_tree_merge(
                opener,
                copy.remote.as_str(),
                &plan,
                auth,
                allowed,
                claim_merge,
                publication_allowed,
            )
        } else if tree_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.create_root)
        {
            file::download_tree_create(
                opener,
                copy.remote.as_str(),
                &copy.local,
                auth,
                allowed,
                claim_merge,
                publication_allowed,
            )
        } else {
            file::download_tree_merge(
                opener,
                copy.remote.as_str(),
                &copy.local,
                auth,
                allowed,
                claim_merge,
                publication_allowed,
            )
        };
        let _ = session.close();
        let merged = merged.map_err(failure)?;
        let outcome = if let Some(error) = merged.error {
            Err(failure(error))
        } else if let (Some(receipt), Some((_, admission))) = (&tree_receipt, standing) {
            merged.receiver.ok_or_else(|| io::Error::other("Missing protected folder completion receipt"))
                .and_then(|receiver| context.store.finish_tree(&request.job, receipt, admission.firing(), receiver))
                .map(|()| 0).map_err(|_| Failure::failed("The protected folder was published but its completion could not be saved. Review the receiver again."))
        } else {
            Ok(0)
        };
        let mut completed = Completion::from_result(request.id, outcome);
        completed.append_detail(&format!(
            "Published {} new folder entries and replaced {} files ({} bytes).",
            merged.created, merged.replaced, merged.bytes
        ));
        if let Some(prefix) = merged.recovery_prefix {
            if copy.direction == sync::Direction::Upload {
                completed.append_detail(&format!(
                    "Check for originals under {} in private directories matching .kortty-{prefix}*.kortty-part. Each contains original and replaced-name.",
                    copy.remote.as_str()
                ));
            } else {
                completed.append_detail(&format!("Originals are retained in recovery directories matching .kortty-{prefix}*.kortty-part."));
            }
        }
        return Ok(completed);
    }
    let result = if let Some(plan) = plan {
        file::upload_tree(
            opener,
            copy.remote.as_str(),
            &plan,
            auth,
            allowed,
            claim,
            publication_allowed,
        )
        .map(|()| plan.totals())
    } else {
        file::download_tree(
            opener,
            copy.remote.as_str(),
            &copy.local,
            auth,
            allowed,
            claim,
            publication_allowed,
        )
    };
    let _ = session.close();
    let (entries, bytes) = result.map_err(failure)?;
    let mut completion = Completion::from_result(request.id, Ok(0));
    completion.detail = Some(format!(
        "Published one new privileged folder with {entries} entries and {bytes} bytes."
    ));
    Ok(completion)
}

fn tree_opener(session: &SshSession) -> Result<ExecOpener, Failure> {
    session
        .exec_opener()
        .map(ExecOpener::Direct)
        .ok_or_else(|| {
            Failure::blocked("The authenticated SSH route cannot open the fixed file helper.")
        })
}

fn tree_detail(error: file::Failure) -> &'static str {
    match error {
        file::Failure::Blocked => {
            "The privileged folder, sudo authorization, or no-replace publisher was rejected."
        }
        file::Failure::Conflict => {
            "The reviewed tree or destination changed. Review the folder again."
        }
        file::Failure::Cancelled => {
            "The privileged folder transfer was cancelled before publication."
        }
        file::Failure::Uncertain => {
            "The privileged folder may have been published. Verify the destination and review again."
        }
        file::Failure::Failed => "The privileged folder exchange failed before publication.",
    }
}

fn authorization(mode: SudoMode, saved: Option<&mut Credentials>) -> Result<file::Auth, Failure> {
    match mode {
        SudoMode::Disabled => Err(Failure::blocked(APPROVAL_REQUIRED)),
        SudoMode::Passwordless => Ok(file::Auth::Passwordless),
        SudoMode::Vault => saved
            .and_then(Credentials::take_sudo_password)
            .and_then(|secret| SensitiveLine::new(secret.as_str()).ok())
            .map(file::Auth::Password)
            .ok_or_else(|| Failure::blocked("An unlocked saved sudo credential is required.")),
    }
}

fn allowed_now(reference: &ConnectionRef) -> bool {
    privileged_allowed_now(reference, true)
}

fn privileged_allowed_now(reference: &ConnectionRef, transfer: bool) -> bool {
    let policy = ClipboardPolicy::load();
    policy.problem.is_none()
        && (!transfer || policy.file_transfer_allowed)
        && policy.scheduler_sudo_allowed
        && policy.minimum_isolation.is_none()
        && connection::reference_allowed(reference, policy.server_access())
}

fn gate_failure(
    cancellation: &Cancellation,
    reference: &ConnectionRef,
    deadline: Instant,
) -> Option<Failure> {
    check_cancelled(cancellation)
        .err()
        .or_else(|| {
            (Instant::now() >= deadline)
                .then(|| Failure::failed("The privileged transfer timed out."))
        })
        .or_else(|| (!allowed_now(reference)).then(|| Failure::blocked(DENIED)))
}

fn transfer_failure(
    error: file::Failure,
    cancellation: &Cancellation,
    reference: &ConnectionRef,
    deadline: Instant,
) -> Failure {
    if error == file::Failure::Uncertain {
        return Failure::failed(
            "The server may have published the privileged upload. Verify its destination before retrying.",
        );
    }
    if let Some(failure) = gate_failure(cancellation, reference, deadline) {
        return failure;
    }
    match error {
        file::Failure::Blocked => Failure::blocked(
            "The sudo credential, Linux file path or fixed file helper was rejected.",
        ),
        file::Failure::Conflict => {
            Failure::failed("The privileged transfer source changed or its destination exists.")
        }
        file::Failure::Cancelled => Failure {
            outcome: RunOutcome::Cancelled,
            detail: "The privileged transfer was cancelled before publication.",
        },
        file::Failure::Failed | file::Failure::Uncertain => {
            Failure::failed("The privileged file exchange failed before publication.")
        }
    }
}
