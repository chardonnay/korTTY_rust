use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use zeroize::Zeroizing;

use super::SERVICE;
use crate::{Error, startup_unlock::Record};

const ITEM_NOT_FOUND: i32 = -25300;

pub(crate) fn read(account: &str) -> Result<Record, Error> {
    get_generic_password(SERVICE, account)
        .map(Zeroizing::new)
        .map_err(|_| Error::StartupCredentialUnavailable)
}

pub(crate) fn write(account: &str, value: &[u8]) -> Result<(), Error> {
    set_generic_password(SERVICE, account, value).map_err(|_| Error::StartupCredentialUnavailable)
}

pub(crate) fn delete(account: &str) -> Result<(), Error> {
    match delete_generic_password(SERVICE, account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
        Err(_) => Err(Error::StartupCredentialUnavailable),
    }
}
