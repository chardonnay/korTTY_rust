use std::path::{Component, Path};

use kortty_server_archive::rsync_tree::Inventory;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sftp_window::{LocalFileSnapshot, VacantLocalFileAlias, VacantLocalFileSnapshot};

pub(super) const MAX_MISSING_PARENTS: u8 = 32;

/// The stable native identity and access of one parent created by confirmed publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalParentReceipt {
    pub(super) path_key: [u8; 32],
    pub(super) fingerprint: [u8; 32],
}

impl LocalParentReceipt {
    pub(crate) fn key_for(path: &Path) -> [u8; 32] {
        let mut fingerprint = Sha256::new_with_prefix(b"kortty-local-rsync-parent-path-v1\0");
        for component in path.components() {
            let bytes = component.as_os_str().as_encoded_bytes();
            fingerprint.update((bytes.len() as u64).to_le_bytes());
            fingerprint.update(bytes);
        }
        fingerprint.finalize().into()
    }

    pub(crate) fn path_key(&self) -> [u8; 32] {
        self.path_key
    }

    #[cfg(test)]
    pub(super) fn matches_path(&self, path: &Path) -> bool {
        self.path_key == Self::key_for(path)
    }

    pub(crate) fn valid(&self) -> bool {
        self.path_key != [0; 32] && self.fingerprint != [0; 32]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalTreeLocation {
    pub(super) alias: VacantLocalFileAlias,
    pub(super) mount: (u64, u64),
}

impl LocalTreeLocation {
    pub(crate) fn new(alias: VacantLocalFileAlias, mount: (u64, u64)) -> Self {
        Self { alias, mount }
    }

    pub(crate) fn valid(&self) -> bool {
        self.alias.valid()
    }

    pub(crate) fn alias(&self) -> &VacantLocalFileAlias {
        &self.alias
    }

    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        // Bind mounts can hide that a path lies inside a different route's tree.
        self.alias.aliases(&other.alias)
            || (cfg!(target_os = "linux")
                && self.mount.0 == other.mount.0
                && self.mount.1 != other.mount.1)
    }
}

/// A vacant direct download with either an existing or a missing immediate parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum VacantFileProof {
    File(VacantLocalFileSnapshot),
    Hierarchy(VacantLocalTreeSnapshot),
}

impl VacantFileProof {
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::File(snapshot) => snapshot.valid(),
            Self::Hierarchy(snapshot) => snapshot.valid() && snapshot.missing_parent_count() != 0,
        }
    }

    pub(crate) fn alias(&self) -> Option<&VacantLocalFileAlias> {
        match self {
            Self::File(snapshot) => snapshot.alias(),
            Self::Hierarchy(snapshot) => Some(snapshot.location().alias()),
        }
    }

    pub(crate) fn location(&self) -> Option<&LocalTreeLocation> {
        match self {
            Self::File(snapshot) => snapshot.location(),
            Self::Hierarchy(snapshot) => Some(snapshot.location()),
        }
    }

    pub(crate) fn missing_parent_count(&self) -> u8 {
        match self {
            Self::File(_) => 0,
            Self::Hierarchy(snapshot) => snapshot.missing_parent_count(),
        }
    }

    pub(crate) fn first_missing<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        match self {
            Self::File(_) => Some(path),
            Self::Hierarchy(snapshot) => snapshot.first_missing(path),
        }
    }

    pub(crate) fn same_publication_prefix(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Hierarchy(left), Self::Hierarchy(right))
            if left.same_publication_prefix(right))
    }

    pub(crate) fn allows_successor(&self, successor: &LocalFileSnapshot) -> bool {
        match self {
            Self::File(snapshot) => {
                successor.published_from.is_none()
                    && snapshot
                        .alias()
                        .is_none_or(|alias| successor.alias() == Some(alias))
                    && snapshot
                        .location()
                        .is_none_or(|location| successor.location() == Some(location))
            }
            Self::Hierarchy(snapshot) => {
                successor.published_from == Some(snapshot.fingerprint)
                    && successor
                        .location()
                        .is_some_and(|location| location.mount == snapshot.location.mount)
            }
        }
    }
}

/// The bounds, content and native identity of one reviewed local rsync receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalExistingSnapshot {
    pub(crate) inventory: Inventory,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) root_identity: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) location: Option<LocalTreeLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) published_from: Option<[u8; 32]>,
}

impl LocalExistingSnapshot {
    pub(crate) fn valid(&self) -> bool {
        self.inventory.valid()
            && self.location.as_ref().is_none_or(LocalTreeLocation::valid)
            && self
                .published_from
                .is_none_or(|proof| proof != [0; 32] && self.location.is_some())
    }

    pub(crate) fn aliases(&self, other: &Self) -> bool {
        self.root_identity == other.root_identity
    }

    pub(crate) fn location(&self) -> Option<&LocalTreeLocation> {
        self.location.as_ref()
    }
}

/// The exact vacant name and checked parent of one local rsync directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VacantLocalTreeSnapshot {
    pub(super) fingerprint: [u8; 32],
    pub(super) location: LocalTreeLocation,
    #[serde(default, skip_serializing_if = "missing_parents_zero")]
    pub(super) missing_parents: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prefix: Option<[u8; 32]>,
}

impl VacantLocalTreeSnapshot {
    pub(crate) fn valid(&self) -> bool {
        self.fingerprint != [0; 32]
            && self.location.valid()
            && self.missing_parents <= MAX_MISSING_PARENTS
            && self
                .prefix
                .is_none_or(|proof| proof != [0; 32] && self.missing_parents != 0)
    }

    pub(crate) fn location(&self) -> &LocalTreeLocation {
        &self.location
    }

    pub(crate) fn missing_parent_count(&self) -> u8 {
        self.missing_parents
    }

    pub(crate) fn same_publication_prefix(&self, other: &Self) -> bool {
        self.prefix.is_some() && self.prefix == other.prefix && self.location == other.location
    }

    /// The reviewed absent parent paths, starting with the immediate parent.
    pub(crate) fn missing_parent_paths<'a>(
        &self,
        destination: &'a Path,
    ) -> Option<impl Iterator<Item = &'a Path> + use<'a>> {
        self.first_missing(destination)?;
        Some(
            destination
                .ancestors()
                .skip(1)
                .take(usize::from(self.missing_parents)),
        )
    }

    pub(crate) fn first_missing<'a>(&self, destination: &'a Path) -> Option<&'a Path> {
        if !self.valid()
            || !destination.is_absolute()
            || destination
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return None;
        }
        let mut first_missing = destination;
        for _ in 0..self.missing_parents {
            first_missing.file_name()?;
            first_missing = first_missing.parent()?;
        }
        first_missing.file_name()?;
        first_missing.parent()?;
        Some(first_missing)
    }

    pub(crate) fn allows_successor(&self, successor: &LocalExistingSnapshot) -> bool {
        if self.missing_parents == 0 {
            successor.location() == Some(self.location()) && successor.published_from.is_none()
        } else {
            successor.published_from == Some(self.fingerprint)
                && successor
                    .location()
                    .is_some_and(|location| location.mount == self.location.mount)
        }
    }
}

fn missing_parents_zero(count: &u8) -> bool {
    *count == 0
}
