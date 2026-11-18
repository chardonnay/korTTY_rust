//! Optional local rsync client with an authenticated native SSH protocol bridge.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kortty_server_archive::{
    privileged_rsync,
    rsync_tree::{DeleteScope, ExistingTree, RecurringReceiver},
};
use kortty_ssh::TrustStore;

use crate::scheduler_window::rsync::{MixedReview, MixedReviewTarget};
use crate::scheduler_window::{ConnectionRef, RemotePath};

use super::Credentials;

mod local_existing_snapshot;
pub(crate) use local_existing_snapshot::{
    LocalExistingSnapshot, LocalParentReceipt, LocalTreeLocation, VacantFileProof,
    VacantLocalTreeSnapshot,
};

pub(crate) type BatchParents = Arc<std::sync::Mutex<Vec<LocalParentReceipt>>>;

#[cfg(not(any(unix, windows)))]
use std::io;
use std::path::Path;

#[cfg(any(unix, windows))]
mod native;
#[cfg(any(unix, windows))]
pub(super) use native::run;
#[cfg(any(unix, windows))]
pub(crate) use native::{
    BRIDGE_ARGUMENT, program_digest, serve_bridge, upload_digest, upload_tree_inventory,
};

pub(crate) fn review_local_existing(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<LocalExistingSnapshot, String> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    return native::review_local_existing(path, cancelled).map_err(|error| error.to_string());
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (path, cancelled);
        Err("Existing local rsync directories are unavailable on this platform.".into())
    }
}

pub(crate) fn review_local_vacant_tree(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<VacantLocalTreeSnapshot, String> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    return native::review_local_vacant_tree(path, cancelled).map_err(|error| error.to_string());
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (path, cancelled);
        Err("Reviewed vacant local rsync directories are unavailable on this platform.".into())
    }
}

pub(crate) fn review_local_file_vacancy(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<VacantFileProof, String> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    return native::review_local_file_vacancy(path, &|| {
        cancelled.load(std::sync::atomic::Ordering::Acquire)
    })
    .map_err(|error| error.to_string());
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (path, cancelled);
        Err("Reviewed vacant local rsync files are unavailable on this platform.".into())
    }
}

/// Inspect a receiver through the same pinned connection type used by upload.
#[cfg(any(unix, windows))]
pub(crate) fn preview_existing(
    credentials: Credentials,
    reference: &ConnectionRef,
    destination: &RemotePath,
    trust: &TrustStore,
    cancelled: Arc<AtomicBool>,
    require_current_routes: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) -> Result<ExistingTree, String> {
    let (session, authority) = open_review_session(
        credentials,
        reference,
        trust,
        Arc::clone(&cancelled),
        require_current_routes,
        true,
        false,
    )?;
    let exec = session
        .exec_opener()
        .ok_or("The pinned SSH session cannot review an existing rsync receiver.")?;
    native::review_existing(
        &exec,
        destination,
        move || cancelled.load(std::sync::atomic::Ordering::Acquire),
        authority,
    )
    .map_err(|error| error.detail.into())
}

pub(crate) struct RecurringReceiverTarget<'a> {
    pub(crate) destination: &'a RemotePath,
    pub(crate) directory: bool,
    pub(crate) delete: &'a DeleteScope,
}

pub(crate) fn preview_recurring_receiver(
    credentials: Credentials,
    reference: &ConnectionRef,
    target: RecurringReceiverTarget<'_>,
    trust: &TrustStore,
    cancelled: Arc<AtomicBool>,
    require_current_routes: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) -> Result<RecurringReceiver, String> {
    #[cfg(any(unix, windows))]
    {
        let (session, authority) = open_review_session(
            credentials,
            reference,
            trust,
            Arc::clone(&cancelled),
            require_current_routes,
            true,
            false,
        )?;
        let exec = session
            .exec_opener()
            .ok_or("The pinned SSH session cannot review a recurring rsync receiver.")?;
        native::review_recurring_receiver(
            &exec,
            target,
            move || cancelled.load(std::sync::atomic::Ordering::Acquire),
            authority,
        )
        .map_err(|error| error.detail.into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            credentials,
            reference,
            target,
            trust,
            cancelled,
            require_current_routes,
        );
        Err("Recurring rsync upload receivers are unavailable on this platform.".into())
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) fn preview_mixed(
    credentials: Credentials,
    target: &MixedReviewTarget,
    trust: &TrustStore,
    cancelled: Arc<AtomicBool>,
    require_current_routes: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) -> Result<MixedReview, String> {
    use crate::scheduler_window::rsync::{Elevation, MixedDestination};
    use std::sync::atomic::Ordering;

    let uploading = matches!(&target.destination, MixedDestination::Server(_));
    let privileged = target.elevation == Elevation::Passwordless;
    let (mut session, authority) = open_review_session(
        credentials,
        &target.reference,
        trust,
        Arc::clone(&cancelled),
        require_current_routes,
        uploading || privileged,
        privileged,
    )?;
    let exec = session
        .exec_opener()
        .ok_or("The pinned SSH session cannot review the mixed rsync route.")?;
    let live_cancelled = Arc::clone(&cancelled);
    super::remote::sync::supervise(
        &mut session,
        &|| live_cancelled.load(Ordering::Acquire),
        |client, stopped, _| {
            native::review_mixed(
                client,
                &exec,
                target,
                move || cancelled.load(Ordering::Acquire),
                stopped,
                authority,
            )
        },
    )
    .map_err(|error| error.detail.to_owned())?
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum PrivilegedPreviewMode {
    Standard,
    VacantFile,
    VacantTree,
}

pub(crate) fn preview_privileged(
    credentials: Credentials,
    reference: &ConnectionRef,
    spec: &privileged_rsync::Spec,
    mode: PrivilegedPreviewMode,
    trust: &TrustStore,
    cancelled: Arc<AtomicBool>,
    require_current_routes: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) -> Result<privileged_rsync::Review, String> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        let (session, authority) = open_review_session(
            credentials,
            reference,
            trust,
            Arc::clone(&cancelled),
            require_current_routes,
            false,
            true,
        )?;
        let exec = session
            .exec_opener()
            .ok_or("The pinned SSH session cannot review privileged rsync.")?;
        native::review_privileged_destination(
            &exec,
            spec,
            move || cancelled.load(std::sync::atomic::Ordering::Acquire),
            authority,
            mode,
        )
        .map_err(|failure| failure.detail.to_owned())
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            credentials,
            reference,
            spec,
            mode,
            trust,
            cancelled,
            require_current_routes,
        );
        Err("Privileged rsync is unavailable on this platform.".into())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) fn preview_mixed(
    _: Credentials,
    _: &MixedReviewTarget,
    _: &TrustStore,
    _: Arc<AtomicBool>,
    _: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) -> Result<MixedReview, String> {
    Err("Mixed rsync transfers are unavailable on this platform.".into())
}

#[cfg(any(unix, windows))]
fn open_review_session(
    credentials: Credentials,
    reference: &ConnectionRef,
    trust: &TrustStore,
    cancelled: Arc<AtomicBool>,
    require_current_routes: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
    transfer_helper: bool,
    privileged: bool,
) -> Result<(kortty_ssh::SshSession, native::ExecutionGuard), String> {
    use std::io;
    use std::sync::atomic::Ordering;

    use crate::clipboard_policy::ClipboardPolicy;
    use crate::scheduler_window::sync;

    use super::{Failure, connection, remote};

    let stopped = move || cancelled.load(Ordering::Acquire);
    let pins = sync::pins(reference, trust).map_err(|error| error.to_string())?;
    let expected = pins.clone();
    let enrolled = reference.clone();
    let trust = trust.clone();
    let authority: Arc<dyn Fn() -> io::Result<()> + Send + Sync> = Arc::new(move || {
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some()
            || !policy.file_transfer_allowed
            || !policy.minimum_isolation.is_none()
            || (privileged && !policy.scheduler_sudo_allowed)
            || !connection::reference_allowed(&enrolled, policy.server_access())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy forbids this rsync review.",
            ));
        }
        require_current_routes()?;
        if !sync::pins(&enrolled, &trust).is_ok_and(|current| current == expected) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The rsync review host keys changed.",
            ));
        }
        Ok(())
    });
    if stopped() {
        return Err("Rsync review cancelled.".into());
    }
    authority().map_err(|error| error.to_string())?;
    let (target, jumps) = credentials.into_route().map_err(|error| error.detail)?;
    let session = remote::sync::open_pinned_with(target, jumps, &pins, |target, jumps, trust| {
        remote::open_guarded_subsystem(target, jumps, trust, transfer_helper, &stopped, || {
            authority().map_err(|_| Failure::blocked("The saved rsync review authority changed."))
        })
    })
    .map_err(|error| error.detail)?;
    Ok((session, authority))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn preview_existing(
    _: Credentials,
    _: &ConnectionRef,
    _: &RemotePath,
    _: &TrustStore,
    _: Arc<AtomicBool>,
    _: impl Fn() -> io::Result<()> + Send + Sync + 'static,
) -> Result<ExistingTree, String> {
    Err("Scheduled rsync is unavailable on this platform.".into())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn program_digest(_: &Path) -> io::Result<[u8; 32]> {
    Err(io::Error::other(
        "Scheduled rsync is unavailable on this platform.",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn upload_digest(_: &Path) -> io::Result<(u64, [u8; 32])> {
    Err(io::Error::other(
        "Scheduled rsync is unavailable on this platform.",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn upload_tree_inventory(
    _: &Path,
) -> io::Result<kortty_server_archive::rsync_tree::Inventory> {
    Err(io::Error::other(
        "Scheduled rsync is unavailable on this platform.",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn run(
    _: &kortty_jobs::RunRequest<crate::scheduler_window::SshCommand>,
    _: &crate::scheduler_window::rsync::Transfer,
    _: bool,
    _: &super::ExecutionPolicy,
    _: kortty_ssh::SshTarget,
    _: Vec<kortty_ssh::SshTarget>,
    _: &kortty_ssh::TrustStore,
) -> Result<super::Completion, super::Failure> {
    Err(super::Failure::blocked(
        "Scheduled rsync is unavailable on this platform.",
    ))
}
