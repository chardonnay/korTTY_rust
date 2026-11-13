use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Arc;
use std::time::Duration;

use iced::{Element, Size, Task, window};
use kortty_vault::Secret;

use crate::connection_store::{Connection, LocalProfile, SerialProfile, Store};
use crate::connection_window::{self, ConnectionWindow, CredentialChoice, ViewChoices};
use crate::profile;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_effects::Effect;
use crate::vault_window::{self, Kind, VaultWindow};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    Message, SessionId, SshTarget, Status, Workspace,
    chrome::{connection_export, connection_import},
    os_badge_manager,
    ssh_access::TeamworkSelection,
    window_lifecycle,
};

#[cfg(test)]
mod connection_manager_tests;
pub(super) mod connection_overview;
pub(super) mod teamwork_restore;
mod terminal_preferences;
pub(super) mod vault_modal;
pub(super) use terminal_preferences::SavedTerminalPreferences;

#[derive(Default)]
pub(super) struct ConnectionUsage {
    active: bool,
    pending: VecDeque<(u64, UsedConnection, u64)>,
    store_revision: u64,
}

pub(super) struct ConnectionManager {
    pub(super) owner: window::Id,
    pub(super) window: ConnectionWindow,
    refresh_pending: bool,
    closing: bool,
    launch: Option<ConnectionLaunch>,
    editor: Option<window::Id>,
    editor_closing: bool,
}

impl ConnectionManager {
    pub(super) fn new(owner: window::Id, window: ConnectionWindow) -> Self {
        Self {
            owner,
            window,
            refresh_pending: false,
            closing: false,
            launch: None,
            editor: None,
            editor_closing: false,
        }
    }
}

enum ConnectionLaunch {
    Ssh(Box<Connection>, bool),
    SshInNewWindow(Box<Connection>),
    Teamwork(TeamworkSelection, bool),
    Serial(SerialProfile),
    Local(Box<LocalProfile>),
}

enum UsedConnection {
    Ssh(SshTarget),
    Serial(SerialProfile),
    Local(Box<LocalProfile>),
}

impl Workspace {
    pub(super) fn connection_store_revision(&self) -> u64 {
        self.connection_usage.store_revision
    }

    pub(super) fn show_connections(&mut self, source: window::Id) -> Task<Message> {
        if self.exiting || !self.windows.contains_key(&source) {
            return Task::none();
        }
        if let Some(id) = self.connection_manager_for_owner(source) {
            return self.focus_connection_manager(id);
        }
        if let Some((id, _)) = self.owner_dialog(source) {
            return window::gain_focus(id);
        }
        self.refresh_ssh_key_choices();
        match profile::connections_path()
            .map_err(|error| error.to_string())
            .and_then(ConnectionWindow::open)
        {
            Ok(connections) => self.install_connection_manager(source, connections).1,
            Err(error) => {
                self.notice(
                    source,
                    format!("Could not open the connection manager ({error})"),
                );
                Task::none()
            }
        }
    }

    fn install_connection_manager(
        &mut self,
        owner: window::Id,
        mut connections: ConnectionWindow,
    ) -> (window::Id, Task<Message>) {
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::ConnectionManager,
            window::Settings {
                size: Size::new(1240.0, 760.0),
                min_size: Some(Size::new(940.0, 520.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        connections.set_server_access(self.clipboard.policy.server_access().clone());
        self.initialize_teamwork_manager(&mut connections);
        self.initialize_teamwork_auth(&mut connections);
        self.connection_managers
            .insert(id, ConnectionManager::new(owner, connections));
        self.refresh_connection_counts();
        (id, opened)
    }

    pub(super) fn connection_manager_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.connection_managers
            .iter()
            .find_map(|(&id, manager)| (manager.owner == owner).then_some(id))
    }

    pub(super) fn focus_connection_manager(&self, id: window::Id) -> Task<Message> {
        if self
            .connection_managers
            .get(&id)
            .is_none_or(|manager| manager.closing)
        {
            return Task::none();
        }
        window::gain_focus(
            self.connection_manager_modal(id)
                .map_or(id, |(child, _)| child),
        )
    }

    pub(super) fn connection_window_closed(&mut self, id: window::Id) -> Option<Task<Message>> {
        if let Some(owner) = self.connection_manager_open_after_close.remove(&id) {
            return Some(self.show_connections(owner));
        }
        if let Some(owner) = self.connection_editor_manager(id) {
            let manager = self
                .connection_managers
                .get_mut(&owner)
                .expect("current manager");
            manager.editor = None;
            manager.editor_closing = false;
            return Some(if manager.closing || self.exiting {
                Task::none()
            } else {
                window::gain_focus(owner)
            });
        }
        let manager = self.connection_managers.remove(&id)?;
        if self.quit_waiting_profile == Some(id) || self.should_exit_after_window_close() {
            self.quit_waiting_profile = None;
            return Some(self.quit());
        }
        let owner = manager.owner;
        if self.exiting || !self.windows.contains_key(&owner) {
            return Some(Task::none());
        }
        Some(match manager.launch {
            Some(ConnectionLaunch::Ssh(connection, sftp)) => {
                self.show_saved_ssh(owner, *connection, sftp)
            }
            Some(ConnectionLaunch::SshInNewWindow(connection)) => {
                self.show_saved_ssh_in_new_window(owner, *connection)
            }
            Some(ConnectionLaunch::Teamwork(selection, sftp)) => {
                self.show_teamwork_ssh(owner, selection, sftp)
            }
            Some(ConnectionLaunch::Serial(profile)) => self.open_saved_serial(owner, profile),
            Some(ConnectionLaunch::Local(profile)) => self.open_saved_local(owner, *profile),
            None => window::gain_focus(owner),
        })
    }

    pub(super) fn connection_editor_manager(&self, id: window::Id) -> Option<window::Id> {
        self.connection_managers
            .iter()
            .find_map(|(&manager_id, manager)| (manager.editor == Some(id)).then_some(manager_id))
    }

    pub(super) fn connection_editor_title(&self, id: window::Id) -> Option<&'static str> {
        let manager = self.connection_editor_manager(id)?;
        Some(
            self.connection_managers[&manager]
                .window
                .editor_title()
                .unwrap_or("Connection Editor"),
        )
    }

    fn sync_connection_editor(&mut self, id: window::Id) -> Task<Message> {
        let Some(manager) = self
            .connection_managers
            .get_mut(&id)
            .filter(|manager| !manager.closing)
        else {
            return Task::none();
        };
        let editing = manager.window.editor_title().is_some();
        if let Some(editor) = manager.editor {
            if !editing && !manager.editor_closing {
                manager.editor_closing = true;
                return self.close_placed_dialog(editor);
            }
            return Task::none();
        }
        if !editing {
            return Task::none();
        }
        let (editor, open) = self.open_owned_placed_dialog(
            id,
            DialogKind::ConnectionEditor,
            window::Settings {
                size: Size::new(860.0, 690.0),
                min_size: Some(Size::new(640.0, 430.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.connection_managers
            .get_mut(&id)
            .expect("current manager")
            .editor = Some(editor);
        open
    }

    pub(super) fn close_connection_editor(&mut self, editor: window::Id) -> Option<Task<Message>> {
        let id = self.connection_editor_manager(editor)?;
        let manager = &self.connection_managers[&id];
        if manager.closing || manager.editor_closing || manager.window.store_busy() {
            return Some(Task::none());
        }
        self.cancel_profile_quit(id);
        self.connection_managers
            .get_mut(&id)
            .expect("current manager")
            .window
            .discard_editor();
        Some(self.sync_connection_editor(id))
    }

    pub(super) fn refresh_connection_counts(&mut self) {
        if self.connection_managers.is_empty() {
            return;
        }
        let mut counts = BTreeMap::new();
        for tab in self
            .windows
            .values()
            .flat_map(|window| window.terminal_tabs())
            .filter(|tab| !tab.incognito)
        {
            let connections: BTreeSet<_> = tab
                .grid
                .iter()
                .filter_map(|(_, id)| {
                    let pane = self.sessions.get(id)?;
                    pane.ssh
                        .as_ref()
                        .and_then(SshTarget::saved_id)
                        .or_else(|| pane.saved_serial.as_ref().map(|profile| profile.id))
                        .or_else(|| pane.saved_local.as_ref().map(|profile| profile.id))
                })
                .collect();
            for id in connections {
                *counts.entry(id).or_default() += 1;
            }
        }
        for manager in self.connection_managers.values_mut() {
            manager.window.set_open_counts(counts.clone());
        }
    }

    pub(super) fn show_vault(&mut self, source: window::Id) -> Task<Message> {
        if let Some(id) = self.vault_id {
            return window::gain_focus(self.environment_for_vault(id).unwrap_or(id));
        }
        if self.vault.is_none() {
            match profile::vault_path()
                .map_err(|error| error.to_string())
                .and_then(VaultWindow::open)
            {
                Ok(vault) => {
                    self.vault = Some(vault);
                    self.vault_change_revision = Arc::new(());
                }
                Err(error) => {
                    let target = self
                        .windows
                        .contains_key(&source)
                        .then_some(source)
                        .or_else(|| {
                            self.connection_managers
                                .get(&source)
                                .map(|manager| manager.owner)
                        });
                    if let Some(target) = target {
                        self.notice(target, format!("Could not open the vault ({error})"));
                        return window::gain_focus(target);
                    }
                    return Task::none();
                }
            }
        }
        if let Some(vault) = &mut self.vault
            && let Ok(store) = profile::connections_path().and_then(Store::open)
        {
            vault.set_sudo_groups(store.groups());
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::CredentialVault,
            window::Settings {
                size: Size::new(930.0, 585.0),
                min_size: Some(Size::new(690.0, 410.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.vault_id = Some(id);
        opened.map(|_| Message::Noop)
    }

    pub(super) fn load_vault_status(&mut self) {
        self.vault = profile::vault_path()
            .ok()
            .and_then(|path| VaultWindow::open(path).ok())
            .filter(VaultWindow::can_unlock_existing);
        self.vault_change_revision = Arc::new(());
    }

    pub(in crate::workspace) fn vault_change_revision(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.vault_change_revision)
    }

    pub(super) fn vault_unlock_available(&self) -> bool {
        self.vault
            .as_ref()
            .is_some_and(VaultWindow::can_unlock_existing)
    }

    pub(super) fn show_vault_unlock(&mut self, owner: window::Id) -> Task<Message> {
        if !self.windows.contains_key(&owner) || !self.vault_unlock_available() {
            return Task::none();
        }
        if let Some(id) = self.vault_id {
            return window::gain_focus(id);
        }
        let Some(vault) = self.vault.as_mut() else {
            return Task::none();
        };
        vault.open_unlock_prompt(owner);
        let (id, opened) = window::open(window::Settings {
            size: Size::new(420.0, 190.0),
            min_size: Some(Size::new(380.0, 170.0)),
            position: window::Position::Centered,
            resizable: false,
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        self.vault_id = Some(id);
        opened.map(move |_| Message::Vault(id, vault_window::Message::FocusUnlock))
    }

    pub(super) fn current_vault_unlock(&self) -> Option<window::Id> {
        let id = self.vault_id?;
        self.vault_unlock_prompt_owner(id)?;
        Some(id)
    }

    pub(super) fn vault_unlock_modal(&self, surface: window::Id) -> Option<window::Id> {
        self.current_vault_unlock().filter(|&id| id != surface)
    }

    pub(super) fn vault_unlock_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        let id = self.vault_id?;
        (self.vault_unlock_prompt_owner(id) == Some(owner)).then_some(id)
    }

    pub(super) fn vault_unlock_prompt_owner(&self, id: window::Id) -> Option<window::Id> {
        if self.vault_id != Some(id) {
            return None;
        }
        self.vault.as_ref()?.unlock_prompt_owner()
    }

    pub(super) fn is_profile_tool(&self, id: window::Id) -> bool {
        self.connection_managers.contains_key(&id)
            || self.connection_editor_manager(id).is_some()
            || self.teamwork_restore.contains_key(&id)
            || self.vault_id == Some(id)
    }

    pub(super) fn connection_manager_modal(
        &self,
        owner: window::Id,
    ) -> Option<(window::Id, Message)> {
        if !self.connection_managers.contains_key(&owner) {
            return None;
        }
        self.connection_managers[&owner]
            .editor
            .map(|id| (id, Message::FocusConnectionEditor(id)))
            .or_else(|| {
                self.connection_overview_for_owner(owner)
                    .map(|id| (id, connection_overview::focus(id)))
            })
            .or_else(|| {
                self.teamwork_restore_for_owner(owner).map(|id| {
                    (
                        id,
                        Message::TeamworkRestore(id, teamwork_restore::Event::Focus),
                    )
                })
            })
            .or_else(|| {
                self.connection_export_for_owner(owner)
                    .map(|id| (id, connection_export::focus(id)))
            })
            .or_else(|| {
                self.connection_import_for_owner(owner).map(|id| {
                    (
                        id,
                        connection_import::event(id, connection_import::Event::Focus),
                    )
                })
            })
            .or_else(|| {
                self.os_badge_manager_for_owner(owner).map(|id| {
                    (
                        id,
                        os_badge_manager::message(id, os_badge_manager::Event::Focus),
                    )
                })
            })
    }

    pub(super) fn profile_tool_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let editor_owner = self.connection_editor_manager(id);
        if let Some(manager) = self.connection_managers.get(&editor_owner.unwrap_or(id)) {
            let effects = self.terminal_effects_enabled().then(|| {
                Effect::ALL
                    .into_iter()
                    .filter(|&effect| self.allowed_terminal_effect(effect))
                    .collect::<Vec<_>>()
            });
            let mut passwords = self.credential_choices(Kind::Password);
            passwords.extend(self.credential_choices(Kind::PasswordCommand));
            let passphrases = self.credential_choices(Kind::KeyPassphrase);
            let ai_profiles = self.ai_configuration().map_or_else(Vec::new, |config| {
                config
                    .terminal_chat_profiles()
                    .filter_map(|(id, name, ..)| {
                        Some((
                            config.legacy_policy_alias(id),
                            config.profile_reference(id)?,
                            name.to_owned(),
                        ))
                    })
                    .collect()
            });
            let ai_skills = self
                .ai_configuration()
                .map_or_else(Vec::new, |config| config.connection_skill_choices());
            let ai_agent_designs = self
                .window_settings
                .ai_agent_designs
                .connection_choices(self.window_settings.ai_agent_design);
            let choices = ViewChoices {
                ai_skills: &ai_skills,
                ai_agent_designs: &ai_agent_designs,
                highlighting: self.highlight.connection_choices(),
                themes: &self.window_settings.themes,
                journal_max_parts: self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_max_log_parts(),
                passwords: &passwords,
                passphrases: &passphrases,
                keys: &self.ssh_key_choices,
                ai_profiles: &ai_profiles,
                incognito_allowed: self.clipboard.policy.incognito_allowed,
                serial_allowed: self.serial_profiles_allowed(),
                terminal_effects: effects.as_deref(),
                default_paste_warning: self.window_settings.paste_warning,
                minimum_paste_warning: self.clipboard.policy.minimum_paste_warning,
            };
            if editor_owner.is_some() {
                return Some(manager.window.editor_view(choices).map_or_else(
                    || iced::widget::text("Closing…").into(),
                    |content| content.map(move |message| Message::ConnectionEditor(id, message)),
                ));
            }
            let content = manager
                .window
                .view(choices)
                .map(move |message| Message::Connections(id, message));
            return Some(vault_modal::gate(
                content,
                self.connection_manager_modal(id).map(|(_, focus)| focus),
            ));
        }
        if self.vault_id == Some(id) {
            return self.vault.as_ref().map(|vault| {
                vault
                    .view(
                        self.environments.catalog(),
                        self.clipboard.policy.job_webhooks_allowed,
                    )
                    .map(move |message| Message::Vault(id, message))
            });
        }
        None
    }

    pub(super) fn credential_choices(&self, kind: Kind) -> Vec<CredentialChoice> {
        self.vault
            .as_ref()
            .map_or_else(Vec::new, |vault| vault.choices(kind))
    }

    pub(super) fn refresh_ssh_key_choices(&mut self) {
        self.ssh_key_choices = crate::ssh_key_catalog::Catalog::open_profile().map_or_else(
            |_| Vec::new(),
            |catalog| {
                catalog
                    .records()
                    .iter()
                    .map(|record| crate::ssh_dialog::KeyChoice {
                        id: record.id,
                        name: record.name.clone(),
                    })
                    .collect()
            },
        );
    }

    pub(super) fn scoped_credential_choices(
        &self,
        kind: Kind,
        endpoint: &LoginTarget,
    ) -> Vec<CredentialChoice> {
        self.vault
            .as_ref()
            .map_or_else(Vec::new, |vault| vault.choices_for(kind, Some(endpoint)))
    }

    pub(super) fn vault_credential(
        &self,
        id: &str,
        kind: Kind,
        endpoint: &LoginTarget,
    ) -> Result<Secret, &'static str> {
        self.vault
            .as_ref()
            .ok_or(crate::vault_window::VAULT_LOCKED)?
            .resolve(id, kind, endpoint)
    }

    pub(super) fn saved_connection(&self, id: u64) -> Option<Connection> {
        Store::open(profile::connections_path().ok()?)
            .ok()?
            .get(id)
            .cloned()
    }

    pub(super) fn saved_connection_with_group(
        &mut self,
        id: u64,
    ) -> Option<(Connection, Option<String>)> {
        let store = profile::connections_path().and_then(Store::open).ok();
        self.saved_terminal_preferences.remember(id, store.as_ref());
        let store = store?;
        let connection = store.get(id)?.clone();
        let group = store.group_of(id).and_then(profile::normalize_tab_group);
        Some((connection, group))
    }

    pub(super) fn update_connection_store(&mut self, store: Store) {
        self.refresh_saved_terminal_preferences(&store);
        self.connection_usage.store_revision = self.connection_usage.store_revision.wrapping_add(1);
        self.set_open_recent_connections(&store);
        self.refresh_quick_connect(&store);
        self.chrome.refresh_toolbar_connections(&store);
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_connections(Some(&store));
        }
        if let Some(vault) = &mut self.vault {
            vault.set_sudo_groups(store.groups());
        }
        self.refresh_connection_managers(&store, None);
    }

    pub(in crate::workspace) fn refresh_connection_managers(
        &mut self,
        store: &Store,
        except: Option<window::Id>,
    ) {
        for (&id, manager) in &mut self.connection_managers {
            if Some(id) == except || manager.closing || manager.window.store_path() != store.path()
            {
                continue;
            }
            if manager.window.store_busy() {
                manager.refresh_pending = true;
            } else {
                manager.window.update_store(store.clone());
            }
        }
    }

    pub(super) fn record_connection_used(&mut self, target: SshTarget) -> Task<Message> {
        let Some(id) = target.saved_id() else {
            return Task::none();
        };
        self.queue_connection_use(id, UsedConnection::Ssh(target))
    }

    pub(super) fn serial_profiles_allowed(&self) -> bool {
        self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.serial_allowed
            && !self.isolation_required()
    }

    pub(super) fn record_serial_session_used(&mut self, id: SessionId) -> Task<Message> {
        let Some(pane) = self.sessions.get_mut(&id) else {
            return Task::none();
        };
        if !pane.serial_use_pending || pane.frame.status != Status::Running {
            return Task::none();
        }
        let Some(incognito) = self.windows.values().find_map(|window| {
            let (index, _) = window.pane_location(id)?;
            Some(window.tabs[index].terminal()?.incognito)
        }) else {
            return Task::none();
        };
        pane.serial_use_pending = false;
        if incognito {
            return Task::none();
        }
        let Some(profile) = pane.saved_serial.clone() else {
            return Task::none();
        };
        self.queue_connection_use(profile.id, UsedConnection::Serial(profile))
    }

    pub(super) fn open_saved_local(
        &mut self,
        owner: window::Id,
        profile: LocalProfile,
    ) -> Task<Message> {
        if !self.windows.contains_key(&owner) || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        match self.store_saved_local_session(profile, None) {
            Ok(session) => {
                self.add_tab(owner, session);
                self.record_local_session_used(session)
            }
            Err(error) => {
                self.notice(owner, format!("Could not start the local shell ({error})"));
                Task::none()
            }
        }
    }

    pub(super) fn record_local_session_used(&mut self, id: SessionId) -> Task<Message> {
        let Some(pane) = self.sessions.get_mut(&id) else {
            return Task::none();
        };
        if !pane.local_use_pending
            || !matches!(
                pane.session.frame().status,
                Status::Running | Status::Exited(_)
            )
        {
            return Task::none();
        }
        let Some(incognito) = self.windows.values().find_map(|window| {
            let (index, _) = window.pane_location(id)?;
            Some(window.tabs[index].terminal()?.incognito)
        }) else {
            return Task::none();
        };
        pane.local_use_pending = false;
        if incognito {
            return Task::none();
        }
        let Some(profile) = pane.saved_local.clone() else {
            return Task::none();
        };
        self.queue_connection_use(profile.id, UsedConnection::Local(Box::new(profile)))
    }

    fn queue_connection_use(&mut self, id: u64, target: UsedConnection) -> Task<Message> {
        self.connection_usage
            .pending
            .retain(|(queued, _, _)| *queued != id);
        let time = crate::connection_store::use_timestamp()
            .max(self.open_recent_cutoff().saturating_add(1));
        self.connection_usage.pending.push_back((id, target, time));
        self.write_connection_use()
    }

    fn write_connection_use(&mut self) -> Task<Message> {
        if self.connection_usage.active {
            return Task::none();
        }
        let Some((id, target, time)) = self.connection_usage.pending.pop_front() else {
            return Task::none();
        };
        self.connection_usage.active = true;
        let revision = self.connection_usage.store_revision;
        Task::future(async move {
            Message::ConnectionUsed(revision, record_use(id, &target, time).ok().flatten())
        })
    }

    pub(super) fn connection_use_finished(
        &mut self,
        revision: u64,
        store: Option<Store>,
    ) -> Task<Message> {
        let latest = self.connection_usage.store_revision;
        if revision != latest && store.is_some() {
            return Task::future(async move {
                Message::ConnectionUsed(
                    latest,
                    profile::connections_path().and_then(Store::open).ok(),
                )
            });
        }
        self.connection_usage.active = false;
        if let Some(store) = store {
            self.refresh_saved_terminal_preferences(&store);
            self.set_open_recent_connections(&store);
            self.refresh_quick_connect(&store);
            self.refresh_connection_managers(&store, None);
        }
        self.write_connection_use()
    }

    pub(super) fn update_connections(
        &mut self,
        id: window::Id,
        message: connection_window::Message,
    ) -> Task<Message> {
        self.update_connection_profile(id, message, false)
    }

    pub(super) fn update_connection_editor(
        &mut self,
        editor: window::Id,
        message: connection_window::Message,
    ) -> Task<Message> {
        let Some(id) = self
            .connection_editor_manager(editor)
            .filter(|id| !self.connection_managers[id].editor_closing)
        else {
            return Task::none();
        };
        self.update_connection_profile(id, message, true)
    }

    fn update_connection_profile(
        &mut self,
        id: window::Id,
        message: connection_window::Message,
        editing: bool,
    ) -> Task<Message> {
        use connection_window::{Edit, Message as Profile};
        let Some(manager) = self
            .connection_managers
            .get(&id)
            .filter(|manager| !manager.closing)
        else {
            return Task::none();
        };
        let owner = manager.owner;
        if !editing
            && let Some((dialog, _)) = self.connection_manager_modal(id)
            && !matches!(
                message,
                Profile::Completed(_) | Profile::Serial(crate::serial_dialog::Message::Ports(..))
            )
        {
            return window::gain_focus(dialog);
        }
        let task = match message {
            Profile::SaveTeamworkAuth if !editing => self.save_teamwork_auth(id),
            Profile::RestoreTeamwork if !editing => self.open_teamwork_restore(id),
            Profile::RefreshTeamwork if !editing && manager.window.can_restore_teamwork() => {
                self.reload_teamwork_snapshot()
            }
            Profile::ExportOverview => self.open_connection_overview(id),
            Profile::ExportMapPdf => self.open_connection_map_pdf(id),
            Profile::ExportConnections => self.show_connection_manager_export(id),
            Profile::ImportConnections => self.show_connection_import(id),
            Profile::NewSerial if !self.serial_profiles_allowed() => Task::none(),
            Profile::ManageOsBadges if manager.window.can_import_connections() => {
                self.show_os_badge_manager(id)
            }
            Profile::Edit(Edit::TerminalEffect(effect))
                if !self.terminal_effects_enabled()
                    || effect.is_some_and(|effect| !self.allowed_terminal_effect(effect)) =>
            {
                Task::none()
            }
            Profile::Edit(Edit::TerminalEffectSpeed(_))
                if !manager
                    .window
                    .editing_terminal_effect()
                    .is_some_and(|effect| self.allowed_terminal_effect(effect)) =>
            {
                Task::none()
            }
            Profile::ConnectInNewWindow if !editing && self.windows.contains_key(&owner) => {
                let Some(connection) = manager.window.selected() else {
                    return Task::none();
                };
                if let Some((host, port)) =
                    connection.blocked_server(self.clipboard.policy.server_access())
                {
                    let notice = super::server_access::blocked_notice(host, port);
                    self.connection_managers
                        .get_mut(&id)
                        .expect("current manager")
                        .window
                        .set_notice(notice);
                    return Task::none();
                }
                let launch = ConnectionLaunch::SshInNewWindow(Box::new(connection.clone()));
                self.close_connection_manager(id, false, Some(launch))
            }
            Profile::Connect(sftp) => {
                if !self.windows.contains_key(&owner) {
                    return Task::none();
                }
                let serial = (!sftp)
                    .then(|| manager.window.selected_serial())
                    .flatten()
                    .cloned()
                    .map(ConnectionLaunch::Serial);
                let local = (!sftp)
                    .then(|| manager.window.selected_local())
                    .flatten()
                    .cloned()
                    .map(|profile| ConnectionLaunch::Local(Box::new(profile)));
                let ssh = if sftp {
                    manager.window.selected()
                } else {
                    manager.window.connect_selection()
                }
                .cloned()
                .map(|connection| ConnectionLaunch::Ssh(Box::new(connection), sftp));
                let shared = manager.window.selected_shared().map(|entry| {
                    let path = manager.window.store_path().to_owned();
                    ConnectionLaunch::Teamwork(TeamworkSelection::new(entry, path), sftp)
                });
                if let Some(launch) = local.or(serial).or(ssh).or(shared) {
                    let connection = match &launch {
                        ConnectionLaunch::Ssh(connection, _) => Some(connection.as_ref()),
                        ConnectionLaunch::Teamwork(selection, _) => Some(selection.connection()),
                        ConnectionLaunch::Serial(_)
                        | ConnectionLaunch::Local(_)
                        | ConnectionLaunch::SshInNewWindow(_) => None,
                    };
                    if let Some((host, port)) = connection.and_then(|connection| {
                        connection.blocked_server(self.clipboard.policy.server_access())
                    }) {
                        self.connection_managers
                            .get_mut(&id)
                            .expect("current manager")
                            .window
                            .set_notice(super::server_access::blocked_notice(host, port));
                        return Task::none();
                    }
                    self.close_connection_manager(id, false, Some(launch))
                } else {
                    Task::none()
                }
            }
            Profile::OpenVault => self.show_vault(id),
            Profile::Close => self.close_profile_tool(id, false),
            Profile::DiscardAndClose if editing && self.quit_waiting_profile != Some(id) => {
                let manager = self
                    .connection_managers
                    .get_mut(&id)
                    .expect("current manager");
                if !manager.window.store_busy() {
                    manager.window.discard_editor();
                }
                Task::none()
            }
            Profile::DiscardAndClose => self.close_profile_tool(id, true),
            Profile::KeepEditing => {
                self.cancel_profile_quit(id);
                self.connection_managers
                    .get_mut(&id)
                    .map_or_else(Task::none, |manager| {
                        manager
                            .window
                            .update(Profile::KeepEditing)
                            .map(move |message| Message::Connections(id, message))
                    })
            }
            Profile::Completed(result) => {
                if let (Ok(saved), Some(scheduler)) = (&result, &mut self.scheduler) {
                    scheduler.set_connections(Some(saved.store()));
                }
                let apply = result.as_ref().map_or_else(
                    |_| Task::none(),
                    |saved| self.apply_saved_tunnel_profiles(saved.store()),
                );
                let toolbar = if let Ok(saved) = &result {
                    self.connection_usage.store_revision =
                        self.connection_usage.store_revision.wrapping_add(1);
                    self.refresh_saved_terminal_preferences(saved.store());
                    self.set_open_recent_connections(saved.store());
                    self.refresh_quick_connect(saved.store());
                    self.apply_saved_tab_groups(saved.store());
                    self.chrome.refresh_toolbar_connections(saved.store());
                    self.refresh_connection_managers(saved.store(), Some(id));
                    saved.folder_rename().map_or_else(Task::none, |rename| {
                        self.chrome.rename_toolbar_folder(owner, rename)
                    })
                } else {
                    Task::none()
                };
                let failed = result.is_err();
                let manager = self
                    .connection_managers
                    .get_mut(&id)
                    .expect("current manager");
                let update = manager
                    .window
                    .update(Profile::Completed(result))
                    .map(move |message| Message::Connections(id, message));
                let refresh = if std::mem::take(&mut manager.refresh_pending) || failed {
                    let path = manager.window.store_path().to_path_buf();
                    let revision = self.connection_usage.store_revision;
                    Task::future(async move {
                        Message::ConnectionCatalogReloaded(revision, Store::open(path).ok())
                    })
                } else {
                    Task::none()
                };
                Task::batch([apply, toolbar, update, refresh])
            }
            message => self
                .connection_managers
                .get_mut(&id)
                .map_or_else(Task::none, |manager| {
                    manager
                        .window
                        .update(message)
                        .map(move |message| Message::Connections(id, message))
                }),
        };
        Task::batch([task, self.sync_connection_editor(id)])
    }

    pub(super) fn update_vault(
        &mut self,
        id: window::Id,
        message: vault_window::Message,
    ) -> Task<Message> {
        use vault_window::Message as VaultMessage;
        if self.vault_id != Some(id) {
            return Task::none();
        }
        let message = match message {
            VaultMessage::FocusUnlock => return self.focus_vault_unlock(id),
            VaultMessage::ManageEnvironments => return self.show_environment(id),
            VaultMessage::Environment(dialog, message) => {
                return self.update_environment(dialog, message);
            }
            message => message,
        };
        if let Some(dialog) = self.environment_for_vault(id)
            && !matches!(
                message,
                VaultMessage::Close
                    | VaultMessage::DiscardAndClose
                    | VaultMessage::WebhookTestFinished(..)
            )
        {
            return window::gain_focus(dialog);
        }
        let completed = matches!(message, VaultMessage::Completed(_));
        let recovery_key_changed =
            matches!(&message, VaultMessage::Completed(reply) if reply.recovery_key_changed());
        let credentials_changed = completed || matches!(message, VaultMessage::Lock);
        let task = match message {
            VaultMessage::Close => self.close_profile_tool(id, false),
            VaultMessage::DiscardAndClose => self.close_profile_tool(id, true),
            VaultMessage::TestWebhook(source) if !self.exiting && !self.scheduler_draining => {
                self.vault.as_mut().map_or_else(Task::none, |vault| {
                    vault
                        .test_webhook(&source, &self.clipboard.policy)
                        .map(move |message| Message::Vault(id, message))
                })
            }
            VaultMessage::KeepEditing => {
                self.cancel_profile_quit(id);
                self.vault.as_mut().map_or_else(Task::none, |vault| {
                    vault
                        .update(VaultMessage::KeepEditing)
                        .map(move |message| Message::Vault(id, message))
                })
            }
            message => self.vault.as_mut().map_or_else(Task::none, |vault| {
                vault
                    .update(message)
                    .map(move |message| Message::Vault(id, message))
            }),
        };
        if recovery_key_changed {
            self.purge_session_output();
        }
        let task = Task::batch([task, self.cancel_revoked_password_commands()]);
        if credentials_changed {
            self.vault_change_revision = Arc::new(());
            self.revalidate_journal_questions();
            self.revalidate_remote_snippet_analysis();
            self.refresh_scheduler_credentials();
        }
        if completed && self.vault_unlock_prompt_owner(id).is_some() {
            if self
                .vault
                .as_ref()
                .is_some_and(VaultWindow::unlocked_prompt)
            {
                return self.close_profile_tool(id, false);
            }
            return Task::batch([task, self.focus_vault_unlock(id)]);
        }
        task
    }

    fn focus_vault_unlock(&self, id: window::Id) -> Task<Message> {
        self.vault
            .as_ref()
            .and_then(VaultWindow::unlock_prompt_input)
            .map_or_else(Task::none, |input| {
                window::gain_focus(id).chain(iced::widget::operation::focus(input))
            })
    }

    pub(super) fn request_profile_quit(&mut self) -> Option<Task<Message>> {
        self.quit_waiting_profile = None;
        if let Some(id) = self.connection_overview_busy() {
            return Some(self.defer_profile_quit(id, true));
        }
        if let Some(id) = self.teamwork_restore_busy() {
            return Some(self.defer_profile_quit(id, true));
        }
        let blocked = self
            .connection_managers
            .iter_mut()
            .find_map(|(&id, manager)| {
                (!manager.closing && !manager.window.request_close())
                    .then(|| (id, manager.window.busy()))
            });
        if let Some((id, busy)) = blocked {
            return Some(self.defer_profile_quit(id, busy));
        }
        if let Some(id) = self.vault_id
            && let Some(vault) = &mut self.vault
            && !vault.request_close()
        {
            let busy = vault.busy();
            return Some(self.defer_profile_quit(id, busy));
        }
        None
    }

    fn defer_profile_quit(&mut self, id: window::Id, busy: bool) -> Task<Message> {
        if busy {
            window_lifecycle::reply_to_native_quit(false);
        } else {
            self.quit_waiting_profile = Some(id);
        }
        if self.connection_managers.contains_key(&id) {
            self.focus_connection_manager(id)
        } else {
            window::gain_focus(id)
        }
    }

    fn cancel_profile_quit(&mut self, id: window::Id) {
        if self.quit_waiting_profile == Some(id) {
            self.quit_waiting_profile = None;
            window_lifecycle::reply_to_native_quit(false);
        }
    }

    fn close_connection_manager(
        &mut self,
        id: window::Id,
        discard: bool,
        launch: Option<ConnectionLaunch>,
    ) -> Task<Message> {
        if let Some((dialog, _)) = self.connection_manager_modal(id)
            && !(discard
                && self.quit_waiting_profile == Some(id)
                && self.connection_editor_manager(dialog) == Some(id))
        {
            return window::gain_focus(dialog);
        }
        let Some(manager) = self.connection_managers.get_mut(&id) else {
            return Task::none();
        };
        if manager.closing || manager.window.busy() || (!discard && !manager.window.request_close())
        {
            return Task::none();
        }
        manager.closing = true;
        manager.launch = launch;
        let close = self.close_placed_dialog(id);
        self.close_placed_dialogs_for_owner_shutdown(id, close)
    }

    pub(super) fn close_profile_tool(&mut self, id: window::Id, discard: bool) -> Task<Message> {
        if self.teamwork_restore.contains_key(&id) {
            return self.close_teamwork_restore(id);
        }
        if let Some(close) = self.close_connection_editor(id) {
            return close;
        }
        if self.connection_managers.contains_key(&id) {
            return self.close_connection_manager(id, discard, None);
        }
        if self.vault_id != Some(id) {
            return Task::none();
        }
        let unlock = self.vault_unlock_prompt_owner(id);
        let focus = unlock
            .filter(|owner| self.windows.contains_key(owner))
            .map_or_else(Task::none, window::gain_focus);
        if let Some(vault) = &mut self.vault {
            if vault.busy() || (!discard && !vault.request_close()) {
                return Task::none();
            }
            vault.close_form();
        }
        self.vault_id = None;
        let environment = self.close_environment_owner(id);
        let close = if unlock.is_some() {
            window::close(id)
        } else {
            self.close_placed_dialog(id)
        };
        let close = Task::batch([close, environment]).chain(focus);
        if self.quit_waiting_profile == Some(id) || self.should_exit_after_window_close() {
            self.quit_waiting_profile = None;
            Task::batch([close, self.quit()])
        } else {
            close
        }
    }
}

fn record_use(id: u64, target: &UsedConnection, time: u64) -> io::Result<Option<Store>> {
    let path = profile::connections_path()?;
    let mut retries = 4;
    loop {
        let mut store = Store::open(path.clone())?;
        let result = match target {
            UsedConnection::Ssh(target) => {
                if store
                    .get(id)
                    .is_none_or(|connection| connection.incognito || !target.matches(connection))
                {
                    return Ok(None);
                }
                store.record_used_at(id, time)
            }
            UsedConnection::Serial(profile) => store.record_serial_used_at(profile, time),
            UsedConnection::Local(profile) => store.record_local_used_at(profile, time),
        };
        match result {
            Ok(()) => return Ok(Some(store)),
            Err(error) if retries != 0 && error.kind() == io::ErrorKind::Other => {
                retries -= 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}
