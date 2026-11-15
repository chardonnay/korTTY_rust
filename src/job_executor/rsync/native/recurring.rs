use std::path::Path;

use kortty_jobs::RunOutcome;
use kortty_server_archive::privileged_rsync::SourceLimits;
use kortty_server_archive::rsync_tree::{ExistingTree, Inventory};

use crate::job_executor::rsync::LocalExistingSnapshot;
use crate::scheduler_window::rsync::standing::{Budget, Lease, Receiver};
use crate::scheduler_window::{ConnectionRef, RemotePath};

use super::{Completion, Failure};

#[derive(Clone, Copy)]
pub(super) struct Route<'a> {
    pub(super) lease: &'a Lease,
    pub(super) reference: &'a ConnectionRef,
}

impl Route<'_> {
    pub(super) fn remaining(self) -> Result<Budget, Failure> {
        self.lease.remaining().map_err(|_| unavailable())
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn source_limits(self) -> Result<SourceLimits, Failure> {
        let remaining = self.remaining()?;
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

    pub(super) fn local(self, path: &Path) -> Result<LocalExistingSnapshot, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::Local {
                destination,
                snapshot,
            }) if destination == path => Ok(snapshot),
            _ => Err(unavailable()),
        }
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn local_file(
        self,
        path: &Path,
    ) -> Result<crate::sftp_window::LocalFileSnapshot, Failure> {
        match self.lease.expected_receiver(self.reference) {
            Ok(Receiver::LocalFile {
                destination,
                snapshot,
            }) if destination == path => Ok(snapshot),
            _ => Err(unavailable()),
        }
    }

    pub(super) fn reserve(
        self,
        inventories: impl IntoIterator<Item = Inventory>,
    ) -> Result<(), Failure> {
        let mut budget = Budget {
            bytes: 0,
            entries: 0,
            deletes: 0,
        };
        for inventory in inventories {
            budget.bytes = budget.bytes.saturating_add(inventory.bytes);
            budget.entries = budget.entries.saturating_add(inventory.entries);
        }
        self.lease.reserve(self.reference, budget).map_err(|_| {
            Failure::blocked(
                "The recurring rsync firing reached its transfer limits or lost its approval.",
            )
        })
    }

    pub(super) fn arm(self, removed: u32) -> Result<(), Failure> {
        self.lease.arm(self.reference, removed).map_err(|_| {
            Failure::blocked(
                "The recurring rsync firing reached its removal limit or lost its approval.",
            )
        })
    }

    pub(super) fn confirm(
        self,
        mut completion: Completion,
        receiver: Option<Receiver>,
    ) -> Completion {
        if completion.outcome == RunOutcome::Success
            && !receiver
                .is_some_and(|receiver| self.lease.confirm(self.reference, receiver).is_ok())
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
