//! Connect the ownership worker's cancellation and policy checks to Unix lookup.

use std::cell::RefCell;
use std::io;

use kortty_files::unix_accounts;

use crate::sftp_window::worker::Result;

use super::Kind;

pub(in crate::sftp_window::worker::ownership) fn resolve(
    kind: Kind,
    name: &str,
    guard: &impl Fn() -> Result<()>,
) -> Result<u32> {
    guard()?;
    let kind = match kind {
        Kind::User => unix_accounts::Kind::User,
        Kind::Group => unix_accounts::Kind::Group,
    };
    let denied = RefCell::new(None);
    let resolved = unix_accounts::resolve(kind, name, &|| {
        guard().map_err(|error| {
            *denied.borrow_mut() = Some(error);
            io::ErrorKind::Interrupted.into()
        })
    });
    guard()?;
    if let Some(error) = denied.into_inner() {
        return Err(error);
    }
    resolved.map_err(Into::into)
}
