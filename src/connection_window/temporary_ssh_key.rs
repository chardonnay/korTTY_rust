use std::fmt;
use std::sync::{Arc, Mutex};

use iced::Task;
use kortty_vault::{CredentialKind, Secret, TemporarySshKeyOptions, Vault};

use super::{ConnectionWindow, Message, Persisted, Scope};
use crate::connection_store::{Auth, Connection, Store};
use crate::ssh_dialog::Password;

type Outcome = (Vault, Result<Box<Persisted>, String>);

#[derive(Clone)]
pub struct CredentialSaveReply(Arc<Mutex<Option<Outcome>>>);

impl fmt::Debug for CredentialSaveReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialSaveReply")
    }
}

impl CredentialSaveReply {
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

    pub(crate) fn credential_save_needs_vault(&self) -> bool {
        !self.busy
            && !self.draft_conflict
            && self.draft.as_ref().is_some_and(|draft| {
                let Ok(prepared) = draft.prepared() else {
                    return false;
                };
                let Some(connection) = prepared.ssh() else {
                    return false;
                };
                draft.pending_temporary_key().is_some()
                    || !draft.pending_inline_credentials().is_empty()
                    || self
                        .store
                        .get(connection.id)
                        .into_iter()
                        .chain(
                            self.store
                                .retained_ssh_profiles()
                                .filter(|profile| profile.id == connection.id),
                        )
                        .any(|original| {
                            temporary_references(original).any(|id| {
                                !temporary_references(connection).any(|retained| retained == id)
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

    pub(crate) fn save_connection_with_credentials(&mut self, mut vault: Vault) -> Task<Message> {
        let Some(draft) = &self.draft else {
            unreachable!("credential save is gated by the active editor");
        };
        let prepared = draft.prepared();
        let key = draft.pending_temporary_key().cloned();
        let inline = draft.pending_inline_credentials();
        let group = self.draft_group.clone();
        let store = self
            .draft_store
            .clone()
            .unwrap_or_else(|| self.store.clone());
        let expected = self
            .draft_expected
            .clone()
            .or_else(|| store.protocol_profile(draft.id()));
        self.busy = true;
        Task::future(async move {
            let result = prepared.and_then(|prepared| {
                persist_prepared(
                    store,
                    Save {
                        prepared,
                        expected,
                        group,
                        key,
                        inline,
                    },
                    &mut vault,
                )
                .map(Box::new)
            });
            Message::CredentialSaveCompleted(CredentialSaveReply(Arc::new(Mutex::new(Some((
                vault, result,
            ))))))
        })
    }
}

struct Save {
    prepared: super::form::protocol::Prepared,
    expected: Option<crate::connection_store::protocol::Profile>,
    group: Option<String>,
    key: Option<Password>,
    inline: super::inline_credentials::Pending,
}

fn persist_prepared(
    mut store: Store,
    mut save: Save,
    vault: &mut Vault,
) -> Result<Persisted, String> {
    let profile_id = save.prepared.selected.id();
    let previous = store
        .connections()
        .iter()
        .chain(store.retained_ssh_profiles())
        .filter(|connection| connection.id == profile_id)
        .flat_map(temporary_references)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let select_folder = profile_id == 0 || save.group.as_deref() != store.group_of(profile_id);
    let mut added = Vec::new();
    if let Some(key) = save.key {
        let connection = save
            .prepared
            .ssh_mut()
            .ok_or("Select SSH key authentication first.")?;
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
        added.push(id);
    }
    if !save.inline.is_empty() {
        let connection = save
            .prepared
            .ssh_mut()
            .ok_or("Complete the SSH profile before saving its credentials.")?;
        if let Err(error) = save.inline.save(connection, vault, &mut added) {
            super::inline_credentials::rollback(&store, vault, &added);
            return Err(error);
        }
    }
    let id = match store.save_protocol(
        save.expected.as_ref(),
        save.prepared.selected,
        save.prepared.retained,
        save.group.as_deref(),
        Some(save.prepared.settings),
    ) {
        Ok(id) => id,
        Err(error) => {
            super::inline_credentials::rollback(&store, vault, &added);
            return Err(error.to_string());
        }
    };
    let mut cleanup_failed = false;
    for previous in previous {
        if !store
            .connections()
            .iter()
            .chain(store.retained_ssh_profiles())
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
        done.scope = Some(save.group.map_or(Scope::All, Scope::Group));
    }
    if cleanup_failed {
        done.status = "Connection saved. An unused temporary SSH key could not be removed.";
    }
    Ok(done)
}

#[cfg(test)]
fn persist(
    store: Store,
    connection: Connection,
    group: Option<String>,
    badge: Option<String>,
    key: Option<Password>,
    inline: super::inline_credentials::Pending,
    vault: &mut Vault,
) -> Result<Persisted, String> {
    let expected = store.protocol_profile(connection.id);
    let mut settings =
        crate::connection_store::protocol::EditorSettings::from_connection(&connection);
    settings.os_badge = badge;
    let prepared = super::form::protocol::Prepared {
        selected: crate::connection_store::protocol::Profile::Ssh(Box::new(connection)),
        retained: vec![],
        settings,
    };
    persist_prepared(
        store,
        Save {
            prepared,
            expected,
            group,
            key,
            inline,
        },
        vault,
    )
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

    fn inline_draft() -> Draft {
        let mut draft = Draft::new();
        draft.update(Edit::Name("Inline fixture".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("host.example".into())));
        draft
    }

    #[test]
    fn credentials_are_sealed_when_ssh_becomes_an_inactive_protocol() {
        use crate::connection_store::protocol::{Kind, Profile};
        let directory = crate::snippets::tests::Profile::new();
        let store = Store::open(directory.0.join("connections.json")).unwrap();
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(directory.0.join("vault.bin"), &master[..]).unwrap();
        let mut draft = inline_draft();
        draft.update(Edit::Login(
            false,
            LoginEdit::InlinePassword(Password::new("retained secret".into())),
        ));
        let _ = draft.switch_protocol(Kind::Local);
        let save = Save {
            prepared: draft.prepared().unwrap(),
            expected: None,
            group: None,
            key: None,
            inline: draft.pending_inline_credentials(),
        };
        let result = persist_prepared(store, save, &mut vault).unwrap();
        let local = &result.store().local_profiles()[0];
        let retained = result.store().retained_ssh_profiles().next().unwrap();
        let id = retained.login.auth.credential_reference().unwrap();
        assert_eq!(vault.secret(id), Some("retained secret"));
        assert_eq!(retained.id, local.id);
        let mut reopened = Draft::from_protocol(
            &Profile::Local(Box::new(local.clone())),
            result.store(),
            false,
        );
        let _ = reopened.switch_protocol(Kind::Ssh);
        let prepared = reopened.prepared().unwrap();
        assert_eq!(
            prepared.ssh().unwrap().login.auth.credential_reference(),
            Some(id)
        );
    }

    #[test]
    fn inline_credentials_save_as_independent_encrypted_references_and_survive_restart() {
        let profile = crate::snippets::tests::Profile::new();
        let vault_path = profile.0.join("vault.bin");
        let store_path = profile.0.join("connections.json");
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(&vault_path, &master[..]).unwrap();
        let mut draft = inline_draft();
        draft.update(Edit::Login(
            false,
            LoginEdit::InlinePassword(Password::new("  target secret  ".into())),
        ));
        draft.update(Edit::UseJump(true));
        draft.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        draft.update(Edit::Login(true, LoginEdit::Username("jumper".into())));
        draft.update(Edit::Login(
            true,
            LoginEdit::InlinePassword(Password::new("jump secret".into())),
        ));
        let result = persist(
            Store::open(store_path.clone()).unwrap(),
            draft.connection().unwrap(),
            None,
            None,
            None,
            draft.pending_inline_credentials(),
            &mut vault,
        )
        .unwrap();
        let saved = result.store().connections()[0].clone();
        let target = saved.login.auth.credential_reference().unwrap().to_owned();
        let jump = saved
            .jump
            .as_ref()
            .unwrap()
            .auth
            .credential_reference()
            .unwrap()
            .to_owned();
        assert_ne!(target, jump);
        let bytes = fs::read(&vault_path).unwrap();
        assert!(
            !bytes
                .windows(b"target secret".len())
                .any(|value| value == b"target secret")
        );
        assert!(!fs::read_to_string(&store_path).unwrap().contains("secret"));
        drop(vault);
        let mut vault = Vault::unlock(&vault_path, &master[..]).unwrap();
        assert_eq!(vault.secret(&target), Some("  target secret  "));
        assert_eq!(vault.secret(&jump), Some("jump secret"));
        let reopened = Draft::from_profile(&saved, None, false);
        assert!(reopened.pending_inline_credentials().is_empty());
        assert_eq!(reopened.connection().unwrap().login, saved.login);
        let mut replacement = reopened;
        replacement.update(Edit::Login(
            false,
            LoginEdit::InlinePassword(Password::new("replacement".into())),
        ));
        let result = persist(
            result.store().clone(),
            replacement.connection().unwrap(),
            None,
            None,
            None,
            replacement.pending_inline_credentials(),
            &mut vault,
        )
        .unwrap();
        let replaced = result.store().connections()[0]
            .login
            .auth
            .credential_reference()
            .unwrap();
        assert_ne!(target, replaced);
        assert_eq!(vault.secret(&target), Some("  target secret  "));
        assert_eq!(vault.secret(replaced), Some("replacement"));
    }

    #[test]
    fn failed_connection_publication_removes_only_its_new_inline_credentials() {
        let profile = crate::snippets::tests::Profile::new();
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(profile.0.join("vault.bin"), &master[..]).unwrap();
        let path = profile.0.join("connections.json");
        let stale = Store::open(path.clone()).unwrap();
        let mut concurrent = Store::open(path).unwrap();
        concurrent
            .save(inline_draft().connection().unwrap())
            .unwrap();
        let mut draft = inline_draft();
        draft.update(Edit::Login(
            false,
            LoginEdit::InlinePassword(Password::new("uncommitted".into())),
        ));
        let before = vault.entries().count();
        assert!(
            persist(
                stale,
                draft.connection().unwrap(),
                None,
                None,
                None,
                draft.pending_inline_credentials(),
                &mut vault
            )
            .is_err()
        );
        assert_eq!(vault.entries().count(), before);
        assert_eq!(concurrent.connections().len(), 1);
    }

    #[test]
    fn a_failed_inline_batch_rolls_back_its_earlier_additions() {
        let profile = crate::snippets::tests::Profile::new();
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(profile.0.join("vault.bin"), &master[..]).unwrap();
        let pending = super::super::inline_credentials::Pending {
            password: Some(Password::new("first addition".into())),
            passphrase: Some(Password::new("x".repeat(65_537))),
            ..Default::default()
        };
        assert!(
            persist(
                Store::open(profile.0.join("connections.json")).unwrap(),
                inline_draft().connection().unwrap(),
                None,
                None,
                None,
                pending,
                &mut vault,
            )
            .is_err()
        );
        assert_eq!(vault.entries().count(), 0);
    }

    #[test]
    fn inline_file_passphrase_and_inactive_password_are_both_retained() {
        let profile = crate::snippets::tests::Profile::new();
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(profile.0.join("vault.bin"), &master[..]).unwrap();
        let mut draft = inline_draft();
        draft.update(Edit::Login(
            false,
            LoginEdit::InlinePassword(Password::new("inactive password".into())),
        ));
        draft.update(Edit::Login(false, LoginEdit::Method(Method::Key)));
        draft.update(Edit::Login(
            false,
            LoginEdit::KeyPath(profile.0.join("private-key").to_string_lossy().into_owned()),
        ));
        draft.update(Edit::Login(
            false,
            LoginEdit::InlinePassphrase(Password::new("  passphrase  ".into())),
        ));
        let result = persist(
            Store::open(profile.0.join("connections.json")).unwrap(),
            draft.connection().unwrap(),
            None,
            None,
            None,
            draft.pending_inline_credentials(),
            &mut vault,
        )
        .unwrap();
        let saved = &result.store().connections()[0].login;
        let Auth::KeyFile {
            passphrase: Some(id),
            ..
        } = &saved.auth
        else {
            panic!("expected file key")
        };
        assert_eq!(vault.secret(id), Some("  passphrase  "));
        assert_eq!(vault.get(id).unwrap().kind, CredentialKind::KeyPassphrase);
        assert_eq!(
            vault.secret(saved.preferences.password.as_deref().unwrap()),
            Some("inactive password")
        );
    }

    #[test]
    #[ignore = "requires OpenSSH ssh-keygen for disposable signing keys"]
    fn temporary_key_editor_publishes_replaces_and_removes_the_vault_reference() {
        let profile = crate::snippets::tests::Profile::new();
        let vault_path = profile.0.join("vault.bin");
        let store_path = profile.0.join("connections.json");
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(&vault_path, &master[..]).unwrap();
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
            Default::default(),
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

        let mut vault = Vault::unlock(&vault_path, &master[..]).unwrap();
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
            Default::default(),
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
            Default::default(),
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
