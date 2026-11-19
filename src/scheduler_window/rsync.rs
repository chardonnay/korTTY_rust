//! Device-local approval for rsync transfers on saved SSH routes.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task};
use kortty_server_archive::privileged_rsync::{Review as PrivilegedReview, Spec as PrivilegedSpec};
use kortty_server_archive::rsync_tree::{DeleteScope, ExistingTree, Inventory, RecurringReceiver};
use serde::{Deserialize, Serialize};

use crate::job_executor::rsync::{
    LocalExistingSnapshot, RecurringReceiverTarget, VacantFileProof, VacantLocalTreeSnapshot,
};

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_vacant_directory_download: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_vacant_file_download: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_vacant_server_file_upload: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_vacant_server_tree_upload: bool,
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
pub(crate) struct LocalVacantDirectoryReview {
    pub(crate) destination: PathBuf,
    pub(crate) snapshot: VacantLocalTreeSnapshot,
    pub(crate) settings: ExistingDestination,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalExistingFileReview {
    pub(crate) destination: PathBuf,
    pub(crate) snapshot: crate::sftp_window::LocalFileSnapshot,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalVacantFileReview {
    pub(crate) destination: PathBuf,
    pub(crate) snapshot: VacantFileProof,
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
    pub(crate) vacant_directory_download: Option<Box<LocalVacantDirectoryReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) existing_file_download: Option<Box<LocalExistingFileReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vacant_file_download: Option<Box<LocalVacantFileReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mixed: Option<Box<MixedReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<Box<batch::Review>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) download: Option<Box<DirectoryReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) privileged: Option<Box<PrivilegedReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ordinary_receiver: Option<OrdinaryReceiverReview>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrdinaryReceiverReview {
    pub(crate) destination: RemotePath,
    pub(crate) directory: bool,
    pub(crate) settings: ExistingDestination,
    pub(crate) receiver: RecurringReceiver,
}

impl Approval {
    fn first_server_file_upload(&self) -> bool {
        self.privileged
            .as_ref()
            .is_some_and(|review| review.first_file)
    }

    fn first_server_tree_upload(&self) -> bool {
        self.privileged
            .as_ref()
            .is_some_and(|review| review.first_tree)
    }

    fn existing_upload_scope(&self) -> Option<(ExistingDestination, &str)> {
        if let Some(review) = &self.ordinary_receiver {
            return Some((review.settings, review.destination.as_str()));
        }
        if let Some(review) = &self.existing {
            return Some((review.settings, review.destination.as_str()));
        }
        if self.first_server_file_upload() {
            let review = self.upload.as_ref()?;
            return Some((ExistingDestination::FILE, review.destination.as_str()));
        }
        let proof = self.privileged.as_ref().filter(|proof| proof.first_tree)?;
        let review = self.directory_upload.as_ref()?;
        let kortty_server_archive::privileged_rsync::Operation::Upload { delete, .. } =
            &proof.spec.operation
        else {
            return None;
        };
        let settings = ExistingDestination {
            delete: match delete {
                DeleteScope::Retain => false,
                DeleteScope::All => true,
                DeleteScope::Named(_) => return None,
            },
            include_source_name: review.destination.as_str() != proof.spec.path,
        };
        let mapped = mapped_receiver(&review.destination, &review.source, settings).ok()?;
        (mapped.as_str() == proof.spec.path).then_some((settings, proof.spec.path.as_str()))
    }

    fn same_existing_source(&self, other: &Self) -> bool {
        if let Some(mixed) = &self.mixed
            && matches!(mixed.receiver, MixedReceiver::Server { .. })
        {
            return other
                .mixed
                .as_ref()
                .is_some_and(|other| mixed.plan == other.plan);
        }
        if let Some(upload) = self
            .upload
            .as_ref()
            .filter(|_| self.privileged.is_some() || self.ordinary_receiver.is_some())
        {
            return other.upload.as_ref().is_some_and(|candidate| {
                candidate.source == upload.source
                    && candidate.bytes == upload.bytes
                    && candidate.digest == upload.digest
            });
        }
        (self.existing.is_none() && self.privileged.is_none() && self.ordinary_receiver.is_none())
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
                || self.vacant_directory_download.is_some()
                || self.existing_file_download.is_some()
                || self.vacant_file_download.is_some()
                || self.batch.is_some()
                || self.download.is_some()
                || self.privileged.is_some()
                || self.ordinary_receiver.is_some()
                || !mixed.valid()
            {
                return Err("The reviewed mixed rsync approval is invalid.");
            }
            if let MixedReceiver::Local {
                destination,
                vacant,
                ..
            } = &mixed.receiver
            {
                let scope = match vacant.as_deref() {
                    Some(snapshot) => reviewed_vacant_tree_scope(destination, snapshot)?,
                    None => destination,
                };
                require_program_outside_receiver(&self.program, scope)?;
            }
            return Ok(());
        }
        if let Some(batch) = &self.batch {
            if self.upload.is_some()
                || self.directory.is_some()
                || self.directory_upload.is_some()
                || self.existing.is_some()
                || self.existing_download.is_some()
                || self.vacant_directory_download.is_some()
                || self.existing_file_download.is_some()
                || self.vacant_file_download.is_some()
                || self.download.is_some()
                || self.privileged.is_some()
                || self.ordinary_receiver.is_some()
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
        if let Some(ordinary) = &self.ordinary_receiver {
            let exact = if ordinary.directory {
                self.directory_upload.as_ref().is_some_and(|upload| {
                    mapped_receiver(&upload.destination, &upload.source, ordinary.settings)
                        .is_ok_and(|destination| destination == ordinary.destination)
                })
            } else {
                ordinary.settings == ExistingDestination::FILE
                    && self
                        .upload
                        .as_ref()
                        .is_some_and(|upload| upload.destination == ordinary.destination)
            };
            if !exact
                || self.existing.is_some()
                || self.privileged.is_some()
                || !ordinary.receiver.valid(ordinary.directory)
            {
                return Err("The reviewed recurring server rsync destination is invalid.");
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
        if let Some(vacant) = &self.vacant_directory_download {
            if self.directory.as_ref().is_none_or(|directory| {
                mapped_local_receiver(&directory.destination, &directory.source, vacant.settings)
                    .ok()
                    .as_ref()
                    != Some(&vacant.destination)
            }) || !vacant.snapshot.valid()
                || self.existing.is_some()
                || self.existing_download.is_some()
                || self.download.is_some()
            {
                return Err("The reviewed vacant local rsync directory is invalid.");
            }
            require_program_outside_receiver(
                &self.program,
                reviewed_vacant_tree_scope(&vacant.destination, &vacant.snapshot)?,
            )?;
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
        if let Some(vacant) = &self.vacant_file_download {
            if self
                .download
                .as_ref()
                .is_none_or(|review| review.destination != vacant.destination)
                || self.existing.is_some()
                || self.existing_download.is_some()
                || self.existing_file_download.is_some()
                || !vacant.snapshot.valid()
            {
                return Err("The reviewed vacant local rsync file path is invalid.");
            }
            let scope = vacant
                .snapshot
                .first_missing(&vacant.destination)
                .ok_or("Review the exact vacant local rsync file path again.")?;
            require_program_outside_receiver(&self.program, scope)?;
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

    pub(crate) fn uses_ordinary_recurring_receiver(&self, upload: bool) -> bool {
        upload
            && !self.uses_passwordless_sudo()
            && (self.allow_vacant_server_file_upload || self.allow_vacant_server_tree_upload)
    }

    pub(crate) fn uses_vacant_recurring_file_receiver(&self, upload: bool) -> bool {
        self.standing.is_some() && self.uses_reviewed_file_creation(upload)
    }

    pub(crate) fn uses_reviewed_file_creation(&self, upload: bool) -> bool {
        !upload
            && !self.directory
            && !self.uses_existing_destination()
            && self.mixed.is_none()
            && self.additional_sources.is_empty()
            && (self.standing.is_some() || self.allow_vacant_file_download)
    }

    pub(crate) fn requires_reviewed_receiver(&self, upload: bool) -> bool {
        self.uses_existing_destination()
            || self.uses_reviewed_file_creation(upload)
            || self.uses_passwordless_sudo()
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
        let destination = self.existing_local_destination()?;
        let scope = match self
            .approval
            .as_ref()
            .and_then(|approval| approval.vacant_directory_download.as_deref())
        {
            Some(vacant) => reviewed_vacant_tree_scope(&destination, &vacant.snapshot)?,
            None => &destination,
        };
        let present = self.approval.as_ref().is_none_or(|approval| {
            approval.vacant_directory_download.is_none()
                && approval.mixed.as_deref().is_none_or(|mixed| {
                    matches!(
                        &mixed.receiver,
                        MixedReceiver::Local {
                            snapshot: Some(_),
                            ..
                        }
                    )
                })
        });
        let destination = canonical_local_receiver(scope, present)?;
        let program = self
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &destination)
    }

    pub(crate) fn require_safe_vacant_local_program(&self) -> Result<(), &'static str> {
        let scope = match self
            .approval
            .as_ref()
            .and_then(|approval| approval.vacant_file_download.as_deref())
        {
            Some(vacant) => vacant
                .snapshot
                .first_missing(&self.local)
                .ok_or("Review the exact vacant local rsync file path again.")?,
            None => &self.local,
        };
        let destination = canonical_local_receiver(scope, false)?;
        let program = self
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &destination)
    }

    pub(crate) fn require_safe_mixed_local_program(&self) -> Result<(), &'static str> {
        let vacancy = self
            .approval
            .as_ref()
            .and_then(|approval| approval.mixed.as_deref())
            .and_then(|mixed| match &mixed.receiver {
                MixedReceiver::Local { vacant, .. } => vacant.as_deref(),
                MixedReceiver::Server { .. } => None,
            });
        let destination = if let Some(vacancy) = vacancy {
            canonical_local_receiver(reviewed_vacant_tree_scope(&self.local, vacancy)?, false)?
        } else {
            match self.local.canonicalize() {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    canonical_local_receiver(&self.local, false)?
                }
                Err(_) => return Err("The local rsync destination changed or is unavailable."),
            }
        };
        let program = self
            .program
            .canonicalize()
            .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
        require_program_outside_receiver(&program, &destination)
    }

    pub(crate) fn validate(&self, upload: bool) -> Result<(), &'static str> {
        if self.allow_vacant_server_tree_upload
            && (!upload
                || !self.directory
                || self.existing.is_none()
                || self.existing_download.is_some()
                || !self.additional_sources.is_empty()
                || self.standing.is_none())
        {
            return Err("Creating a reviewed server folder requires a recurring upload.");
        }
        if self.allow_vacant_server_file_upload
            && (!upload
                || self.directory
                || self.existing != Some(ExistingDestination::FILE)
                || self.existing_download.is_some()
                || self.mixed.is_some()
                || !self.additional_sources.is_empty()
                || self.standing.is_none())
        {
            return Err("Creating a reviewed server file requires a recurring file upload.");
        }
        if self.allow_vacant_directory_download
            && (upload
                || !self.directory
                || !LOCAL_EXISTING_SUPPORTED
                || self.existing_download.is_none()
                || self.existing.is_some()
                || !self.additional_sources.is_empty())
        {
            return Err(
                "Creation at a reviewed vacant path requires a checked local directory download.",
            );
        }
        let checked_file = self.uses_reviewed_file_creation(upload);
        if self.allow_vacant_file_download && !checked_file {
            return Err(
                "Creation at a reviewed vacant file path requires one direct file download.",
            );
        }
        if checked_file {
            if !LOCAL_EXISTING_SUPPORTED {
                return Err("Rsync to a reviewed file path is unavailable on this platform.");
            }
            require_program_outside_receiver(&self.program, &self.local)?;
        }
        let privileged = (self.uses_passwordless_sudo() && self.mixed.is_none())
            .then(|| self.privileged_spec(upload))
            .transpose()?;
        if let Some(approval) = &self.approval
            && (!approval.matches_elevation(privileged.as_ref())
                || approval.first_server_file_upload() && !self.allow_vacant_server_file_upload
                || approval.first_server_tree_upload() && !self.allow_vacant_server_tree_upload)
        {
            return Err(elevation::REVIEW_REQUIRED);
        }
        if let Some(approval) = &self.approval
            && approval.ordinary_receiver.is_some()
                != (self.mixed.is_none() && self.uses_ordinary_recurring_receiver(upload))
        {
            return Err("Review the exact recurring server rsync destination again.");
        }
        if let Some(config) = &self.standing {
            config
                .settings
                .validate()
                .map_err(|_| "Choose valid limits for recurring rsync.")?;
            if !(self.uses_existing_destination() || checked_file || self.mixed.is_some())
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
                    && !((self.uses_passwordless_sudo() || self.allow_vacant_server_file_upload)
                        && self.existing == Some(ExistingDestination::FILE))
            {
                return Err("Choose one source directory or a reviewed server file upload.");
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
            let reviewed_existing = approval
                .existing_file_download
                .as_ref()
                .map(|review| &review.destination);
            let reviewed_vacant = approval
                .vacant_file_download
                .as_ref()
                .map(|review| &review.destination);
            let file_receiver_matches = if checked_file {
                reviewed_vacant == Some(&self.local)
                    || self.standing.is_some() && reviewed_existing == Some(&self.local)
            } else {
                reviewed_vacant.is_none()
                    && reviewed_existing
                        == self
                            .existing_download
                            .filter(|_| !self.directory)
                            .map(|_| &self.local)
            };
            let reviewed_directory = approval
                .existing_download
                .as_ref()
                .map(|review| (&review.destination, review.settings))
                .or_else(|| {
                    approval
                        .vacant_directory_download
                        .as_ref()
                        .map(|review| (&review.destination, review.settings))
                });
            if (upload && !self.directory) != approval.upload.is_some()
                || (!upload && self.directory) != approval.directory.is_some()
                || (upload && self.directory) != approval.directory_upload.is_some()
                || approval.batch.is_some()
                || ((checked_file || self.allow_vacant_directory_download)
                    && approval.program != self.program)
                || approval
                    .existing_upload_scope()
                    .map(|(settings, _)| settings)
                    != self.existing
                || approval
                    .existing_upload_scope()
                    .is_some_and(|(_, destination)| {
                        !self
                            .existing_destination()
                            .is_ok_and(|mapped| mapped.as_str() == destination)
                    })
                || (!self.allow_vacant_directory_download
                    && approval.vacant_directory_download.is_some())
                || reviewed_directory.map(|(_, settings)| settings)
                    != self.existing_download.filter(|_| self.directory)
                || reviewed_directory.is_some_and(|(destination, _)| {
                    self.existing_local_destination().ok().as_ref() != Some(destination)
                })
                || !file_receiver_matches
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

fn reviewed_vacant_tree_scope<'a>(
    destination: &'a Path,
    snapshot: &VacantLocalTreeSnapshot,
) -> Result<&'a Path, &'static str> {
    snapshot
        .first_missing(destination)
        .ok_or("Review the exact missing local rsync directories again.")
}

fn parent_creation_detail(
    destination: &Path,
    snapshot: &VacantLocalTreeSnapshot,
) -> Option<String> {
    missing_parent_detail(
        snapshot.missing_parent_count(),
        snapshot.first_missing(destination),
    )
}

fn missing_parent_detail(count: u8, first_missing: Option<&Path>) -> Option<String> {
    if count == 0 {
        return None;
    }
    let first_missing = first_missing?;
    let folder = if count == 1 { "folder" } else { "folders" };
    Some(format!(
        "Creates {count} missing parent {folder} starting at {}.",
        first_missing.display()
    ))
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
    pub(super) allow_vacant_directory_download: bool,
    pub(super) vacant_file_download: bool,
    pub(super) allow_existing_file_download: bool,
    pub(super) review_vacant_server_upload: bool,
    pub(super) mixed: Option<MixedReviewTarget>,
    pub(super) batch: Option<batch::Plan>,
    pub(super) download: Option<DirectoryReview>,
    pub(super) privileged: Option<PrivilegedSpec>,
}

pub(crate) enum UploadReceiverPreview<'a> {
    ExistingDirectory(&'a RemotePath),
    Recurring(RecurringReceiverTarget<'a>),
}

impl UploadReceiverPreview<'_> {
    pub(crate) fn destination(&self) -> &RemotePath {
        match self {
            Self::ExistingDirectory(destination) => destination,
            Self::Recurring(target) => target.destination,
        }
    }
}

impl ReviewRequest {
    fn review_ordinary_receiver(&self) -> bool {
        self.privileged.is_none() && self.review_vacant_server_upload && self.existing.is_some()
    }

    pub(super) fn matches(&self, approval: &Approval, legacy_single: bool) -> bool {
        let recurring_existing = self.vacant_file_download
            && self.allow_existing_file_download
            && approval.existing_file_download.is_some();
        approval.validate().is_ok()
            && self.program == approval.program
            && self.reference == approval.reference
            && approval.matches_elevation(self.privileged.as_ref())
            && self.review_ordinary_receiver() == approval.ordinary_receiver.is_some()
            && (self.review_vacant_server_upload
                || !(approval.first_server_file_upload() || approval.first_server_tree_upload()))
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
            && (self.allow_vacant_directory_download
                || approval.vacant_directory_download.is_none())
            && (self.vacant_file_download && !recurring_existing)
                == approval.vacant_file_download.is_some()
            && self
                .existing
                .as_ref()
                .map(|(settings, path)| (*settings, path.as_str()))
                == approval.existing_upload_scope()
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
                            .vacant_directory_download
                            .as_ref()
                            .map(|review| (&review.settings, &review.destination))
                    })
                    .or_else(|| {
                        approval
                            .existing_file_download
                            .as_ref()
                            .filter(|_| !recurring_existing)
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
        UploadReceiverPreview<'_>,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<RecurringReceiver, String>,
    inspect_mixed: &mut impl FnMut(
        &MixedReviewTarget,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<MixedReview, String>,
    inspect_privileged: &mut impl FnMut(
        &ConnectionRef,
        &PrivilegedSpec,
        bool,
        &kortty_ssh::TrustStore,
        &Arc<AtomicBool>,
    ) -> Result<PrivilegedReview, String>,
    cancelled: &Arc<AtomicBool>,
) -> Result<Approval, String> {
    require_review_running(cancelled)?;
    let review_ordinary_receiver = request.review_ordinary_receiver();
    super::remote::validate_local_path(&request.program)?;
    let reviewed_trust = request.reference.strict_trust(trust.clone())?;
    let pins = super::sync::pins(&request.reference, &reviewed_trust)
        .map_err(|error| error.to_string())?;
    if request.privileged.is_some()
        && request.existing_download.is_none()
        && !request.vacant_file_download
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
            let proof = inspect_privileged(
                &request.reference,
                spec,
                request.review_vacant_server_upload,
                trust,
                cancelled,
            )?;
            if !proof.valid()
                || &proof.spec != spec
                || (proof.first_file || proof.first_tree) && !request.review_vacant_server_upload
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
    let (existing, ordinary_receiver) = request
        .existing
        .filter(|_| {
            privileged
                .as_ref()
                .is_none_or(|proof| !(proof.first_file || proof.first_tree))
        })
        .map(|(settings, destination)| {
            require_review_running(cancelled)?;
            let receiver = if let Some(proof) = &privileged {
                RecurringReceiver::Existing {
                    snapshot: elevation::existing_snapshot(proof)
                        .ok_or(elevation::REVIEW_REQUIRED)?,
                }
            } else if review_ordinary_receiver {
                let delete = if settings.delete {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                };
                inspect(
                    &request.reference,
                    UploadReceiverPreview::Recurring(RecurringReceiverTarget {
                        destination: &destination,
                        directory: directory_upload.is_some(),
                        delete: &delete,
                    }),
                    trust,
                    cancelled,
                )?
            } else {
                inspect(
                    &request.reference,
                    UploadReceiverPreview::ExistingDirectory(&destination),
                    trust,
                    cancelled,
                )?
            };
            if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?
                != pins
            {
                return Err("The saved SSH route or host keys changed during review.".to_owned());
            }
            require_review_running(cancelled)?;
            if review_ordinary_receiver {
                Ok((
                    None,
                    Some(OrdinaryReceiverReview {
                        destination,
                        directory: directory_upload.is_some(),
                        settings,
                        receiver,
                    }),
                ))
            } else if let RecurringReceiver::Existing { snapshot } = receiver {
                Ok((
                    Some(Box::new(ExistingReview {
                        destination,
                        snapshot,
                        settings,
                    })),
                    None,
                ))
            } else {
                Err("The server rsync destination is unavailable for replacement.".to_owned())
            }
        })
        .transpose()?
        .unwrap_or_default();
    let (existing_download, vacant_directory_download) = request
        .existing_download
        .as_ref()
        .filter(|_| request.directory.is_some())
        .map(|(settings, destination)| {
            require_review_running(cancelled)?;
            let vacant = if request.allow_vacant_directory_download {
                match std::fs::symlink_metadata(destination) {
                    Ok(_) => false,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    Err(error) => return Err(error.to_string()),
                }
            } else {
                false
            };
            let receiver = if vacant {
                let snapshot =
                    crate::job_executor::rsync::review_local_vacant_tree(destination, cancelled)?;
                (
                    None,
                    Some(Box::new(LocalVacantDirectoryReview {
                        destination: destination.clone(),
                        snapshot,
                        settings: *settings,
                    })),
                )
            } else {
                let snapshot =
                    crate::job_executor::rsync::review_local_existing(destination, cancelled)?;
                (
                    Some(Box::new(LocalExistingReview {
                        destination: destination.clone(),
                        snapshot,
                        settings: *settings,
                    })),
                    None,
                )
            };
            if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())?
                != pins
            {
                return Err("The saved SSH route or host keys changed during review.".to_owned());
            }
            require_review_running(cancelled)?;
            Ok(receiver)
        })
        .transpose()?
        .unwrap_or_default();
    let (mut existing_file_download, mut vacant_file_download) = (None, None);
    if request.vacant_file_download
        || request.download.is_some() && request.existing_download.is_some()
    {
        require_review_running(cancelled)?;
        let destination = request
            .download
            .as_ref()
            .ok_or("Choose one server file and an exact local file path.")?
            .destination
            .clone();
        let existing = if request.allow_existing_file_download {
            match std::fs::symlink_metadata(&destination) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.to_string()),
            }
        } else {
            request.existing_download.is_some()
        };
        let stopped = || cancelled.load(Ordering::Acquire);
        if existing {
            let snapshot =
                crate::sftp_window::ReviewedFileReceiver::inspect(&destination, &stopped)
                    .map_err(|error| error.to_string())?;
            existing_file_download = Some(Box::new(LocalExistingFileReview {
                destination,
                snapshot,
            }));
        } else {
            let snapshot =
                crate::job_executor::rsync::review_local_file_vacancy(&destination, cancelled)?;
            vacant_file_download = Some(Box::new(LocalVacantFileReview {
                destination,
                snapshot,
            }));
        }
        if super::sync::pins(&request.reference, trust).map_err(|error| error.to_string())? != pins
        {
            return Err("The saved SSH route or host keys changed during review.".into());
        }
        require_review_running(cancelled)?;
    }
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
        vacant_directory_download,
        existing_file_download,
        vacant_file_download,
        mixed,
        batch: request
            .batch
            .map(batch::Plan::review)
            .transpose()?
            .map(Box::new),
        download: request.download.map(Box::new),
        privileged,
        ordinary_receiver,
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
            && primary.vacant_directory_download.is_none()
            && primary.existing_file_download.is_none()
            && primary.vacant_file_download.is_none()
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
                text("Each firing checks the current sources and expected destinations. A successful replacement retains the prior receiver for recovery. Review any uncertain outcome before continuing.").size(12),
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
                approval.shares_missing_parent().then(|| text("Shared missing parent folders are created once. Each route publishes its own reviewed destination.").size(12)),
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
                    (self.existing_file_download.is_none() && self.vacant_file_download.is_none()).then(|| text(format!("New local file · {}", download.destination.display()))),
                ].spacing(4)),
                self.directory.as_ref().map(|directory| column![
                    text(format!("Server directory · {}", directory.source.as_str())),
                    (self.existing_download.is_none() && self.vacant_directory_download.is_none()).then(|| column![
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
                self.privileged.as_ref().filter(|proof| proof.first_file).map(|proof| column![
                    text(format!("Vacant server file path · {}", proof.spec.path)),
                    text("The first successful firing creates the file. Later firings replace each confirmed file and retain it for recovery.").size(12),
                ].spacing(4)),
                self.privileged.as_ref().filter(|proof| proof.first_tree).map(|proof| column![
                    text(format!("Vacant server folder · {}", proof.spec.path)),
                    text("The first successful firing creates this folder. Later firings replace only the last confirmed folder and retain it for recovery.").size(12),
                    text(if self.existing_upload_scope().is_some_and(|(settings, _)| settings.delete) {
                        "Future firings remove entries missing from the local source within this folder."
                    } else {
                        "Future firings retain server entries missing from the local source."
                    }).size(12),
                ].spacing(4)),
                self.ordinary_receiver.as_ref().map(|review| column![
                    text(format!("{} server {} · {}",
                        if matches!(review.receiver, RecurringReceiver::Vacant { .. }) { "Vacant" } else { "Existing" },
                        if review.directory { "folder" } else { "file" },
                        review.destination.as_str()
                    )),
                    text(if matches!(review.receiver, RecurringReceiver::Vacant { .. }) {
                        "The first successful firing creates this destination. Later firings replace only the last confirmed receiver and retain it for recovery."
                    } else {
                        "The first firing replaces this exact destination and retains it for recovery. Later firings replace each confirmed successor."
                    }).size(12),
                    review.directory.then(|| text(if review.settings.delete {
                        "Future firings remove entries missing from the local source within this folder."
                    } else {
                        "Future firings retain server entries missing from the local source."
                    }).size(12)),
                ].spacing(4)),
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
                self.vacant_directory_download.as_ref().map(|vacant| {
                    column![
                        text(format!("Vacant local folder · {}", vacant.destination.display())),
                        parent_creation_detail(&vacant.destination, &vacant.snapshot)
                            .map(|detail| text(detail).wrapping(text::Wrapping::Glyph).size(12)),
                        text(if recurring {
                            "The first successful firing creates this folder. Later firings replace only the last confirmed folder and retain it for recovery."
                        } else {
                            "The run creates this folder only while the reviewed destination is vacant."
                        }).size(12),
                        recurring.then(|| text(if vacant.settings.delete {
                            "Future firings remove local entries missing from the checked server tree within this destination."
                        } else {
                            "Future firings retain local entries that are absent from the checked server tree."
                        }).size(12)),
                    ].spacing(4)
                }),
                self.existing_file_download.as_ref().map(|existing| {
                    column![
                        text(format!("Existing local file · {}", existing.destination.display())),
                        text(format!("The reviewed file contains {} bytes.", existing.snapshot.inventory.bytes)).size(12),
                        text(if recurring {
                            "The first firing replaces this exact file and retains it for recovery. Later firings replace each confirmed successor."
                        } else {
                            "The run replaces this exact file and keeps the original for recovery."
                        }).size(12),
                        (!recurring).then(|| text("Review the replacement before running this transfer again.").size(12)),
                    ].spacing(4)
                }),
                self.vacant_file_download.as_ref().map(|vacant| {
                    column![
                        text(format!("Vacant local file path · {}", vacant.destination.display())),
                        missing_parent_detail(vacant.snapshot.missing_parent_count(), vacant.snapshot.first_missing(&vacant.destination))
                            .map(|detail| text(detail).wrapping(text::Wrapping::Glyph).size(12)),
                        text(if recurring {
                            "The first successful firing creates the file. Later firings replace only the last confirmed file and retain it for recovery."
                        } else {
                            "The run creates this file only while the reviewed destination is vacant."
                        }).size(12),
                    ].spacing(4)
                }),
                self.batch.as_ref().map(|review| review.view()),
                self.mixed.as_ref().map(|review| review.view(recurring)),
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
            UploadReceiverPreview<'_>,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<RecurringReceiver, String>
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
            bool,
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
            allow_vacant_directory_download: false,
            allow_vacant_file_download: false,
            allow_vacant_server_file_upload: false,
            allow_vacant_server_tree_upload: false,
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
                vacant_directory_download: None,
                existing_file_download: None,
                vacant_file_download: None,
                mixed: None,
                batch: None,
                download: None,
                privileged: None,
                ordinary_receiver: None,
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

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn recurring_rsync_file_review_binds_exact_paths_and_receivers() {
        use std::ffi::OsStr;
        use std::io::Write;

        use kortty_ssh::{Endpoint, HostKey, TrustStore};
        use serde_json::json;

        use crate::sftp_window::local_tree::{self, Directory};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let protected = OsStr::new("protected");
        drop(local_tree::create_reviewed_tree_stage(&parent, protected).unwrap());
        let root = root.join(protected);
        let program = root.join("rsync");
        let destination = root.join("report.txt");
        let source = "/site/report.txt";
        let host = "server.example";
        let reference: ConnectionRef = serde_json::from_value(json!({
            "id": 1, "target": {"host": host, "port": 22, "username": "operator"},
        }))
        .unwrap();
        let key = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap();
        let trust = TrustStore::from_pins(vec![(Endpoint::new(host, 22).unwrap(), key)]).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut transfer: Transfer = serde_json::from_value(json!({
            "program": program, "local": destination, "remote": source,
            "standing": {"settings": standing::Settings::default()},
        }))
        .unwrap();
        let make_review = |request| {
            review(
                request,
                &trust,
                [8; 32],
                &mut |_, _, _, _| panic!("unexpected server receiver review"),
                &mut |_, _, _| panic!("unexpected mixed review"),
                &mut |_, _, _, _, _| panic!("unexpected protected review"),
                &cancelled,
            )
        };

        assert!(transfer.require_group_review(false).is_err());
        let request = || ReviewRequests::for_transfer(&transfer, &reference, &[], false).unwrap();
        assert!(request().primary.vacant_file_download);
        assert!(!request().primary.allow_existing_file_download);
        let approval = make_review(request().primary).unwrap();
        assert!(request().primary.matches(&approval, true));
        assert_eq!(
            approval.vacant_file_download.as_ref().unwrap().destination,
            destination
        );
        let directory = Directory::open_path(&root).unwrap();
        local_tree::create_reviewed_tree_file(&directory, destination.file_name().unwrap())
            .unwrap()
            .write_all(b"unreviewed receiver")
            .unwrap();
        drop(directory);
        assert!(make_review(request().primary).is_err());

        let mut one_use = transfer.clone();
        one_use.standing = None;
        let one_use_request = ReviewRequests::for_transfer(&one_use, &reference, &[], false)
            .unwrap()
            .primary;
        assert!(!one_use_request.matches(&approval, true));
        let one_use_approval = make_review(one_use_request).unwrap();
        assert!(!request().primary.matches(&one_use_approval, true));
        assert!(
            !serde_json::to_value(one_use_approval)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("vacant_file_download")
        );

        one_use.allow_vacant_file_download = true;
        let explicit_request = |transfer: &Transfer| {
            ReviewRequests::for_transfer(transfer, &reference, &[], false)
                .unwrap()
                .primary
        };
        assert!(make_review(explicit_request(&one_use)).is_err());
        let missing_parent = root.join("reports");
        one_use.local = missing_parent.join("daily/report.txt");
        let request_for_creation = explicit_request(&one_use);
        assert!(request_for_creation.vacant_file_download);
        assert!(!request_for_creation.allow_existing_file_download);
        let creation = make_review(request_for_creation).unwrap();
        let vacant = creation.vacant_file_download.as_deref().unwrap();
        assert_eq!(vacant.destination, one_use.local);
        assert_eq!(
            vacant.snapshot.first_missing(&one_use.local),
            Some(missing_parent.as_path())
        );
        assert_eq!(vacant.snapshot.missing_parent_count(), 2);
        assert!(explicit_request(&one_use).matches(&creation, true));
        assert!(!missing_parent.exists());
        one_use.approval = Some(creation);
        assert!(one_use.validate(false).is_ok());
        assert!(one_use.require_group_review(false).is_ok());
        assert!(!one_use.requires_automatic_receiver(false));
        one_use.program = missing_parent.join("bin/rsync");
        one_use.approval.as_mut().unwrap().program = one_use.program.clone();
        assert!(one_use.validate(false).is_err());

        transfer.approval = Some(approval);
        assert!(transfer.validate(false).is_ok());
        let mut wrong_route = reference.clone();
        wrong_route.id = 2;
        assert!(transfer.validate_routes(&wrong_route, &[], false).is_err());
        let mut wrong_path = transfer.clone();
        wrong_path.local = root.join("other.txt");
        assert!(wrong_path.validate(false).is_err());
        let mut wrong_program = transfer.clone();
        wrong_program.program = root.join("other-rsync");
        assert!(wrong_program.validate(false).is_err());
        transfer.routes.push(Route {
            profile: wrong_route.id,
            destination: wrong_path.local.to_str().unwrap().into(),
            approval: None,
        });
        let group = std::slice::from_ref(&wrong_route);
        let grouped_requests =
            || ReviewRequests::for_transfer(&transfer, &reference, group, false).unwrap();
        let requests = grouped_requests();
        assert!(requests.primary.allow_existing_file_download);
        assert!(requests.additional[0].allow_existing_file_download);
        let mut additional = requests.additional.into_iter();
        let reviewed = Reviewed {
            primary: make_review(requests.primary).unwrap(),
            additional: vec![make_review(additional.next().unwrap()).unwrap()],
        };
        assert!(grouped_requests().matches(&reviewed));
        assert_eq!(
            reviewed
                .primary
                .existing_file_download
                .as_ref()
                .unwrap()
                .destination,
            destination
        );
        assert!(reviewed.primary.vacant_file_download.is_none());
        assert_eq!(
            reviewed.additional[0]
                .vacant_file_download
                .as_ref()
                .unwrap()
                .destination,
            wrong_path.local
        );
        assert!(reviewed.additional[0].existing_file_download.is_none());

        reviewed.apply(&mut transfer);
        assert!(transfer.validate_routes(&reference, group, false).is_ok());
        assert!(transfer.require_group_review(false).is_ok());
    }
}
