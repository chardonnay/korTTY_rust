//! Device-local approval for rsync transfers on saved SSH routes.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task};
use kortty_server_archive::privileged_rsync::{Review as PrivilegedReview, Spec as PrivilegedSpec};
use kortty_server_archive::rsync_tree::{ExistingTree, Inventory};
use serde::{Deserialize, Serialize};

use crate::job_executor::rsync::LocalExistingSnapshot;

use super::{ConnectionRef, Message, Page, RemotePath, SchedulerView};

mod batch;
pub(crate) use batch::MAX_SOURCES;
mod elevation;
pub(crate) use elevation::Elevation;
pub(crate) mod mixed;
pub(crate) use mixed::{
    MixedDestination, MixedPlan, MixedPrivilegedReview, MixedReceiver, MixedReview,
    MixedReviewTarget,
};
mod routes;
pub(crate) mod standing;
pub(super) use batch::{MAX_SOURCE_TEXT, sources as batch_sources};
pub(crate) use routes::ReviewRequests;
pub use routes::Reviewed;
pub(crate) use routes::Route;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    pub(crate) program: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) remote: RemotePath,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) directory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing: Option<ExistingDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing_download: Option<ExistingDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mixed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) additional_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) standing: Option<standing::Config>,
    #[serde(default, skip_serializing_if = "Elevation::is_ordinary")]
    pub(crate) elevation: Elevation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) routes: Vec<Route>,
}

pub(crate) const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_TREE_ENTRIES: usize = 4_096;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryReview {
    pub(crate) source: RemotePath,
    pub(crate) destination: PathBuf,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadReview {
    pub(crate) source: PathBuf,
    pub(crate) destination: RemotePath,
    pub(crate) bytes: u64,
    pub(crate) digest: [u8; 32],
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryUploadReview {
    pub(crate) source: PathBuf,
    pub(crate) destination: RemotePath,
    pub(crate) inventory: Inventory,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExistingDestination {
    pub(crate) delete: bool,
    pub(crate) include_source_name: bool,
}

impl ExistingDestination {
    pub(super) const FILE: Self = Self {
        delete: false,
        include_source_name: false,
    };
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExistingReview {
    pub(crate) destination: RemotePath,
    pub(crate) snapshot: ExistingTree,
    pub(crate) settings: ExistingDestination,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalExistingReview {
    pub(crate) destination: PathBuf,
    pub(crate) snapshot: LocalExistingSnapshot,
    pub(crate) settings: ExistingDestination,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalExistingFileReview {
    pub(crate) destination: PathBuf,
    pub(crate) snapshot: crate::sftp_window::LocalFileSnapshot,
}

pub(super) const LOCAL_EXISTING_SUPPORTED: bool =
    cfg!(any(windows, target_os = "linux", target_os = "macos"));

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub(crate) program: PathBuf,
    pub(crate) digest: [u8; 32],
    pub(crate) reference: ConnectionRef,
    pub(crate) pins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) upload: Option<UploadReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory: Option<Box<DirectoryReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory_upload: Option<Box<DirectoryUploadReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing: Option<Box<ExistingReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing_download: Option<Box<LocalExistingReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing_file_download: Option<Box<LocalExistingFileReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mixed: Option<Box<MixedReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<Box<batch::Review>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) download: Option<Box<DirectoryReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) privileged: Option<Box<PrivilegedReview>>,
}

impl Approval {
    fn same_existing_source(&self, other: &Self) -> bool {
        if let Some(mixed) = &self.mixed
            && matches!(mixed.receiver, MixedReceiver::Server { .. })
        {
            return other
                .mixed
                .as_ref()
                .is_some_and(|other| mixed.plan == other.plan);
        }
        if let Some(upload) = self.upload.as_ref().filter(|_| self.privileged.is_some()) {
            return other.upload.as_ref().is_some_and(|candidate| {
                candidate.source == upload.source
                    && candidate.bytes == upload.bytes
                    && candidate.digest == upload.digest
            });
        }
        (self.existing.is_none() && self.privileged.is_none())
            || self
                .directory_upload
                .as_ref()
                .map(|review| review.inventory)
                == other
                    .directory_upload
                    .as_ref()
                    .map(|review| review.inventory)
    }

    pub(super) fn fingerprint(&self) -> String {
        use std::fmt::Write as _;
        let mut result = String::with_capacity(64);
        for byte in self.digest {
            let _ = write!(result, "{byte:02x}");
        }
        result
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        super::remote::validate_local_path(&self.program)?;
        self.reference.validate()?;
        if !(1..=2).contains(&self.pins.len())
            || self
                .pins
                .iter()
                .any(|pin| pin.len() > 512 || pin.chars().any(char::is_control))
        {
            return Err("The rsync route approval is invalid.");
        }
        if let Some(mixed) = &self.mixed {
            if self.upload.is_some()
                || self.directory.is_some()
                || self.directory_upload.is_some()
                || self.existing.is_some()
                || self.existing_download.is_some()
                || self.existing_file_download.is_some()
                || self.batch.is_some()
                || self.download.is_some()
                || self.privileged.is_some()
                || !mixed.valid()
            {
                return Err("The reviewed mixed rsync approval is invalid.");
            }
            if let MixedReceiver::Local { destination, .. } = &mixed.receiver {
                require_program_outside_receiver(&self.program, destination)?;
            }
            return Ok(());
        }
        if let Some(batch) = &self.batch {
            if self.upload.is_some()
                || self.directory.is_some()
                || self.directory_upload.is_some()
                || self.existing.is_some()
                || self.existing_download.is_some()
                || self.existing_file_download.is_some()
                || self.download.is_some()
                || self.privileged.is_some()
            {
                return Err("An rsync batch approval cannot authorize a separate transfer.");
            }
            return batch.validate();
        }
        if let Some(upload) = &self.upload {
            super::remote::validate_local_path(&upload.source)?;
            if upload.source == self.program
                || !valid_remote(upload.destination.as_str())
                || upload.bytes > MAX_FILE_BYTES
            {
                return Err("The reviewed rsync upload is invalid.");
            }
        }
        if let Some(directory) = &self.directory {
            super::remote::validate_local_path(&directory.destination)?;
            if self.upload.is_some()
                || self.directory_upload.is_some()
                || directory.destination == self.program
                || !valid_remote(directory.source.as_str())
            {
                return Err("The reviewed rsync directory download is invalid.");
            }
        }
        if let Some(directory) = &self.directory_upload {
            super::remote::validate_local_path(&directory.source)?;
            if self.upload.is_some()
                || directory.source == self.program
                || !valid_remote(directory.destination.as_str())
                || !directory.inventory.valid()
            {
                return Err("The reviewed rsync directory upload is invalid.");
            }
        }
        if let Some(existing) = &self.existing {
            let exact = match &self.upload {
                Some(file) => {
                    self.privileged
                        .as_deref()
                        .is_some_and(elevation::is_file_replacement)
                        && existing.settings == ExistingDestination::FILE
                        && file.destination == existing.destination
                }
                None => self.directory_upload.as_ref().is_some_and(|directory| {
                    mapped_receiver(&directory.destination, &directory.source, existing.settings)
                        .ok()
                        .as_ref()
                        == Some(&existing.destination)
                }),
            };
            if !exact
                || !existing.snapshot.inventory.valid()
                || self.download.is_some()
                || self.directory.is_some()
            {
                return Err("The reviewed existing rsync destination is invalid.");
            }
        }
        if let Some(existing) = &self.existing_download
            && (self.directory.as_ref().is_none_or(|directory| {
                mapped_local_receiver(&directory.destination, &directory.source, existing.settings)
                    .ok()
                    .as_ref()
                    != Some(&existing.destination)
            }) || !existing.snapshot.valid()
                || self.existing.is_some()
                || self.download.is_some())
        {
            return Err("The reviewed existing local rsync destination is invalid.");
        }
        if let Some(existing) = &self.existing_download {
            require_program_outside_receiver(&self.program, &existing.destination)?;
        }
        if let Some(existing) = &self.existing_file_download {
            if self
                .download
                .as_ref()
                .is_none_or(|review| review.destination != existing.destination)
                || self.existing.is_some()
                || self.existing_download.is_some()
                || !existing.snapshot.valid()
            {
                return Err("The reviewed existing local rsync file is invalid.");
            }
            require_program_outside_receiver(&self.program, &existing.destination)?;
        }
        if let Some(download) = &self.download {
            super::remote::validate_local_path(&download.destination)?;
            if download.destination == self.program
                || !valid_remote(download.source.as_str())
                || self.directory.is_some()
                || self.upload.is_some()
                || self.directory_upload.is_some()
            {
                return Err("The reviewed rsync file download is invalid.");
            }
        }
        if let Some(review) = &self.privileged
            && !self.matches_elevation(Some(&review.spec))
        {
            return Err(elevation::REVIEW_REQUIRED);
        }
        Ok(())
    }
}

impl Transfer {
    pub(crate) fn uses_existing_destination(&self) -> bool {
        self.existing.is_some() || self.existing_download.is_some()
    }

    pub(crate) fn existing_destination(&self) -> Result<RemotePath, &'static str> {
        let settings = self
            .existing
            .ok_or("Choose an existing rsync destination.")?;
        mapped_receiver(&self.remote, &self.local, settings)
    }

    pub(crate) fn existing_local_destination(&self) -> Result<PathBuf, &'static str> {
        let settings = self
            .existing_download
            .ok_or("Choose an existing local rsync destination.")?;
        mapped_local_receiver(&self.local, &self.remote, settings)
    }

    pub(crate) fn require_safe_existing_local_program(&self) -> Result<(), &'static str> {
        let present = self
            .approval
            .as_ref()
            .and_then(|approval| approval.mixed.as_deref())
            .is_none_or(|mixed| {
                matches!(
                    &mixed.receiver,
                    MixedReceiver::Local {
                        snapshot: Some(_),
                        ..
                    }
                )
            });
        let destination = canonical_local_receiver(&self.existing_local_destination()?, present)?;
        let program = self
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &destination)
    }

    pub(crate) fn require_safe_mixed_local_program(&self) -> Result<(), &'static str> {
        let destination = match self.local.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                canonical_local_receiver(&self.local, false)?
            }
            Err(_) => return Err("The local rsync destination changed or is unavailable."),
        };
        let program = self
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &destination)
    }

    pub(crate) fn validate(&self, upload: bool) -> Result<(), &'static str> {
        let privileged = (self.uses_passwordless_sudo() && self.mixed.is_none())
            .then(|| self.privileged_spec(upload))
            .transpose()?;
        if let Some(approval) = &self.approval
            && !approval.matches_elevation(privileged.as_ref())
        {
            return Err(elevation::REVIEW_REQUIRED);
        }
        if let Some(config) = &self.standing {
            config
                .settings
                .validate()
                .map_err(|_| "Choose valid limits for recurring rsync.")?;
            if !(self.uses_existing_destination() || self.mixed.is_some())
                || self
                    .approval
                    .as_ref()
                    .is_some_and(|approval| standing::Receiver::from_review(approval).is_err())
            {
                return Err(standing::EXISTING_RECEIVER_REQUIRED);
            }
            if config
                .approval
                .as_deref()
                .is_some_and(|id| !super::sync::valid_nonce(id))
            {
                return Err("Review the recurring rsync approval again.");
            }
        }
        super::remote::validate_local_path(&self.program)?;
        super::remote::validate_local_path(&self.local)?;
        if self.program == self.local {
            return Err("Choose a transfer path separate from the rsync executable.");
        }
        if !valid_remote(self.remote.as_str()) {
            return Err(
                "Use an absolute server path containing letters, digits, slash, dot, underscore, or hyphen.",
            );
        }
        if let Some(sources) = &self.mixed {
            self.validate_mixed(sources, upload)?;
            if !upload {
                require_program_outside_receiver(&self.program, &self.local)?;
            }
            if let Some(approval) = &self.approval {
                approval.validate()?;
                let request = self.mixed_target(&approval.reference, upload)?;
                if approval.program != self.program
                    || !approval
                        .mixed
                        .as_ref()
                        .is_some_and(|review| request.matches(review))
                {
                    return Err("Review the mixed rsync sources and destination again.");
                }
            }
            return Ok(());
        }
        if self.existing.is_some() {
            if !upload
                || !self.additional_sources.is_empty()
                || !self.directory
                    && !(self.uses_passwordless_sudo()
                        && self.existing == Some(ExistingDestination::FILE))
            {
                return Err(
                    "Choose one local source directory or one file with passwordless sudo.",
                );
            }
            self.existing_destination()?;
        }
        if let Some(settings) = self.existing_download {
            if upload
                || !self.additional_sources.is_empty()
                || !self.directory
                    && (settings != ExistingDestination::FILE || !LOCAL_EXISTING_SUPPORTED)
            {
                return Err(
                    "Choose one server file or directory for an existing local rsync destination.",
                );
            }
            require_program_outside_receiver(&self.program, &self.existing_local_destination()?)?;
        }
        let batch = self.batch_plan(upload)?;
        if let Some(approval) = &self.approval {
            approval.validate()?;
            if approval.mixed.is_some() {
                return Err("Review the exact rsync sources and destination again.");
            }
            if let Some(plan) = batch {
                if approval.program != self.program
                    || !approval
                        .batch
                        .as_ref()
                        .is_some_and(|review| review.matches(&plan))
                {
                    return Err("Review the exact rsync sources and destinations again.");
                }
                return Ok(());
            }
            if (upload && !self.directory) != approval.upload.is_some()
                || (!upload && self.directory) != approval.directory.is_some()
                || (upload && self.directory) != approval.directory_upload.is_some()
                || approval.batch.is_some()
                || approval.existing.as_ref().map(|review| review.settings) != self.existing
                || approval.existing.as_ref().is_some_and(|review| {
                    self.existing_destination().ok().as_ref() != Some(&review.destination)
                })
                || approval
                    .existing_download
                    .as_ref()
                    .map(|review| review.settings)
                    != self.existing_download.filter(|_| self.directory)
                || approval.existing_download.as_ref().is_some_and(|review| {
                    self.existing_local_destination().ok().as_ref() != Some(&review.destination)
                })
                || approval
                    .existing_file_download
                    .as_ref()
                    .map(|review| &review.destination)
                    != self
                        .existing_download
                        .filter(|_| !self.directory)
                        .map(|_| &self.local)
                || approval.upload.as_ref().is_some_and(|review| {
                    review.source != self.local || review.destination != self.remote
                })
                || approval.directory.as_ref().is_some_and(|review| {
                    review.source != self.remote || review.destination != self.local
                })
                || approval.directory_upload.as_ref().is_some_and(|review| {
                    review.source != self.local || review.destination != self.remote
                })
                || approval.download.as_ref().is_some_and(|review| {
                    upload
                        || self.directory
                        || review.source != self.remote
                        || review.destination != self.local
                })
            {
                return Err("Review the exact rsync transfer paths again.");
            }
        }
        Ok(())
    }
}

fn mapped_receiver(
    root: &RemotePath,
    source: &std::path::Path,
    settings: ExistingDestination,
) -> Result<RemotePath, &'static str> {
    let destination = if settings.include_source_name {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Choose a local source directory with a literal name.")?;
        root.child(name.as_bytes())?
    } else {
        root.clone()
    };
    if !valid_remote(destination.as_str()) {
        return Err("Choose a literal absolute server path for the existing rsync destination.");
    }
    Ok(destination)
}

fn mapped_local_receiver(
    root: &Path,
    source: &RemotePath,
    settings: ExistingDestination,
) -> Result<PathBuf, &'static str> {
    let destination = if settings.include_source_name {
        let name = source.basename();
        if !kortty_server_archive::rsync_tree::valid_name(name) {
            return Err("Choose a server source directory with a literal name.");
        }
        root.join(name)
    } else {
        root.to_owned()
    };
    super::remote::validate_local_path(&destination)?;
    Ok(destination)
}

fn require_program_outside_receiver(
    program: &Path,
    destination: &Path,
) -> Result<(), &'static str> {
    let program = local_scope_path(program)?;
    let destination = local_scope_path(destination)?;
    if program.starts_with(&destination) {
        return Err("Choose a local destination that does not contain the rsync executable.");
    }
    #[cfg(windows)]
    if destination.parent() == program.parent()
        && destination
            .extension()
            .is_some_and(|extension| extension == "dll")
    {
        return Err("Choose a local destination separate from the rsync program libraries.");
    }
    Ok(())
}

fn canonical_local_receiver(destination: &Path, present: bool) -> Result<PathBuf, &'static str> {
    let changed = "The local rsync destination changed or is unavailable.";
    if present {
        return destination.canonicalize().map_err(|_| changed);
    }
    let name = destination.file_name().ok_or(changed)?;
    let parent = destination
        .parent()
        .ok_or(changed)?
        .canonicalize()
        .map_err(|_| changed)?;
    Ok(parent.join(name))
}

fn local_scope_path(path: &Path) -> Result<PathBuf, &'static str> {
    let path = path
        .to_str()
        .ok_or("Choose a UTF-8 local path for rsync.")?
        .to_lowercase();
    #[cfg(windows)]
    let path = {
        let native = path.replace('/', "\\");
        if let Some(network) = native.strip_prefix(r"\\?\unc\") {
            format!(r"\\{network}")
        } else {
            native.strip_prefix(r"\\?\").unwrap_or(&native).to_owned()
        }
    };
    Ok(PathBuf::from(path))
}

pub(crate) fn valid_remote(path: &str) -> bool {
    path.len() <= 1024
        && path.starts_with('/')
        && !path.ends_with('/')
        && path
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

pub(super) struct ReviewRequest {
    pub(super) program: PathBuf,
    pub(super) reference: ConnectionRef,
    pub(super) upload: Option<(PathBuf, RemotePath)>,
    pub(super) directory: Option<DirectoryReview>,
    pub(super) directory_upload: Option<(PathBuf, RemotePath)>,
    pub(super) existing: Option<(ExistingDestination, RemotePath)>,
    pub(super) existing_download: Option<(ExistingDestination, PathBuf)>,
    pub(super) mixed: Option<MixedReviewTarget>,
    pub(super) batch: Option<batch::Plan>,
    pub(super) download: Option<DirectoryReview>,
    pub(super) privileged: Option<PrivilegedSpec>,
}

impl ReviewRequest {
    pub(super) fn matches(&self, approval: &Approval, legacy_single: bool) -> bool {
        approval.validate().is_ok()
            && self.program == approval.program
            && self.reference == approval.reference
            && approval.matches_elevation(self.privileged.as_ref())
            && match (&self.mixed, &approval.mixed) {
                (Some(request), Some(review)) => request.matches(review),
                (None, None) => true,
                _ => false,
            }
            && match (&self.batch, &approval.batch) {
                (None, None) => true,
                (Some(plan), Some(review)) => review.matches(plan),
                _ => false,
            }
            && self.directory.as_ref() == approval.directory.as_deref()
            && self
                .existing
                .as_ref()
                .map(|(settings, path)| (settings, path))
                == approval
                    .existing
                    .as_ref()
                    .map(|review| (&review.settings, &review.destination))
            && self
                .existing_download
                .as_ref()
                .map(|(settings, path)| (settings, path))
                == approval
                    .existing_download
                    .as_ref()
                    .map(|review| (&review.settings, &review.destination))
                    .or_else(|| {
                        approval
                            .existing_file_download
                            .as_ref()
                            .map(|review| (&ExistingDestination::FILE, &review.destination))
                    })
            && (self.download.as_ref() == approval.download.as_deref()
                || legacy_single && approval.download.is_none())
            && self
                .directory_upload
                .as_ref()
                .map(|(source, destination)| (source, destination))
                == approval
                    .directory_upload
                    .as_ref()
                    .map(|review| (&review.source, &review.destination))
            && match (&self.upload, &approval.upload) {
                (None, None) => true,
                (Some((source, destination)), Some(review)) => {
                    &review.source == source && &review.destination == destination
                }
                _ => false,
            }
    }
}

fn review(
    request: ReviewRequest,
    trust: &kortty_ssh::TrustStore,
    digest: [u8; 32],
    inspect: &mut impl FnMut(
        &ConnectionRef,
        &RemotePath,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<ExistingTree, String>,
    inspect_mixed: &mut impl FnMut(
        &MixedReviewTarget,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<MixedReview, String>,
    inspect_privileged: &mut impl FnMut(
        &ConnectionRef,
        &PrivilegedSpec,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<PrivilegedReview, String>,
    cancelled: &Arc<AtomicBool>,
) -> Result<Approval, String> {
    require_review_running(cancelled)?;
    super::remote::validate_local_path(&request.program)?;
    let pins = super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?;
    if request.privileged.is_some()
        && request.existing_download.is_none()
        && let Some(paths) = request.directory.as_ref().or(request.download.as_ref())
    {
        match std::fs::symlink_metadata(&paths.destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err("Choose a new local destination in an existing checked folder.".into());
            }
        }
        let canonical = canonical_local_receiver(&paths.destination, false)?;
        let program = request
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &canonical)?;
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        if request.directory.is_some() {
            crate::sftp_window::PrivateTreeDestination::inspect(&paths.destination)
                .map_err(|error| error.to_string())?;
        }
    }
    let upload = if let Some((source, destination)) = request.upload {
        super::remote::validate_local_path(&source)?;
        if source == request.program || !valid_remote(destination.as_str()) {
            return Err("Choose a regular local source and an absolute server file path.".into());
        }
        let (bytes, digest) = crate::job_executor::rsync::upload_digest(&source)
            .map_err(|error| error.to_string())?;
        Some(UploadReview {
            source,
            destination,
            bytes,
            digest,
        })
    } else {
        None
    };
    let directory_upload = request
        .directory_upload
        .map(|(source, destination)| {
            if source == request.program || !valid_remote(destination.as_str()) {
                return Err("Choose one local directory and an absolute server path.".to_owned());
            }
            let inventory = crate::job_executor::rsync::upload_tree_inventory(&source)
                .map_err(|error| error.to_string())?;
            Ok(Box::new(DirectoryUploadReview {
                source,
                destination,
                inventory,
            }))
        })
        .transpose()?;
    let privileged = request
        .privileged
        .as_ref()
        .map(|spec| {
            require_review_running(cancelled)?;
            let proof = inspect_privileged(&request.reference, spec, trust, cancelled)?;
            if !proof.valid()
                || &proof.spec != spec
                || super::sync::pins(&request.reference, trust)
                    .map_err(|error| error.to_string())?
                    != pins
            {
                return Err(
                    "The protected rsync paths, policy or saved host keys changed.".to_owned(),
                );
            }
            require_review_running(cancelled)?;
            Ok(Box::new(proof))
        })
        .transpose()?;
    let existing = request
        .existing
        .map(|(settings, destination)| {
            require_review_running(cancelled)?;
            let snapshot = if let Some(proof) = &privileged {
                elevation::existing_snapshot(proof).ok_or(elevation::REVIEW_REQUIRED)?
            } else {
                inspect(&request.reference, &destination, trust, cancelled)?
            };
            if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?
                != pins
            {
                return Err("The saved SSH route or host keys changed during review.".to_owned());
            }
            require_review_running(cancelled)?;
            Ok(Box::new(ExistingReview {
                destination,
                snapshot,
                settings,
            }))
        })
        .transpose()?;
    let existing_download = request
        .existing_download
        .as_ref()
        .filter(|_| request.directory.is_some())
        .map(|(settings, destination)| {
            require_review_running(cancelled)?;
            let snapshot =
                crate::job_executor::rsync::review_local_existing(destination, cancelled)?;
            if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?
                != pins
            {
                return Err("The saved SSH route or host keys changed during review.".to_owned());
            }
            require_review_running(cancelled)?;
            Ok::<_, String>(Box::new(LocalExistingReview {
                destination: destination.clone(),
                snapshot,
                settings: *settings,
            }))
        })
        .transpose()?;
    let existing_file_download = request
        .existing_download
        .filter(|_| request.download.is_some())
        .map(|(_, destination)| {
            require_review_running(cancelled)?;
            let snapshot = crate::sftp_window::ReviewedFileReceiver::inspect(&destination, &|| {
                cancelled.load(Ordering::Acquire)
            })
            .map_err(|error| error.to_string())?;
            if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?
                != pins
            {
                return Err("The saved SSH route or host keys changed during review.".to_owned());
            }
            require_review_running(cancelled)?;
            Ok(Box::new(LocalExistingFileReview {
                destination,
                snapshot,
            }))
        })
        .transpose()?;
    let mixed = request
        .mixed
        .map(|target| {
            require_review_running(cancelled)?;
            let review = inspect_mixed(&target, trust, cancelled)?;
            if !target.matches(&review)
                || super::sync::pins(&request.reference, trust)
                    .map_err(|error| error.to_string())?
                    != pins
            {
                return Err("The mixed rsync review or saved host keys changed.".to_owned());
            }
            require_review_running(cancelled)?;
            Ok(Box::new(review))
        })
        .transpose()?;
    let approval = Approval {
        program: request.program,
        digest,
        reference: request.reference,
        pins: super::sync::fingerprints(&pins),
        upload,
        directory: request.directory.map(Box::new),
        directory_upload,
        existing,
        existing_download,
        existing_file_download,
        mixed,
        batch: request
            .batch
            .map(batch::Plan::review)
            .transpose()?
            .map(Box::new),
        download: request.download.map(Box::new),
        privileged,
    };
    approval.validate()?;
    Ok(approval)
}

fn require_review_running(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("Rsync review was cancelled.".into())
    } else {
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Editor {
    pending: Option<(String, Arc<AtomicBool>, Option<standing::Intent>)>,
    approval: Option<(Reviewed, Option<standing::Intent>)>,
    pub(super) grant: Option<(kortty_jobs::JobId, standing::Grant)>,
}

impl Editor {
    pub(super) fn begin(
        &mut self,
        ticket: String,
        intent: Option<standing::Intent>,
    ) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        *self = Self::default();
        self.pending = Some((ticket, Arc::clone(&cancelled), intent));
        cancelled
    }

    pub(super) fn complete(
        &mut self,
        ticket: &str,
        result: Result<Reviewed, String>,
    ) -> Option<String> {
        if self.pending.as_ref().map(|(id, _, _)| id.as_str()) != Some(ticket) {
            return None;
        }
        let (_, _, intent) = self.pending.take()?;
        match result {
            Ok(approval) => {
                if intent.is_some() {
                    let routes = std::iter::once(&approval.primary).chain(&approval.additional);
                    for route in routes {
                        if let Err(error) = standing::Receiver::from_review(route) {
                            return Some(error.to_string());
                        }
                    }
                }
                self.approval = Some((approval, intent));
                Some("Review the executable, verified SSH routes and transfer details.".into())
            }
            Err(error) => Some(error),
        }
    }

    pub(super) fn take(&mut self) -> Option<(Reviewed, Option<standing::Intent>)> {
        self.approval.take()
    }

    pub(super) fn view(&self) -> Option<Element<'_, Message>> {
        if self.pending.is_some() {
            return Some(
                row![
                    text("Checking the executable and saved SSH routes…").width(Length::Fill),
                    button("Cancel review").on_press(Message::DiscardRsyncProgram),
                ]
                .spacing(6)
                .into(),
            );
        }
        let (approval, intent) = self.approval.as_ref()?;
        let recurring = intent.is_some();
        let primary = &approval.primary;
        let privileged = primary.privileged.is_some()
            || primary
                .mixed
                .as_ref()
                .is_some_and(|review| review.privileged.is_some());
        let details = std::iter::once(primary)
            .chain(&approval.additional)
            .fold(column![].spacing(10), |items, route| {
                items.push(route.details(recurring))
            });
        let details: Element<'_, Message> = if approval.additional.is_empty()
            && primary.batch.is_none()
            && primary.existing.is_none()
            && primary.existing_download.is_none()
            && primary.existing_file_download.is_none()
            && primary.mixed.is_none()
        {
            details.into()
        } else {
            scrollable(details).height(Length::Fixed(210.0)).into()
        };
        let standing = intent.as_ref().map(|intent| {
            let budget = intent.settings.budget;
            let triggers = &intent.triggers;
            let schedule = triggers.schedule.as_ref().filter(|schedule| schedule.enabled());
            let calendar = schedule.and_then(|schedule| {
                let zone = kortty_jobs::TimeZone::get(&intent.zone).ok()?;
                Some(super::sync::standing_schedule(schedule, &zone))
            });
            column![
                text(format!(
                    "Valid for {} days and up to {} automatic firings.",
                    intent.settings.days, intent.settings.firings,
                )),
                text(format!(
                    "All routes combined per firing · {} entries · {} bytes · {} deletions",
                    budget.entries, budget.bytes, budget.deletes,
                )),
                calendar.map(text),
                triggers.chain().then(|| text("After a new successful round of all predecessors")),
                (!triggers.predecessors.is_empty()).then(|| text(format!(
                    "Predecessors · {}",
                    triggers.predecessors.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                ))),
                triggers.max_age_minutes.map(|age| text(format!("Predecessor maximum age · {age} minutes"))),
                text("Each firing checks the current sources and expected destinations. A successful publication retains the prior directory for recovery. Review any uncertain outcome before continuing.").size(12),
            ]
            .spacing(6)
        });
        Some(
            column![
                text(match (recurring, privileged) {
                    (true, true) => "Review recurring passwordless rsync",
                    (true, false) => "Review recurring rsync",
                    (false, true) => "Review passwordless rsync",
                    (false, false) => "Review rsync transfer",
                })
                .size(17),
                text(primary.program.display().to_string()),
                text(format!("SHA-256 · {}", primary.fingerprint())).size(12),
                standing,
                details,
                row![
                    button("Discard review").on_press(Message::DiscardRsyncProgram),
                    button(if recurring {
                        "Approve recurring firings"
                    } else if privileged {
                        "Approve passwordless transfer"
                    } else {
                        "Approve"
                    })
                    .on_press(Message::ApproveRsyncProgram),
                ]
                .spacing(6),
            ]
            .spacing(6)
            .into(),
        )
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if let Some((_, cancelled, _)) = &self.pending {
            cancelled.store(true, Ordering::Release);
        }
    }
}

impl Approval {
    fn details(&self, recurring: bool) -> iced::widget::Column<'_, Message> {
        let pins = self.pins.iter().fold(column![].spacing(4), |items, pin| {
            items.push(text(pin).size(12))
        });
        column![
                text(format!("Profile {} · {}", self.reference.id, self.reference.target.label())),
                pins,
                self.privileged.as_ref().map(|review| {
                    column![
                        text("Passwordless sudo · configured Linux helper"),
                        text(format!("Protected server path · {}", review.spec.path)),
                        review.inventory.map(|inventory| text(format!(
                            "{} files · {} folders · {} bytes in the checked server path",
                            inventory.files, inventory.entries - inventory.files, inventory.bytes,
                        )).size(12)),
                    ]
                    .spacing(4)
                }),
                self.upload.as_ref().map(|upload| {
                    let digest = upload
                        .digest
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    column![
                        text(format!("Local source · {}", upload.source.display())),
                        self.existing.is_none().then(|| text(format!(
                            "New server destination · {}",
                            upload.destination.as_str()
                        ))),
                        text(format!("{} bytes · SHA-256 {digest}", upload.bytes)).size(12),
                    ]
                    .spacing(4)
                }),
                self.download.as_ref().map(|download| column![
                    text(format!("Server file · {}", download.source.as_str())),
                    self.existing_file_download.is_none().then(|| text(format!("New local file · {}", download.destination.display()))),
                ].spacing(4)),
                self.directory.as_ref().map(|directory| column![
                    text(format!("Server directory · {}", directory.source.as_str())),
                    self.existing_download.is_none().then(|| column![
                        text(format!("New local folder · {}", directory.destination.display())),
                        text("Each run checks the complete tree, up to 4,096 entries and 256 MiB. A destination that exists blocks the run.").size(12),
                    ].spacing(4)),
                ].spacing(4)),
                self.directory_upload.as_ref().map(|directory| {
                    let inventory = directory.inventory;
                    let digest = inventory.digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                    column![
                        text(format!("Local directory · {}", directory.source.display())),
                        self.existing.is_none().then(|| text(format!("New server folder · {}", directory.destination.as_str()))),
                        text(format!("{} files · {} folders · {} bytes", inventory.files, inventory.entries - inventory.files, inventory.bytes)).size(12),
                        text(format!("Inventory SHA-256 · {digest}")).size(12),
                    ].spacing(4)
                }),
                self.existing.as_ref().map(|existing| {
                    let inventory = existing.snapshot.inventory;
                    if self.upload.is_some() {
                        return column![
                            text(format!("Existing server file · {}", existing.destination.as_str())),
                            text(if recurring {
                                "A successful replacement retains the prior file for recovery."
                            } else {
                                "A successful replacement retains the original file for recovery and requires a fresh review for the next run."
                            }).size(12),
                        ].spacing(4);
                    }
                    column![
                        text(format!("Existing server folder · {}", existing.destination.as_str())),
                        text(format!("Reviewed receiver contains {} files, {} folders and {} bytes.", inventory.files, inventory.entries - inventory.files, inventory.bytes)).size(12),
                        text(if existing.settings.delete {
                            "Mirror this folder. Receiver entries absent from the local source will be removed."
                        } else {
                            "Merge into this folder, replacing matching files and retaining receiver-only entries."
                        }).size(12),
                        (!recurring).then(|| text("A successful run retains the prior server folder for recovery and requires a fresh review before the next run.").size(12)),
                    ].spacing(4)
                }),
                self.existing_download.as_ref().map(|existing| {
                    let inventory = existing.snapshot.inventory;
                    column![
                        text(format!("Existing local folder · {}", existing.destination.display())),
                        text(format!("Reviewed receiver contains {} files, {} folders and {} bytes.", inventory.files, inventory.entries - inventory.files, inventory.bytes)).size(12),
                        text(if existing.settings.delete {
                            "Mirror this folder. The run removes local entries absent from its checked server tree."
                        } else {
                            "Merge into this folder, replacing matching files and retaining local-only entries."
                        }).size(12),
                        (!recurring).then(|| text("A successful run retains the prior local folder for recovery and requires a fresh review before the next run.").size(12)),
                    ].spacing(4)
                }),
                self.existing_file_download.as_ref().map(|existing| {
                    column![
                        text(format!("Existing local file · {}", existing.destination.display())),
                        text(format!("The reviewed file contains {} bytes.", existing.snapshot.inventory.bytes)).size(12),
                        text("The run replaces this exact file and keeps the original for recovery.").size(12),
                        (!recurring).then(|| text("Review the replacement before running this transfer again.").size(12)),
                    ].spacing(4)
                }),
                self.batch.as_ref().map(|review| review.view()),
                self.mixed.as_ref().map(|review| review.view()),
            ]
            .spacing(4)
    }
}

impl SchedulerView {
    pub(crate) fn rsync_review_groups(&self) -> Option<super::TargetGroups> {
        self.draft.rsync_groups()
    }

    pub(crate) fn rsync_review_requests(&self) -> Option<ReviewRequests> {
        if !cfg!(any(unix, windows)) || self.page != Page::Action || self.confirm.is_some() {
            return None;
        }
        self.draft.rsync_candidate()
    }

    pub(crate) fn begin_rsync_review(
        &mut self,
        request: ReviewRequests,
        inspect: impl FnMut(
            &ConnectionRef,
            &RemotePath,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<ExistingTree, String>
        + Send
        + 'static,
        inspect_mixed: impl FnMut(
            &MixedReviewTarget,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<MixedReview, String>
        + Send
        + 'static,
        inspect_privileged: impl FnMut(
            &ConnectionRef,
            &PrivilegedSpec,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<PrivilegedReview, String>
        + Send
        + 'static,
        verify_routes: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Task<Message> {
        let intent = match self.draft.rsync_standing_intent() {
            Ok(intent) => intent,
            Err(error) => {
                self.report_status(error);
                return Task::none();
            }
        };
        let Ok(ticket) = crate::private_files::nonce() else {
            self.report_status("Could not allocate the rsync review.".into());
            return Task::none();
        };
        let cancelled = self.rsync_review.begin(ticket.clone(), intent);
        self.status = None;
        let (sender, receiver) = async_channel::bounded(1);
        let _ = std::thread::Builder::new()
            .name("kortty-rsync-review".into())
            .spawn(move || {
                let result = request
                    .review(inspect, inspect_mixed, inspect_privileged, &cancelled)
                    .and_then(|review| {
                        verify_routes()?;
                        Ok(review)
                    });
                let _ = sender.send_blocking(result);
            });
        Task::future(async move {
            let result = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("Could not check the rsync executable.".into()));
            Message::RsyncProgramReviewed(ticket, result.map(Box::new))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn rsync_receiver_cannot_replace_an_adjacent_program_library() {
        let program = Path::new(r"C:\Tools\rsync.exe");
        let adjacent = Path::new(r"\\?\C:\tools\msys-2.0.DLL");
        assert!(require_program_outside_receiver(program, adjacent).is_err());
        let separate = Path::new(r"C:\Downloads\msys-2.0.dll");
        assert!(require_program_outside_receiver(program, separate).is_ok());
    }

    #[test]
    fn replacing_and_discarding_an_rsync_review_cancels_its_worker() {
        let mut editor = Editor::default();
        let first = editor.begin("first".into(), None);
        let second = editor.begin("second".into(), None);
        assert!(first.load(Ordering::Acquire));
        assert!(editor.complete("first", Err("stale".into())).is_none());
        assert!(!second.load(Ordering::Acquire));
        drop(editor);
        assert!(second.load(Ordering::Acquire));
    }

    #[test]
    fn rsync_upload_approval_cannot_be_reused_for_other_paths_or_a_download() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let local = root.join("source");
        let remote = RemotePath::try_from("/server/destination".to_owned()).unwrap();
        let reference: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "user"}
        }))
        .unwrap();
        let mut transfer = Transfer {
            program: program.clone(),
            local: local.clone(),
            remote: remote.clone(),
            directory: false,
            existing: None,
            existing_download: None,
            mixed: None,
            additional_sources: Vec::new(),
            routes: Vec::new(),
            standing: None,
            elevation: Elevation::Ordinary,
            approval: Some(Approval {
                program,
                digest: [1; 32],
                reference,
                pins: vec!["server.example SHA256:fixture".into()],
                upload: Some(UploadReview {
                    source: local,
                    destination: remote,
                    bytes: 4,
                    digest: [2; 32],
                }),
                directory: None,
                directory_upload: None,
                existing: None,
                existing_download: None,
                existing_file_download: None,
                mixed: None,
                batch: None,
                download: None,
                privileged: None,
            }),
        };
        assert!(transfer.validate(true).is_ok());
        assert!(transfer.validate(false).is_err());
        transfer.local = root.join("other-source");
        assert!(transfer.validate(true).is_err());
    }

    #[test]
    fn rsync_directory_requires_an_approval_for_the_exact_roots() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let local = root.join("new-folder");
        let remote = RemotePath::try_from("/server/source".to_owned()).unwrap();
        let program = root.join("rsync");
        let reference: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "user"}
        }))
        .unwrap();
        let digest = [1; 32];
        let mut transfer: Transfer = serde_json::from_value(serde_json::json!({
            "program": program, "local": local, "remote": remote,
            "approval": {"program": program, "digest": digest, "reference": reference,
                "pins": ["server.example SHA256:fixture"]}
        }))
        .unwrap();
        assert!(transfer.validate(false).is_ok());
        assert!(
            serde_json::to_value(&transfer)
                .unwrap()
                .get("directory")
                .is_none()
        );
        transfer.directory = true;
        assert!(transfer.validate(false).is_err());
        transfer.approval.as_mut().unwrap().directory = Some(Box::new(DirectoryReview {
            source: remote,
            destination: local,
        }));
        assert!(transfer.validate(false).is_ok());
        assert!(transfer.validate(true).is_err());
        transfer.local = root.join("other-folder");
        assert!(transfer.validate(false).is_err());
    }
}
