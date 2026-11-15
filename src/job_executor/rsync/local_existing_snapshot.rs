use kortty_server_archive::rsync_tree::Inventory;
use serde::{Deserialize, Serialize};

/// The bounds, content and native identity of one reviewed local rsync receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalExistingSnapshot {
    pub(crate) inventory: Inventory,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) root_identity: [u8; 32],
}

impl LocalExistingSnapshot {
    pub(crate) fn valid(&self) -> bool {
        self.inventory.valid()
    }

    pub(crate) fn aliases(&self, other: &Self) -> bool {
        self.root_identity == other.root_identity
    }
}
