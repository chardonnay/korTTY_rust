use iced::{Task, window};
use kortty_terminal::TerminalProfile;

use crate::connection_store::Connection;
use crate::ssh_dialog::{Authentication, Password, Request};
use crate::ssh_key_catalog::{Catalog, KeyRecord};
use crate::vault_window::Kind;

use super::{SavedSshProfile, SshTarget, Workspace, WorkspaceMessage};

#[derive(Default)]
pub(in crate::workspace) struct KeySnapshots {
    records: Vec<KeyRecord>,
    saved: Option<Connection>,
}

impl KeySnapshots {
    pub(in crate::workspace) fn is_current(&self) -> bool {
        self.saved.as_ref().is_none_or(|saved| {
            SavedSshProfile::from_profile(saved.id)
                .is_ok_and(|current| current.is_some_and(|current| current.connection == *saved))
        }) && (self.records.is_empty()
            || Catalog::open_profile().is_ok_and(|catalog| {
                self.records
                    .iter()
                    .all(|key| catalog.get(key.id) == Some(key))
            }))
    }
}

impl Workspace {
    pub(super) fn resolve_ssh_logins(
        &self,
        request: &mut Request,
        saved: Option<&Connection>,
    ) -> Result<KeySnapshots, &'static str> {
        let mut catalog = None;
        let mut keys = KeySnapshots::default();
        for login in std::iter::once(&mut request.target).chain(request.jump.as_mut()) {
            if let Authentication::StoredKey { id, explicit } = login.auth {
                if catalog.is_none() {
                    catalog = Some(
                        Catalog::open_profile()
                            .map_err(|_| "Could not open the registered SSH keys.")?,
                    );
                }
                let key = catalog
                    .as_ref()
                    .and_then(|catalog| catalog.get(id))
                    .ok_or("The registered SSH key was removed or is unavailable.")?;
                let passphrase = key
                    .passphrase
                    .as_deref()
                    .map(|id| {
                        self.vault_credential(id, Kind::KeyPassphrase, &login.endpoint)
                            .map(Password::from_secret)
                    })
                    .transpose()?;
                login.auth = Authentication::ResolvedKey(Box::new(key.clone()), passphrase);
                if !keys.records.iter().any(|snapshot| snapshot.id == key.id) {
                    keys.records.push(key.clone());
                }
                if !explicit && keys.saved.is_none() {
                    keys.saved = saved.cloned();
                }
            } else {
                self.resolve_ssh_login(login)?;
            }
        }
        Ok(keys)
    }

    pub(super) fn refresh_registered_key_attempt(
        &mut self,
        owner: window::Id,
        target: &SshTarget,
        profile: TerminalProfile,
    ) -> Option<Task<WorkspaceMessage>> {
        let pending = self.pending_ssh_mut(owner)?;
        if pending.key_snapshots.is_current() {
            return None;
        }
        self.refresh_ssh_key_choices();
        Some(self.reopen_ssh_form(
            owner,
            target.clone(),
            profile,
            "The saved connection or registered SSH key changed. Review the login and connect again.".into(),
        ))
    }
}
