//! The reviewed server destination stays reserved after retiring its data receipt.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::sftp_window::worker::resume::persist::{Lease, Store};

use super::*;

static ACTIVE: LazyLock<Mutex<HashSet<[u8; 32]>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub(super) struct Reservation {
    key: [u8; 32],
    lease: Option<Lease>,
}

impl Reservation {
    pub(super) fn claim(scope: ReplaceScope<'_>, path: &[u8]) -> io::Result<Self> {
        let key = key(&scope.approval.route, &scope.approval.peer, path)?;
        let profile = scope
            .recovery
            .map(|recovery| recovery.snapshot.1.profile.as_path());
        Self::acquire(key, profile)
    }

    fn acquire(key: [u8; 32], profile: Option<&Path>) -> io::Result<Self> {
        if !ACTIVE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another reviewed replacement owns this server destination",
            ));
        }
        let mut reservation = Self { key, lease: None };
        if let Some(profile) = profile {
            reservation.lease = Some(Store::new(profile).lease_replacement_target(&key)?);
        }
        Ok(reservation)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        drop(self.lease.take());
        ACTIVE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

fn key(route: &SshTarget, peer: &HostKey, path: &[u8]) -> io::Result<[u8; 32]> {
    let binding = serde_json::to_vec(&(
        "kortty-sftp-replacement-target-v1",
        route,
        peer.fingerprint(),
        path,
    ))?;
    Ok(hash_bytes(
        HashAlgorithm::Sha256
            .digest(&binding)
            .map_err(io::Error::other)?
            .into_boxed_slice(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_target_reservation_survives_receipt_retirement() {
        let profile = crate::snippets::tests::Profile::new();
        let store = Store::new(&profile.0);
        let key = [42; 32];
        let reserved = Reservation::acquire(key, Some(&profile.0)).unwrap();
        let mut receipt = store.lease_upload(&"a".repeat(32), true).unwrap();
        receipt.retain(false);
        drop(receipt);
        assert!(matches!(
            Reservation::acquire(key, None),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(matches!(
            store.lease_replacement_target(&key),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(Reservation::acquire([43; 32], Some(&profile.0)).is_ok());
        drop(reserved);
        assert!(Reservation::acquire(key, Some(&profile.0)).is_ok());
    }
}
