#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::path::Path;

use kortty_jobs::RunOutcome;
use kortty_server_archive::privileged_rsync::SourceLimits;
use kortty_server_archive::rsync_tree::{ExistingTree, Inventory};

use crate::job_executor::rsync::LocalParentReceipt;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use crate::job_executor::rsync::{LocalExistingSnapshot, VacantFileProof, VacantLocalTreeSnapshot};
use crate::scheduler_window::rsync::standing::{Budget, Lease, Receiver, RemoteVacancy};
use crate::scheduler_window::{ConnectionRef, RemotePath};
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use crate::sftp_window::LocalFileSnapshot;

use super::{Completion, Failure};

#[derive(Clone, Copy)]
pub(super) struct Route<'a> {
    pub(super) lease: &'a Lease,
    pub(super) reference: &'a ConnectionRef,
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) enum LocalFile {
    Existing(LocalFileSnapshot),
    Vacant(VacantFileProof),
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) enum LocalTree {
    Existing(LocalExistingSnapshot),
    Vacant(VacantLocalTreeSnapshot),
}

pub(super) enum RemoteUpload {
    Existing(ExistingTree),
    VacantFile,
    VacantTree,
}

impl Route<'_> {
    pub(super) fn remaining(self, parent_entries: u32) -> Result<Budget, Failure> {
        let mut remaining = self.lease.remaining().map_err(|_| unavailable())?;
        remaining.entries = remaining
            .entries
            .checked_sub(parent_entries)
            .ok_or_else(reservation_unavailable)?;
        Ok(remaining)
    }

    pub(super) fn source_limits(self, parent_entries: u32) -> Result<SourceLimits, Failure> {
        let remaining = self.remaining(parent_entries)?;
        Ok(SourceLimits {
            entries: remaining.entries.min(SourceLimits::MAXIMUM.entries),
            bytes: remaining.bytes.min(SourceLimits::MAXIMUM.bytes),
        })
    }

    pub(super) fn remote(self, path: &RemotePath) -> Result<ExistingTree, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::Remote {
                destination,
                snapshot,
            }) if &destination == path => Ok(snapshot),
            _ => Err(unavailable()),
        }
    }

    pub(super) fn remote_upload(
        self,
        path: &RemotePath,
        vacancy: Option<RemoteVacancy>,
    ) -> Result<RemoteUpload, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::Remote {
                destination,
                snapshot,
            }) if &destination == path => Ok(RemoteUpload::Existing(snapshot)),
            Ok(Receiver::VacantRemoteFile {
                destination,
                fingerprint,
            }) if &destination == path && vacancy == Some(RemoteVacancy::File(fingerprint)) => {
                Ok(RemoteUpload::VacantFile)
            }
            Ok(Receiver::VacantRemoteTree {
                destination,
                fingerprint,
            }) if &destination == path && vacancy == Some(RemoteVacancy::Tree(fingerprint)) => {
                Ok(RemoteUpload::VacantTree)
            }
            _ => Err(unavailable()),
        }
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn local_tree(
        self,
        path: &Path,
        reviewed_vacancy: Option<&VacantLocalTreeSnapshot>,
    ) -> Result<LocalTree, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::Local {
                destination,
                snapshot,
            }) if destination == path => Ok(LocalTree::Existing(snapshot)),
            Ok(Receiver::VacantLocalTree {
                destination,
                snapshot,
            }) if destination == path && reviewed_vacancy == Some(&snapshot) => {
                Ok(LocalTree::Vacant(snapshot))
            }
            _ => Err(unavailable()),
        }
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn local_file(self, path: &Path, reviewed: LocalFile) -> Result<LocalFile, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::LocalFile {
                destination,
                snapshot,
            }) if destination == path => Ok(LocalFile::Existing(snapshot)),
            Ok(Receiver::VacantLocalFile {
                destination,
                snapshot,
            }) if destination == path
                && matches!(reviewed, LocalFile::Vacant(initial) if initial == snapshot) =>
            {
                Ok(LocalFile::Vacant(snapshot))
            }
            _ => Err(unavailable()),
        }
    }

    pub(super) fn reserve(
        self,
        inventories: impl IntoIterator<Item = Inventory>,
        parent_entries: u32,
    ) -> Result<(), Failure> {
        let mut budget = Budget {
            entries: parent_entries,
            ..Budget::default()
        };
        for inventory in inventories {
            budget.bytes = budget.bytes.saturating_add(inventory.bytes);
            budget.entries = budget.entries.saturating_add(inventory.entries);
        }
        self.lease
            .reserve(self.reference, budget)
            .map_err(|_| reservation_unavailable())
    }

    pub(super) fn arm(self, removed: u32) -> Result<(), Failure> {
        self.lease.arm(self.reference, removed).map_err(|_| {
            Failure::blocked(
                "The recurring rsync firing reached its removal limit or lost its approval.",
            )
        })
    }

    pub(super) fn confirm(self, completion: Completion, receiver: Option<Receiver>) -> Completion {
        self.confirm_with_parents(completion, receiver, &[])
    }

    pub(super) fn confirm_with_parents(
        self,
        mut completion: Completion,
        receiver: Option<Receiver>,
        parents: &[LocalParentReceipt],
    ) -> Completion {
        if completion.outcome == RunOutcome::Success
            && !receiver.is_some_and(|receiver| {
                if parents.is_empty() {
                    self.lease.confirm(self.reference, receiver).is_ok()
                } else {
                    self.lease
                        .confirm_with_parents(self.reference, receiver, parents)
                        .is_ok()
                }
            })
        {
            completion.outcome = RunOutcome::Failed { exit_code: None };
            completion.append_detail("Could not record the published rsync receiver. Review the destination before continuing.");
        }
        completion
    }

    pub(super) fn confirm_first_remote(
        self,
        mut completion: Completion,
        receiver: Option<Receiver>,
        vacancy: RemoteVacancy,
    ) -> Completion {
        if completion.outcome == RunOutcome::Success
            && !receiver.is_some_and(|receiver| {
                self.lease
                    .confirm_first_remote(self.reference, receiver, vacancy)
                    .is_ok()
            })
        {
            completion.outcome = RunOutcome::Failed { exit_code: None };
            completion.append_detail("Could not record the published rsync receiver. Review the destination before continuing.");
        }
        completion
    }
}

fn unavailable() -> Failure {
    Failure::blocked("The recurring rsync receiver is unavailable. Review the destination again.")
}

fn reservation_unavailable() -> Failure {
    Failure::blocked("The recurring rsync firing reached its transfer limits or lost its approval.")
}
