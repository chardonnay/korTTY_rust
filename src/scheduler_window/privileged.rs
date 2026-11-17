//! Device-local review of exact scheduled privileged file actions.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::SystemTime;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};
use kortty_jobs::{DependencyMode, Job, JobId, JobSchedule, RunRequest, TimeZone};
use kortty_ssh::{Endpoint, HostKey};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::sftp_window::ReviewedFileReceiver;
use crate::sftp_window::privileged_file::{self, ProtectedOperation};

use super::sync::standing;
use super::{
    Archive, ConnectionRef, Message, Page, RemoteAction, RemotePath, SchedulerView,
    SchedulerWindow, SshCommand, SudoMode, new_id, remote, sync,
};

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrivilegedDirection {
    Upload,
    Download,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrivilegedTransfer {
    pub(crate) direction: PrivilegedDirection,
    pub(crate) local: PathBuf,
    pub(crate) remote: RemotePath,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) replace_existing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) standing: Option<standing::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<String>,
}

impl PrivilegedTransfer {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        remote::validate_local_path(&self.local)?;
        if self.remote.root() != "/" || self.remote.as_str().len() > 2048 {
            return Err(
                "Privileged staging requires an absolute Linux file path of at most 2 KiB.",
            );
        }
        if let Some(settings) = &self.standing
            && (!self.replace_existing || settings.validate_replacement().is_err())
        {
            return Err(
                "Recurring protected transfers require an existing file and valid replacement limits.",
            );
        }
        if self
            .approval
            .as_deref()
            .is_some_and(|approval| !sync::valid_nonce(approval))
        {
            return Err("The privileged file approval identifier is invalid.");
        }
        Ok(())
    }

    fn scope(&self, reference: &ConnectionRef, sudo: SudoMode) -> io::Result<[u8; 32]> {
        let operation = if self.replace_existing {
            "kortty-scheduled-privileged-file-replacement-v1"
        } else {
            "kortty-scheduled-privileged-file-v1"
        };
        let scope = scope_digest(&(
            operation,
            reference,
            self.direction,
            &self.local,
            &self.remote,
            sudo,
        ))?;
        self.standing.map_or(Ok(scope), |settings| {
            scope_digest(&("kortty-recurring-protected-file-v1", scope, settings))
        })
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrivilegedFileAction {
    pub(crate) path: RemotePath,
    pub(crate) operation: ProtectedOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<String>,
}

impl PrivilegedFileAction {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if !privileged_file::valid_path(self.path.as_str()) || self.path.root() != "/" {
            return Err("Choose a canonical absolute Linux path for the privileged action.");
        }
        let destination = self.operation.destination();
        if !self.operation.valid_for(self.path.as_str())
            || destination.is_some_and(|path| {
                !RemotePath::try_from(path.to_owned()).is_ok_and(|path| path.root() == "/")
            })
        {
            return Err(if destination.is_some() {
                "Choose a distinct canonical Linux destination path outside any copied folder."
            } else {
                "Choose valid numeric ownership or octal or symbolic permissions without special bits."
            });
        }
        if self
            .approval
            .as_deref()
            .is_some_and(|id| !sync::valid_nonce(id))
        {
            return Err("The privileged file approval identifier is invalid.");
        }
        Ok(())
    }

    fn scope(&self, reference: &ConnectionRef, sudo: SudoMode) -> io::Result<[u8; 32]> {
        scope_digest(&(
            "kortty-scheduled-privileged-file-action-v1",
            reference,
            &self.path,
            &self.operation,
            sudo,
        ))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrivilegedArchive {
    pub(crate) archive: Archive,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<String>,
}

impl PrivilegedArchive {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.archive.server_output.is_some() {
            return Err("Privileged archives create root-owned private output.");
        }
        self.archive.server_spec()?;
        if self.archive.target.root() != "/"
            || self
                .archive
                .sources
                .iter()
                .any(|source| source.root() != "/" || source.contains(&self.archive.target))
        {
            return Err(
                "Choose absolute Linux archive paths and a destination outside the sources.",
            );
        }
        if self
            .approval
            .as_deref()
            .is_some_and(|approval| !sync::valid_nonce(approval))
        {
            return Err("The privileged archive approval identifier is invalid.");
        }
        Ok(())
    }

    fn scope(&self, reference: &ConnectionRef, sudo: SudoMode) -> io::Result<[u8; 32]> {
        let spec = self.archive.server_spec().map_err(io::Error::other)?;
        if self.archive.reuse_output {
            return scope_digest(&(
                "kortty-scheduled-privileged-archive-reuse-v1",
                reference,
                spec,
                sudo,
                &self.archive.download,
                &self.archive.password_credential,
            ));
        }
        if let Some(download) = &self.archive.download {
            return scope_digest(&(
                "kortty-scheduled-privileged-archive-copy-v1",
                reference,
                spec,
                sudo,
                download,
                &self.archive.password_credential,
            ));
        }
        if let Some(password) = &self.archive.password_credential {
            return scope_digest(&(
                "kortty-scheduled-privileged-password-archive-v1",
                reference,
                spec,
                sudo,
                password,
            ));
        }
        scope_digest(&(
            "kortty-scheduled-privileged-archive-v1",
            reference,
            spec,
            sudo,
        ))
    }
}

fn scope_digest(subject: &impl Serialize) -> io::Result<[u8; 32]> {
    let bytes = serde_json::to_vec(subject)?;
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    hash.update(&bytes);
    Ok(hash
        .finalize()
        .as_ref()
        .try_into()
        .expect("SHA-256 produces 32 bytes"))
}

#[derive(Clone)]
pub(crate) struct RunContext {
    store: sync::Store,
    job: JobId,
    scope: [u8; 32],
    recurring: Option<RecurringRun>,
}

#[derive(Clone)]
struct RecurringRun {
    scope: [u8; 32],
    admission: standing::Admission,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum TransferApproval {
    Operation(sync::Receipt),
    Recurring(standing::ReplacementApproval),
}

impl TransferApproval {
    pub(crate) fn proof(&self) -> [u8; 32] {
        match self {
            Self::Operation(receipt) => receipt.proof,
            Self::Recurring(receipt) => receipt.fingerprint,
        }
    }

    pub(crate) fn max_bytes(&self) -> u64 {
        match self {
            Self::Operation(_) => privileged_file::MAX_TRANSFER_BYTES,
            Self::Recurring(receipt) => receipt.approval.limits.max_bytes,
        }
    }
}

impl RunContext {
    pub(crate) fn authorize(
        &self,
        transfer: &PrivilegedTransfer,
        reference: &ConnectionRef,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<TransferApproval> {
        transfer.validate().map_err(io::Error::other)?;
        let scope = transfer.scope(reference, sudo)?;
        if scope != self.scope || sudo == SudoMode::Disabled {
            return Err(review_required());
        }
        if let Some(run) = &self.recurring {
            let receipt = self.store.load_replacement(
                &self.job,
                transfer.approval.as_deref(),
                run.scope,
                SystemTime::now(),
                Some(&run.admission),
            )?;
            if receipt.approval.pins != sync::fingerprints(pins) {
                return Err(review_required());
            }
            return Ok(TransferApproval::Recurring(receipt));
        }
        if transfer.standing.is_some() {
            return Err(review_required());
        }
        self.require_scope(scope, transfer.approval.as_deref(), sudo, pins)
            .map(TransferApproval::Operation)
    }

    pub(crate) fn claim_replacement(
        &self,
        receipt: &TransferApproval,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        match receipt {
            TransferApproval::Operation(receipt) => {
                self.store.claim_with_wait(&self.job, receipt, stopped)
            }
            TransferApproval::Recurring(receipt) => self.store.arm_replacement(
                &self.job,
                receipt,
                &self
                    .recurring
                    .as_ref()
                    .ok_or_else(review_required)?
                    .admission,
                bytes,
                stopped,
            ),
        }
    }

    pub(crate) fn require_claimed(&self, receipt: &TransferApproval) -> io::Result<()> {
        match receipt {
            TransferApproval::Operation(receipt) => self.store.require_claimed(&self.job, receipt),
            TransferApproval::Recurring(receipt) => self.store.require_armed_replacement(
                &self.job,
                receipt,
                self.recurring
                    .as_ref()
                    .ok_or_else(review_required)?
                    .admission
                    .firing(),
            ),
        }
    }

    pub(crate) fn finish_replacement(
        &self,
        receipt: &TransferApproval,
        successor: Option<[u8; 32]>,
    ) -> io::Result<()> {
        match receipt {
            TransferApproval::Operation(_) => Ok(()),
            TransferApproval::Recurring(receipt) => self.store.finish_replacement(
                &self.job,
                receipt,
                self.recurring
                    .as_ref()
                    .ok_or_else(review_required)?
                    .admission
                    .firing(),
                successor,
            ),
        }
    }

    pub(crate) fn saved_route_current(&self, reference: &ConnectionRef) -> bool {
        let path = crate::profile::connections_path_at(self.store.profile());
        saved_route_current(&path, reference)
    }

    pub(crate) fn authorize_file_action(
        &self,
        action: &PrivilegedFileAction,
        reference: &ConnectionRef,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<()> {
        action.validate().map_err(io::Error::other)?;
        self.require_scope(
            action.scope(reference, sudo)?,
            action.approval.as_deref(),
            sudo,
            pins,
        )
        .map(|_| ())
    }

    pub(crate) fn authorize_archive(
        &self,
        archive: &PrivilegedArchive,
        reference: &ConnectionRef,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<()> {
        archive.validate().map_err(io::Error::other)?;
        self.require_scope(
            archive.scope(reference, sudo)?,
            archive.approval.as_deref(),
            sudo,
            pins,
        )
        .map(|_| ())
    }

    fn require_scope(
        &self,
        scope: [u8; 32],
        approval: Option<&str>,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<sync::Receipt> {
        if sudo == SudoMode::Disabled || scope != self.scope {
            return Err(review_required());
        }
        let receipt = self.store.load(&self.job, approval, self.scope)?;
        if receipt.pins != sync::fingerprints(pins) {
            return Err(review_required());
        }
        Ok(receipt)
    }
}

pub(crate) fn saved_route_current(path: &Path, reference: &ConnectionRef) -> bool {
    crate::connection_store::Store::open(path.to_owned()).is_ok_and(|store| {
        store.get(reference.id).map(ConnectionRef::from).as_ref() == Some(reference)
    })
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn approve_fixture(
    profile: &std::path::Path,
    job: &JobId,
    action: &mut SshCommand,
    pins: &[(Endpoint, HostKey)],
) -> io::Result<RunContext> {
    action.validate_elevation().map_err(io::Error::other)?;
    let reference = action.connection.as_ref().ok_or_else(review_required)?;
    let Some(RemoteAction::Privileged(transfer)) = &mut action.remote else {
        return Err(review_required());
    };
    let scope = transfer.scope(reference, action.sudo)?;
    let proof = if transfer.replace_existing {
        ReviewedFileReceiver::inspect_transfer(&transfer.local, &|| false)?.fingerprint
    } else {
        [0; 32]
    };
    let receipt = sync::Receipt::for_operation(scope, proof, sync::fingerprints(pins))?;
    let store = sync::Store::new(profile);
    store.swap(job, receipt.clone(), |_| true)?;
    transfer.approval = Some(receipt.approval);
    Ok(RunContext {
        store,
        job: job.clone(),
        scope,
        recurring: None,
    })
}

fn review_required() -> io::Error {
    io::Error::other("Review the exact privileged file action and SSH route again.")
}

#[derive(Clone, Eq, PartialEq)]
struct Subject {
    job: JobId,
    scope: [u8; 32],
    action_scope: [u8; 32],
}

fn subject<'a>(
    job: &'a Job<SshCommand>,
    zone: &TimeZone,
) -> Result<(Subject, &'a RemoteAction, &'a ConnectionRef), String> {
    let action = job.action();
    action.validate_elevation()?;
    let reference = action
        .connection
        .as_ref()
        .ok_or("Choose one saved SSH connection.")?;
    let (action_scope, remote) = match &action.remote {
        Some(remote @ RemoteAction::Privileged(transfer)) => {
            (transfer.scope(reference, action.sudo), remote)
        }
        Some(remote @ RemoteAction::PrivilegedFileAction(metadata)) => {
            (metadata.scope(reference, action.sudo), remote)
        }
        Some(remote @ RemoteAction::PrivilegedArchive(archive)) => {
            (archive.scope(reference, action.sudo), remote)
        }
        _ => return Err("Choose a file action with privileged execution enabled.".into()),
    };
    let action_scope = action_scope.map_err(|_| "Could not bind the privileged file review.")?;
    let scope = if matches!(remote, RemoteAction::Privileged(transfer) if transfer.standing.is_some())
    {
        let calendar = job.schedule().is_some_and(JobSchedule::enabled);
        let chain = job.mode() == DependencyMode::Chain && !job.dependencies().is_empty();
        if !calendar && !chain {
            return Err("Recurring protected replacement requires an enabled calendar or Chain with predecessors.".into());
        }
        let zone = zone
            .iana_name()
            .ok_or("Recurring protected replacement requires a named scheduler time zone.")?;
        scope_digest(&(
            "kortty-scheduled-protected-replacement-v1",
            job.id(),
            action_scope,
            job.schedule(),
            job.mode(),
            job.dependencies(),
            job.max_age_minutes(),
            zone,
        ))
        .map_err(|_| "Could not bind the recurring replacement to the job schedule.")?
    } else {
        action_scope
    };
    Ok((
        Subject {
            job: job.id().clone(),
            scope,
            action_scope,
        },
        remote,
        reference,
    ))
}

#[derive(Default)]
pub(super) struct DraftReview {
    review: Option<Review>,
    approved: Option<(Subject, sync::SaveApproval)>,
}

struct Review {
    subject: Subject,
    action: RemoteAction,
    reference: ConnectionRef,
    sudo: SudoMode,
    pins: Vec<String>,
    destination: Destination,
    triggers: Vec<String>,
}

enum Destination {
    Unbound,
    Inspecting(Inspection),
    Existing(ReviewedDestination),
}

#[derive(Clone, Copy)]
pub(crate) struct ReviewedDestination {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) bytes: u64,
    pub(crate) source_bytes: Option<u64>,
}

struct Inspection(Arc<AtomicBool>);

impl Drop for Inspection {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct ReplacementPreview {
    target: PreviewTarget,
    cancelled: Arc<AtomicBool>,
}

enum PreviewTarget {
    Local(PathBuf),
    Server(Box<UploadPreview>),
}

pub(crate) struct UploadPreview {
    pub(crate) reference: ConnectionRef,
    pub(crate) connections: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) path: RemotePath,
    pub(crate) sudo: SudoMode,
    pub(crate) pins: Vec<String>,
    pub(crate) standing: Option<standing::Settings>,
}

#[derive(Clone)]
pub struct ReplacementPreviewed {
    cancelled: Arc<AtomicBool>,
    result: Result<ReviewedDestination, String>,
}

impl ReplacementPreview {
    pub(crate) fn upload(&self) -> Option<&UploadPreview> {
        match &self.target {
            PreviewTarget::Local(_) => None,
            PreviewTarget::Server(upload) => Some(upload),
        }
    }

    pub(crate) async fn inspect(
        self,
        credentials: Option<Result<crate::job_executor::Credentials, &'static str>>,
    ) -> ReplacementPreviewed {
        let cancelled = Arc::clone(&self.cancelled);
        let (sender, receiver) = async_channel::bounded(1);
        let start = std::thread::Builder::new()
            .name("scheduler-privileged-file-review".into())
            .spawn(move || {
                let result = match self.target {
                    PreviewTarget::Local(path) => {
                        ReviewedFileReceiver::inspect_transfer(&path, &|| {
                            self.cancelled.load(Ordering::Relaxed)
                        })
                        .map(|snapshot| ReviewedDestination {
                            fingerprint: snapshot.fingerprint,
                            bytes: snapshot.inventory.bytes,
                            source_bytes: None,
                        })
                        .map_err(|error| {
                            format!("Could not review the existing local file. {error}")
                        })
                    }
                    PreviewTarget::Server(upload) => (|| {
                        let credentials =
                            credentials.ok_or("The saved SSH connection is unavailable.")??;
                        let trust = crate::ssh_connection::trust_store()
                            .map_err(|_| "The SSH host-key store is unavailable.")?;
                        crate::job_executor::preview_privileged_file_upload(
                            &upload,
                            credentials,
                            self.cancelled,
                            trust,
                        )
                    })(),
                };
                let _ = sender.try_send(result);
            });
        let result = if start.is_ok() {
            receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("The privileged file review worker stopped.".into()))
        } else {
            Err("Could not start the privileged file review worker.".into())
        };
        ReplacementPreviewed { cancelled, result }
    }
}

impl DraftReview {
    pub(super) fn approved_job(&self) -> Option<&JobId> {
        self.approved.as_ref().map(|(subject, _)| &subject.job)
    }

    pub(super) fn view(&self) -> Option<Element<'_, Message>> {
        let review = self.review.as_ref()?;
        let (operation, local, remote, destination) = match &review.action {
            RemoteAction::Privileged(transfer) => (
                match (transfer.direction, transfer.replace_existing) {
                    (PrivilegedDirection::Upload, false) => "Upload to a new root-owned remote file",
                    (PrivilegedDirection::Upload, true) => "Replace a protected server file with the reviewed upload",
                    (PrivilegedDirection::Download, true) => "Replace an existing local file with a protected download",
                    (PrivilegedDirection::Download, false) => {
                        "Download a root-owned file to a new local file"
                    }
                }
                .into(),
                Some(&transfer.local),
                Some(&transfer.remote),
                None,
            ),
            RemoteAction::PrivilegedFileAction(metadata) => (
                match &metadata.operation {
                    ProtectedOperation::Mkdir => {
                        "Create one root-owned directory with mode 0700".into()
                    }
                    ProtectedOperation::Chmod { mode } => format!("Set permissions to {mode:04o}"),
                    ProtectedOperation::ChmodSymbolic { mode } => format!("Change permissions using {mode}"),
                    ProtectedOperation::Chown { uid, gid } => format!(
                        "Set owner UID {} and group GID {}",
                        uid.map_or_else(|| "unchanged".into(), |id| id.to_string()),
                        gid.map_or_else(|| "unchanged".into(), |id| id.to_string()),
                    ),
                    ProtectedOperation::Copy { .. } => {
                        "Copy a protected regular file to a new root-owned file with mode 0600"
                            .into()
                    }
                    ProtectedOperation::CopyTree { .. } => {
                        "Copy a protected folder to a vacant path, using mode 0700 for folders and 0600 for files"
                            .into()
                    }
                    ProtectedOperation::Rename { .. } => {
                        "Move a protected file or folder to an unused destination".into()
                    }
                    ProtectedOperation::Delete { recursive: true } => {
                        "Permanently delete the protected tree, up to 128 entries in total".into()
                    }
                    ProtectedOperation::Delete { recursive: false } => {
                        "Permanently delete a protected regular file or empty folder".into()
                    }
                },
                None,
                Some(&metadata.path),
                metadata.operation.destination(),
            ),
            RemoteAction::PrivilegedArchive(archive) => (
                if archive.archive.reuse_output {
                    format!("Create or update a root-owned {} archive", archive.archive.format)
                } else {
                    format!("Create a new root-owned {} archive", archive.archive.format)
                },
                archive.archive.download.as_ref(),
                None,
                Some(archive.archive.target.as_str()),
            ),
            _ => return None,
        };
        let mut detail = column![
            text(operation).size(17),
            text(format!("SSH account {:?}", review.reference.target.label())),
            local.map(|path| text(format!("Local path {path:?}"))),
            remote.map(|path| text(format!("Remote path {:?}", path.as_str()))),
            destination.map(|path| text(format!("Remote destination {path:?}"))),
            text(format!("Elevation {}", review.sudo)),
        ]
        .spacing(5);
        let ready = match &review.destination {
            Destination::Unbound => {
                detail = detail.push(
                    text("Approval applies to future runs of this exact job action.").size(12),
                );
                true
            }
            Destination::Inspecting(_) => {
                detail = detail.push(text("Checking the replacement destination…").size(12));
                false
            }
            Destination::Existing(snapshot) => {
                if let Some(bytes) = snapshot.source_bytes {
                    detail = detail
                        .push(text(format!("Local upload source of {bytes} bytes")))
                        .push(text(format!(
                            "Existing server file of {} bytes",
                            snapshot.bytes
                        )));
                } else {
                    detail = detail.push(text(format!(
                        "Existing local file of {} bytes",
                        snapshot.bytes
                    )));
                }
                let recurring = match &review.action {
                    RemoteAction::Privileged(transfer) => transfer.standing,
                    _ => None,
                };
                if let Some(settings) = recurring {
                    detail = detail.push(
                        text(format!(
                            "Up to {} firings in {} days, with at most {} bytes per firing.",
                            settings.limits.max_firings, settings.days, settings.limits.max_bytes,
                        ))
                        .size(12),
                    );
                    for trigger in &review.triggers {
                        detail = detail.push(text(trigger).size(12));
                    }
                    detail = detail.push(text("Each firing reads the current file at the approved source path. The receiver must match the last confirmed publication. Previous files are kept for recovery.").size(12));
                } else {
                    detail = detail.push(text("Approval permits one replacement of this exact file. The original is kept for recovery.").size(12));
                }
                true
            }
        };
        if let RemoteAction::PrivilegedArchive(action) = &review.action {
            let archive = &action.archive;
            if archive.format.supports_server_member_paths() {
                use kortty_server_archive::protocol::ArchiveMemberLayout;

                detail = detail.push(
                    text(match archive.member_layout {
                        ArchiveMemberLayout::ServerPath => {
                            "Archive entries preserve complete server paths without a leading slash."
                        }
                        ArchiveMemberLayout::SourceBasename => {
                            "Archive entries start with each source's file or folder name."
                        }
                    })
                    .size(12),
                );
            }
            if archive.reuse_output {
                detail = detail.push(
                    text(match archive.format {
                        super::ArchiveFormat::Zip | super::ArchiveFormat::ZipPassword => {
                            "ZIP updates replace matching entries and keep unmatched entries."
                        }
                        _ => "Updates rebuild the archive from the current sources.",
                    })
                    .size(12),
                );
                detail = detail.push(text("Each replacement checks the current archive at the reviewed path and keeps the previous file for recovery.").size(12));
            }
            if archive.download.is_some() {
                detail = detail.push(text(if archive.reuse_output {
                    "Save a verified local copy and keep any replaced copy for recovery. The server path must be within a configured protected download root."
                } else {
                    "Save a verified local copy at a new path. The server path must be within a configured protected download root."
                }).size(12));
            }
            if archive.password_credential.is_some() {
                detail = detail.push(
                    text("Encrypt file contents using the selected vault archive password.")
                        .size(12),
                );
            }
            if archive.format != super::ArchiveFormat::Tar {
                detail = detail.push(text(format!("Compression level {}", archive.level)));
            }
            for source in &archive.sources {
                detail = detail.push(text(format!("Source {:?}", source.as_str())));
            }
            for pattern in &archive.excludes {
                detail = detail.push(text(format!("Exclude {pattern:?}")));
            }
            detail = detail.push(
                text("Each run checks the current sources and the server's permitted paths.")
                    .size(12),
            );
        }
        for pin in &review.pins {
            detail = detail.push(text(pin).size(12));
        }
        Some(
            column![
                scrollable(detail).height(Length::Fixed(142.0)),
                row![
                    button("Cancel review").on_press(Message::DiscardPrivileged),
                    button("Approve privileged action")
                        .on_press_maybe(ready.then_some(Message::ApprovePrivileged)),
                ]
                .spacing(8)
            ]
            .spacing(8)
            .into(),
        )
    }
}

impl SchedulerWindow {
    pub(crate) fn review_privileged(
        &self,
        view: &mut SchedulerView,
        pins: impl FnOnce(&ConnectionRef) -> Result<Vec<String>, String>,
    ) -> Result<Option<ReplacementPreview>, String> {
        if view.page != Page::Action
            || view.confirm.is_some()
            || !self.can_browse_directory()
            || !self.can_edit()
            || !self.draft_matches(view)
        {
            return Err("The selected job cannot be reviewed right now.".into());
        }
        let id = view.draft.id.clone().map_or_else(new_id, Ok)?;
        let job = self.build_current_draft(view, id)?;
        let (subject, action, reference) = subject(&job, &self.zone)?;
        let pins = pins(reference)?;
        let mut triggers = Vec::new();
        if matches!(action, RemoteAction::Privileged(transfer) if transfer.standing.is_some()) {
            if let Some(schedule) = job.schedule().filter(|schedule| schedule.enabled()) {
                triggers.push(sync::standing_schedule(schedule, &self.zone));
            }
            if job.mode() == DependencyMode::Chain && !job.dependencies().is_empty() {
                triggers.push(format!(
                    "After a fresh successful round of {} prerequisites",
                    job.dependencies().len()
                ));
            }
        }
        let (destination, preview) = match action {
            RemoteAction::Privileged(transfer) if transfer.replace_existing => {
                let cancelled = Arc::new(AtomicBool::new(false));
                let target = match transfer.direction {
                    PrivilegedDirection::Download => PreviewTarget::Local(transfer.local.clone()),
                    PrivilegedDirection::Upload => PreviewTarget::Server(Box::new(UploadPreview {
                        reference: reference.clone(),
                        connections: crate::profile::connections_path_at(self.sync_store.profile()),
                        local: transfer.local.clone(),
                        path: transfer.remote.clone(),
                        sudo: job.action().sudo,
                        pins: pins.clone(),
                        standing: transfer.standing,
                    })),
                };
                (
                    Destination::Inspecting(Inspection(Arc::clone(&cancelled))),
                    Some(ReplacementPreview { target, cancelled }),
                )
            }
            _ => (Destination::Unbound, None),
        };
        view.privileged_review = DraftReview {
            review: Some(Review {
                subject,
                action: action.clone(),
                reference: reference.clone(),
                sudo: job.action().sudo,
                pins,
                destination,
                triggers,
            }),
            approved: None,
        };
        view.status = None;
        Ok(preview)
    }

    pub(super) fn finish_privileged_preview(
        &self,
        view: &mut SchedulerView,
        preview: ReplacementPreviewed,
    ) {
        let Some(review) = &mut view.privileged_review.review else {
            return;
        };
        if !matches!(&review.destination, Destination::Inspecting(inspection)
            if Arc::ptr_eq(&inspection.0, &preview.cancelled))
        {
            return;
        }
        match preview.result {
            Ok(snapshot) => review.destination = Destination::Existing(snapshot),
            Err(error) => {
                view.privileged_review.review = None;
                view.status = Some(error);
            }
        }
    }

    pub(super) fn approve_privileged(&self, view: &mut SchedulerView) -> Result<(), String> {
        let Some(review) = view.privileged_review.review.take() else {
            return Ok(());
        };
        let proof = match &review.destination {
            Destination::Unbound => [0; 32],
            Destination::Existing(snapshot) => snapshot.fingerprint,
            Destination::Inspecting(_) => {
                view.privileged_review.review = Some(review);
                return Ok(());
            }
        };
        if view.page != Page::Action
            || !self.can_browse_directory()
            || !self.can_edit()
            || !self.draft_matches(view)
            || subject(
                &self.build_current_draft(view, review.subject.job.clone())?,
                &self.zone,
            )?
            .0 != review.subject
        {
            return Err("The reviewed file action changed. Review it again.".into());
        }
        let recurring = match review.action {
            RemoteAction::Privileged(transfer) => transfer.standing,
            _ => None,
        };
        let receipt = if let Some(settings) = recurring {
            standing::Approval::for_replacement(review.subject.scope, review.pins, proof, settings)
                .map(sync::SaveApproval::RecurringReplacement)
        } else {
            sync::Receipt::for_operation(review.subject.scope, proof, review.pins)
                .map(sync::SaveApproval::OneUse)
        }
        .map_err(|_| "Could not allocate the privileged file approval.")?;
        view.privileged_review.approved = Some((review.subject, receipt));
        view.dirty = true;
        view.status =
            Some("Privileged action approved. Save the job to keep this approval.".into());
        Ok(())
    }

    pub(super) fn save_privileged(
        &self,
        view: &SchedulerView,
        job: Job<SshCommand>,
    ) -> Result<(Job<SshCommand>, Option<sync::SaveApproval>), String> {
        if !matches!(
            job.action().remote,
            Some(
                RemoteAction::Privileged(_)
                    | RemoteAction::PrivilegedFileAction(_)
                    | RemoteAction::PrivilegedArchive(_)
            )
        ) {
            return Ok((job, None));
        }
        let (current, action, _) = subject(&job, &self.zone)?;
        let Some((approved, receipt)) = &view.privileged_review.approved else {
            if matches!(action, RemoteAction::Privileged(_)) && !job.enabled() {
                return Ok((job, None));
            }
            let approval = match action {
                RemoteAction::Privileged(transfer) => &transfer.approval,
                RemoteAction::PrivilegedFileAction(metadata) => &metadata.approval,
                RemoteAction::PrivilegedArchive(archive) => &archive.approval,
                _ => unreachable!("reviewed action"),
            };
            if matches!(action, RemoteAction::Privileged(transfer) if transfer.standing.is_some()) {
                self.sync_store
                    .load_replacement(
                        &current.job,
                        approval.as_deref(),
                        current.scope,
                        SystemTime::now(),
                        None,
                    )
                    .map_err(|error| error.to_string())?;
            } else {
                self.sync_store
                    .load(&current.job, approval.as_deref(), current.scope)
                    .map_err(|_| review_required().to_string())?;
            }
            return Ok((job, None));
        };
        if &current != approved {
            return Err("The approved file action changed. Review it again.".into());
        }
        let mut action = job.action().clone();
        match action.remote.as_mut() {
            Some(RemoteAction::Privileged(transfer)) => {
                transfer.approval = Some(receipt.nonce().to_owned())
            }
            Some(RemoteAction::PrivilegedFileAction(metadata)) => {
                metadata.approval = Some(receipt.nonce().to_owned())
            }
            Some(RemoteAction::PrivilegedArchive(archive)) => {
                archive.approval = Some(receipt.nonce().to_owned())
            }
            _ => unreachable!("privileged file action was checked"),
        }
        job.definition()
            .with_action(action)
            .map(|job| (job, Some(receipt.clone())))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn privileged_run_context(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Option<RunContext> {
        let job = self.scheduler.graph().job(&request.job)?;
        if job.action() != request.action.as_ref() {
            return None;
        }
        let (subject, action, _) = subject(job, &self.zone).ok()?;
        let recurring = if matches!(action, RemoteAction::Privileged(transfer) if transfer.standing.is_some())
        {
            if !self.can_run() || request.cancellation.is_requested() {
                return None;
            }
            let eligible = self
                .scheduler
                .active_predecessor_blockers(request, SystemTime::now())?;
            if !eligible.is_empty() {
                return None;
            }
            let occurrence = self.scheduler.active_firing_occurrence(request)?;
            let firing = standing::Firing {
                epoch: self.sync_epoch?,
                run: request.id,
            };
            Some(RecurringRun {
                scope: subject.scope,
                admission: standing::Admission::new(firing, job, request, occurrence).ok()?,
            })
        } else {
            None
        };
        Some(RunContext {
            store: self.sync_store.clone(),
            job: subject.job,
            scope: subject.action_scope,
            recurring,
        })
    }
}
