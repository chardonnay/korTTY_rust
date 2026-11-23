use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{Error, Vault, envelope, hex, storage};

mod native;
#[cfg(test)]
mod tests;

const SUFFIX: &str = ".startup-unlock";
const ENABLED: &[u8] = b"enabled-v1";
const REMOVE: &[u8] = b"remove-v1";
const RECORD_VERSION: u8 = 1;
const KEY_OFFSET: usize = 1 + size_of::<envelope::Salt>();
const RECORD_LEN: usize = KEY_OFFSET + 32;
type Record = Zeroizing<Vec<u8>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupUnlockState {
    Disabled,
    Enabled,
    RemovalPending,
}

impl Vault {
    /// Reads only the local opt-in marker. This never contacts the credential store.
    pub fn startup_unlock_state(path: impl AsRef<Path>) -> Result<StartupUnlockState, Error> {
        state(&storage::sidecar(path.as_ref(), SUFFIX)?)
    }

    /// Opens the vault with the device credential after an explicit local opt-in.
    /// Callers must enforce their startup policy before attempting this operation.
    pub fn unlock_remembered(path: impl AsRef<Path>) -> Result<Self, Error> {
        unlock(path.as_ref(), native::read)
    }

    /// Saves this vault's current derived key in the device credential store.
    pub fn remember_startup_unlock(&self) -> Result<(), Error> {
        self.remember_with(native::write)
    }

    /// Updates an existing opt-in after a master password change.
    pub fn refresh_startup_unlock(&self) -> Result<(), Error> {
        if Self::startup_unlock_state(&self.path)? == StartupUnlockState::Enabled {
            self.remember_startup_unlock()?;
        }
        Ok(())
    }

    /// Disables automatic unlock before removing the device credential.
    pub fn forget_startup_unlock(&self) -> Result<(), Error> {
        forget(&self.path, native::delete)
    }

    /// Disables automatic unlock without requiring the vault password.
    pub fn forget_locked_startup_unlock(path: impl AsRef<Path>) -> Result<(), Error> {
        let path = storage::absolute_destination(path.as_ref())?;
        let _lock = storage::lock(&path)?;
        forget(&path, native::delete)
    }

    fn remember_with(
        &self,
        write: impl FnOnce(&str, &[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.ensure_current()?;
        let marker = storage::sidecar(&self.path, SUFFIX)?;
        let old_state = state(&marker)?;
        if old_state == StartupUnlockState::Disabled {
            // Keep a way to remove the native record if the process stops before opt-in is published.
            set_state(&marker, REMOVE, false)?;
        }
        let mut record = Zeroizing::new(Vec::with_capacity(RECORD_LEN));
        record.push(RECORD_VERSION);
        record.extend_from_slice(&self.salt);
        record.extend_from_slice(self.key.as_ref());
        write(&account(&self.path), record.as_slice())?;
        if old_state != StartupUnlockState::Enabled {
            set_state(&marker, ENABLED, true)?;
        }
        Ok(())
    }
}

fn unlock(path: &Path, read: impl FnOnce(&str) -> Result<Record, Error>) -> Result<Vault, Error> {
    let path = storage::absolute_destination(path)?;
    let lock = storage::lock(&path)?;
    if Vault::startup_unlock_state(&path)? != StartupUnlockState::Enabled {
        return Err(Error::NotFound);
    }
    let snapshot = storage::read(&path)?;
    let salt = envelope::salt(&snapshot)?;
    let record = read(&account(&path))?;
    if record.len() != RECORD_LEN || record[0] != RECORD_VERSION || record[1..KEY_OFFSET] != salt {
        return Err(Error::Authentication);
    }
    let mut key = envelope::Key::new([0; 32]);
    key.copy_from_slice(&record[KEY_OFFSET..]);
    let entries = envelope::authenticate_key(&snapshot, &key)?;
    Ok(Vault {
        entries,
        key,
        salt,
        snapshot,
        path,
        _lock: lock,
    })
}

fn forget(path: &Path, delete: impl FnOnce(&str) -> Result<(), Error>) -> Result<(), Error> {
    let marker = storage::sidecar(path, SUFFIX)?;
    match state(&marker)? {
        StartupUnlockState::Disabled => return Ok(()),
        StartupUnlockState::Enabled => set_state(&marker, REMOVE, true)?,
        StartupUnlockState::RemovalPending => {}
    }
    delete(&account(path))?;
    fs::remove_file(&marker)?;
    storage::sync_parent(&marker)
}

fn state(marker: &Path) -> Result<StartupUnlockState, Error> {
    match storage::read_bounded(marker, ENABLED.len().max(REMOVE.len())) {
        Ok(value) if value == ENABLED => Ok(StartupUnlockState::Enabled),
        Ok(value) if value == REMOVE => Ok(StartupUnlockState::RemovalPending),
        Ok(_) => Err(Error::InvalidFormat),
        Err(Error::NotFound) => Ok(StartupUnlockState::Disabled),
        Err(error) => Err(error),
    }
}

fn set_state(marker: &Path, value: &[u8], replace: bool) -> Result<(), Error> {
    storage::publish(marker, value, replace)?;
    storage::sync_parent(marker)
}

fn account(path: &Path) -> String {
    // This identity is used only on the platform that owns the canonical path.
    hex::<32>(Sha256::digest(path.as_os_str().as_encoded_bytes()).into())
}
