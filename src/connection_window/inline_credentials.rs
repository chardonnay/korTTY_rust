use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

use crate::connection_store::{Auth, AuthMethod, Connection, SavedLogin, Store};
use crate::ssh_dialog::Password;

#[derive(Default)]
pub(super) struct Pending {
    pub(super) password: Option<Password>,
    pub(super) passphrase: Option<Password>,
    pub(super) jump_password: Option<Password>,
    pub(super) jump_passphrase: Option<Password>,
}

impl Pending {
    pub(super) fn is_empty(&self) -> bool {
        self.password.is_none()
            && self.passphrase.is_none()
            && self.jump_password.is_none()
            && self.jump_passphrase.is_none()
    }

    pub(super) fn save(
        self,
        connection: &mut Connection,
        vault: &mut Vault,
        added: &mut Vec<String>,
    ) -> Result<(), String> {
        let name: String = connection.name.chars().take(100).collect();
        for (jump, password, passphrase) in [
            (false, self.password, self.passphrase),
            (true, self.jump_password, self.jump_passphrase),
        ] {
            let login = if jump {
                let Some(login) = &mut connection.jump else {
                    continue;
                };
                login
            } else {
                &mut connection.login
            };
            for (kind, value) in [
                (CredentialKind::Password, password),
                (CredentialKind::KeyPassphrase, passphrase),
            ] {
                let Some(value) = value else { continue };
                let title = match (jump, kind) {
                    (false, CredentialKind::Password) => "password",
                    (false, _) => "key passphrase",
                    (true, CredentialKind::Password) => "jump password",
                    (true, _) => "jump key passphrase",
                };
                let id = vault
                    .save(
                        CredentialInput {
                            label: format!("{name} — {title}"),
                            username: login.endpoint.username.clone(),
                            server_pattern: login.endpoint.host.clone(),
                            kind,
                            ..Default::default()
                        },
                        Secret::new(value.into_string()),
                    )
                    .map_err(|error| error.to_string())?;
                added.push(id.clone());
                assign(login, kind, id);
            }
        }
        Ok(())
    }
}

fn assign(login: &mut SavedLogin, kind: CredentialKind, id: String) {
    match kind {
        CredentialKind::Password => match &mut login.auth {
            Auth::Prompt | Auth::VaultPassword(_) => login.auth = Auth::VaultPassword(id),
            Auth::Configured { credential, .. } => *credential = Some(id),
            Auth::KeyboardInteractive => {
                login.auth = Auth::Configured {
                    method: AuthMethod::KeyboardInteractive,
                    credential: Some(id),
                    key: None,
                };
            }
            Auth::StoredKey(key) => {
                login.auth = Auth::Configured {
                    method: AuthMethod::PublicKey,
                    credential: Some(id),
                    key: Some(*key),
                };
            }
            Auth::KeyFile { .. } | Auth::TemporaryKey { .. } => {
                login.preferences.password = Some(id);
            }
        },
        CredentialKind::KeyPassphrase => match &mut login.auth {
            Auth::KeyFile { passphrase, .. } => *passphrase = Some(id),
            _ => login.preferences.passphrase = Some(id),
        },
        _ => unreachable!("inline SSH fields only store passwords or passphrases"),
    }
}

/// Remove only additions which a fresh connection store proves unreferenced.
pub(super) fn rollback(store: &Store, vault: &mut Vault, added: &[String]) {
    let Ok(latest) = Store::open(store.path().to_owned()) else {
        return;
    };
    for id in added {
        if !latest
            .connections()
            .iter()
            .chain(latest.retained_ssh_profiles())
            .any(|connection| {
                std::iter::once(&connection.login)
                    .chain(connection.jump.iter())
                    .any(|login| references(login, id))
            })
        {
            let _ = vault.delete(id);
        }
    }
}

fn references(login: &SavedLogin, id: &str) -> bool {
    login.auth.credential_reference() == Some(id)
        || matches!(&login.auth, Auth::KeyFile { passphrase: Some(value), .. } | Auth::TemporaryKey { credential: Some(value), .. } if value == id)
        || login.preferences.password.as_deref() == Some(id)
        || login.preferences.passphrase.as_deref() == Some(id)
}
