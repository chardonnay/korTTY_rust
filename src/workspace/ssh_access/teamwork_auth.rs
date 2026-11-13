use crate::connection_store::teamwork::DefaultAuth;
use crate::ssh_dialog::{LoginMessage, LoginTarget, Message, SshDialog};
use crate::vault_window::Kind;

use super::{TeamworkSelection, Workspace};

pub(super) struct AuthSnapshot {
    preference: Option<DefaultAuth>,
    method: AppliedAuth,
}

#[derive(Eq, PartialEq)]
enum AppliedAuth {
    Prompt,
    Password(String),
    Key(u64),
    TemporaryKey,
}

impl AuthSnapshot {
    pub(super) fn choose(
        workspace: &Workspace,
        selected: &TeamworkSelection,
    ) -> (LoginTarget, Self) {
        let mut login = selected.connection.login.endpoint.clone();
        let preference = selected
            .default_auth_eligible
            .then(|| workspace.teamwork_auth_default().clone());
        let method = match &preference {
            Some(DefaultAuth::Password { credential }) => {
                match eligible_password(workspace, credential, &login) {
                    Some(target) => {
                        login = target;
                        AppliedAuth::Password(credential.clone())
                    }
                    None => AppliedAuth::Prompt,
                }
            }
            Some(DefaultAuth::Key { id, username })
                if crate::ssh_key_catalog::Catalog::open_profile()
                    .is_ok_and(|catalog| catalog.get(*id).is_some()) =>
            {
                set_username(&mut login, username);
                AppliedAuth::Key(*id)
            }
            Some(DefaultAuth::TemporaryKey { username }) => {
                set_username(&mut login, username);
                AppliedAuth::TemporaryKey
            }
            _ => AppliedAuth::Prompt,
        };
        (login, Self { preference, method })
    }

    pub(super) fn is_current(
        &self,
        workspace: &Workspace,
        selected: &TeamworkSelection,
        login: &LoginTarget,
    ) -> bool {
        if self
            .preference
            .as_ref()
            .is_some_and(|pref| pref != workspace.teamwork_auth_default())
        {
            return false;
        }
        if self.method == AppliedAuth::Prompt {
            return true;
        }
        let (current, snapshot) = Self::choose(workspace, selected);
        &current == login && snapshot.method == self.method
    }

    pub(super) fn apply_to(&self, form: &mut SshDialog, login: &LoginTarget) {
        match &self.method {
            AppliedAuth::Prompt => {}
            AppliedAuth::Password(credential) => form.update(Message::Target(
                LoginMessage::VaultPassword(credential.clone()),
            )),
            AppliedAuth::Key(id) => form.restore_explicit_key(false, login, *id),
            AppliedAuth::TemporaryKey => form.enable_teamwork_temporary_key(true),
        }
    }

    pub(super) fn enable_retry(&self, form: &mut SshDialog) {
        if self.method == AppliedAuth::TemporaryKey {
            form.enable_teamwork_temporary_key(false);
        }
    }

    pub(super) fn notice(&self) -> Option<String> {
        (self.method == AppliedAuth::Prompt && self.preference.as_ref().is_some_and(|preference| *preference != DefaultAuth::Prompt))
            .then(|| "The Teamwork authentication default is unavailable for this login. Enter authentication to connect.".into())
    }
}

fn eligible_password(workspace: &Workspace, id: &str, login: &LoginTarget) -> Option<LoginTarget> {
    let password = workspace
        .credential_choices(Kind::Password)
        .into_iter()
        .find(|choice| choice.id == id)?;
    let mut target = login.clone();
    set_username(&mut target, &password.username);
    target.validate().ok()?;
    workspace
        .scoped_credential_choices(Kind::Password, &target)
        .into_iter()
        .any(|choice| choice.id == id)
        .then_some(target)
}

fn set_username(login: &mut LoginTarget, username: &str) {
    let username = username.trim();
    if !username.is_empty() {
        login.username = username.to_owned();
    }
}
