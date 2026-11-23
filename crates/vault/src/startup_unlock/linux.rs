use std::path::Path;

use futures_lite::future::block_on;
use oo7::{Item, Keyring, dbus::Service};
use zeroize::Zeroizing;

use super::SERVICE;
use crate::{Error, startup_unlock::Record};

async fn open() -> Result<Keyring, Error> {
    let store = if Path::new("/.flatpak-info").is_file() {
        // Use the per-application Secret portal without accessing the host keyring.
        Keyring::sandboxed()
            .await
            .map_err(|_| Error::StartupCredentialUnavailable)?
    } else {
        let service = Service::encrypted()
            .await
            .map_err(|_| Error::StartupCredentialUnavailable)?;
        let collection = service
            .default_collection()
            .await
            .map_err(|_| Error::StartupCredentialUnavailable)?;
        Keyring::DBus(collection)
    };
    if store.is_locked().await.unwrap_or(true) {
        return Err(Error::StartupCredentialUnavailable);
    }
    Ok(store)
}

fn attributes(account: &str) -> [(&str, &str); 2] {
    [("application", SERVICE), ("profile", account)]
}

async fn find(store: &Keyring, account: &str) -> Result<Option<Item>, Error> {
    let mut items = store
        .search_items(&attributes(account))
        .await
        .map_err(|_| Error::StartupCredentialUnavailable)?;
    match items.len() {
        0 | 1 => Ok(items.pop()),
        _ => Err(Error::StartupCredentialUnavailable),
    }
}

pub(crate) fn read(account: &str) -> Result<Record, Error> {
    block_on(async {
        let store = open().await?;
        let item = find(&store, account)
            .await?
            .ok_or(Error::StartupCredentialUnavailable)?;
        let secret = item
            .secret()
            .await
            .map_err(|_| Error::StartupCredentialUnavailable)?;
        Ok(Zeroizing::new(secret.as_bytes().to_vec()))
    })
}

pub(crate) fn write(account: &str, value: &[u8]) -> Result<(), Error> {
    block_on(async {
        let store = open().await?;
        let result = if let Some(item) = find(&store, account).await? {
            item.set_secret(value).await
        } else {
            store
                .create_item(
                    "kortty vault automatic unlock",
                    &attributes(account),
                    value,
                    false,
                )
                .await
        };
        result.map_err(|_| Error::StartupCredentialUnavailable)
    })
}

pub(crate) fn delete(account: &str) -> Result<(), Error> {
    block_on(async {
        let store = open().await?;
        if let Some(item) = find(&store, account).await? {
            item.delete()
                .await
                .map_err(|_| Error::StartupCredentialUnavailable)?;
        }
        Ok(())
    })
}
