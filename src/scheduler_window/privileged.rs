//! Device-local review of exact scheduled privileged file actions.

use std::io;
use std::path::PathBuf;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};
use kortty_jobs::{Job, JobId, RunRequest};
use kortty_ssh::{Endpoint, HostKey};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::sftp_window::privileged_file::{self, ProtectedOperation};

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
        scope_digest(&(
            "kortty-scheduled-privileged-file-v1",
            reference,
            self.direction,
            &self.local,
            &self.remote,
            sudo,
        ))
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
                "Choose valid numeric ownership or permissions without special bits."
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
}

impl RunContext {
    pub(crate) fn authorize(
        &self,
        transfer: &PrivilegedTransfer,
        reference: &ConnectionRef,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<()> {
        transfer.validate().map_err(io::Error::other)?;
        self.require_scope(
            transfer.scope(reference, sudo)?,
            transfer.approval.as_deref(),
            sudo,
            pins,
        )
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
    }

    pub(crate) fn authorize_archive(
        &self,
        archive: &PrivilegedArchive,
        reference: &ConnectionRef,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<()> {
        if sudo != SudoMode::Passwordless {
            return Err(review_required());
        }
        archive.validate().map_err(io::Error::other)?;
        self.require_scope(
            archive.scope(reference, sudo)?,
            archive.approval.as_deref(),
            sudo,
            pins,
        )
    }

    fn require_scope(
        &self,
        scope: [u8; 32],
        approval: Option<&str>,
        sudo: SudoMode,
        pins: &[(Endpoint, HostKey)],
    ) -> io::Result<()> {
        if sudo == SudoMode::Disabled || scope != self.scope {
            return Err(review_required());
        }
        let receipt = self.store.load(&self.job, approval, self.scope)?;
        if receipt.pins != sync::fingerprints(pins) {
            return Err(review_required());
        }
        Ok(())
    }
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
    let receipt = sync::Receipt::for_operation(scope, [0; 32], sync::fingerprints(pins))?;
    let store = sync::Store::new(profile);
    store.swap(job, receipt.clone(), |_| true)?;
    transfer.approval = Some(receipt.approval);
    Ok(RunContext {
        store,
        job: job.clone(),
        scope,
    })
}

fn review_required() -> io::Error {
    io::Error::other("Review the exact privileged file action and SSH route again.")
}

#[derive(Clone, Eq, PartialEq)]
struct Subject {
    job: JobId,
    scope: [u8; 32],
}

fn subject(job: &Job<SshCommand>) -> Result<(Subject, &RemoteAction, &ConnectionRef), String> {
    let action = job.action();
    action.validate_elevation()?;
    let reference = action
        .connection
        .as_ref()
        .ok_or("Choose one saved SSH connection.")?;
    let (scope, remote) = match &action.remote {
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
    let scope = scope.map_err(|_| "Could not bind the privileged file review.")?;
    Ok((
        Subject {
            job: job.id().clone(),
            scope,
        },
        remote,
        reference,
    ))
}

#[derive(Default)]
pub(super) struct DraftReview {
    review: Option<Review>,
    approved: Option<(Subject, sync::Receipt)>,
}

struct Review {
    subject: Subject,
    action: RemoteAction,
    reference: ConnectionRef,
    sudo: SudoMode,
    pins: Vec<String>,
}

impl DraftReview {
    pub(super) fn approved_job(&self) -> Option<&JobId> {
        self.approved.as_ref().map(|(subject, _)| &subject.job)
    }

    pub(super) fn view(&self) -> Option<Element<'_, Message>> {
        let review = self.review.as_ref()?;
        let (operation, local, remote, destination) = match &review.action {
            RemoteAction::Privileged(transfer) => (
                match transfer.direction {
                    PrivilegedDirection::Upload => "Upload to a new root-owned remote file",
                    PrivilegedDirection::Download => {
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
                format!("Create a new root-owned {} archive", archive.archive.format),
                None,
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
            text("Approval applies to future runs of this exact job action.").size(12),
        ]
        .spacing(5);
        if let RemoteAction::PrivilegedArchive(action) = &review.action {
            let archive = &action.archive;
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
                    button("Approve privileged action").on_press(Message::ApprovePrivileged),
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
    ) -> Result<(), String> {
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
        let (subject, action, reference) = subject(&job)?;
        let pins = pins(reference)?;
        view.privileged_review = DraftReview {
            review: Some(Review {
                subject,
                action: action.clone(),
                reference: reference.clone(),
                sudo: job.action().sudo,
                pins,
            }),
            approved: None,
        };
        view.status = None;
        Ok(())
    }

    pub(super) fn approve_privileged(&self, view: &mut SchedulerView) -> Result<(), String> {
        let Some(review) = view.privileged_review.review.take() else {
            return Ok(());
        };
        if view.page != Page::Action
            || !self.can_browse_directory()
            || !self.can_edit()
            || !self.draft_matches(view)
            || subject(&self.build_current_draft(view, review.subject.job.clone())?)?.0
                != review.subject
        {
            return Err("The reviewed file action changed. Review it again.".into());
        }
        let receipt = sync::Receipt::for_operation(review.subject.scope, [0; 32], review.pins)
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
    ) -> Result<(Job<SshCommand>, Option<sync::Receipt>), String> {
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
        let (current, action, _) = subject(&job)?;
        let Some((approved, receipt)) = &view.privileged_review.approved else {
            let approval = match action {
                RemoteAction::Privileged(transfer) => &transfer.approval,
                RemoteAction::PrivilegedFileAction(metadata) => &metadata.approval,
                RemoteAction::PrivilegedArchive(archive) => &archive.approval,
                _ => unreachable!("reviewed action"),
            };
            self.sync_store
                .load(&current.job, approval.as_deref(), current.scope)
                .map_err(|_| review_required().to_string())?;
            return Ok((job, None));
        };
        if &current != approved {
            return Err("The approved file action changed. Review it again.".into());
        }
        let mut action = job.action().clone();
        match action.remote.as_mut() {
            Some(RemoteAction::Privileged(transfer)) => {
                transfer.approval = Some(receipt.approval.clone())
            }
            Some(RemoteAction::PrivilegedFileAction(metadata)) => {
                metadata.approval = Some(receipt.approval.clone())
            }
            Some(RemoteAction::PrivilegedArchive(archive)) => {
                archive.approval = Some(receipt.approval.clone())
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
        let (subject, _, _) = subject(job).ok()?;
        Some(RunContext {
            store: self.sync_store.clone(),
            job: subject.job,
            scope: subject.scope,
        })
    }
}
