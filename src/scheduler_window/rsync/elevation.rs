use kortty_server_archive::privileged_rsync::{Operation, Review, Spec};
use kortty_server_archive::rsync_tree::{DeleteScope, ExistingTree};
use serde::{Deserialize, Serialize};

use super::{Approval, ExistingDestination, Transfer, valid_remote};

pub(super) const REVIEW_REQUIRED: &str =
    "Review the exact rsync paths and configured passwordless sudo route again.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Elevation {
    #[default]
    Ordinary,
    Passwordless,
}

impl Elevation {
    pub(super) fn is_ordinary(&self) -> bool {
        *self == Self::Ordinary
    }
}

impl Transfer {
    pub(crate) fn uses_passwordless_sudo(&self) -> bool {
        self.elevation == Elevation::Passwordless
    }

    pub(crate) fn privileged_spec(&self, upload: bool) -> Result<Spec, &'static str> {
        if !self.uses_passwordless_sudo()
            || self.mixed.is_some()
            || !self.additional_sources.is_empty()
            || self.existing.is_some_and(|existing| {
                !upload || !self.directory && existing != ExistingDestination::FILE
            })
            || self.existing_download.is_some_and(|existing| {
                upload || !self.directory && existing != ExistingDestination::FILE
            })
        {
            return Err(
                "Passwordless rsync uses one reviewed file or directory per route and a fresh approval.",
            );
        }
        let path = if upload && self.existing.is_some() {
            self.existing_destination()?
        } else {
            self.remote.clone()
        };
        if !valid_remote(path.as_str()) {
            return Err("Choose a literal absolute path under the configured Linux sudo roots.");
        }
        let operation = if upload {
            Operation::Upload {
                directory: self.directory,
                replace: self.existing.is_some(),
                delete: if self.existing.is_some_and(|existing| existing.delete) {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                },
            }
        } else {
            Operation::Download {
                directory: self.directory,
            }
        };
        let spec = Spec {
            path: path.as_str().to_owned(),
            operation,
        };
        spec.valid()
            .then_some(spec)
            .ok_or("Choose a literal absolute path under the configured Linux sudo roots.")
    }
}

pub(super) fn existing_snapshot(review: &Review) -> Option<ExistingTree> {
    if !matches!(
        &review.spec.operation,
        Operation::Upload { replace: true, .. }
    ) {
        return None;
    }
    Some(ExistingTree {
        inventory: review.inventory?,
        fingerprint: review.fingerprint,
    })
}

pub(super) fn is_file_replacement(review: &Review) -> bool {
    matches!(
        &review.spec.operation,
        Operation::Upload {
            directory: false,
            replace: true,
            delete: DeleteScope::Retain,
        }
    )
}

impl Approval {
    pub(super) fn matches_elevation(&self, spec: Option<&Spec>) -> bool {
        match (spec, self.privileged.as_deref()) {
            (None, None) => true,
            (Some(spec), Some(review)) => {
                review.valid()
                    && &review.spec == spec
                    && existing_snapshot(review)
                        == self.existing.as_ref().map(|existing| existing.snapshot)
                    && (!matches!(spec.operation, Operation::Download { directory: false })
                        || self.download.is_some())
            }
            _ => false,
        }
    }
}
