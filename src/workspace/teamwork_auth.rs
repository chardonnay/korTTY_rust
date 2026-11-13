//! Workspace-wide local defaults for authenticating to shared SSH targets.

use iced::{Task, window};

use crate::connection_store::teamwork::{AuthSave, DefaultAuth};
use crate::connection_window::ConnectionWindow;
use crate::profile;
use crate::vault_window::Kind;

use super::{Message, Workspace};

#[derive(Default)]
pub(super) struct State {
    saved: DefaultAuth,
    load_problem: Option<String>,
    pending: Option<(window::Id, DefaultAuth)>,
}

impl Workspace {
    pub(super) fn load_teamwork_auth(&mut self) {
        if !self.teamwork_sync_allowed() {
            return;
        }
        match profile::root().and_then(|root| DefaultAuth::load(&root)) {
            Ok(saved) => self.teamwork_auth.saved = saved,
            Err(error) => {
                self.teamwork_auth.load_problem = Some(format!(
                    "Could not load Teamwork authentication settings ({error})."
                ));
            }
        }
    }

    pub(super) fn teamwork_auth_default(&self) -> &DefaultAuth {
        &self.teamwork_auth.saved
    }

    #[cfg(test)]
    pub(super) fn set_teamwork_auth_for_test(&mut self, value: DefaultAuth) {
        self.teamwork_auth.saved = value;
    }

    pub(super) fn initialize_teamwork_auth(&self, window: &mut ConnectionWindow) {
        if !self.teamwork_sync_allowed() {
            return;
        }
        let auth = &self.teamwork_auth;
        if let Some(problem) = &auth.load_problem {
            window.set_notice(problem.clone());
        } else {
            window.set_teamwork_auth(Some(auth.saved.clone()));
            window.set_teamwork_auth_saving(auth.pending.is_some());
        }
    }

    pub(super) fn save_teamwork_auth(&mut self, id: window::Id) -> Task<Message> {
        let auth = &self.teamwork_auth;
        if self.exiting
            || !self.teamwork_sync_allowed()
            || auth.load_problem.is_some()
            || auth.pending.is_some()
        {
            return Task::none();
        }
        let Some((before, candidate)) = self
            .connection_managers
            .get(&id)
            .and_then(|manager| manager.window.teamwork_auth_change())
        else {
            return Task::none();
        };
        if before != auth.saved {
            return Task::none();
        }
        if matches!(candidate, DefaultAuth::Key { .. }) {
            self.refresh_ssh_key_choices();
        }
        let available = match &candidate {
            DefaultAuth::Password { credential } => self
                .credential_choices(Kind::Password)
                .iter()
                .any(|choice| choice.id == *credential),
            DefaultAuth::Key { id, .. } => {
                self.ssh_key_choices.iter().any(|choice| choice.id == *id)
            }
            DefaultAuth::Prompt | DefaultAuth::TemporaryKey { .. } => true,
        };
        if !available {
            if let Some(manager) = self.connection_managers.get_mut(&id) {
                manager
                    .window
                    .set_notice("The selected local credential or SSH key is unavailable.".into());
            }
            return Task::none();
        }
        self.teamwork_auth.pending = Some((id, candidate.clone()));
        for manager in self.connection_managers.values_mut() {
            manager.window.set_teamwork_auth_saving(true);
        }
        Task::perform(
            async move {
                profile::root()
                    .and_then(|root| candidate.save(&root, &before))
                    .map_err(|error| error.to_string())
            },
            Message::TeamworkAuthSaved,
        )
    }

    pub(super) fn finish_teamwork_auth_save(&mut self, result: Result<AuthSave, String>) {
        let Some((id, candidate)) = self.teamwork_auth.pending.take() else {
            return;
        };
        let notice = match result {
            Ok(AuthSave::Saved) => {
                self.teamwork_auth.saved = candidate;
                "Teamwork authentication settings saved.".into()
            }
            Ok(AuthSave::Changed(current)) => {
                self.teamwork_auth.saved = current;
                "Teamwork authentication settings changed elsewhere. Discard the draft before editing again.".into()
            }
            Err(error) => format!("Could not save Teamwork authentication settings ({error})."),
        };
        let saved = self
            .teamwork_sync_allowed()
            .then(|| self.teamwork_auth.saved.clone());
        for (&manager_id, manager) in &mut self.connection_managers {
            manager.window.set_teamwork_auth_saving(false);
            manager.window.set_teamwork_auth(saved.clone());
            if manager_id == id {
                manager.window.set_notice(notice.clone());
            }
        }
    }
}
