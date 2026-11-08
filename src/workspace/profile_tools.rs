use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::time::Duration;

use iced::{Element, Size, Task, window};
use kortty_vault::Secret;

use crate::connection_store::{Connection, Store};
use crate::connection_window::{self, ConnectionWindow, CredentialChoice, ViewChoices};
use crate::profile;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_effects::Effect;
use crate::vault_window::{self, Kind, VaultWindow};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    Message, SshTarget, Workspace,
    chrome::{connection_export, connection_import},
    os_badge_manager, window_lifecycle,
};

pub(super) mod connection_overview;
mod terminal_preferences;
pub(super) mod vault_modal;
pub(super) use terminal_preferences::SavedTerminalPreferences;

#[derive(Default)]
pub(super) struct ConnectionUsage {
    active: bool,
    pending: VecDeque<(u64, SshTarget, u64)>,
    store_revision: u64,
}

impl Workspace {
    pub(super) fn connection_store_revision(&self) -> u64 {
        self.connection_usage.store_revision
    }

    pub(super) fn show_connections(&mut self, source: window::Id) -> Task<Message> {
        self.refresh_ssh_key_choices();
        if self.windows.contains_key(&source) {
            self.connection_source = Some(source);
        }
        if let Some(id) = self.connections_id {
            return window::gain_focus(self.connection_manager_modal(id).map_or(id, |(id, _)| id));
        }
        match profile::connections_path()
            .map_err(|error| error.to_string())
            .and_then(ConnectionWindow::open)
        {
            Ok(connections) => {
                let (id, opened) = self.open_placed_dialog(
                    DialogKind::ConnectionManager,
                    window::Settings {
                        size: Size::new(1240.0, 760.0),
                        min_size: Some(Size::new(940.0, 520.0)),
                        exit_on_close_request: false,
                        ..window::Settings::default()
                    },
                );
                self.connections = Some(connections);
                self.connections_id = Some(id);
                self.refresh_connection_counts();
                opened.map(|_| Message::Noop)
            }
            Err(error) => {
                self.notice(
                    source,
                    format!("Could not open the connection manager ({error})"),
                );
                Task::none()
            }
        }
    }

    pub(super) fn refresh_connection_counts(&mut self) {
        let Some(manager) = &mut self.connections else {
            return;
        };
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
                .filter_map(|(_, id)| self.sessions.get(id)?.ssh.as_ref()?.saved_id())
                .collect();
            for id in connections {
                *counts.entry(id).or_default() += 1;
            }
        }
        manager.set_open_counts(counts);
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
                Ok(vault) => self.vault = Some(vault),
                Err(error) => {
                    let target = self
                        .windows
                        .contains_key(&source)
                        .then_some(source)
                        .or(self.connection_source);
                    if let Some(target) = target {
                        self.notice(target, format!("Could not open the vault ({error})"));
                        return window::gain_focus(target);
                    }
                    return Task::none();
                }
            }
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
        self.connections_id == Some(id) || self.vault_id == Some(id)
    }

    pub(super) fn connection_manager_modal(
        &self,
        owner: window::Id,
    ) -> Option<(window::Id, Message)> {
        if self.connections_id != Some(owner) {
            return None;
        }
        self.connection_overview_for_owner(owner)
            .map(|id| (id, connection_overview::focus(id)))
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
        if self.connections_id == Some(id) {
            return self.connections.as_ref().map(|connections| {
                let effects = self.terminal_effects_enabled().then(|| {
                    Effect::ALL
                        .into_iter()
                        .filter(|&effect| self.allowed_terminal_effect(effect))
                        .collect::<Vec<_>>()
                });
                let content = connections
                    .view(ViewChoices {
                        passwords: &self.credential_choices(Kind::Password),
                        passphrases: &self.credential_choices(Kind::KeyPassphrase),
                        keys: &self.ssh_key_choices,
                        incognito_allowed: self.clipboard.policy.incognito_allowed,
                        terminal_effects: effects.as_deref(),
                        default_paste_warning: self.window_settings.paste_warning,
                        minimum_paste_warning: self.clipboard.policy.minimum_paste_warning,
                    })
                    .map(Message::Connections);
                vault_modal::gate(
                    content,
                    self.connection_manager_modal(id).map(|(_, focus)| focus),
                )
            });
        }
        if self.vault_id == Some(id) {
            return self.vault.as_ref().map(|vault| {
                vault
                    .view(self.environments.catalog())
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
        self.saved_terminal_preferences.refresh(&store);
        self.connection_usage.store_revision = self.connection_usage.store_revision.wrapping_add(1);
        self.set_open_recent_connections(&store);
        self.refresh_quick_connect(&store);
        self.chrome.refresh_toolbar_connections(&store);
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.set_connections(Some(&store));
        }
        if let Some(manager) = &mut self.connections {
            manager.update_store(store);
        }
    }

    pub(super) fn record_connection_used(&mut self, target: SshTarget) -> Task<Message> {
        let Some(id) = target.saved_id() else {
            return Task::none();
        };
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
            self.saved_terminal_preferences.refresh(&store);
            self.set_open_recent_connections(&store);
            self.refresh_quick_connect(&store);
            if let Some(manager) = &mut self.connections {
                manager.update_store(store);
            }
        }
        self.write_connection_use()
    }

    pub(super) fn update_connections(
        &mut self,
        message: connection_window::Message,
    ) -> Task<Message> {
        use connection_window::{Edit, Message as Profile};
        let Some(id) = self.connections_id else {
            return Task::none();
        };
        if let Some(dialog) = self
            .connection_manager_modal(id)
            .map(|(id, _)| id)
            .or_else(|| self.connection_import_saving())
            && !matches!(message, Profile::Completed(_))
        {
            return window::gain_focus(dialog);
        }
        match message {
            Profile::ExportOverview => self.open_connection_overview(id),
            Profile::ExportConnections => self.show_connection_manager_export(id),
            Profile::ImportConnections => self.show_connection_import(id),
            Profile::ManageOsBadges
                if self
                    .connections
                    .as_ref()
                    .is_some_and(ConnectionWindow::can_import_connections) =>
            {
                self.show_os_badge_manager(id)
            }
            Profile::Edit(Edit::TerminalEffect(effect))
                if !self.terminal_effects_enabled()
                    || effect.is_some_and(|effect| !self.allowed_terminal_effect(effect)) =>
            {
                Task::none()
            }
            Profile::Edit(Edit::TerminalEffectSpeed(_))
                if !self
                    .connections
                    .as_ref()
                    .and_then(ConnectionWindow::editing_terminal_effect)
                    .is_some_and(|effect| self.allowed_terminal_effect(effect)) =>
            {
                Task::none()
            }
            Profile::Connect(sftp) => {
                let Some(connection) = self
                    .connections
                    .as_ref()
                    .and_then(|manager| {
                        if sftp {
                            manager.selected()
                        } else {
                            manager.connect_selection()
                        }
                    })
                    .cloned()
                else {
                    return Task::none();
                };
                let (source, open) = self
                    .connection_source
                    .filter(|id| self.windows.contains_key(id))
                    .or_else(|| {
                        self.windows
                            .iter()
                            .max_by_key(|(_, window)| window.number)
                            .map(|(id, _)| *id)
                    })
                    .map_or_else(|| self.open_policy_window(), |id| (id, Task::none()));
                self.connection_source = Some(source);
                let close = self.close_profile_tool(id, false);
                let start = self.show_saved_ssh(source, connection, sftp);
                Task::batch([open, close, start, window::gain_focus(source)])
            }
            Profile::OpenVault => self.show_vault(id),
            Profile::Close => self.close_profile_tool(id, false),
            Profile::DiscardAndClose => self.close_profile_tool(id, true),
            Profile::KeepEditing => {
                self.cancel_profile_quit(id);
                self.connections.as_mut().map_or_else(Task::none, |window| {
                    window
                        .update(Profile::KeepEditing)
                        .map(Message::Connections)
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
                    self.saved_terminal_preferences.refresh(saved.store());
                    self.set_open_recent_connections(saved.store());
                    self.refresh_quick_connect(saved.store());
                    self.apply_saved_tab_groups(saved.store());
                    self.chrome.refresh_toolbar_connections(saved.store());
                    saved.folder_rename().map_or_else(Task::none, |rename| {
                        self.chrome
                            .rename_toolbar_folder(self.connection_source.unwrap_or(id), rename)
                    })
                } else {
                    Task::none()
                };
                let update = self.connections.as_mut().map_or_else(Task::none, |window| {
                    window
                        .update(Profile::Completed(result))
                        .map(Message::Connections)
                });
                Task::batch([apply, toolbar, update])
            }
            message => self.connections.as_mut().map_or_else(Task::none, |window| {
                window.update(message).map(Message::Connections)
            }),
        }
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
            && !matches!(message, VaultMessage::Close | VaultMessage::DiscardAndClose)
        {
            return window::gain_focus(dialog);
        }
        let completed = matches!(message, VaultMessage::Completed(_));
        let credentials_changed = completed || matches!(message, VaultMessage::Lock);
        let task = match message {
            VaultMessage::Close => self.close_profile_tool(id, false),
            VaultMessage::DiscardAndClose => self.close_profile_tool(id, true),
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
        if credentials_changed {
            self.refresh_scheduler_archive_passwords();
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
        if let Some(id) = self.connections_id
            && let Some(connections) = &mut self.connections
            && !connections.request_close()
        {
            let busy = connections.busy();
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
        window::gain_focus(id)
    }

    fn cancel_profile_quit(&mut self, id: window::Id) {
        if self.quit_waiting_profile == Some(id) {
            self.quit_waiting_profile = None;
            window_lifecycle::reply_to_native_quit(false);
        }
    }

    pub(super) fn close_profile_tool(&mut self, id: window::Id, discard: bool) -> Task<Message> {
        let mut environment = Task::none();
        let mut focus = Task::none();
        let unlock = self.vault_unlock_prompt_owner(id);
        if self.connections_id == Some(id) {
            if let Some((dialog, _)) = self.connection_manager_modal(id) {
                return window::gain_focus(dialog);
            }
            if let Some(connections) = &mut self.connections
                && (connections.busy() || (!discard && !connections.request_close()))
            {
                return Task::none();
            }
            self.connections = None;
            self.connections_id = None;
        } else if self.vault_id == Some(id) {
            if let Some(owner) = unlock
                && self.windows.contains_key(&owner)
            {
                focus = window::gain_focus(owner);
            }
            if let Some(vault) = &mut self.vault {
                if vault.busy() || (!discard && !vault.request_close()) {
                    return Task::none();
                }
                vault.close_form();
            }
            self.vault_id = None;
            environment = self.close_environment_owner(id);
        } else {
            return Task::none();
        }
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

fn record_use(id: u64, target: &SshTarget, time: u64) -> io::Result<Option<Store>> {
    let path = profile::connections_path()?;
    let mut retries = 4;
    loop {
        let mut store = Store::open(path.clone())?;
        if store
            .get(id)
            .is_none_or(|connection| connection.incognito || !target.matches(connection))
        {
            return Ok(None);
        }
        match store.record_used_at(id, time) {
            Ok(()) => return Ok(Some(store)),
            Err(error) if retries != 0 && error.kind() == io::ErrorKind::Other => {
                retries -= 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}
