use kortty_control::{ErrorCode, Fault};

use super::*;

#[derive(Clone, Copy)]
pub(in crate::workspace) struct CreateOptions {
    pub(in crate::workspace) isolation: Option<IsolationLevel>,
    pub(in crate::workspace) incognito: bool,
}

pub(super) struct ControlLaunch {
    pub(super) options: CreateOptions,
    pub(super) floor: IsolationLevel,
    pub(super) saved: Option<ControlSaved>,
    pub(super) vault: Option<window::Id>,
}

pub(super) struct ControlSaved {
    store_path: PathBuf,
    connection: Box<Connection>,
}

impl ControlSaved {
    pub(super) fn current(&self) -> Option<(Connection, Option<String>)> {
        let store = Store::open(self.store_path.clone()).ok()?;
        let current = store.get(self.connection.id)?;
        (*current == *self.connection).then(|| {
            (
                current.clone(),
                store
                    .group_of(current.id)
                    .and_then(crate::profile::normalize_tab_group),
            )
        })
    }
}

impl Workspace {
    pub(in crate::workspace) fn create_control_saved_ssh(
        &mut self,
        owner: window::Id,
        connection: Connection,
        store_path: PathBuf,
        options: CreateOptions,
    ) -> Result<(Option<u64>, Task<WorkspaceMessage>), Fault> {
        self.control_saved_server(&connection)?;
        let floor = options
            .isolation
            .unwrap_or(connection.isolation)
            .max(self.clipboard.policy.minimum_isolation);
        self.allow_control_ssh(floor, options.incognito || connection.incognito)?;
        let launch = ControlLaunch {
            options,
            floor,
            saved: Some(ControlSaved {
                store_path,
                connection: Box::new(connection.clone()),
            }),
            vault: None,
        };
        let focus = self.stage_saved_ssh(owner, connection, false, Some(launch));
        self.start_control_ssh(owner, focus)
    }

    pub(in crate::workspace) fn create_control_teamwork_ssh(
        &mut self,
        owner: window::Id,
        selection: TeamworkSelection,
        options: CreateOptions,
    ) -> Result<(Option<u64>, Task<WorkspaceMessage>), Fault> {
        self.control_saved_server(&selection.connection)?;
        let floor = selection
            .connection
            .isolation
            .max(options.isolation.unwrap_or_default())
            .max(self.clipboard.policy.minimum_isolation);
        self.allow_control_ssh(floor, options.incognito || selection.connection.incognito)?;
        let focus = self.open_teamwork_ssh(owner, selection, false, true, Some(options));
        self.start_control_ssh(owner, focus)
    }

    fn allow_control_ssh(&self, isolation: IsolationLevel, incognito: bool) -> Result<(), Fault> {
        if self.exiting
            || !self.clipboard.policy.control_allowed()
            || self.clipboard.policy.problem.is_some()
        {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks the connection",
            ));
        }
        if incognito && !self.clipboard.policy.incognito_allowed {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks incognito sessions",
            ));
        }
        if isolation == IsolationLevel::Sandbox && !ssh_worker::sandbox_available() {
            return Err(Fault::new(
                ErrorCode::Unsupported,
                "SSH sandbox is unavailable on this system",
            ));
        }
        Ok(())
    }

    pub(super) fn start_control_ssh(
        &mut self,
        owner: window::Id,
        focus: Task<WorkspaceMessage>,
    ) -> Result<(Option<u64>, Task<WorkspaceMessage>), Fault> {
        if self
            .teamwork_unlock
            .as_ref()
            .is_some_and(|launch| launch.owner == owner && launch.control.is_some())
        {
            return Ok((None, focus));
        }
        let Some(pending) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_ssh.as_ref())
            .filter(|pending| pending.control.is_some())
        else {
            return Err(Fault::new(
                ErrorCode::Unavailable,
                "Could not prepare the SSH login",
            ));
        };
        if !matches!(&pending.stage, Stage::Form(form) if form.can_connect()) {
            return Ok((None, focus));
        }
        let task = self.connect_ssh(owner);
        let state = &self.windows[&owner];
        let Some(pending) = state
            .pending_ssh
            .as_ref()
            .filter(|pending| pending.control.is_some())
        else {
            return Err(Fault::new(
                ErrorCode::Unavailable,
                "Could not prepare the SSH login",
            ));
        };
        if let Some(anchor) = pending.replace {
            let tab = state
                .pane_location(anchor)
                .map(|(index, _)| state.tabs[index].id())
                .ok_or_else(|| {
                    Fault::new(ErrorCode::Unavailable, "The new SSH tab is unavailable")
                })?;
            Ok((Some(tab), task))
        } else {
            Ok((None, Task::batch([focus, task])))
        }
    }

    pub(super) fn attach_control_ssh_tab(&mut self, owner: window::Id) -> io::Result<()> {
        let Some(state) = self.windows.get(&owner) else {
            return Ok(());
        };
        let Some(PendingSsh {
            control: Some(_),
            replace: None,
            incognito,
            stage:
                Stage::Dialing {
                    target,
                    profile,
                    grid,
                    command_timestamps,
                    connection_group,
                    ..
                },
            ..
        }) = state.pending_ssh.as_ref()
        else {
            return Ok(());
        };
        let (target, profile, grid, incognito, command_timestamps, group) = (
            target.clone(),
            *profile,
            *grid,
            *incognito,
            *command_timestamps,
            connection_group.clone(),
        );
        let anchor = self.store_disconnected_ssh_in_grid(
            target,
            profile,
            self.window_settings.terminal_font,
            grid,
        )?;
        let pane = self.sessions.get_mut(&anchor).expect("prepared SSH pane");
        pane.control_ssh_provisional = true;
        pane.inherits_font_size = true;
        let mut pending = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.pending_ssh.take())
            .expect("prepared Control API login");
        pending.replace = Some(anchor);
        self.add_configured_tab(owner, anchor, incognito, command_timestamps, group);
        self.windows
            .get_mut(&owner)
            .expect("open window")
            .pending_ssh = Some(pending);
        Ok(())
    }

    pub(super) fn control_ssh_stale_reason(&self, owner: window::Id) -> Option<&'static str> {
        let state = self.windows.get(&owner)?;
        let pending = state.pending_ssh.as_ref()?;
        let launch = pending.control.as_ref()?;
        if pending
            .replace
            .is_some_and(|anchor| state.pane_location(anchor).is_none())
        {
            return Some("The new SSH tab is no longer in this window.");
        }
        if !self.clipboard.policy.control_allowed() {
            return Some("Managed policy blocks this Control API connection.");
        }
        if pending.incognito && !self.clipboard.policy.incognito_allowed {
            return Some("Managed policy blocks this incognito connection.");
        }
        if launch
            .saved
            .as_ref()
            .is_some_and(|saved| saved.current().is_none())
        {
            return Some(
                "The saved connection changed. Open it again to review the current settings.",
            );
        }
        None
    }

    pub(super) fn replace_control_ssh_session(&mut self, anchor: SessionId, connected: SessionId) {
        self.revoke_terminal_ai_source(anchor);
        let alias = self.sessions[&anchor].coding_agent.alias.clone();
        let mut pane = self.sessions.remove(&connected).expect("new SSH pane");
        // Rebind the stream subscription while retaining the already published pane ID.
        pane.stream_generation = connected.0;
        pane.coding_agent.alias = alias;
        let previous = self
            .sessions
            .insert(anchor, pane)
            .expect("attached SSH pane");
        self.track_session_shutdown(previous.session.stop());
    }

    pub(in crate::workspace) fn close_control_ssh_anchor(&mut self, anchor: SessionId) {
        if !self
            .sessions
            .get(&anchor)
            .is_some_and(|pane| pane.control_ssh_provisional)
        {
            return;
        }
        let Some((owner, index, pane)) = self.windows.iter().find_map(|(&owner, window)| {
            window
                .pane_location(anchor)
                .map(|(index, pane)| (owner, index, pane))
        }) else {
            self.stop_session(anchor);
            return;
        };
        let tab = self.windows[&owner].tabs[index]
            .terminal()
            .expect("SSH tab");
        if tab.grid.len() == 1 {
            self.close_tab(owner, tab.id);
        } else if let Some(window) = self.windows.get_mut(&owner)
            && let Some(tab) = window.tabs[index].terminal_mut()
            && tab.close_pane(pane).is_some()
        {
            self.stop_session(anchor);
            self.session_restore.changed(false);
        }
    }

    pub(super) fn unlock_for_control_ssh(
        &mut self,
        owner: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        self.windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .control
            .as_ref()?;
        if self.vault_id.is_some() || !self.vault_unlock_available() {
            return None;
        }
        let task = self.show_vault_unlock(owner);
        let vault = self.vault_unlock_for_owner(owner)?;
        self.pending_ssh_mut(owner)?.control.as_mut()?.vault = Some(vault);
        Some(task)
    }

    pub(in crate::workspace) fn resume_control_ssh_after_vault(
        &mut self,
        closed: window::Id,
    ) -> Task<WorkspaceMessage> {
        let Some(owner) = self.windows.iter_mut().find_map(|(&owner, window)| {
            let launch = window.pending_ssh.as_mut()?.control.as_mut()?;
            launch.vault.take_if(|id| *id == closed).map(|_| owner)
        }) else {
            return Task::none();
        };
        if !self.exiting
            && self
                .vault
                .as_ref()
                .is_some_and(crate::vault_window::VaultWindow::is_unlocked)
        {
            self.connect_ssh(owner)
        } else {
            self.cancel_ssh_login(owner)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_store::{Auth, SavedLogin};

    fn save(store: &mut Store, auth: Auth) -> Connection {
        let id = store
            .save(Connection {
                id: 0,
                name: "Control fixture".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: LoginTarget {
                        host: "fixture.invalid".into(),
                        port: 2222,
                        username: "fixture".into(),
                    },
                    auth,
                    keyboard_interactive: false,
                },
                jump: None,
                emulation: TerminalProfile::Xterm.term_name().into(),
                tunnels: Vec::new(),
                incognito: false,
                isolation: IsolationLevel::Process,
                startup: Default::default(),
                logging: Default::default(),
                terminal_effect: None,
                command_timestamps: None,
                tab_color: None,
                paste_warning: None,
                window_geometry: None,
                terminal: Default::default(),
            })
            .unwrap();
        store.get(id).unwrap().clone()
    }

    #[test]
    fn control_tab_create_defers_auth_and_refuses_a_changed_saved_route() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let mut connection = save(&mut store, Auth::Prompt);
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let options = CreateOptions {
            isolation: Some(IsolationLevel::None),
            incognito: true,
        };
        let (tab, _) = workspace
            .create_control_saved_ssh(owner, connection.clone(), path, options)
            .unwrap();
        assert!(tab.is_none());
        assert!(workspace.windows[&owner].tabs.is_empty());
        let pending = workspace.pending_ssh_mut(owner).unwrap();
        assert!(pending.incognito);
        assert_eq!(
            pending.control.as_ref().unwrap().floor,
            IsolationLevel::None
        );
        let Stage::Form(form) = &mut pending.stage else {
            panic!("expected the SSH form");
        };
        form.update(ssh_dialog::Message::Target(ssh_dialog::LoginMessage::Host(
            "other.invalid".into(),
        )));
        assert_eq!(form.target_endpoint().unwrap().host, "fixture.invalid");
        connection.login.endpoint.host = "changed.invalid".into();
        store.save(connection).unwrap();
        let _ = workspace.connect_ssh(owner);
        assert!(workspace.windows[&owner].pending_ssh.is_none());
        assert!(workspace.windows[&owner].tabs.is_empty());
    }

    #[test]
    fn control_tab_create_attaches_an_incognito_tab_before_the_dial() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let mut connection = save(
            &mut store,
            Auth::KeyFile {
                path: profile.0.join("fixture-key"),
                passphrase: None,
            },
        );
        connection.terminal.terminal_columns = Some(132);
        store.save(connection.clone()).unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_columns = Some(99);
        workspace.window_settings.terminal_rows = Some(33);
        let (owner, _) = workspace.open_window(false);
        let options = CreateOptions {
            isolation: None,
            incognito: true,
        };
        let (created, dial) = workspace
            .create_control_saved_ssh(owner, connection, path, options)
            .unwrap();
        let state = &workspace.windows[&owner];
        let tab = state.tab().unwrap();
        let anchor = tab.focused_session().unwrap();
        assert_eq!(created, Some(tab.id));
        assert!(tab.incognito && tab.reopen_origin.is_none());
        assert_eq!(
            workspace.sessions[&anchor].frame.status,
            Status::Disconnected
        );
        let pending = state.pending_ssh.as_ref().unwrap();
        assert_eq!(pending.replace, Some(anchor));
        let Stage::Dialing {
            target,
            profile,
            grid,
            ..
        } = &pending.stage
        else {
            panic!("expected a ready dial");
        };
        assert_eq!(target.isolation, IsolationLevel::Process);
        assert_eq!((grid.columns, grid.rows), (132, 33));
        assert_eq!(workspace.sessions[&anchor].grid, *grid);
        let (target, profile) = (target.clone(), *profile);
        // The task is intentionally never polled, so this check opens no network connection.
        drop(dial);
        let replacement = workspace
            .store_disconnected_ssh(target, profile, 14, Default::default())
            .unwrap();
        workspace.replace_control_ssh_session(anchor, replacement);
        assert_eq!(
            workspace.windows[&owner].tab().unwrap().focused_session(),
            Some(anchor)
        );
        assert_eq!(workspace.sessions[&anchor].stream_generation, replacement.0);
        assert!(!workspace.sessions.contains_key(&replacement));
    }
}
