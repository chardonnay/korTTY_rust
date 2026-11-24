//! Owner-scoped native quick connection window.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Size, Task, window};
use kortty_pty::{LocalShell, validate_shell_integration};
use kortty_ssh::ConnectOptions;
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::connection_store::Auth;
use crate::connection_store::{
    AiAssignments, Connection, MAX_ASSIGNED_AI_SKILLS, Store, TerminalAppearance, ThemeOverride,
};
use crate::profile;
use crate::serial_dialog::{self, SerialDialog};
use crate::session_security::LocalSettings;
use crate::ssh_dialog::{self, SshDialog};
use crate::terminal_effects::{Effect, Selection, Speed, editor::EffectDraft};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    IbmKind, IsolationLevel, Message as Desktop, PendingIbm, PendingSerial, SerialConnection,
    Workspace, project_access, split_connection::SplitTarget,
};

const TITLE: &str = "Quick Connect";
mod ai;
mod appearance;
mod local;
mod view;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    ConnectionSettings,
    Appearance,
    Logging,
    Effect,
    Ai,
}

pub(super) type ExpandedSections = BTreeSet<Section>;

impl Section {
    pub(super) fn from_java(key: &str) -> Option<Self> {
        Some(match key {
            "connectionTimeout" => Self::ConnectionSettings,
            "terminalAppearance" => Self::Appearance,
            "terminalLog" => Self::Logging,
            "terminalEffect" => Self::Effect,
            "ai" => Self::Ai,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Ssh,
    Local,
    Serial,
    Sftp,
    Tn3270,
    Tn5250,
}

impl fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ssh => "SSH (TCP)",
            Self::Local => "Local Shell",
            Self::Serial => "Serial",
            Self::Sftp => "SFTP",
            Self::Tn3270 => "TN3270E",
            Self::Tn5250 => "TN5250E",
        })
    }
}

impl Protocol {
    fn is_ssh(self) -> bool {
        matches!(self, Self::Ssh | Self::Sftp)
    }

    fn ibm_models(self) -> Option<[IbmKind; 2]> {
        match self {
            Self::Tn3270 => Some([IbmKind::Tn3270e, IbmKind::Tn3270eExtended]),
            Self::Tn5250 => Some([IbmKind::Tn5250, IbmKind::Tn5250Wide]),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Loaded(Result<Store, String>),
    Saved(Result<(Store, u64), String>),
    Query(String),
    Select(u64),
    Recent(u64),
    GroupTab(bool),
    Group(String),
    Protocol(Protocol),
    Profile(TerminalProfile),
    Font(u8),
    Theme(Option<ThemeOverride>),
    FontFamily(Option<String>),
    Foreground(String),
    Background(String),
    TerminalColors(bool),
    ToggleSection(Section),
    Effect(Option<Effect>),
    EffectSpeed(String),
    AiProfile(Option<crate::ai_manager::ProfileRef>),
    AiSkill(u64, bool),
    LocalShell(LocalShell),
    Local(local::Edit),
    ShellIntegration(bool),
    AdHocLocal,
    Ssh(ssh_dialog::Message),
    BrowseTerminalLogFolder,
    TerminalLogFolderPicked(Arc<()>, Option<PathBuf>),
    TemporaryKeyPasted(u64, bool, ssh_dialog::Password),
    Serial(serial_dialog::Message),
    IbmAddress(String),
    IbmDevice(IbmKind),
    IbmName(String),
    IbmPlaintext(bool),
    SaveConnection(bool),
    ConnectionName(String),
    Reset,
    Connect,
    LoadProject,
    Manage,
    Focus,
    Cancel,
}

pub(super) struct Dialog {
    id: window::Id,
    owner: window::Id,
    split: Option<SplitTarget>,
    store: Option<Store>,
    query: String,
    selected_ssh: Option<(u64, bool)>,
    selected_serial: Option<u64>,
    selected_local: Option<u64>,
    group_tab: bool,
    group: Option<String>,
    protocol: Protocol,
    ssh: SshDialog,
    serial: SerialDialog,
    ibm: PendingIbm,
    appearance: appearance::Draft,
    appearance_defaults: TerminalAppearance,
    appearance_edited: bool,
    effect: EffectDraft,
    effect_default_speed: Speed,
    effect_edited: bool,
    ai: AiAssignments,
    ai_edited: bool,
    minimum_isolation: IsolationLevel,
    default_isolation: IsolationLevel,
    local_shell: LocalShell,
    local_emulation: TerminalProfile,
    local: local::Draft,
    shell_integration: bool,
    save: bool,
    name: String,
    saving: bool,
    status: Option<String>,
}

impl Dialog {
    fn new(
        id: window::Id,
        owner: window::Id,
        split: Option<SplitTarget>,
        serial_id: u64,
        profile: TerminalProfile,
        minimum_isolation: IsolationLevel,
    ) -> Self {
        Self {
            id,
            owner,
            split,
            store: None,
            query: String::new(),
            selected_ssh: None,
            selected_serial: None,
            selected_local: None,
            group_tab: false,
            group: None,
            protocol: Protocol::Ssh,
            ssh: blank_ssh(profile, minimum_isolation),
            serial: SerialDialog::new(serial_id, SerialConnection::default(), profile),
            ibm: new_ibm(IbmKind::Tn3270e),
            appearance: appearance::Draft::default(),
            appearance_defaults: TerminalAppearance::default(),
            appearance_edited: false,
            effect: EffectDraft::default(),
            effect_default_speed: Speed::DEFAULT,
            effect_edited: false,
            ai: AiAssignments::default(),
            ai_edited: false,
            minimum_isolation,
            default_isolation: IsolationLevel::None,
            local_shell: LocalShell::default(),
            local_emulation: profile,
            local: local::Draft::default(),
            shell_integration: false,
            save: false,
            name: String::new(),
            saving: false,
            status: None,
        }
    }

    fn choose(&mut self, id: u64, temporary_key_enabled: bool) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        if let Some(connection) = store.get(id)
            && let Some(profile) = connection.profile()
        {
            let isolation = store
                .connection_isolation(
                    connection,
                    self.default_isolation,
                    None,
                    self.minimum_isolation,
                )
                .0;
            self.ssh = SshDialog::from_saved(profile, connection)
                .with_isolation(isolation, self.minimum_isolation);
            self.selected_ssh = Some((id, connection.incognito));
            self.protocol = Protocol::Ssh;
        } else if let Some(saved) = store.serial(id)
            && let Some(profile) = saved.profile()
        {
            self.serial.load(saved.connection.clone(), profile);
            self.selected_serial = Some(id);
            self.protocol = Protocol::Serial;
        } else if let Some(saved) = store.local(id)
            && let Some(profile) = saved.profile()
        {
            self.selected_local = Some(id);
            self.local = local::Draft::from_saved(saved);
            self.local_shell = saved.shell;
            self.shell_integration = saved.shell_integration;
            self.local_emulation = profile;
            self.protocol = Protocol::Local;
        } else {
            return false;
        }
        self.ssh.logging_form_mut().cancel_folder_picker();
        self.save = false;
        self.name.clear();
        self.status = None;
        self.set_temporary_key_enabled(temporary_key_enabled);
        self.reset_effect_draft();
        self.reset_ai_draft();
        self.reset_appearance_draft();
        true
    }

    fn set_temporary_key_enabled(&mut self, enabled: bool) {
        self.ssh
            .set_quick_temporary_key_enabled(enabled && self.protocol.is_ssh() && !self.group_tab);
    }

    fn selected(&self) -> Option<&Connection> {
        self.store.as_ref()?.get(self.selected_ssh?.0)
    }

    fn selected_ssh_incognito(&self) -> bool {
        self.selected_ssh.is_some_and(|(_, incognito)| incognito)
            || self.selected().is_some_and(|profile| profile.incognito)
    }

    fn selected_serial(&self) -> Option<&crate::connection_store::SerialProfile> {
        self.store.as_ref()?.serial(self.selected_serial?)
    }

    fn selected_local(&self) -> Option<&crate::connection_store::LocalProfile> {
        self.store.as_ref()?.local(self.selected_local?)
    }

    fn section_visible(&self, section: Section) -> bool {
        if self.group_tab {
            return false;
        }
        match section {
            Section::ConnectionSettings => self.protocol.is_ssh(),
            Section::Logging => self.protocol == Protocol::Ssh,
            Section::Appearance => self.protocol != Protocol::Sftp,
            Section::Effect => self.protocol != Protocol::Sftp,
            Section::Ai => self.protocol != Protocol::Sftp,
        }
    }

    fn reset_effect_draft(&mut self) {
        let selection = match self.protocol {
            Protocol::Ssh => self.selected().and_then(|saved| saved.terminal_effect),
            Protocol::Local => self
                .selected_local()
                .and_then(|saved| saved.terminal_effect),
            Protocol::Serial => self
                .selected_serial()
                .and_then(|saved| saved.terminal_effect),
            _ => None,
        };
        self.effect = EffectDraft::with_default(selection, self.effect_default_speed);
        self.effect_edited = false;
    }

    fn effect_override(&self, enabled: bool) -> Option<Option<Selection>> {
        (enabled
            && (self.effect_edited || self.effect.effect.is_some())
            && self.section_visible(Section::Effect))
        .then(|| self.effect.selection())
    }

    fn selected_profile_id(&self) -> Option<u64> {
        match self.protocol {
            Protocol::Ssh | Protocol::Sftp => self.selected_ssh.map(|(id, _)| id),
            Protocol::Serial => self.selected_serial,
            Protocol::Local => self.selected_local,
            _ => None,
        }
    }

    fn focus_selected(&mut self) -> Task<Desktop> {
        let id = self.id;
        if self.protocol == Protocol::Serial {
            self.serial
                .refresh()
                .map(move |message| wrap(id, Message::Serial(message)))
        } else {
            self.ssh
                .focus()
                .map(move |message| wrap(id, Message::Ssh(message)))
        }
    }

    fn can_connect(&self) -> bool {
        if self.saving {
            return false;
        }
        if self.group_tab {
            return self.group.is_some();
        }
        if self.section_visible(Section::Appearance) && !self.appearance.valid() {
            return false;
        }
        match self.protocol {
            Protocol::Ssh | Protocol::Sftp => self.ssh.can_connect(),
            Protocol::Local => self
                .local_profile()
                .is_some_and(|profile| profile.validate().is_ok()),
            Protocol::Serial => self.serial.validated().is_ok(),
            Protocol::Tn3270 | Protocol::Tn5250 => self.ibm.connection().is_some(),
        }
    }

    fn group_members(&self, group: &str) -> usize {
        self.store
            .as_ref()
            .map_or(0, |store| store.direct_group_ids(group).count())
    }
}

impl Workspace {
    fn quick_shell_integration_for_command(
        &self,
        profile: TerminalProfile,
        shell: LocalShell,
        command: Option<&[String]>,
        isolation: Option<IsolationLevel>,
    ) -> Result<LocalSettings, &'static str> {
        if !self.window_settings.automatic_local_shell_integration() {
            return Err("Automatic setup for local shells is disabled in Settings → Terminal.");
        }
        if !self.window_settings.prompt_navigation_enabled
            || !super::shell_integration::supports_marks(profile)
        {
            return Err(
                "Select an emulation that reads command marks and enable them in Settings → Terminal.",
            );
        }
        let mut settings = self
            .local_settings(None)
            .map_err(|_| "Local session settings are unavailable.")?;
        if isolation.unwrap_or(settings.isolation) == IsolationLevel::Sandbox {
            return Err("A sandboxed shell uses its own isolated startup environment.");
        }
        validate_shell_integration(shell, command)
            .map_err(|_| "Automatic setup is unavailable for this shell.")?;
        let supported = if command.is_some() {
            true
        } else {
            kortty_pty::shell_program(shell)
                .ok()
                .and_then(|program| {
                    program
                        .to_str()
                        .and_then(kortty_pty::ShellIntegrationSnippet::from_program)
                })
                .is_some()
        };
        if !supported {
            return Err("Automatic setup is unavailable for this shell.");
        }
        settings.shell = shell;
        settings.shell_integration = true;
        Ok(settings)
    }

    #[cfg(test)]
    fn quick_shell_integration(
        &self,
        profile: TerminalProfile,
        shell: LocalShell,
    ) -> Result<LocalSettings, &'static str> {
        self.quick_shell_integration_for_command(profile, shell, None, None)
    }

    pub(super) fn show_quick_connect(&mut self, owner: window::Id) -> Task<Desktop> {
        self.open_quick_connect(owner, None)
    }

    pub(super) fn open_quick_connect(
        &mut self,
        owner: window::Id,
        split: Option<SplitTarget>,
    ) -> Task<Desktop> {
        if let Some(dialog) = &self.quick_connect {
            return window::gain_focus(dialog.id);
        }
        if let Some((_, focus)) = self.owner_dialog(owner) {
            return self.update(focus);
        }
        self.refresh_ssh_key_choices();
        let Some(profile) = self.windows.get(&owner).map(|window| window.local_profile) else {
            return Task::none();
        };
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        let (id, open) = self.open_owned_placed_dialog(
            owner,
            DialogKind::QuickConnect,
            window::Settings {
                size: Size::new(775.0, 610.0),
                min_size: Some(Size::new(700.0, 400.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        let mut dialog = Dialog::new(
            id,
            owner,
            split,
            self.allocate(),
            profile,
            self.clipboard.policy.minimum_isolation,
        );
        dialog.default_isolation = self
            .window_settings
            .default_session_isolation
            .unwrap_or_default();
        dialog.ssh = dialog.ssh.with_isolation(
            dialog.default_isolation,
            self.clipboard.policy.minimum_isolation,
        );
        dialog.set_temporary_key_enabled(self.quick_temporary_key_enabled());
        dialog.effect_default_speed = self
            .window_settings
            .quick_connect_effect_speed
            .unwrap_or(Speed::DEFAULT);
        dialog.reset_effect_draft();
        dialog.appearance_defaults = self
            .window_settings
            .quick_connect_appearance
            .clone()
            .unwrap_or_default();
        dialog.reset_appearance_draft();
        let focus = dialog
            .ssh
            .focus()
            .map(move |message| wrap(id, Message::Ssh(message)));
        self.quick_connect = Some(dialog);
        let load = Task::future(async move {
            let result = profile::connections_path()
                .and_then(Store::open)
                .map_err(|error| format!("Could not load saved connections ({error})"));
            wrap(id, Message::Loaded(result))
        });
        Task::batch([open.chain(focus), load])
    }

    pub(super) fn quick_connect_title(&self, id: window::Id) -> Option<&'static str> {
        self.quick_connect
            .as_ref()
            .is_some_and(|dialog| dialog.id == id)
            .then_some(TITLE)
    }

    pub(super) fn quick_connect_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.quick_connect
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn close_quick_connect(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self
            .quick_connect
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        if dialog.saving {
            return Some(Task::none());
        }
        let owner = dialog.owner;
        self.quick_connect = None;
        Some(self.close_placed_dialog_and_focus(id, owner))
    }

    pub(super) fn take_quick_connect_for_owner(&mut self, owner: window::Id) -> Option<window::Id> {
        let id = self.quick_connect_for_owner(owner)?;
        self.quick_connect = None;
        Some(id)
    }

    pub(super) fn refresh_quick_connect(&mut self, store: &Store) {
        if let Some(dialog) = &mut self.quick_connect {
            if dialog
                .group
                .as_deref()
                .is_some_and(|name| !store.groups().any(|g| g == name))
            {
                dialog.group = None;
            }
            dialog.store = Some(store.clone());
        }
    }

    fn quick_protocols(&self, split: bool) -> Vec<Protocol> {
        if self.clipboard.policy.problem.is_some() {
            return Vec::new();
        }
        let mut protocols = vec![Protocol::Ssh, Protocol::Local];
        if self.serial_profiles_allowed() {
            protocols.push(Protocol::Serial);
        }
        if !split {
            protocols.push(Protocol::Sftp);
            if !self.isolation_required() {
                protocols.extend([Protocol::Tn3270, Protocol::Tn5250]);
            }
        }
        protocols
    }

    pub(super) fn quick_temporary_key_enabled(&self) -> bool {
        self.window_settings.temporary_ssh_key_enabled && self.clipboard.policy.problem.is_none()
    }

    pub(super) fn sync_quick_temporary_key_preference(&mut self) {
        let enabled = self.quick_temporary_key_enabled();
        if let Some(dialog) = &mut self.quick_connect {
            dialog.set_temporary_key_enabled(enabled);
        }
        if !enabled {
            self.discard_quick_temporary_ssh_retries();
        }
    }

    fn quick_logging_available(&self, dialog: &Dialog) -> bool {
        !dialog.saving
            && dialog.section_visible(Section::Logging)
            && self.clipboard.policy.problem.is_none()
            && !dialog.selected_ssh_incognito()
            && dialog.split.is_none_or(|target| {
                self.split_connection_privacy(dialog.owner, target, false) == Ok(false)
            })
    }

    pub(super) fn update_quick_connect(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let split = self
            .quick_connect
            .as_ref()
            .is_some_and(|dialog| dialog.split.is_some());
        let allowed = self.quick_protocols(split);
        let temporary_key_enabled = self.quick_temporary_key_enabled();
        let internal_clipboard = self.clipboard.policy.internal;
        let effects_enabled = self.terminal_effects_enabled();
        let ai = self.ai_configuration();
        let themes = &self.window_settings.themes;
        let preferences = &self.window_settings.terminal_effects;
        let logging_available = self
            .quick_connect
            .as_ref()
            .is_some_and(|dialog| self.quick_logging_available(dialog));
        let Some(dialog) = self.quick_connect.as_mut().filter(|dialog| dialog.id == id) else {
            return Task::none();
        };
        let owner = dialog.owner;
        if dialog.saving
            && !matches!(
                message,
                Message::Saved(_) | Message::TerminalLogFolderPicked(..)
            )
        {
            return Task::none();
        }
        dialog.set_temporary_key_enabled(temporary_key_enabled);
        match message {
            Message::Loaded(Ok(store)) => {
                if dialog.store.is_none() {
                    dialog.store = Some(store);
                }
            }
            Message::Loaded(Err(error)) => dialog.status = Some(error),
            Message::Saved(Ok((store, id))) => {
                dialog.saving = false;
                if let Some(connection) = store.get(id) {
                    dialog.selected_ssh = Some((id, connection.incognito));
                } else if store.serial(id).is_some() {
                    dialog.selected_serial = Some(id);
                } else if store.local(id).is_some() {
                    dialog.selected_local = Some(id);
                }
                dialog.save = false;
                self.update_connection_store(store);
                return self.quick_submit();
            }
            Message::Saved(Err(error)) => {
                dialog.saving = false;
                dialog.status = Some(error);
            }
            Message::Query(query) if query.len() <= 240 => dialog.query = query,
            Message::Select(connection) if dialog.choose(connection, temporary_key_enabled) => {
                return dialog.focus_selected();
            }
            Message::Recent(connection) if split => {
                if dialog.choose(connection, temporary_key_enabled) {
                    if dialog.can_connect() {
                        return self.quick_submit();
                    }
                    return dialog.focus_selected();
                }
            }
            Message::Recent(connection) => {
                if dialog.store.as_ref().is_some_and(|store| {
                    store.get(connection).is_some()
                        || store.serial(connection).is_some()
                        || store.local(connection).is_some()
                }) {
                    let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
                    return Task::batch([close, self.open_recent_connection(owner, connection)]);
                }
            }
            Message::GroupTab(group) if !split => {
                dialog.group_tab = group;
                if group {
                    dialog.ssh.logging_form_mut().cancel_folder_picker();
                }
                dialog.set_temporary_key_enabled(temporary_key_enabled);
            }
            Message::Group(group) if !split && dialog.group_members(&group) > 0 => {
                dialog.group = Some(group)
            }
            Message::Protocol(protocol) if allowed.contains(&protocol) => {
                let changed = protocol != dialog.protocol;
                if protocol != dialog.protocol
                    && (matches!(protocol, Protocol::Serial | Protocol::Local)
                        || matches!(dialog.protocol, Protocol::Serial | Protocol::Local))
                {
                    dialog.save = false;
                    dialog.name.clear();
                }
                dialog.protocol = protocol;
                dialog.set_temporary_key_enabled(temporary_key_enabled);
                if changed {
                    dialog.ssh.logging_form_mut().cancel_folder_picker();
                    dialog.reset_effect_draft();
                    dialog.reset_ai_draft();
                    dialog.reset_appearance_draft();
                }
                if let Some(models) = protocol.ibm_models()
                    && !models.contains(&dialog.ibm.kind)
                {
                    dialog.ibm = new_ibm(models[0]);
                }
                if protocol == Protocol::Serial {
                    return dialog
                        .serial
                        .refresh()
                        .map(move |message| wrap(id, Message::Serial(message)));
                }
            }
            Message::Profile(profile) if dialog.protocol == Protocol::Local => {
                dialog.local_emulation = profile;
            }
            Message::Profile(profile) => {
                dialog.ssh.update(ssh_dialog::Message::Profile(profile));
                let _ = dialog
                    .serial
                    .update(serial_dialog::Message::Profile(profile));
            }
            Message::Font(font)
                if (profile::MIN_TERMINAL_FONT..=profile::MAX_TERMINAL_FONT).contains(&font) =>
            {
                dialog.appearance.style.font_size = Some(font);
                dialog.appearance_edited = true;
            }
            Message::Theme(theme)
                if theme
                    .as_ref()
                    .is_none_or(|theme| theme.resolved_theme(themes).is_some()) =>
            {
                if theme.is_some() {
                    dialog.appearance.foreground.clear();
                    dialog.appearance.background.clear();
                    dialog.appearance.style.foreground = None;
                    dialog.appearance.style.background = None;
                    dialog.appearance.style.font_family = None;
                    dialog.appearance.style.font_size = None;
                }
                dialog.appearance.style.theme = theme;
                dialog.appearance_edited = true;
            }
            Message::FontFamily(family)
                if family
                    .as_ref()
                    .is_none_or(|name| crate::terminal_theme::installed_font(name).is_some()) =>
            {
                dialog.appearance.style.font_family = family;
                dialog.appearance_edited = true;
            }
            Message::Foreground(value) if value.len() <= 16 => {
                dialog.appearance.style.foreground = crate::terminal_theme::Rgb::parse(&value);
                dialog.appearance.foreground = value;
                dialog.appearance_edited = true;
            }
            Message::Background(value) if value.len() <= 16 => {
                dialog.appearance.style.background = crate::terminal_theme::Rgb::parse(&value);
                dialog.appearance.background = value;
                dialog.appearance_edited = true;
            }
            Message::TerminalColors(enabled) => {
                dialog.appearance.style.sgr_enabled = Some(enabled);
                dialog.appearance_edited = true;
            }
            Message::ToggleSection(section)
                if dialog.section_visible(section)
                    && (section != Section::Effect || effects_enabled)
                    && (section != Section::Ai || ai.is_some()) =>
            {
                let expanded = self
                    .window_settings
                    .quick_connect_expanded_sections
                    .get_or_insert_default();
                if !expanded.remove(&section) {
                    expanded.insert(section);
                }
                self.save_window_settings(Some(owner));
            }
            Message::AiProfile(profile)
                if dialog.section_visible(Section::Ai)
                    && ai.as_ref().is_some_and(|config| {
                        profile
                            .as_ref()
                            .is_none_or(|reference| ai::profile_available(config, reference))
                    }) =>
            {
                dialog.ai.profile = profile;
                dialog.ai_edited = true;
            }
            Message::AiSkill(id, enabled)
                if dialog.section_visible(Section::Ai)
                    && ai.as_ref().is_some_and(|config| {
                        !enabled
                            || config
                                .connection_skill_choices()
                                .iter()
                                .any(|(available, _)| *available == id)
                    }) =>
            {
                if enabled {
                    if dialog.ai.skills.len() < MAX_ASSIGNED_AI_SKILLS
                        && !dialog.ai.skills.contains(&id)
                    {
                        dialog.ai.skills.push(id);
                        dialog.ai_edited = true;
                    }
                } else {
                    dialog.ai.skills.retain(|skill| *skill != id);
                    dialog.ai_edited = true;
                }
            }
            Message::Effect(effect)
                if effects_enabled
                    && dialog.section_visible(Section::Effect)
                    && effect.is_none_or(|effect| preferences.allows(effect)) =>
            {
                dialog.effect.effect = effect;
                dialog.effect_edited = true;
            }
            Message::EffectSpeed(speed)
                if effects_enabled
                    && dialog.section_visible(Section::Effect)
                    && dialog
                        .effect
                        .effect
                        .is_some_and(|effect| preferences.allows(effect)) =>
            {
                dialog.effect.set_speed(speed);
                dialog.effect_edited = true;
            }
            Message::LocalShell(shell)
                if dialog.protocol == Protocol::Local
                    && !dialog.group_tab
                    && kortty_pty::available_local_shells().contains(&shell) =>
            {
                dialog.local_shell = shell;
                dialog.shell_integration &= validate_shell_integration(shell, None).is_ok();
            }
            Message::Local(edit) if dialog.protocol == Protocol::Local && !dialog.group_tab => {
                dialog.local.update(edit);
                if let Some(profile) = dialog.local_profile() {
                    dialog.shell_integration &=
                        validate_shell_integration(profile.shell, profile.command.as_deref())
                            .is_ok();
                }
            }
            Message::ShellIntegration(enabled) if dialog.protocol == Protocol::Local => {
                dialog.shell_integration = enabled;
            }
            Message::AdHocLocal if dialog.protocol == Protocol::Local => {
                dialog.selected_local = None;
                dialog.local = local::Draft::default();
                dialog.local_shell = LocalShell::default();
                dialog.shell_integration = false;
                dialog.save = false;
                dialog.name.clear();
                dialog.reset_effect_draft();
                dialog.reset_ai_draft();
                dialog.reset_appearance_draft();
            }
            Message::Ssh(ssh_dialog::Message::Connect) if dialog.protocol.is_ssh() => {
                return self.quick_submit();
            }
            Message::Ssh(ssh_dialog::Message::OpenVault) => return self.show_vault(owner),
            Message::Ssh(ssh_dialog::Message::PasteTemporaryKey) => {
                return self.paste_quick_temporary_key(id);
            }
            Message::TemporaryKeyPasted(request, captured_internal, key) => {
                let key = if captured_internal == internal_clipboard {
                    key
                } else {
                    ssh_dialog::Password::default()
                };
                dialog.ssh.finish_temporary_key_paste(request, key);
            }
            Message::BrowseTerminalLogFolder if logging_available => {
                let root = match profile::root() {
                    Ok(root) => root,
                    Err(error) => {
                        dialog.status =
                            Some(format!("Could not locate the log directory ({error})"));
                        return Task::none();
                    }
                };
                let Some((ticket, directory)) = dialog
                    .ssh
                    .logging_form_mut()
                    .start_folder_picker(logging_available, &root)
                else {
                    return Task::none();
                };
                return crate::terminal_logging::pick_folder(id, directory).map(move |path| {
                    wrap(id, Message::TerminalLogFolderPicked(ticket.clone(), path))
                });
            }
            Message::TerminalLogFolderPicked(ticket, path) => {
                let logging = dialog.ssh.logging_form_mut();
                if let Some(edit) =
                    logging.finish_folder_picker(&ticket, logging_available, path.as_deref())
                {
                    logging.update(edit);
                }
            }
            Message::Ssh(ssh_dialog::Message::Logging(edit)) => {
                let logging = dialog.ssh.logging_form_mut();
                if logging.accepts_editor_edit(logging_available, &edit) {
                    logging.update(edit);
                }
            }
            Message::Ssh(message) => {
                dialog.ssh.update(message);
                if dialog.ssh.temporary_key_selected() {
                    dialog.save = false;
                }
            }
            Message::Serial(serial_dialog::Message::Connect)
                if dialog.protocol == Protocol::Serial =>
            {
                return self.quick_submit();
            }
            Message::Serial(message) => {
                return dialog
                    .serial
                    .update(message)
                    .map(move |message| wrap(id, Message::Serial(message)));
            }
            Message::IbmAddress(address) if address.len() <= 128 => {
                dialog.ibm.address = address;
                dialog.ibm.plaintext = false;
            }
            Message::IbmDevice(kind)
                if dialog
                    .protocol
                    .ibm_models()
                    .is_some_and(|models| models.contains(&kind)) =>
            {
                dialog.ibm.kind = kind;
                dialog.ibm.plaintext = false;
            }
            Message::IbmName(name) if name.len() <= 64 => {
                dialog.ibm.device_name = name;
                dialog.ibm.plaintext = false;
            }
            Message::IbmPlaintext(allow) => dialog.ibm.plaintext = allow,
            Message::SaveConnection(save) if !dialog.ssh.temporary_key_selected() => {
                dialog.save = save;
            }
            Message::ConnectionName(name) if name.len() <= 160 => dialog.name = name,
            Message::Reset => {
                dialog.appearance = appearance::Draft::default();
                dialog.appearance_edited = true;
            }
            Message::Connect => return self.quick_submit(),
            Message::LoadProject if !split => {
                let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
                return Task::batch([
                    close,
                    self.update(Desktop::Project(owner, project_access::Message::Show)),
                ]);
            }
            Message::Manage if !split => {
                self.quick_connect = None;
                self.connection_manager_open_after_close.insert(id, owner);
                return self.close_placed_dialog(id);
            }
            Message::Focus => return window::gain_focus(id),
            Message::Cancel => return self.close_quick_connect(id).unwrap_or_else(Task::none),
            _ => {}
        }
        Task::none()
    }

    fn paste_quick_temporary_key(&mut self, id: window::Id) -> Task<Desktop> {
        let request = self.allocate();
        let internal = self.clipboard.policy.internal;
        let Some(dialog) = &mut self.quick_connect else {
            return Task::none();
        };
        if !dialog.ssh.temporary_key_selected() {
            return Task::none();
        }
        if dialog.ssh.target_endpoint().is_none() {
            dialog.status = Some("Enter the SSH host and username before pasting a key.".into());
            return Task::none();
        }
        if !dialog.ssh.begin_temporary_key_paste(request) {
            return Task::none();
        }
        dialog.status = None;
        self.read_clipboard().map(move |value| {
            wrap(
                id,
                Message::TemporaryKeyPasted(
                    request,
                    internal,
                    ssh_dialog::Password::new(value.unwrap_or_default()),
                ),
            )
        })
    }

    fn quick_submit(&mut self) -> Task<Desktop> {
        let Some(dialog) = self
            .quick_connect
            .as_ref()
            .filter(|dialog| dialog.can_connect())
        else {
            return Task::none();
        };
        let (id, owner) = (dialog.id, dialog.owner);
        if !self.windows.contains_key(&owner) {
            self.quick_connect = None;
            return self.close_placed_dialog(id);
        }
        let incognito = dialog.protocol == Protocol::Ssh && dialog.selected_ssh_incognito();
        let incognito = if let Some(target) = dialog.split {
            match self.split_connection_privacy(owner, target, incognito) {
                Ok(value) => value,
                Err(reason) => {
                    if let Some(dialog) = self.quick_connect.as_mut() {
                        dialog.status = Some(reason.into());
                    }
                    return Task::none();
                }
            }
        } else {
            incognito
        };
        let protocols = self.quick_protocols(dialog.split.is_some());
        let style = dialog.appearance.style.clone();
        let launch_style = (dialog.section_visible(Section::Appearance)
            && (dialog.appearance_edited
                || (dialog.selected_profile_id().is_none() && !style.is_default())))
        .then(|| style.clone());
        let font = launch_style.as_ref().and_then(|style| style.font_size);
        let enabled = self.terminal_effects_enabled();
        let effect_override = dialog.effect_override(enabled).map(|selection| {
            selection.filter(|selection| self.allowed_terminal_effect(selection.effect))
        });
        let selected_effect = dialog.effect.selection().filter(|selection| {
            dialog.section_visible(Section::Effect)
                && self.allowed_terminal_effect(selection.effect)
        });
        let ai_enabled = self.ai_features_enabled();
        let ai = (dialog.ai_edited && dialog.section_visible(Section::Ai) && ai_enabled)
            .then(|| dialog.ai.clone());
        let Some(dialog) = self.quick_connect.as_mut() else {
            return Task::none();
        };
        if dialog.group_tab {
            let Some(group) = dialog.group.clone() else {
                return Task::none();
            };
            let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
            return Task::batch([close, self.open_saved_group(owner, &group)]);
        }
        if !protocols.contains(&dialog.protocol) {
            dialog.status = Some("Managed policy blocks this connection type.".into());
            return Task::none();
        }
        if dialog.protocol == Protocol::Ssh && incognito && !self.clipboard.policy.incognito_allowed
        {
            dialog.status = Some("Managed policy blocks this incognito connection.".into());
            return Task::none();
        }
        if dialog.protocol == Protocol::Ssh && dialog.save {
            let mut draft = match dialog.ssh.saved_draft(&dialog.name) {
                Ok(draft) => draft,
                Err(error) => {
                    dialog.status =
                        Some(format!("Could not prepare the saved connection ({error})"));
                    return Task::none();
                }
            };
            if !dialog.selected().is_some_and(|saved| {
                saved.matches_route(
                    &draft.login.endpoint,
                    draft.jump.as_ref().map(|login| &login.endpoint),
                )
            }) {
                draft.incognito = dialog.selected_ssh_incognito();
                draft.terminal_effect = selected_effect;
                style.write_terminal(&mut draft.terminal);
                if ai_enabled {
                    dialog.ai.write_terminal(&mut draft.terminal);
                }
                dialog.saving = true;
                dialog.status = None;
                return Task::future(async move {
                    let result = profile::connections_path()
                        .and_then(Store::open)
                        .and_then(|mut store| {
                            let id = store.save(draft)?;
                            Ok((store, id))
                        })
                        .map_err(|error| format!("Could not save the connection ({error})"));
                    wrap(id, Message::Saved(result))
                });
            }
        }
        if dialog.protocol == Protocol::Serial && dialog.save {
            let Ok((connection, terminal)) = dialog.serial.validated() else {
                return Task::none();
            };
            let mut draft =
                crate::connection_store::SerialProfile::new(&dialog.name, connection, terminal);
            draft.terminal_effect = selected_effect;
            draft.appearance = (!style.is_default()).then(|| Box::new(style.clone()));
            if ai_enabled {
                draft.terminal_ai.clone_from(&dialog.ai);
            }
            dialog.saving = true;
            dialog.status = None;
            return Task::future(async move {
                let result = profile::connections_path()
                    .and_then(Store::open)
                    .and_then(|mut store| {
                        let id = store.save_serial(draft)?;
                        Ok((store, id))
                    })
                    .map_err(|error| format!("Could not save the connection ({error})"));
                wrap(id, Message::Saved(result))
            });
        }
        if dialog.protocol == Protocol::Local && dialog.save {
            if incognito {
                dialog.status = Some("A private terminal cannot save a shell profile.".into());
                return Task::none();
            }
            let Some(mut draft) = dialog.local_profile() else {
                return Task::none();
            };
            draft.id = 0;
            if !dialog.name.trim().is_empty() {
                draft.name = dialog.name.trim().into();
            }
            draft.terminal_effect = selected_effect;
            style.write_terminal(&mut draft.terminal);
            dialog.saving = true;
            dialog.status = None;
            let source = dialog.selected_local;
            return Task::future(async move {
                let result = profile::connections_path()
                    .and_then(Store::open)
                    .and_then(|mut store| {
                        let id = if let Some(source) = source {
                            if store.local(source).is_none() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::NotFound,
                                    "The saved local shell profile no longer exists",
                                ));
                            }
                            let group = store.group_of(source).map(str::to_owned);
                            store.save_local_in_group(draft, group.as_deref())?
                        } else {
                            store.save_local(draft)?
                        };
                        Ok((store, id))
                    })
                    .map_err(|error| format!("Could not save the shell profile ({error})"));
                wrap(id, Message::Saved(result))
            });
        }
        let Some(dialog) = self.quick_connect.take() else {
            return Task::none();
        };
        let saved_ssh = dialog.selected().map(|connection| connection.id);
        let mut save_preferences = false;
        if !incognito
            && dialog.section_visible(Section::Appearance)
            && self.window_settings.quick_connect_appearance.as_ref() != Some(&style)
        {
            self.window_settings.quick_connect_appearance = Some(style);
            save_preferences = true;
        }
        if !incognito && enabled && dialog.section_visible(Section::Effect) {
            let speed = selected_effect.map(|effect| effect.speed);
            if self.window_settings.quick_connect_effect_speed != speed {
                self.window_settings.quick_connect_effect_speed = speed;
                save_preferences = true;
            }
        }
        if save_preferences {
            self.save_window_settings(Some(owner));
        }
        let start = match dialog.protocol {
            Protocol::Ssh | Protocol::Sftp => self.start_quick_ssh(
                owner,
                dialog.ssh,
                super::ssh_access::QuickLaunch {
                    saved: saved_ssh,
                    sftp: dialog.protocol == Protocol::Sftp,
                    incognito,
                    font,
                    appearance: launch_style,
                    effect: effect_override,
                    ai,
                    split: dialog.split,
                    ..Default::default()
                },
            ),
            Protocol::Local => {
                let profile = dialog.local_emulation;
                let selected = dialog
                    .selected_local
                    .zip(dialog.store.as_ref().map(|store| store.path().to_owned()));
                let view =
                    super::LocalLaunchView::QuickConnect(effect_override, launch_style, selected);
                let started = if let Some(local) = dialog.local_profile()
                    && (dialog.selected_local.is_some()
                        || local.command.is_some()
                        || local.working_directory.is_some()
                        || !local.isolation.is_global())
                {
                    self.store_saved_local_session_in_view(local, font, view)
                } else {
                    let settings = if dialog.shell_integration {
                        self.quick_shell_integration_for_command(
                            profile,
                            dialog.local_shell,
                            None,
                            None,
                        )
                        .map(Some)
                        .map_err(std::io::Error::other)
                    } else if dialog.local_shell.is_default() {
                        Ok(None)
                    } else {
                        self.local_settings_for_shell(dialog.local_shell).map(Some)
                    };
                    settings.and_then(|settings| {
                        self.store_local_session_with_grid(profile, font, settings, view, false)
                    })
                };
                match started {
                    Ok(session) => {
                        if let Some(pane) = self.sessions.get_mut(&session) {
                            pane.quick_ai = ai;
                        }
                        if let Some(target) = dialog.split {
                            self.start_split_worker_with_effect(
                                owner,
                                target,
                                session,
                                effect_override.is_none(),
                            );
                        } else {
                            self.add_tab(owner, session);
                        }
                        self.record_local_session_used(session)
                    }
                    Err(error) => {
                        self.notice(owner, format!("Could not start the local shell ({error})"));
                        Task::none()
                    }
                }
            }
            Protocol::Serial => {
                let saved = dialog.selected_serial().cloned();
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.pending_serial = Some(PendingSerial {
                        form: dialog.serial,
                        saved,
                        replace: None,
                        reconnect: None,
                        split: dialog.split,
                        font,
                    });
                }
                self.connect_serial_with_privacy(owner, false, effect_override, ai, launch_style)
            }
            Protocol::Tn3270 | Protocol::Tn5250 => {
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.pending_ibm = Some(PendingIbm { font, ..dialog.ibm });
                }
                self.connect_ibm_with_effect(owner, effect_override.flatten(), ai, launch_style)
            }
        };
        Task::batch([self.close_placed_dialog_and_focus(id, owner), start])
    }

    pub(super) fn open_recent_connection(&mut self, owner: window::Id, id: u64) -> Task<Desktop> {
        let store = profile::connections_path().and_then(Store::open).ok();
        if let Some(serial) = store.as_ref().and_then(|store| store.serial(id)).cloned() {
            return self.open_saved_serial(owner, serial);
        }
        if let Some(local) = store.as_ref().and_then(|store| store.local(id)).cloned() {
            return self.open_saved_local(owner, local);
        }
        let Some(connection) = store.and_then(|store| store.get(id).cloned()) else {
            self.notice(owner, "The saved connection no longer exists.".into());
            return Task::none();
        };
        if needs_entered_password(&connection) {
            return self.show_saved_ssh(owner, connection, false);
        }
        self.start_group_member(owner, connection, VecDeque::new())
    }

    pub(super) fn open_saved_group(&mut self, owner: window::Id, group: &str) -> Task<Desktop> {
        let store = match profile::connections_path().and_then(Store::open) {
            Ok(store) => store,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not open the connection group ({error})"),
                );
                return Task::none();
            }
        };
        let mut members = VecDeque::new();
        let mut skipped = Vec::new();
        let mut blocked = Vec::new();
        for connection in store
            .connections()
            .iter()
            .filter(|connection| store.group_of(connection.id) == Some(group))
        {
            if let Some((host, port)) =
                connection.blocked_server(self.clipboard.policy.server_access())
            {
                let endpoint = format!("{host}:{port}");
                if !blocked.contains(&endpoint) {
                    blocked.push(endpoint);
                }
            } else if needs_entered_password(connection)
                || (connection.incognito && !self.clipboard.policy.incognito_allowed)
            {
                skipped.push(connection.name.as_str());
            } else {
                members.push_back(connection.id);
            }
        }
        let mut launches = vec![self.advance_quick_group(owner, members)];
        for profile in store
            .local_profiles()
            .iter()
            .filter(|profile| store.group_of(profile.id) == Some(group))
        {
            launches.push(self.open_saved_local(owner, profile.clone()));
        }
        for profile in store
            .serial_profiles()
            .iter()
            .filter(|profile| store.group_of(profile.id) == Some(group))
        {
            launches.push(self.open_saved_serial(owner, profile.clone()));
        }
        let mut notices = Vec::new();
        if !skipped.is_empty() {
            notices.push(format!(
                "Skipped connections requiring a password or blocked by policy: {}.",
                skipped.join(", ")
            ));
        }
        if !blocked.is_empty() {
            notices.push(format!(
                "Your organization's server policy blocks {}.",
                blocked.join(", ")
            ));
        }
        if !notices.is_empty() {
            self.notice(owner, notices.join(" "));
        }
        Task::batch(launches)
    }

    pub(super) fn advance_quick_group(
        &mut self,
        owner: window::Id,
        mut remaining: VecDeque<u64>,
    ) -> Task<Desktop> {
        while let Some(id) = remaining.pop_front() {
            if let Some(connection) = self.saved_connection(id)
                && !needs_entered_password(&connection)
                && (!connection.incognito || self.clipboard.policy.incognito_allowed)
                && self.blocked_saved_server(&connection).is_none()
            {
                return self.start_group_member(owner, connection, remaining);
            }
        }
        Task::none()
    }

    fn start_group_member(
        &mut self,
        owner: window::Id,
        connection: Connection,
        remaining: VecDeque<u64>,
    ) -> Task<Desktop> {
        let Some(profile) = connection.profile() else {
            return Task::none();
        };
        let isolation = match self.saved_launch_isolation(&connection, None, None) {
            Ok(level) => level,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not resolve session isolation ({error})"),
                );
                return Task::none();
            }
        };
        let form = SshDialog::from_saved(profile, &connection)
            .with_isolation(isolation, self.clipboard.policy.minimum_isolation);
        self.start_quick_ssh(
            owner,
            form,
            super::ssh_access::QuickLaunch {
                saved: Some(connection.id),
                incognito: connection.incognito,
                group: remaining,
                ..Default::default()
            },
        )
    }
}

fn needs_entered_password(connection: &Connection) -> bool {
    std::iter::once(&connection.login)
        .chain(connection.jump.as_ref())
        .any(|login| login.endpoint.validate().is_err() || login.auth.requires_interactive())
}

fn blank_ssh(profile: TerminalProfile, minimum: IsolationLevel) -> SshDialog {
    SshDialog::new(profile)
        .with_isolation(IsolationLevel::None, minimum)
        .with_startup(ConnectOptions::QUICK)
}

fn new_ibm(kind: IbmKind) -> PendingIbm {
    PendingIbm {
        kind,
        replace: None,
        reconnect: None,
        font: None,
        address: String::new(),
        device_name: String::new(),
        plaintext: false,
    }
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::QuickConnect(id, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_dialog::{LoginMessage, Password};

    #[test]
    fn quick_connect_applies_the_default_to_new_ssh_and_sftp_logins() {
        let mut workspace = Workspace::default();
        workspace.window_settings.default_session_isolation = Some(IsolationLevel::Process);
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        for message in [
            LoginMessage::Host("isolation.test".into()),
            LoginMessage::Username("operator".into()),
            LoginMessage::Password(Password::new("fixture".into())),
        ] {
            let _ = workspace
                .update_quick_connect(id, Message::Ssh(ssh_dialog::Message::Target(message)));
        }
        let draft = workspace
            .quick_connect
            .as_ref()
            .unwrap()
            .ssh
            .saved_draft("fixture")
            .unwrap();
        assert_eq!(draft.isolation, IsolationLevel::Process);
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Sftp));
        let request = workspace
            .quick_connect
            .as_mut()
            .unwrap()
            .ssh
            .take_request()
            .unwrap();
        assert_eq!(request.isolation, IsolationLevel::Process);
    }

    #[test]
    fn quick_temporary_key_follows_the_preference_and_drops_stale_paste_results() {
        use crate::ssh_dialog::Method;
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action, clipboard, task};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let select = Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Authentication(
            Method::TemporaryKey,
        )));
        let _ = workspace.update_quick_connect(id, select.clone());
        assert!(
            !workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .ssh
                .temporary_key_selected()
        );

        workspace.window_settings.temporary_ssh_key_enabled = true;
        workspace.sync_quick_temporary_key_preference();
        let host =
            |name: &str| Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Host(name.into())));
        let _ = workspace.update_quick_connect(id, host("first.test"));
        let _ = workspace.update_quick_connect(
            id,
            Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Username(
                "fixture".into(),
            ))),
        );
        let _ = workspace.update_quick_connect(id, Message::SaveConnection(true));
        let _ = workspace.update_quick_connect(id, select);
        let _ = workspace.update_quick_connect(id, Message::SaveConnection(true));
        assert!(!workspace.quick_connect.as_ref().unwrap().save);

        let clipboard_result = |workspace: &mut Workspace| {
            let paste = Message::Ssh(ssh_dialog::Message::PasteTemporaryKey);
            let mut actions = task::into_stream(workspace.update_quick_connect(id, paste)).unwrap();
            let Some(Action::Clipboard(clipboard::Action::Read { channel, .. })) =
                block_on(actions.next())
            else {
                panic!("expected a clipboard request for the selected temporary-key method");
            };
            let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nfixture-only\n-----END OPENSSH PRIVATE KEY-----";
            assert!(channel.send(Some(key.into())).is_ok());
            let Some(Action::Output(Desktop::QuickConnect(_, result))) = block_on(actions.next())
            else {
                panic!("expected a Quick Connect paste result");
            };
            result
        };
        let stale = clipboard_result(&mut workspace);
        let _ = workspace.update_quick_connect(id, host("second.test"));
        let _ = workspace.update_quick_connect(id, stale);
        assert!(!workspace.quick_connect.as_ref().unwrap().ssh.can_connect());

        let current = clipboard_result(&mut workspace);
        let _ = workspace.update_quick_connect(id, current);
        let ssh = &workspace.quick_connect.as_ref().unwrap().ssh;
        assert!(ssh.can_connect());
        assert!(ssh.saved_draft("Fixture").is_err());

        let late = clipboard_result(&mut workspace);
        workspace.window_settings.temporary_ssh_key_enabled = false;
        workspace.sync_quick_temporary_key_preference();
        let _ = workspace.update_quick_connect(id, late);
        let ssh = &workspace.quick_connect.as_ref().unwrap().ssh;
        assert!(!ssh.temporary_key_selected());
        assert!(!ssh.can_connect());
    }

    #[test]
    fn quick_local_shell_setup_respects_the_global_choice_without_disabling_marks() {
        let mut workspace = Workspace::default();
        workspace.window_settings.automatic_local_shell_integration = Some(false);
        assert_eq!(
            workspace
                .quick_shell_integration(TerminalProfile::Xterm, LocalShell::Default)
                .unwrap_err(),
            "Automatic setup for local shells is disabled in Settings → Terminal."
        );
        assert!(workspace.window_settings.prompt_navigation_enabled);
    }

    #[cfg(windows)]
    #[test]
    fn quick_local_shell_integration_preserves_windows_shell_and_security_gates() {
        let mut workspace = Workspace::default();
        workspace.window_settings.prompt_navigation_enabled = true;
        for shell in [
            LocalShell::Default,
            LocalShell::PowerShell,
            LocalShell::CommandPrompt,
        ] {
            let settings = workspace
                .quick_shell_integration(TerminalProfile::Xterm, shell)
                .unwrap();
            assert_eq!(settings.shell, shell);
            assert!(settings.shell_integration);
        }
        workspace.window_settings.prompt_navigation_enabled = false;
        assert!(
            workspace
                .quick_shell_integration(TerminalProfile::Xterm, LocalShell::PowerShell)
                .is_err()
        );
        workspace.window_settings.prompt_navigation_enabled = true;
        workspace.clipboard.policy.minimum_isolation = IsolationLevel::Sandbox;
        assert!(
            workspace
                .quick_shell_integration(TerminalProfile::Xterm, LocalShell::CommandPrompt)
                .is_err()
        );
    }

    #[test]
    fn quick_connect_keeps_each_saved_profiles_identity_when_switching_protocols() {
        use crate::connection_store::SerialProfile;
        use crate::session::SerialLineEnding;
        use kortty_terminal::TextEncoding;

        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "kortty-quick-serial-{}",
            crate::private_files::nonce().unwrap()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
        let mut ssh = blank_ssh(TerminalProfile::Xterm, IsolationLevel::None);
        ssh.update(ssh_dialog::Message::Target(LoginMessage::Host(
            "private.test".into(),
        )));
        ssh.update(ssh_dialog::Message::Target(LoginMessage::Password(
            Password::new("fixture".into()),
        )));
        let mut private = ssh.saved_draft("Private").unwrap();
        private.incognito = true;
        let private = store.save(private).unwrap();
        let connection = SerialConnection {
            device: if cfg!(windows) {
                "COM7"
            } else {
                "/dev/test-serial"
            }
            .into(),
            line_ending: SerialLineEnding::Lf,
            local_echo: true,
            encoding: Some(TextEncoding::Latin1),
            ..SerialConnection::default()
        };
        let saved = store
            .save_serial(SerialProfile::new(
                "Router",
                connection.clone(),
                TerminalProfile::Vt220,
            ))
            .unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Loaded(Ok(store)));
        let _ = workspace.update_quick_connect(id, Message::Select(private));
        let _ = workspace.update_quick_connect(
            id,
            Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Password(
                Password::new("fixture".into()),
            ))),
        );
        let _ = workspace.update_quick_connect(id, Message::Select(saved));
        let dialog = workspace.quick_connect.as_ref().unwrap();
        assert_eq!(dialog.protocol, Protocol::Serial);
        assert_eq!(dialog.selected_profile_id(), Some(saved));
        assert_eq!(dialog.selected_serial().unwrap().id, saved);
        assert_eq!(
            dialog.serial.validated(),
            Ok((connection.clone(), TerminalProfile::Vt220))
        );
        assert!(dialog.can_connect());

        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Ssh));
        let dialog = workspace.quick_connect.as_ref().unwrap();
        assert_eq!(dialog.selected_profile_id(), Some(private));
        assert!(dialog.can_connect());
        workspace.clipboard.policy.incognito_allowed = false;
        let _ = workspace.update_quick_connect(id, Message::Connect);
        assert_eq!(
            workspace.quick_connect.as_ref().unwrap().status.as_deref(),
            Some("Managed policy blocks this incognito connection.")
        );

        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Serial));
        let dialog = workspace.quick_connect.as_ref().unwrap();
        assert_eq!(dialog.selected_profile_id(), Some(saved));
        assert!(
            dialog
                .selected_serial()
                .unwrap()
                .matches(&connection, TerminalProfile::Vt220)
        );
    }

    #[test]
    fn quick_connect_appearance_reset_preserves_the_ssh_login() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).expect("dialog");
        for message in [
            Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Host(
                "Host.Example".into(),
            ))),
            Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Password(
                Password::new("transient login password".into()),
            ))),
            Message::Font(37),
            Message::Reset,
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }
        let dialog = workspace.quick_connect.as_mut().expect("dialog");
        assert_eq!(dialog.appearance.style.font_size, None);
        let saved = dialog.ssh.saved_draft("").expect("saved profile");
        assert_eq!(saved.name, "root@host.example");
        assert_eq!(saved.login.auth, Auth::Prompt);
        let json = serde_json::to_string(&saved).expect("profile JSON");
        assert!(!json.contains("transient login password"));
        let request = dialog.ssh.take_request().expect("valid SSH login");
        let ssh_dialog::Authentication::Password(password) = request.target.auth else {
            panic!("the one-use login must retain its password");
        };
        assert_eq!(password.as_str(), "transient login password");
    }

    #[test]
    fn quick_connect_uses_fixed_default_and_carries_explicit_startup_policy() {
        use crate::ssh_dialog::StartupField;

        let mut dialog = blank_ssh(TerminalProfile::Xterm, IsolationLevel::None);
        dialog.update(ssh_dialog::Message::Target(LoginMessage::Host(
            "localhost".into(),
        )));
        dialog.update(ssh_dialog::Message::Target(LoginMessage::Password(
            Password::new("fixture".into()),
        )));
        assert_eq!(
            dialog.saved_draft("Test").unwrap().startup,
            ConnectOptions::QUICK
        );
        dialog.update(ssh_dialog::Message::Startup(
            StartupField::Timeout,
            "301".into(),
        ));
        assert!(!dialog.can_connect());
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ssh_dialog::ValidationError::Startup
        );
        dialog.update(ssh_dialog::Message::Startup(
            StartupField::Timeout,
            "45".into(),
        ));
        dialog.update(ssh_dialog::Message::Startup(
            StartupField::Attempts,
            "3".into(),
        ));
        let saved = dialog.saved_draft("Test").unwrap();
        let mut loaded = SshDialog::from_saved(TerminalProfile::Xterm, &saved);
        loaded.update(ssh_dialog::Message::Target(LoginMessage::Password(
            Password::new("fixture".into()),
        )));
        assert_eq!(
            loaded.take_request().unwrap().options,
            ConnectOptions::new(45, 3).unwrap()
        );
        assert_eq!(dialog.take_request().unwrap().options, saved.startup);
    }

    #[test]
    fn quick_connect_sections_persist_across_dialogs_and_track_protocol_visibility() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(id, Message::ToggleSection(Section::Logging));
        let _ =
            workspace.update_quick_connect(id, Message::ToggleSection(Section::ConnectionSettings));
        let _ = workspace.update_quick_connect(id, Message::ToggleSection(Section::Logging));
        let _ = workspace.update_quick_connect(id, Message::Cancel);
        let _ = workspace.show_quick_connect(owner);
        let reopened = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(reopened, Message::Protocol(Protocol::Sftp));
        let _ = workspace.update_quick_connect(reopened, Message::ToggleSection(Section::Logging));
        let _ = workspace.update_quick_connect(reopened, Message::Effect(Some(Effect::Mother)));
        assert!(
            workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .effect
                .selection()
                .is_none()
        );
        let _ = workspace.update_quick_connect(id, Message::ToggleSection(Section::Appearance));

        assert_eq!(
            workspace.window_settings.quick_connect_expanded_sections,
            Some(ExpandedSections::from([Section::ConnectionSettings]))
        );
        let _ = workspace.update_quick_connect(
            reopened,
            Message::ToggleSection(Section::ConnectionSettings),
        );
        assert_eq!(
            workspace.window_settings.quick_connect_expanded_sections,
            Some(ExpandedSections::new())
        );
    }

    #[test]
    fn quick_terminal_log_folder_is_owned_and_only_updates_the_current_ssh_draft() {
        use crate::terminal_logging::Edit;
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let edit = |edit| Message::Ssh(ssh_dialog::Message::Logging(edit));
        let config = |workspace: &Workspace| {
            workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .ssh
                .logging_config()
                .unwrap()
        };
        let folder = std::env::temp_dir().join("quick-connect log fixture");
        assert!(
            task::into_stream(workspace.update_quick_connect(id, Message::BrowseTerminalLogFolder))
                .is_none()
        );
        let _ =
            workspace.update_quick_connect(id, edit(Edit::Folder(folder.to_str().unwrap().into())));
        assert_eq!(config(&workspace).custom_folder(), None);

        let _ = workspace.update_quick_connect(id, edit(Edit::Enabled(true)));
        let mut open =
            task::into_stream(workspace.update_quick_connect(id, Message::BrowseTerminalLogFolder))
                .unwrap();
        assert!(
            matches!(block_on(open.next()), Some(Action::Window(WindowAction::Run(parent, _))) if parent == id)
        );
        assert!(
            task::into_stream(workspace.update_quick_connect(id, Message::BrowseTerminalLogFolder))
                .is_none()
        );

        let _ = workspace.update_quick_connect(id, Message::GroupTab(true));
        let _ = workspace.update_quick_connect(id, Message::GroupTab(false));
        let start = |workspace: &mut Workspace| {
            workspace
                .quick_connect
                .as_mut()
                .unwrap()
                .ssh
                .logging_form_mut()
                .start_folder_picker(true, &std::env::temp_dir())
                .unwrap()
                .0
        };
        let stale = start(&mut workspace);
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Sftp));
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Ssh));
        let ticket = start(&mut workspace);
        let _ = workspace.update_quick_connect(
            id,
            Message::TerminalLogFolderPicked(stale, Some(folder.clone())),
        );
        assert_eq!(config(&workspace).custom_folder(), None);
        let _ = workspace.update_quick_connect(id, Message::TerminalLogFolderPicked(ticket, None));

        let ticket = start(&mut workspace);
        let _ = workspace.update_quick_connect(
            id,
            Message::TerminalLogFolderPicked(ticket, Some(folder.clone())),
        );
        let _ = workspace.update_quick_connect(id, edit(Edit::Enabled(false)));
        let disabled = config(&workspace);
        assert!(!disabled.enabled());
        assert_eq!(disabled.custom_folder(), Some(folder.as_path()));

        workspace.quick_connect.as_mut().unwrap().selected_ssh = Some((0, true));
        let _ = workspace.update_quick_connect(id, edit(Edit::Enabled(true)));
        assert!(!config(&workspace).enabled());
        assert!(
            task::into_stream(workspace.update_quick_connect(id, Message::BrowseTerminalLogFolder))
                .is_none()
        );
    }

    #[test]
    fn quick_connect_owner_blocks_an_in_flight_paste_and_leaves_other_windows_alone() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let session = super::super::SessionId(17);
        workspace.add_tab(owner, session);
        let tab = workspace.windows[&owner].tab().expect("tab").id;
        assert!(workspace.current_targets(owner, tab, &[session]));
        let _ = workspace.show_quick_connect(owner);
        let dialog = workspace.quick_connect_for_owner(owner).expect("dialog");
        assert!(!workspace.current_targets(owner, tab, &[session]));
        assert!(workspace.quick_connect_for_owner(other).is_none());
        let _ = workspace.update_quick_connect(dialog, Message::Cancel);
        assert!(workspace.current_targets(owner, tab, &[session]));
    }

    #[test]
    fn quick_connect_escape_cancels_from_focused_host_and_returns_to_owner() {
        use iced::advanced::clipboard;
        use iced::futures::{StreamExt, executor::block_on};
        use iced::keyboard::{self, Key, Location, Modifiers, key, key::Named};
        use iced::{Event, Font, Renderer, event, mouse};
        use iced_runtime::user_interface::Cache;
        use iced_runtime::{Action, UserInterface, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let dialog = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.finish_owned_dialog_open(dialog, owner, None);

        let key_press = |key: Key, text: Option<&str>| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: text.map(Into::into),
                repeat: false,
            })
        };
        let cancel_from_view = |workspace: &Workspace| {
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.view(dialog),
                Size::new(775.0, 610.0),
                Cache::default(),
                &mut renderer,
            );
            let focus = workspace.quick_connect.as_ref().unwrap().ssh.focus();
            let Some(Action::Widget(mut focus)) =
                block_on(task::into_stream(focus).unwrap().next())
            else {
                panic!("the host field needs an initial focus operation");
            };
            interface.operate(&renderer, focus.as_mut());

            let mut messages = Vec::new();
            let (_, statuses) = interface.update(
                &[
                    key_press(Key::Character("x".into()), Some("x")),
                    key_press(Key::Named(Named::Escape), None),
                ],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(statuses, [event::Status::Captured; 2]);
            assert!(matches!(
                messages.as_slice(),
                [Desktop::QuickConnect(id, Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Host(value)))), Desktop::QuickConnect(cancelled, Message::Cancel)]
                    if *id == dialog && *cancelled == dialog && value == "x"
            ));
            messages.pop().unwrap()
        };

        workspace.quick_connect.as_mut().unwrap().saving = true;
        let cancel = cancel_from_view(&workspace);
        assert!(task::into_stream(workspace.update(cancel)).is_none());
        assert_eq!(workspace.quick_connect_for_owner(owner), Some(dialog));

        workspace.quick_connect.as_mut().unwrap().saving = false;
        let cancel = cancel_from_view(&workspace);
        let mut actions = task::into_stream(workspace.update(cancel)).unwrap();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(matches!(
            block_on(actions.next()),
            Some(Action::Window(WindowAction::Run(id, _))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(Action::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(Action::Window(WindowAction::GainFocus(id))) if id == owner
        ));
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.owner_dialog(other).is_none());
    }

    fn split_fixture() -> (Workspace, window::Id, crate::session::SessionId) {
        use crate::session::{GridSize, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let grid = GridSize::default();
        let session = workspace.store_session(
            Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap(),
            grid,
            14,
        );
        workspace.add_tab(owner, session);
        (workspace, owner, session)
    }

    #[test]
    fn quick_split_adds_a_real_local_shell_below_the_original_pane() {
        use crate::ai_manager::{Configuration, ProfileRef};
        use iced::widget::pane_grid::{Axis, Node};
        use std::time::{Duration, Instant};

        let (mut workspace, owner, source) = split_fixture();
        workspace.set_test_ai_configuration(serde_json::from_value::<Configuration>(serde_json::json!({
            "profiles": {
                "last_id": 2, "default": 1,
                "profiles": [
                    {"id": 1, "name": "Default", "endpoint": "127.0.0.1:12345", "model": "test"},
                    {"id": 2, "name": "Quick profile", "endpoint": "127.0.0.1:12346", "model": "test"}
                ]
            },
            "skills": {"last_id": 5, "skills": {
                "5": {"name": "Quick skill", "target": "CONNECTION", "enabled": true, "content": "Read the maintenance checklist."}
            }}
        })).unwrap());
        workspace.select_terminal_effect(source, Some(Effect::Mother));
        let sibling = workspace
            .store_local_session(TerminalProfile::Xterm, Some(14), None)
            .unwrap();
        let tab = workspace.active_tab_mut(owner).unwrap();
        let (sibling_pane, _) = tab
            .grid
            .split(Axis::Vertical, tab.focused, sibling)
            .unwrap();
        tab.focused = sibling_pane;
        let tab_id = tab.id;

        let _ = workspace.show_split_connection(owner, source, Axis::Horizontal);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        for message in [
            Message::GroupTab(true),
            Message::LoadProject,
            Message::Protocol(Protocol::Sftp),
            Message::Protocol(Protocol::Tn3270),
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }
        let dialog = workspace.quick_connect.as_ref().unwrap();
        assert!(!dialog.group_tab);
        assert_eq!(dialog.protocol, Protocol::Ssh);
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        #[cfg(windows)]
        let _ = workspace.update_quick_connect(id, Message::LocalShell(LocalShell::CommandPrompt));
        for message in [
            Message::Profile(TerminalProfile::Vt220),
            Message::Font(17),
            Message::Background("#132435".into()),
            Message::TerminalColors(false),
            Message::Effect(Some(Effect::DigitalRain)),
            Message::EffectSpeed("2.5".into()),
            Message::AiProfile(Some(ProfileRef::Numeric(2))),
            Message::AiSkill(5, true),
            Message::AiSkill(99, true),
            Message::Connect,
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }

        let window = &workspace.windows[&owner];
        let pending = window.pending_split_connection.unwrap();
        assert_eq!(window.tab().unwrap().grid.len(), 2);
        workspace.wait_for_test_split(owner);
        let style = workspace.sessions[&pending]
            .quick_appearance
            .as_ref()
            .unwrap();
        assert_eq!(
            style.background,
            Some(crate::terminal_theme::Rgb::from_hex(0x132435))
        );
        assert_eq!(style.sgr_enabled, Some(false));
        assert!(workspace.sessions[&source].quick_appearance.is_none());
        let (profile, skills) = workspace.terminal_ai_assignments(&workspace.sessions[&pending]);
        assert_eq!(profile, Some(&ProfileRef::Numeric(2)));
        assert_eq!(skills, [5]);
        assert!(workspace.sessions[&source].quick_ai.is_none());
        let speed = Speed::try_from(2.5).unwrap();
        assert_eq!(
            workspace
                .pane_effect(pending)
                .map(|active| active.selection),
            Some(Selection {
                effect: Effect::DigitalRain,
                speed
            })
        );
        assert_eq!(
            workspace.window_settings.quick_connect_effect_speed,
            Some(speed)
        );

        let window = &workspace.windows[&owner];
        assert_eq!(window.tabs.len(), 1);
        let tab = window.tab().unwrap();
        assert_eq!(tab.id, tab_id);
        assert_eq!(tab.focused_session(), Some(pending));
        let Node::Split {
            axis: Axis::Vertical,
            a,
            b,
            ..
        } = tab.grid.layout()
        else {
            panic!("expected the original right split");
        };
        assert!(matches!(b.as_ref(), Node::Pane(pane) if tab.grid.get(*pane) == Some(&sibling)));
        let Node::Split {
            axis: Axis::Horizontal,
            a,
            b,
            ..
        } = a.as_ref()
        else {
            panic!("expected the new split below the source, not below the focused sibling");
        };
        assert!(matches!(a.as_ref(), Node::Pane(pane) if tab.grid.get(*pane) == Some(&source)));
        assert!(matches!(b.as_ref(), Node::Pane(pane) if tab.grid.get(*pane) == Some(&pending)));
        let new = &workspace.sessions[&pending];
        assert!(new.session.local_shell_pid().is_some());
        #[cfg(windows)]
        assert_eq!(
            new.local_security.as_ref().unwrap().shell,
            LocalShell::CommandPrompt
        );
        assert_eq!(new.frame.screen.profile, TerminalProfile::Vt220);
        assert_eq!(new.font, 17);

        let stopped = [
            new.session.stop(),
            workspace.sessions[&sibling].session.stop(),
        ];
        let deadline = Instant::now() + Duration::from_secs(6);
        while stopped.iter().any(|session| !session.is_closed()) {
            assert!(Instant::now() < deadline, "test shells did not shut down");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn quick_split_rejects_stale_source_and_does_not_redirect_to_the_new_window() {
        use iced::widget::pane_grid::Axis;

        let (mut workspace, owner, source) = split_fixture();
        let (other, _) = workspace.open_window(false);
        let tab = workspace.windows[&owner].tab().unwrap().id;
        let _ = workspace.show_split_connection(owner, source, Axis::Vertical);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        let _ = workspace.transfer(owner, Some(other), tab);
        let _ = workspace.update_quick_connect(id, Message::Connect);

        assert_eq!(workspace.sessions.len(), 1);
        assert!(workspace.windows[&owner].tabs.is_empty());
        assert_eq!(workspace.windows[&other].tab().unwrap().grid.len(), 1);
        assert_eq!(
            workspace.quick_connect.as_ref().unwrap().status.as_deref(),
            Some("The original pane is no longer available for splitting.")
        );

        let _ = workspace.update_quick_connect(id, Message::Cancel);
        let _ = workspace.show_split_connection(other, source, Axis::Vertical);
        let id = workspace.quick_connect_for_owner(other).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        let _ = workspace.update_quick_connect(id, Message::Connect);
        let pending = workspace.windows[&other].pending_split_connection.unwrap();
        let _ = workspace.transfer(other, Some(owner), tab);
        assert!(workspace.windows[&other].pending_split_connection.is_none());
        assert!(!workspace.sessions.contains_key(&pending));
        assert_eq!(workspace.windows[&owner].tab().unwrap().grid.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn quick_split_opens_a_real_serial_pty_and_discards_a_failed_device() {
        use std::io::Write as _;
        use std::time::{Duration, Instant};

        use iced::widget::pane_grid::Axis;
        use rustix::io::{FdFlags, fcntl_setfd};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

        let (mut workspace, owner, source) = split_fixture();
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        fcntl_setfd(&master, FdFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let device = ptsname(&master, Vec::new()).unwrap().into_string().unwrap();
        let mut master = std::fs::File::from(master);

        let connect = |workspace: &mut Workspace, device| {
            let _ = workspace.show_split_connection(owner, source, Axis::Vertical);
            let id = workspace.quick_connect_for_owner(owner).unwrap();
            for message in [
                Message::Protocol(Protocol::Serial),
                Message::Serial(serial_dialog::Message::Device(device)),
                Message::Connect,
            ] {
                let _ = workspace.update_quick_connect(id, message);
            }
            workspace.windows[&owner].pending_split_connection.unwrap()
        };

        let serial = connect(&mut workspace, device);
        workspace.wait_for_test_split(owner);
        assert_eq!(
            workspace.windows[&owner].tab().unwrap().focused_session(),
            Some(serial)
        );
        assert_eq!(
            workspace.sessions[&serial].frame.status,
            crate::session::Status::Running
        );
        assert!(!workspace.chrome_enabled(owner, super::super::chrome::Action::SplitRight));
        assert!(!workspace.same_server_split_allowed(owner, serial));
        master.write_all(b"serial-split-ready").unwrap();
        let until = Instant::now() + Duration::from_secs(6);
        loop {
            let _ = workspace.update(Desktop::SessionUpdate(serial));
            let row: String = workspace.sessions[&serial]
                .frame
                .screen
                .row(0)
                .unwrap()
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            if row.contains("serial-split-ready") {
                break;
            }
            assert!(
                Instant::now() < until,
                "the serial split did not receive real PTY output"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let directory = crate::ssh_test_fixture::TempDirectory::new("missing-serial-split-device");
        let missing = directory
            .0
            .join("missing")
            .into_os_string()
            .into_string()
            .unwrap();
        let failed = connect(&mut workspace, missing);
        workspace.wait_for_test_split(owner);
        assert!(!workspace.sessions.contains_key(&failed));
        assert_eq!(workspace.windows[&owner].tab().unwrap().grid.len(), 2);
        assert!(
            workspace.windows[&owner]
                .notice
                .as_deref()
                .unwrap()
                .starts_with("Could not open the split")
        );

        let connection = workspace.sessions[&serial]
            .frame
            .serial
            .as_deref()
            .unwrap()
            .clone();
        let serial_pane = workspace.windows[&owner].pane_location(serial).unwrap().1;
        let _ = workspace.close_pane(owner, serial_pane);
        let shutdown = workspace.pending_shutdown.last().unwrap().clone();
        let deadline = Instant::now() + Duration::from_secs(6);
        while !shutdown.is_closed() {
            assert!(
                Instant::now() < deadline,
                "the serial worker did not release its device"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let disconnected = workspace
            .store_disconnected_serial(connection, TerminalProfile::Vt220, 20)
            .unwrap();
        let tab = workspace.active_tab_mut(owner).unwrap();
        tab.grid
            .split(Axis::Vertical, tab.focused, disconnected)
            .unwrap();
        assert!(workspace.same_server_split_allowed(owner, disconnected));
        let _ = workspace.split_same_server(owner, disconnected, Axis::Horizontal);
        let reopened = workspace.windows[&owner].pending_split_connection.unwrap();
        workspace.wait_for_test_split(owner);
        let tab = workspace.windows[&owner].tab().unwrap();
        assert_eq!(tab.focused_session(), Some(reopened));
        assert!(tab.grid.iter().any(|(_, session)| *session == disconnected));
        assert_eq!(
            workspace.sessions[&reopened].frame.status,
            crate::session::Status::Running
        );
        assert_eq!(
            workspace.sessions[&reopened].frame.screen.profile,
            TerminalProfile::Vt220
        );
        assert_eq!(workspace.sessions[&reopened].font, 20);
    }

    #[test]
    fn quick_connect_requires_new_plaintext_approval_after_an_ibm_edit() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).expect("dialog");
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Tn5250));
        let _ = workspace.update_quick_connect(id, Message::IbmAddress("127.0.0.1:2323".into()));
        let _ = workspace.update_quick_connect(id, Message::IbmPlaintext(true));
        assert!(workspace.quick_connect.as_ref().unwrap().can_connect());
        let _ = workspace.update_quick_connect(id, Message::IbmName("NEWDEV".into()));
        assert!(!workspace.quick_connect.as_ref().unwrap().can_connect());
    }
}
