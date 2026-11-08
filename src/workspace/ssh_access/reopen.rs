//! Recently Closed launches use the same SSH authentication and transport as Quick Connect.

use std::io;
use std::path::PathBuf;

use iced::{Task, window};
use kortty_terminal::TerminalProfile;

use crate::connection_store::{Connection, Store};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::SshDialog;
use crate::ssh_worker;

use super::{QuickLaunch, SshTarget, WorkspaceMessage};
use crate::workspace::{
    PaneSession, Workspace,
    recently_closed::{ReopenToken, SshOutcome},
};

/// The route of the tab's original SSH session. It contains no authentication material.
#[derive(Clone, Debug)]
pub(in crate::workspace) struct ReopenSshOrigin {
    target: SshTarget,
    profile: TerminalProfile,
    font: Option<u8>,
}

impl ReopenSshOrigin {
    pub(in crate::workspace) fn from_pane(pane: &PaneSession) -> Option<Self> {
        Some(Self {
            target: pane.ssh.as_ref()?.clone(),
            profile: pane.frame.screen.profile,
            font: pane.font_size_override(),
        })
    }

    pub(in crate::workspace) fn saved_id(&self) -> Option<u64> {
        self.target.saved_id()
    }

    pub(in crate::workspace) fn label(&self) -> String {
        self.target.label()
    }
}

pub(super) struct ReopenLaunch {
    pub(super) token: ReopenToken,
    pub(super) form_revision: u64,
    pub(super) saved: Option<Box<SavedSshProfile>>,
}

impl ReopenLaunch {
    pub(super) fn form_key(&self) -> (ReopenToken, u64) {
        (self.token, self.form_revision)
    }
}

pub(in crate::workspace) struct SavedSshProfile {
    path: PathBuf,
    pub(super) connection: Connection,
}

impl SavedSshProfile {
    pub(super) fn from_profile(id: u64) -> io::Result<Option<Self>> {
        Self::load(crate::profile::connections_path()?, id)
    }

    pub(super) fn from_project(target: &SshTarget) -> io::Result<Option<Self>> {
        let path = crate::profile::connections_path()?;
        let store = Store::open(path.clone())?;
        Ok(Self::project_connection(target, &store)
            .cloned()
            .map(|connection| Self { path, connection }))
    }

    fn project_connection<'a>(target: &SshTarget, store: &'a Store) -> Option<&'a Connection> {
        let mut matches = store.connections().iter().filter(|connection| {
            target.saved_id().is_none_or(|id| connection.id == id) && target.matches(connection)
        });
        let connection = matches.next()?;
        (matches.next().is_none() && !connection.incognito).then_some(connection)
    }

    pub(super) fn load(path: PathBuf, id: u64) -> io::Result<Option<Self>> {
        let store = Store::open(path.clone())?;
        Ok(store
            .get(id)
            .cloned()
            .map(|connection| Self { path, connection }))
    }

    pub(super) fn current(&self) -> Option<Connection> {
        Store::open(self.path.clone())
            .ok()?
            .get(self.connection.id)
            .cloned()
    }

    pub(in crate::workspace) fn is_current(&self) -> bool {
        self.current().as_ref() == Some(&self.connection)
    }

    pub(super) fn form(&self, profile: TerminalProfile, floor: IsolationLevel) -> SshDialog {
        let floor = self.connection.isolation.max(floor);
        SshDialog::from_saved(profile, &self.connection)
            .with_isolation(floor, floor)
            .with_fixed_route()
    }
}

impl Workspace {
    pub(super) fn reopen_ssh_token(&self, owner: window::Id) -> Option<ReopenToken> {
        self.windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .reopen_token()
    }

    pub(super) fn reopen_ssh_form_key(&self, owner: window::Id) -> Option<(ReopenToken, u64)> {
        self.windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .reopen_form_key()
    }

    pub(in crate::workspace) fn discard_reopen_ssh(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
    ) {
        if self.reopen_ssh_token(owner) == Some(token)
            && let Some(window) = self.windows.get_mut(&owner)
        {
            window.pending_ssh = None;
        }
    }

    pub(super) fn fail_pending_reopen_ssh(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        let Some(token) = self.reopen_ssh_token(owner) else {
            return Task::none();
        };
        self.discard_reopen_ssh(owner, token);
        self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable)
    }

    pub(in crate::workspace) fn reopen_ssh(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        origin: &ReopenSshOrigin,
    ) -> Task<WorkspaceMessage> {
        if let Some(id) = origin.saved_id() {
            match SavedSshProfile::from_profile(id) {
                Ok(Some(connection)) => {
                    return self.launch_saved_reopen_ssh(
                        owner,
                        token,
                        connection,
                        origin.font,
                        true,
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    self.notice(
                        owner,
                        format!("Could not read the saved SSH connection ({error})"),
                    );
                    return self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable);
                }
            }
        }
        let isolation = origin.target.isolation;
        if let Some(failure) = self.reopen_ssh_sandbox_failure(owner, token, isolation) {
            return failure;
        }
        let floor = isolation.max(self.clipboard.policy.minimum_isolation);
        let form = origin
            .target
            .form(origin.profile)
            .with_isolation(floor, floor)
            .with_fixed_route();
        let form_revision = self.allocate();
        self.launch_quick_ssh(
            owner,
            form,
            QuickLaunch {
                font: origin.font,
                ..Default::default()
            },
            Some(ReopenLaunch {
                token,
                form_revision,
                saved: None,
            }),
            true,
        )
    }

    pub(in crate::workspace) fn reopen_saved_ssh(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        id: u64,
    ) -> Task<WorkspaceMessage> {
        let Ok(Some(connection)) = SavedSshProfile::from_profile(id) else {
            self.notice(owner, "The saved SSH connection is unavailable.".into());
            return self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable);
        };
        self.launch_saved_reopen_ssh(owner, token, connection, None, true)
    }

    pub(super) fn launch_saved_reopen_ssh(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        saved: SavedSshProfile,
        font: Option<u8>,
        automatic: bool,
    ) -> Task<WorkspaceMessage> {
        let connection = &saved.connection;
        let Some(profile) = connection.profile() else {
            self.notice(
                owner,
                "The saved SSH connection uses an unsupported terminal profile.".into(),
            );
            return self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable);
        };
        if let Some(failure) = self.reopen_ssh_sandbox_failure(owner, token, connection.isolation) {
            return failure;
        }
        let minimum = self.clipboard.policy.minimum_isolation;
        let form = saved.form(profile, minimum);
        let form_revision = self.allocate();
        self.launch_quick_ssh(
            owner,
            form,
            QuickLaunch {
                saved: Some(connection.id),
                incognito: connection.incognito,
                font,
                ..Default::default()
            },
            Some(ReopenLaunch {
                token,
                form_revision,
                saved: Some(Box::new(saved)),
            }),
            automatic,
        )
    }

    fn reopen_ssh_sandbox_failure(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        isolation: IsolationLevel,
    ) -> Option<Task<WorkspaceMessage>> {
        if isolation != IsolationLevel::Sandbox || ssh_worker::sandbox_available() {
            return None;
        }
        self.notice(owner, "SSH sandbox is unavailable on this system.".into());
        Some(self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable))
    }

    /// A saved target may have changed while waiting for a password or host-key review.
    /// Drop the old request and show a fresh form before using any credentials.
    pub(super) fn refresh_reopen_ssh_config(
        &mut self,
        owner: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        let pending = self.windows.get(&owner)?.pending_ssh.as_ref()?;
        let saved = pending.reopen.as_ref()?.saved.as_deref()?;
        let current = saved.current();
        self.apply_reopen_ssh_config(owner, current.as_ref())
    }

    pub(super) fn apply_reopen_ssh_config(
        &mut self,
        owner: window::Id,
        current: Option<&Connection>,
    ) -> Option<Task<WorkspaceMessage>> {
        let pending = self.windows.get(&owner)?.pending_ssh.as_ref()?;
        let reopen = pending.reopen.as_ref()?;
        let expected = reopen.saved.as_deref()?;
        if current == Some(&expected.connection) {
            return None;
        }
        let (token, font) = (reopen.token, pending.font);
        let current = current.cloned().map(|connection| SavedSshProfile {
            path: expected.path.clone(),
            connection,
        });
        self.discard_reopen_ssh(owner, token);
        Some(if let Some(saved) = current {
            let task = self.launch_saved_reopen_ssh(owner, token, saved, font, false);
            if self.reopen_ssh_token(owner) == Some(token) {
                self.notice(
                    owner,
                    "The saved SSH connection changed. Review it and connect again.".into(),
                );
            }
            task
        } else {
            self.notice(owner, "The saved SSH connection is unavailable.".into());
            self.finish_reopen_ssh(owner, token, SshOutcome::Unavailable)
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn project_reconnect_matches_the_current_saved_route_and_refuses_ambiguity() {
        let scratch = crate::ssh_test_fixture::TempDirectory::new("project-reconnect-route");
        let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
        let mut connection: Connection = serde_json::from_value(serde_json::json!({
            "id": 0,
            "name": "Project SSH",
            "tag": "",
            "login": {"endpoint": {"host": "project.example", "port": 22, "username": "operator"}, "auth": "prompt"},
            "jump": null,
            "emulation": "xterm-256color",
            "tunnels": [],
        })).unwrap();
        let first = store.save(connection.clone()).unwrap();
        let mut target = SshTarget::from_imported_route(
            connection.login.endpoint.clone(),
            None,
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            SavedSshProfile::project_connection(&target, &store)
                .unwrap()
                .id,
            first
        );

        connection.name = "Another saved SSH".into();
        let second = store.save(connection.clone()).unwrap();
        assert!(SavedSshProfile::project_connection(&target, &store).is_none());
        target.saved = Some(first);
        assert_eq!(
            SavedSshProfile::project_connection(&target, &store)
                .unwrap()
                .id,
            first
        );
        connection.id = first;
        connection.login.endpoint.port = 2222;
        store.save(connection.clone()).unwrap();
        assert!(SavedSshProfile::project_connection(&target, &store).is_none());

        target.saved = Some(second);
        connection.id = second;
        connection.login.endpoint.port = 22;
        connection.incognito = true;
        store.save(connection).unwrap();
        assert!(SavedSshProfile::project_connection(&target, &store).is_none());
    }
}
