use std::collections::HashMap;

use keyring_core::{Entry, api::CredentialStoreApi as _};
use windows_native_keyring_store::Store;
use zeroize::Zeroizing;

use super::SERVICE;
use crate::{Error, startup_unlock::Record};

fn entry(account: &str) -> Result<Entry, Error> {
    let store = Store::new().map_err(|_| Error::StartupCredentialUnavailable)?;
    let modifiers = HashMap::from([("persistence", "local")]);
    store
        .build(SERVICE, account, Some(&modifiers))
        .map_err(|_| Error::StartupCredentialUnavailable)
}

pub(crate) fn read(account: &str) -> Result<Record, Error> {
    entry(account)?
        .get_secret()
        .map(Zeroizing::new)
        .map_err(|_| Error::StartupCredentialUnavailable)
}

pub(crate) fn write(account: &str, value: &[u8]) -> Result<(), Error> {
    entry(account)?
        .set_secret(value)
        .map_err(|_| Error::StartupCredentialUnavailable)
}

pub(crate) fn delete(account: &str) -> Result<(), Error> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(_) => Err(Error::StartupCredentialUnavailable),
    }
}
