use std::io::{self, Write};

use kortty_ssh::{Endpoint, HostKey, WindowSize};
use zeroize::Zeroizing;

use crate::ssh_connection;
use crate::ssh_dialog::{Authentication, LoginRequest, Request};

use super::wire::{self, Credential, invalid};

pub(super) fn send(
    out: &mut impl Write,
    request: Request,
    pins: &[(Endpoint, HostKey)],
    size: WindowSize,
    sftp: bool,
) -> io::Result<()> {
    wire::send_login_header(
        out,
        pins,
        request.profile.term_name(),
        size,
        sftp,
        request.options,
    )?;
    send_target(out, request.target)?;
    out.write_all(&[u8::from(request.jump.is_some())])?;
    if let Some(jump) = request.jump {
        send_target(out, jump)?;
    }
    out.flush()
}

fn send_target(out: &mut impl Write, login: LoginRequest) -> io::Result<()> {
    login.endpoint.validate().map_err(|_| invalid())?;
    let endpoint = Endpoint::new(&login.endpoint.host, login.endpoint.port)?;
    let mut send = |credential| {
        wire::send_target(
            out,
            &endpoint,
            &login.endpoint.username,
            login.keyboard_interactive,
            credential,
        )
    };
    match login.auth {
        Authentication::KeyboardInteractive => send(Credential::KeyboardInteractive),
        Authentication::Password(password) => send(Credential::Password(password.as_str())),
        Authentication::PrivateKey(path) => {
            let key = Zeroizing::new(ssh_connection::read_key(&path)?);
            send(Credential::PrivateKey(&key))
        }
        Authentication::KeyWithPassphrase(path, password) => {
            let key = Zeroizing::new(ssh_connection::read_key(&path)?);
            send(Credential::KeyWithPassphrase(&key, password.as_str()))
        }
        Authentication::TemporaryKey(key) => {
            let bytes = key.copy_for_attempt()?;
            send(Credential::PrivateKey(&bytes))
        }
        Authentication::ResolvedKey(record, password) => {
            let key = ssh_connection::read_registered_key(&record)?;
            match password {
                Some(password) => send(Credential::KeyWithPassphrase(&key, password.as_str())),
                None => send(Credential::PrivateKey(&key)),
            }
        }
        Authentication::VaultPassword(_)
        | Authentication::VaultKey(..)
        | Authentication::StoredKey { .. } => Err(invalid()),
    }
}
