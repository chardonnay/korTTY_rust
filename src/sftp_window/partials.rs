//! Flat transfer parts and reservations shared by the SFTP workers.

use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;

use super::{Entry, EntryKind, EntryName, files};

const SUFFIX: &[u8] = b".kortty-part";
static ACTIVE: Mutex<BTreeSet<Vec<u8>>> = Mutex::new(BTreeSet::new());

pub(super) fn candidate(entry: &Entry) -> Option<&[u8]> {
    if entry.kind != EntryKind::File {
        return None;
    }
    let name = match &entry.name {
        EntryName::Local(name) => name.as_encoded_bytes(),
        EntryName::Remote(name) => name,
    };
    let stem = name.strip_suffix(SUFFIX)?;
    (!stem.is_empty() && files::validate_transfer_name(name).is_ok()).then_some(name)
}

pub(super) fn active(name: &[u8]) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(name)
}

pub(super) struct Reservation(Vec<u8>);

impl Drop for Reservation {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.0);
    }
}

pub(super) fn reserve(name: &[u8]) -> Option<Reservation> {
    let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
    active
        .insert(name.to_vec())
        .then(|| Reservation(name.to_vec()))
}

pub(super) fn stage() -> io::Result<(String, Reservation)> {
    let name = files::nonce()?;
    let guard = reserve(name.as_bytes())
        .ok_or_else(|| io::Error::other("Transfer staging name is already in use"))?;
    Ok((name, guard))
}
