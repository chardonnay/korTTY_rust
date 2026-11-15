use std::cell::Cell;
use std::io;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use kortty::ssh_runtime::password_file::{DEADLINE, TRANSFER_DEADLINE};
use kortty_jobs::{Cancellation, RunOutcome, RunRequest, RunTrigger};
use kortty_ssh::{
    ConnectOptions, Endpoint, ExecConnection, HostKey, SensitiveLine, SshSession, TrustStore,
};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store as ConnectionStore;
use crate::job_executor::{Credentials, connection};
use crate::profile;
use crate::scheduler_window::{ConnectionRef, SshCommand, SudoMode, privileged, sync};
use crate::sftp_window::{
    LocalSource, PrivilegedArchiveFailure, StagedDownload, exchange_privileged_server_archive,
    privileged_file as file,
};
use crate::ssh_connection::ExecOpener;

use super::{Completion, ExecutionPolicy, Failure, SavedAuth, check_cancelled};

const APPROVAL_REQUIRED: &str = "Review the privileged transfer and its pinned SSH route first.";
const DENIED: &str = "Managed policy forbids this privileged file transfer.";
const TREE_REVIEW: &str =
    "Review the exact privileged folder transfer and its pinned SSH route first.";
const FILE_ACTION_REVIEW: &str =
    "Review the privileged file action and its pinned SSH route first.";
const FILE_ACTION_DENIED: &str = "Managed policy forbids this privileged file action.";
const ARCHIVE_REVIEW: &str = "Review the privileged archive and its pinned SSH route first.";
const ARCHIVE_DENIED: &str = "Managed policy forbids this privileged archive action.";
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(1800);

enum LocalFile {
    Download(StagedDownload),
    Upload(LocalSource),
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
    let local = archive
        .archive
        .download
        .as_deref()
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
    let deadline = Instant::now() + ARCHIVE_TIMEOUT;
    let checked_reference = reference.clone();
    let cancellation = request.cancellation.clone();
    let stopped = move || {
        cancellation.is_requested()
            || Instant::now() >= deadline
            || !privileged_allowed_now(&checked_reference, copies_archive)
    };
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
    let review_current = || {
        policy.require_target_groups(&request.action).is_ok()
            && sync::pins(reference, &trust).is_ok_and(|current| current == pins)
            && context
                .authorize(transfer, reference, request.action.sudo, &pins)
                .is_ok()
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
    let local = match transfer.direction {
        privileged::PrivilegedDirection::Download => LocalFile::Download(
            StagedDownload::create(&transfer.local)
                .map_err(|_| Failure::blocked("Choose an absent local file destination."))?,
        ),
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
    let checked_reference = reference.clone();
    let stopped = cancellation.clone();
    let allowed = move || {
        !stopped.is_requested() && Instant::now() < deadline && allowed_now(&checked_reference)
    };
    if let Some(failure) = gate_failure(cancellation, reference, deadline) {
        return Err(failure);
    }
    let opened =
        super::sync::open_pinned_for_job(target, jumps, &pins, || !allowed(), request, policy);
    if let Some(failure) = gate_failure(cancellation, reference, deadline) {
        return Err(failure);
    }
    let mut session = opened?;
    let opener = session
        .exec_opener()
        .map(ExecOpener::Direct)
        .ok_or_else(|| {
            Failure::blocked("The authenticated SSH route cannot open the fixed file helper.")
        })?;
    let outcome = match local {
        LocalFile::Download(stage) => (|| {
            let output = stage
                .writer()
                .map_err(|_| Failure::failed("The private download stage is unavailable."))?;
            let bytes = file::download(
                opener,
                transfer.remote.as_str(),
                output,
                auth,
                allowed.clone(),
            )
            .map_err(|error| transfer_failure(error, cancellation, reference, deadline))?;
            stage
                .publish(|| !allowed() || !review_current())
                .map_err(|_| {
                    gate_failure(cancellation, reference, deadline)
                        .or_else(|| {
                            (!review_current()).then(|| Failure::blocked(APPROVAL_REQUIRED))
                        })
                        .unwrap_or_else(|| {
                            Failure::failed(
                                "Could not publish the verified download to a new local file.",
                            )
                        })
                })?;
            Ok(format!("Privileged download published {bytes} bytes."))
        })(),
        LocalFile::Upload(source) => file::upload(
            opener,
            transfer.remote.as_str(),
            &source,
            auth,
            allowed,
            review_current,
        )
        .map(|bytes| format!("Privileged upload published {bytes} bytes."))
        .map_err(|error| {
            if error == file::Failure::Blocked && !review_current() {
                Failure::blocked(APPROVAL_REQUIRED)
            } else {
                transfer_failure(error, cancellation, reference, deadline)
            }
        }),
    };
    let _ = session.close();
    let mut completion = Completion::from_result(request.id, Ok(0));
    completion.detail = Some(outcome?);
    Ok(completion)
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
    let plan = (copy.direction == sync::Direction::Upload)
        .then(|| {
            file::TreeUpload::scan(&copy.local, copy.remote.as_str(), &copy.excludes, &allowed)
        })
        .transpose()
        .map_err(tree_detail)?;
    let (target, jumps) = credentials.into_route().map_err(|error| error.detail)?;
    let mut session = super::sync::open_pinned(target, jumps, pins, || !allowed())
        .map_err(|error| error.detail)?;
    let opener = tree_opener(&session).map_err(|error| error.detail)?;
    let preview = if let Some(plan) = plan {
        file::inspect_tree(opener, copy.remote.as_str(), auth, allowed)
            .and_then(|witness| plan.preview(&witness))
    } else {
        file::preview_tree_download(opener, copy.remote.as_str(), &copy.local, auth, allowed)
    };
    let _ = session.close();
    Ok(sync::Review {
        preview: preview.map_err(tree_detail)?,
        pins: sync::fingerprints(pins),
        standing_roots: None,
    })
}

pub(super) fn run_tree(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    copy: &sync::Copy,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    if !matches!(request.trigger, RunTrigger::Manual | RunTrigger::Scheduled) {
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
    let receipt = context
        .store
        .load(&request.job, copy.approval.as_deref(), context.scope)
        .map_err(|_| Failure::blocked(TREE_REVIEW))?;
    let trust = trust_store().map_err(|_| Failure::blocked(TREE_REVIEW))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(TREE_REVIEW))?;
    if sync::fingerprints(&pins) != receipt.pins {
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
    let plan = (copy.direction == sync::Direction::Upload)
        .then(|| {
            file::TreeUpload::scan(&copy.local, copy.remote.as_str(), &copy.excludes, &allowed)
        })
        .transpose()
        .map_err(|error| gate_error().unwrap_or_else(|| Failure::blocked(tree_detail(error))))?;
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let mut session =
        super::sync::open_pinned_for_job(target, jumps, &pins, || !allowed(), request, policy)?;
    let claimed = Cell::new(false);
    let pins_current = || {
        policy.require_target_groups(&request.action).is_ok()
            && sync::pins(reference, &trust).is_ok_and(|current| current == pins)
    };
    let claim = |proof| {
        if proof != receipt.proof || !pins_current() {
            return Err(file::Failure::Conflict);
        }
        context
            .store
            .claim(&request.job, &receipt)
            .map_err(|_| file::Failure::Blocked)?;
        claimed.set(true);
        Ok(())
    };
    let publication_allowed = || {
        pins_current()
            && context
                .store
                .require_claimed(&request.job, &receipt)
                .is_ok()
    };
    let opener = tree_opener(&session)?;
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
    let (entries, bytes) = result.map_err(|error| {
        if error == file::Failure::Uncertain {
            Failure::failed(tree_detail(error))
        } else if let Some(failure) = gate_error() {
            failure
        } else if claimed.get() {
            Failure::failed(tree_detail(error))
        } else {
            Failure::blocked(tree_detail(error))
        }
    })?;
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
