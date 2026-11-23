use std::fmt;
use std::sync::{Arc, Mutex};

use iced::Task;
use kortty_vault::{CredentialKind, Secret, TemporarySshKeyOptions, Vault};

use super::{ConnectionWindow, Message, Persisted, Scope};
use crate::connection_store::{Auth, Connection, Store};
use crate::ssh_dialog::Password;

type Outcome = (Vault, Result<Box<Persisted>, String>);

#[derive(Clone)]
pub struct TemporarySaveReply(Arc<Mutex<Option<Outcome>>>);

impl fmt::Debug for TemporarySaveReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TemporarySaveReply")
    }
}

impl TemporarySaveReply {
    pub(crate) fn take(&self) -> Option<Outcome> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

impl ConnectionWindow {
    pub(crate) fn begin_temporary_key_paste(&mut self, request: u64) -> bool {
        !self.busy
            && !self.draft_conflict
            && self
                .draft
                .as_mut()
                .is_some_and(|draft| draft.begin_temporary_paste(request))
    }

    pub(crate) fn finish_temporary_key_paste(&mut self, request: u64, key: Password) {
        if !self.busy
            && !self.draft_conflict
            && let Some(draft) = &mut self.draft
        {
            let ready = draft.finish_temporary_paste(request, key);
            if ready {
                self.notice = None;
            }
        }
    }

    pub(crate) fn temporary_save_needs_vault(&self) -> bool {
        !self.busy
            && !self.draft_conflict
            && self.draft.as_ref().is_some_and(|draft| {
                let Ok(connection) = draft.connection() else {
                    return false;
                };
                draft.pending_temporary_key().is_some()
                    || self.store.get(connection.id).is_some_and(|original| {
                        temporary_references(original).any(|id| {
                            !temporary_references(&connection).any(|retained| retained == id)
                        })
                    })
            })
    }

    pub(crate) fn has_pending_temporary_key(&self) -> bool {
        self.draft
            .as_ref()
            .and_then(super::form::Draft::pending_temporary_key)
            .is_some()
    }

    pub(crate) fn save_connection_with_temporary_key(&mut self, mut vault: Vault) -> Task<Message> {
        let Some(draft) = &self.draft else {
            unreachable!("temporary-key save is gated by the active editor");
        };
        let connection = draft.connection();
        let key = draft.pending_temporary_key().cloned();
        let badge = draft.os_badge_id().map(str::to_owned);
        let group = self.draft_group.clone();
        let store = self.store.clone();
        self.busy = true;
        Task::future(async move {
            let result = connection.and_then(|connection| {
                persist(store, connection, group, badge, key, &mut vault).map(Box::new)
            });
            Message::TemporarySaveCompleted(TemporarySaveReply(Arc::new(Mutex::new(Some((
                vault, result,
            ))))))
        })
    }
}

fn persist(
    mut store: Store,
    mut connection: Connection,
    group: Option<String>,
    badge: Option<String>,
    key: Option<Password>,
    vault: &mut Vault,
) -> Result<Persisted, String> {
    let original = store.get(connection.id);
    let previous = original
        .into_iter()
        .flat_map(temporary_references)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let select_folder = connection.id == 0 || group.as_deref() != store.group_of(connection.id);
    let mut added = None;
    if let Some(key) = key {
        let Auth::TemporaryKey {
            credential,
            minutes,
            automatic,
        } = &mut connection.login.auth
        else {
            return Err("Select temporary SSH key authentication first.".into());
        };
        kortty_ssh::validate_private_key(key.as_str().trim().as_bytes(), None)
            .map_err(|_| "Paste a valid unencrypted SSH private key supported by kortty.")?;
        let endpoint = &connection.login.endpoint;
        let id = vault
            .save_temporary_ssh_key(
                connection.name.clone(),
                (&endpoint.host, endpoint.port, &endpoint.username),
                TemporarySshKeyOptions {
                    minutes: *minutes,
                    automatic: *automatic,
                },
                Secret::new(key.into_string()),
            )
            .map_err(|error| error.to_string())?;
        *credential = Some(id.clone());
        added = Some(id);
    }
    let id = match store.save_in_group_with_badge(connection, group.as_deref(), badge.as_deref()) {
        Ok(id) => id,
        Err(error) => {
            if let Some(id) = added
                && Store::open(store.path().to_owned()).is_ok_and(|latest| {
                    !latest
                        .connections()
                        .iter()
                        .flat_map(temporary_references)
                        .any(|reference| reference == id)
                })
            {
                let _ = vault.delete(&id);
            }
            return Err(error.to_string());
        }
    };
    let mut cleanup_failed = false;
    for previous in previous {
        if !store
            .connections()
            .iter()
            .flat_map(temporary_references)
            .any(|reference| reference == previous)
            && vault
                .get(&previous)
                .is_some_and(|entry| entry.kind == CredentialKind::TemporarySshKey)
            && vault.delete(&previous).is_err()
        {
            cleanup_failed = true;
        }
    }
    let mut done = Persisted::saved(store, id);
    if select_folder {
        done.scope = Some(group.map_or(Scope::All, Scope::Group));
    }
    if cleanup_failed {
        done.status = "Connection saved. An unused temporary SSH key could not be removed.";
    }
    Ok(done)
}

fn temporary_references(connection: &Connection) -> impl Iterator<Item = &str> {
    std::iter::once(&connection.login)
        .chain(connection.jump.as_ref())
        .filter_map(|login| match &login.auth {
            Auth::TemporaryKey { credential, .. } => credential.as_deref(),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_window::form::{Draft, Edit, LoginEdit, Method};
    use std::{fs, path::Path, process::Command};

    #[test]
    #[ignore = "requires OpenSSH ssh-keygen for disposable signing keys"]
    fn temporary_key_editor_publishes_replaces_and_removes_the_vault_reference() {
        let profile = crate::snippets::tests::Profile::new();
        let vault_path = profile.0.join("vault.bin");
        let store_path = profile.0.join("connections.json");
        let master = b"temporary key fixture password";
        let mut vault = Vault::create(&vault_path, master).unwrap();
        let store = Store::open(store_path.clone()).unwrap();
        let mut draft = Draft::new();
        draft.update(Edit::Name("Temporary fixture".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("host.example".into())));
        draft.update(Edit::Login(false, LoginEdit::Method(Method::TemporaryKey)));
        draft.update(Edit::Login(false, LoginEdit::TemporaryAutomatic(true)));
        let key = generate_key(&profile.0.join("first"));
        let result = persist(
            store,
            draft.connection().unwrap(),
            None,
            None,
            Some(key),
            &mut vault,
        )
        .unwrap();
        let saved = &result.store().connections()[0];
        let first = temporary_references(saved).next().unwrap().to_owned();
        assert!(
            !fs::read_to_string(&store_path)
                .unwrap()
                .contains("PRIVATE KEY")
        );
        drop(vault);

        let mut vault = Vault::unlock(&vault_path, master).unwrap();
        let target = ("host.example", 22, "root");
        let live = vault.temporary_ssh_key(&first, target).unwrap();
        assert!(live.options.automatic);
        assert_eq!(live.options.minutes, 60);
        let store = Store::open(store_path).unwrap();
        let saved = store.get(saved.id).unwrap().clone();
        let result = persist(
            store,
            saved,
            None,
            None,
            Some(generate_key(&profile.0.join("replacement"))),
            &mut vault,
        )
        .unwrap();
        let saved = &result.store().connections()[0];
        let second = temporary_references(saved).next().unwrap().to_owned();
        assert_ne!(first, second);
        assert!(vault.get(&first).is_none());
        assert!(vault.temporary_ssh_key(&second, target).is_some());

        let mut edited = Draft::from_profile(saved, None, false);
        edited.update(Edit::Login(false, LoginEdit::RemoveTemporaryKey));
        let removed = persist(
            result.store().clone(),
            edited.connection().unwrap(),
            None,
            None,
            None,
            &mut vault,
        )
        .unwrap();
        assert!(
            temporary_references(&removed.store().connections()[0])
                .next()
                .is_none()
        );
        assert!(vault.get(&second).is_none());
    }

    fn generate_key(path: &Path) -> Password {
        let generated = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .status()
            .unwrap();
        assert!(generated.success());
        Password::new(fs::read_to_string(path).unwrap())
    }
}
