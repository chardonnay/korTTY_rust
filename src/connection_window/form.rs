use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input, tooltip};
use iced::{Alignment, Element};
use kortty_terminal::TerminalProfile;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::{
    Auth, AuthMethod, Connection, MAX_TUNNELS, SavedLogin, TunnelConfig, TunnelRule, TunnelTarget,
};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::{
    KeepAliveEdit, KeyChoice, LoginTarget, StartupField, StartupFields, normalize_host,
};
use crate::terminal_effects::Effect;

use super::color::ColorDraft;
use super::view::ViewChoices;
use super::{CredentialChoice, Message, os_badge, terminal_overrides};

mod effects;
mod geometry;
use effects::EffectDraft;

#[derive(Clone, Debug)]
pub enum Edit {
    Name(String),
    Tag(String),
    Emulation(TerminalProfile),
    Incognito(bool),
    Isolation(IsolationLevel),
    Startup(StartupField, String),
    KeepAlive(KeepAliveEdit),
    Logging(crate::terminal_logging::Edit),
    TerminalEffect(Option<Effect>),
    TerminalEffectSpeed(String),
    CommandTimestamps(Option<bool>),
    UseTabColor(bool),
    TabColor(String),
    ToggleOsBadgePicker,
    OsBadge(Option<String>),
    PasteWarning(Option<PasteWarningMode>),
    Terminal(terminal_overrides::Edit),
    WindowGeometry(geometry::Edit),
    UseJump(bool),
    Login(bool, LoginEdit),
    AddTunnel,
    RemoveTunnel(usize),
    Tunnel(usize, TunnelEdit),
}

#[derive(Clone, Debug)]
pub enum LoginEdit {
    Host(String),
    Port(String),
    Username(String),
    Method(Method),
    KeyboardFollowUp(bool),
    Password(CredentialChoice),
    KeyPath(String),
    StoredKey(KeyChoice),
    Passphrase(CredentialChoice),
}

#[derive(Clone, Debug)]
pub enum TunnelEdit {
    Enabled(bool),
    Mode(TunnelMode),
    Bind(String),
    Port(String),
    Host(String),
    TargetPort(String),
    Description(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    #[default]
    Ask,
    Vault,
    Key,
    StoredKey,
    Interactive,
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ask => "Ask for password",
            Self::Vault => "Vault password",
            Self::Key => "SSH private key",
            Self::StoredKey => "Registered SSH key",
            Self::Interactive => "Keyboard-interactive",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TunnelMode {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl fmt::Display for TunnelMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Dynamic => "Dynamic (SOCKS)",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimestampChoice(Option<bool>);

impl TimestampChoice {
    const ALL: [Self; 3] = [Self(None), Self(Some(true)), Self(Some(false))];
}

impl fmt::Display for TimestampChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            None => "Use global setting",
            Some(true) => "Show",
            Some(false) => "Hide",
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct PasteChoice {
    mode: Option<PasteWarningMode>,
    default: PasteWarningMode,
}

impl fmt::Display for PasteChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            Some(mode) => write!(formatter, "{mode}"),
            None => write!(formatter, "Use the default ({})", self.default),
        }
    }
}

pub(super) struct Draft {
    id: u64,
    name: String,
    tag: String,
    profile: TerminalProfile,
    incognito: bool,
    isolation: IsolationLevel,
    startup: StartupFields,
    logging: crate::terminal_logging::Form,
    terminal_effect: EffectDraft,
    command_timestamps: Option<bool>,
    tab_color: ColorDraft,
    os_badge: Option<String>,
    badge_picker_open: bool,
    paste_warning: Option<PasteWarningMode>,
    terminal: terminal_overrides::Draft,
    window_geometry: geometry::Draft,
    login: Login,
    jump: Option<Login>,
    tunnels: Vec<TunnelDraft>,
    pub changed: bool,
}

struct Login {
    host: String,
    port: String,
    username: String,
    method: Method,
    keyboard_interactive: bool,
    password: String,
    key_path: String,
    stored_key: Option<u64>,
    passphrase: String,
}

struct TunnelDraft {
    enabled: bool,
    description: String,
    mode: TunnelMode,
    bind: String,
    port: String,
    host: String,
    target_port: String,
}

impl Draft {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn terminal_effect(&self) -> Option<Effect> {
        self.terminal_effect.effect
    }

    pub fn new() -> Self {
        Self {
            id: 0,
            name: String::new(),
            tag: String::new(),
            profile: TerminalProfile::Xterm,
            incognito: false,
            isolation: IsolationLevel::None,
            startup: StartupFields::new(Default::default()),
            logging: crate::terminal_logging::Form::new(Default::default()),
            terminal_effect: EffectDraft::default(),
            command_timestamps: None,
            tab_color: ColorDraft::new(None, None),
            os_badge: None,
            badge_picker_open: false,
            paste_warning: None,
            terminal: terminal_overrides::Draft::new(&Default::default()),
            window_geometry: geometry::Draft::new(None),
            login: Login::new(),
            jump: None,
            tunnels: Vec::new(),
            changed: false,
        }
    }

    pub fn from_profile(connection: &Connection, os_badge: Option<&str>, duplicate: bool) -> Self {
        Self {
            id: if duplicate { 0 } else { connection.id },
            name: if duplicate {
                format!("{} (copy)", connection.name)
            } else {
                connection.name.clone()
            },
            tag: connection.tag.clone(),
            profile: connection.profile().unwrap_or(TerminalProfile::Xterm),
            incognito: connection.incognito,
            isolation: connection.isolation,
            startup: StartupFields::new(connection.startup),
            logging: crate::terminal_logging::Form::new(connection.logging.clone()),
            terminal_effect: EffectDraft::new(connection.terminal_effect),
            command_timestamps: connection.command_timestamps,
            tab_color: ColorDraft::new(connection.tab_color, None),
            os_badge: os_badge.map(str::to_owned),
            badge_picker_open: false,
            paste_warning: connection.paste_warning,
            terminal: terminal_overrides::Draft::new(&connection.terminal),
            window_geometry: geometry::Draft::new(if duplicate {
                None
            } else {
                connection.window_geometry
            }),
            login: Login::from_saved(&connection.login),
            jump: connection.jump.as_ref().map(Login::from_saved),
            tunnels: connection
                .tunnels
                .iter()
                .map(TunnelDraft::from_config)
                .collect(),
            changed: duplicate,
        }
    }

    pub(super) fn os_badge_id(&self) -> Option<&str> {
        self.os_badge.as_deref()
    }

    pub fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Name(value) => self.name = bounded(value, 160),
            Edit::Tag(value) => self.tag = bounded(value, 96),
            Edit::Emulation(profile) => self.profile = profile,
            Edit::Incognito(enabled) => self.incognito = enabled,
            Edit::Isolation(level) => self.isolation = level,
            Edit::Startup(field, value) => self.startup.update(field, value),
            Edit::KeepAlive(edit) => self.startup.update_keepalive(edit),
            Edit::Logging(edit) => self.logging.update(edit),
            Edit::TerminalEffect(effect) => self.terminal_effect.effect = effect,
            Edit::TerminalEffectSpeed(value) => self.terminal_effect.set_speed(value),
            Edit::CommandTimestamps(visibility) => self.command_timestamps = visibility,
            Edit::UseTabColor(enabled) => self.tab_color.set_enabled(enabled),
            Edit::TabColor(value) => {
                if !self.tab_color.set_input(value) {
                    return;
                }
            }
            Edit::ToggleOsBadgePicker => {
                self.badge_picker_open = !self.badge_picker_open;
                return;
            }
            Edit::OsBadge(badge) if self.badge_picker_open => {
                self.badge_picker_open = false;
                if self.os_badge == badge {
                    return;
                }
                self.os_badge = badge;
            }
            Edit::PasteWarning(mode) => self.paste_warning = mode,
            Edit::Terminal(edit) => {
                self.changed |= self.terminal.update(edit);
                return;
            }
            Edit::WindowGeometry(edit) => {
                self.changed |= self.window_geometry.update(edit);
                return;
            }
            Edit::UseJump(true) => {
                self.jump.get_or_insert_with(Login::new);
            }
            Edit::UseJump(false) => self.jump = None,
            Edit::Login(false, edit) => self.login.update(edit),
            Edit::Login(true, edit) => {
                if let Some(jump) = &mut self.jump {
                    jump.update(edit);
                }
            }
            Edit::AddTunnel if self.tunnels.len() < MAX_TUNNELS => {
                self.tunnels.push(TunnelDraft::new())
            }
            Edit::RemoveTunnel(index) if index < self.tunnels.len() => {
                self.tunnels.remove(index);
            }
            Edit::Tunnel(index, edit) => {
                if let Some(tunnel) = self.tunnels.get_mut(index) {
                    tunnel.update(edit);
                }
            }
            _ => return,
        }
        self.changed = true;
    }

    pub fn connection(&self) -> Result<Connection, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Enter a connection name.".into());
        }
        let tab_color = self.tab_color.value()?;
        let login = self
            .login
            .saved()
            .map_err(|error| format!("Destination. {error}"))?;
        let jump = self
            .jump
            .as_ref()
            .map(Login::saved)
            .transpose()
            .map_err(|error| format!("Jump host. {error}"))?;
        let tunnels = self
            .tunnels
            .iter()
            .enumerate()
            .map(|(index, tunnel)| {
                tunnel
                    .config()
                    .map_err(|error| format!("Tunnel {}. {error}", index + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let connection = Connection {
            id: self.id,
            name: name.into(),
            tag: self.tag.trim().into(),
            login,
            jump,
            emulation: self.profile.term_name().into(),
            tunnels,
            incognito: self.incognito,
            isolation: self.isolation,
            startup: self.startup.options().map_err(|error| error.to_string())?,
            logging: self.logging.config().map_err(|error| error.to_string())?,
            terminal_effect: self.terminal_effect.selection(),
            command_timestamps: self.command_timestamps,
            tab_color,
            paste_warning: self.paste_warning,
            window_geometry: self.window_geometry.value()?,
            terminal: self.terminal.value()?,
        };
        connection.validate().map_err(|error| error.to_string())?;
        Ok(connection)
    }

    pub fn view<'a>(
        &'a self,
        choices: &ViewChoices<'_>,
        custom_badges: &'a Catalog,
        group: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let valid = self.connection();
        let paste_choice = |mode| PasteChoice {
            mode,
            default: choices.default_paste_warning,
        };
        let paste_choices: Vec<_> = std::iter::once(None)
            .chain(PasteWarningMode::ALL.map(Some))
            .map(paste_choice)
            .collect();
        let mut editor = column![
            text(if self.id == 0 {
                "New connection"
            } else {
                "Edit connection"
            })
            .size(21),
            row![
                text_input("Connection name", &self.name)
                    .on_input(|value| Message::Edit(Edit::Name(value))),
                text_input("Tag (optional)", &self.tag)
                    .on_input(|value| Message::Edit(Edit::Tag(value))),
            ]
            .spacing(8),
            group,
            os_badge::picker(
                os_badge::Badge::resolve(self.os_badge_id(), custom_badges),
                custom_badges,
                self.badge_picker_open,
                Some(Message::Edit(Edit::ToggleOsBadgePicker)),
                None,
                |badge| Message::Edit(Edit::OsBadge(badge)),
            ),
            pick_list(TerminalProfile::ALL, Some(self.profile), |value| {
                Message::Edit(Edit::Emulation(value))
            }),
            column![
                text("Tab color").size(16),
                tooltip(
                    self.tab_color.view(
                        "Mark this connection's tabs with a color",
                        |enabled| Message::Edit(Edit::UseTabColor(enabled)),
                        |input| Message::Edit(Edit::TabColor(input)),
                    ),
                    text("Hover over the tab to see its color."),
                    tooltip::Position::Bottom,
                ),
            ]
            .spacing(6),
            row![
                text("Show command timestamps"),
                pick_list(
                    TimestampChoice::ALL,
                    Some(TimestampChoice(self.command_timestamps)),
                    |choice| Message::Edit(Edit::CommandTimestamps(choice.0))
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            column![
                text("Warn about multi-line pastes"),
                tooltip(
                    pick_list(paste_choices, Some(paste_choice(self.paste_warning)), |choice| {
                        Message::Edit(Edit::PasteWarning(choice.mode))
                    }),
                    text("Use the default follows Settings → Terminal → Paste Protection. Off also turns off control-character warnings."),
                    tooltip::Position::Bottom,
                ),
                (choices.minimum_paste_warning != PasteWarningMode::Off).then(|| {
                    text(format!("Organization minimum · {}", choices.minimum_paste_warning)).size(13)
                }),
            ].spacing(5),
            row![
                text("SSH isolation"),
                pick_list(
                    [
                        IsolationLevel::None,
                        IsolationLevel::Process,
                        IsolationLevel::Sandbox
                    ],
                    Some(self.isolation),
                    |value| Message::Edit(Edit::Isolation(value))
                ),
            ]
            .spacing(8),
            self.startup
                .view(135.0)
                .map(|(field, value)| Message::Edit(Edit::Startup(field, value))),
            self.startup.keepalive_view(135.0).map(|edit| Message::Edit(Edit::KeepAlive(edit))),
            checkbox(self.incognito)
                .label(if choices.incognito_allowed {
                    "Incognito session"
                } else {
                    "Incognito session · managed policy"
                })
                .on_toggle_maybe(
                    choices
                        .incognito_allowed
                        .then_some(|value| Message::Edit(Edit::Incognito(value)))
                ),
        ]
        .spacing(8)
        .push(self.terminal.view(choices).map(|edit| Message::Edit(Edit::Terminal(edit))))
        .push(self.window_geometry.view().map(|edit| Message::Edit(Edit::WindowGeometry(edit))));
        if let Some(effects) = choices.terminal_effects {
            editor = editor.push(self.terminal_effect.view(effects));
        }
        editor = editor
            .push(text("Destination").size(16))
            .push(
                self.login
                    .view(false, choices.passwords, choices.passphrases, choices.keys),
            )
            .push(
                checkbox(self.jump.is_some())
                    .label("Use a jump host")
                    .on_toggle(|value| Message::Edit(Edit::UseJump(value))),
            );
        if let Some(jump) = &self.jump {
            editor = editor.push(text("Jump host").size(16)).push(jump.view(
                true,
                choices.passwords,
                choices.passphrases,
                choices.keys,
            ));
        }
        editor = editor.push(
            self.logging
                .view(!self.incognito, true)
                .map(|edit| Message::Edit(Edit::Logging(edit))),
        );
        editor = editor.push(
            row![
                text("SSH tunnels").size(17),
                button("Add tunnel").on_press_maybe(
                    (self.tunnels.len() < MAX_TUNNELS).then_some(Message::Edit(Edit::AddTunnel))
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        for (index, tunnel) in self.tunnels.iter().enumerate() {
            editor = editor.push(tunnel.view(index));
        }
        if let Err(error) = valid {
            editor = editor.push(text(error).size(13));
        }
        editor.into()
    }
}

impl Login {
    fn new() -> Self {
        Self {
            host: String::new(),
            port: "22".into(),
            username: "root".into(),
            method: Method::Ask,
            keyboard_interactive: false,
            password: String::new(),
            key_path: String::new(),
            stored_key: None,
            passphrase: String::new(),
        }
    }

    fn from_saved(login: &SavedLogin) -> Self {
        let mut form = Self::new();
        form.host = login.endpoint.host.clone();
        form.port = login.endpoint.port.to_string();
        form.username = login.endpoint.username.clone();
        form.keyboard_interactive = login.keyboard_interactive;
        match &login.auth {
            Auth::Prompt => {}
            Auth::KeyboardInteractive => form.method = Method::Interactive,
            Auth::VaultPassword(id) => {
                form.method = Method::Vault;
                form.password = id.clone();
            }
            Auth::KeyFile { path, passphrase } => {
                form.method = Method::Key;
                form.key_path = path.to_string_lossy().into_owned();
                form.passphrase = passphrase.clone().unwrap_or_default();
            }
            Auth::StoredKey(id) => {
                form.method = Method::StoredKey;
                form.stored_key = Some(*id);
            }
            Auth::Configured {
                method,
                credential,
                key,
            } => {
                form.password = credential.clone().unwrap_or_default();
                form.stored_key = *key;
                form.method = match method {
                    AuthMethod::Password if credential.is_some() => Method::Vault,
                    AuthMethod::Password => Method::Ask,
                    AuthMethod::PublicKey => Method::StoredKey,
                    AuthMethod::KeyboardInteractive => Method::Interactive,
                };
            }
        }
        form
    }

    fn update(&mut self, edit: LoginEdit) {
        match edit {
            LoginEdit::Host(value) => self.host = bounded(value, 256),
            LoginEdit::Port(value) => self.port = bounded(value, 8),
            LoginEdit::Username(value) => self.username = bounded(value, 255),
            LoginEdit::Method(method) => {
                self.method = method;
                if method == Method::Ask {
                    self.password.clear();
                }
            }
            LoginEdit::KeyboardFollowUp(enabled) => self.keyboard_interactive = enabled,
            LoginEdit::Password(credential) => {
                self.password = credential.id;
                if !credential.username.is_empty() {
                    self.username = credential.username;
                }
            }
            LoginEdit::KeyPath(value) => self.key_path = bounded(value, 4096),
            LoginEdit::StoredKey(key) => self.stored_key = Some(key.id),
            LoginEdit::Passphrase(credential) => self.passphrase = credential.id,
        }
    }

    fn saved(&self) -> Result<SavedLogin, &'static str> {
        let endpoint = LoginTarget {
            host: normalize_host(&self.host).map_err(|_| "Enter a hostname or IP address.")?,
            port: port(&self.port).ok_or("Enter a port from 1 to 65535.")?,
            username: self.username.trim().into(),
        };
        endpoint
            .validate_saved()
            .map_err(|_| "Enter a valid SSH username.")?;
        let auth = match self.method {
            Method::Ask if self.stored_key.is_some() => Auth::Configured {
                method: AuthMethod::Password,
                credential: None,
                key: self.stored_key,
            },
            Method::Ask => Auth::Prompt,
            Method::Interactive if !self.password.is_empty() || self.stored_key.is_some() => {
                Auth::Configured {
                    method: AuthMethod::KeyboardInteractive,
                    credential: (!self.password.is_empty()).then(|| self.password.clone()),
                    key: self.stored_key,
                }
            }
            Method::Interactive => Auth::KeyboardInteractive,
            Method::Vault if self.password.is_empty() => return Err("Select a vault password."),
            Method::Vault if self.stored_key.is_some() => Auth::Configured {
                method: AuthMethod::Password,
                credential: Some(self.password.clone()),
                key: self.stored_key,
            },
            Method::Vault => Auth::VaultPassword(self.password.clone()),
            Method::Key => {
                let path = PathBuf::from(self.key_path.trim());
                if !path.is_absolute() {
                    return Err("Enter an absolute SSH private key path.");
                }
                Auth::KeyFile {
                    path,
                    passphrase: (!self.passphrase.is_empty()).then(|| self.passphrase.clone()),
                }
            }
            Method::StoredKey if !self.password.is_empty() || self.stored_key.is_none() => {
                Auth::Configured {
                    method: AuthMethod::PublicKey,
                    credential: (!self.password.is_empty()).then(|| self.password.clone()),
                    key: self.stored_key,
                }
            }
            Method::StoredKey => Auth::StoredKey(self.stored_key.unwrap_or_default()),
        };
        Ok(SavedLogin {
            endpoint,
            auth,
            keyboard_interactive: self.keyboard_interactive && self.method != Method::Interactive,
        })
    }

    fn view<'a>(
        &'a self,
        jump: bool,
        passwords: &[CredentialChoice],
        passphrases: &[CredentialChoice],
        keys: &[KeyChoice],
    ) -> Element<'a, Message> {
        let edit = move |edit| Message::Edit(Edit::Login(jump, edit));
        let mut fields = column![
            row![
                text_input("Hostname or IP", &self.host)
                    .on_input(move |value| edit(LoginEdit::Host(value))),
                text_input("Port", &self.port)
                    .on_input(move |value| edit(LoginEdit::Port(value)))
                    .width(80),
                text_input("Username", &self.username)
                    .on_input(move |value| edit(LoginEdit::Username(value))),
            ]
            .spacing(8),
            pick_list(
                [
                    Method::Ask,
                    Method::Vault,
                    Method::Key,
                    Method::StoredKey,
                    Method::Interactive
                ],
                Some(self.method),
                move |method| edit(LoginEdit::Method(method))
            ),
        ]
        .spacing(8);
        match self.method {
            Method::Ask | Method::Interactive => {}
            Method::Vault => {
                let (choices, selected) =
                    CredentialChoice::options(passwords, &self.password, false);
                fields = fields.push(
                    pick_list(choices, selected, move |choice| {
                        edit(LoginEdit::Password(choice))
                    })
                    .placeholder("Unlock the vault to select a password"),
                );
            }
            Method::Key => {
                let (choices, selected) =
                    CredentialChoice::options(passphrases, &self.passphrase, true);
                fields = fields
                    .push(
                        text_input("Absolute private key path", &self.key_path)
                            .on_input(move |value| edit(LoginEdit::KeyPath(value))),
                    )
                    .push(pick_list(choices, selected, move |choice| {
                        edit(LoginEdit::Passphrase(choice))
                    }));
            }
            Method::StoredKey => {
                fields = fields.push(
                    pick_list(
                        keys.to_vec(),
                        KeyChoice::selected(keys, self.stored_key),
                        move |key| edit(LoginEdit::StoredKey(key)),
                    )
                    .placeholder("Select a registered SSH key"),
                );
            }
        }
        if self.method != Method::Interactive {
            fields = fields.push(
                checkbox(self.keyboard_interactive)
                    .label("Also allow keyboard-interactive")
                    .on_toggle(move |enabled| edit(LoginEdit::KeyboardFollowUp(enabled))),
            );
        }
        fields.into()
    }
}

impl TunnelDraft {
    fn new() -> Self {
        Self {
            enabled: true,
            description: String::new(),
            mode: TunnelMode::Local,
            bind: String::new(),
            port: String::new(),
            host: String::new(),
            target_port: String::new(),
        }
    }

    fn from_config(config: &TunnelConfig) -> Self {
        let mut draft = Self {
            enabled: config.enabled,
            description: config.description.clone(),
            bind: config.rule.bind.ip().to_string(),
            port: config.rule.bind.port().to_string(),
            ..Self::new()
        };
        match &config.rule.target {
            TunnelTarget::Local { host, port } | TunnelTarget::Remote { host, port } => {
                draft.mode = if matches!(config.rule.target, TunnelTarget::Local { .. }) {
                    TunnelMode::Local
                } else {
                    TunnelMode::Remote
                };
                draft.host = host.clone();
                draft.target_port = port.to_string();
            }
            TunnelTarget::Dynamic => draft.mode = TunnelMode::Dynamic,
        }
        draft
    }

    fn update(&mut self, edit: TunnelEdit) {
        match edit {
            TunnelEdit::Enabled(value) => self.enabled = value,
            TunnelEdit::Mode(mode) => self.mode = mode,
            TunnelEdit::Bind(value) => self.bind = bounded(value, 64),
            TunnelEdit::Port(value) => self.port = bounded(value, 8),
            TunnelEdit::Host(value) => self.host = bounded(value, 256),
            TunnelEdit::TargetPort(value) => self.target_port = bounded(value, 8),
            TunnelEdit::Description(value) => self.description = bounded(value, 160),
        }
    }

    fn config(&self) -> Result<TunnelConfig, &'static str> {
        let address = self.bind.trim();
        let ip: IpAddr = if address.is_empty() || address.eq_ignore_ascii_case("localhost") {
            Ipv4Addr::LOCALHOST.into()
        } else {
            address
                .parse()
                .map_err(|_| "Use localhost or a numeric listen address.")?
        };
        let bind = SocketAddr::new(
            ip,
            port(&self.port).ok_or("Enter a listen port from 1 to 65535.")?,
        );
        let target = match self.mode {
            TunnelMode::Local | TunnelMode::Remote => {
                let host = normalize_host(&self.host)
                    .map_err(|_| "Enter a service hostname or IP address.")?;
                let port =
                    port(&self.target_port).ok_or("Enter a service port from 1 to 65535.")?;
                if self.mode == TunnelMode::Local {
                    TunnelTarget::Local { host, port }
                } else {
                    TunnelTarget::Remote { host, port }
                }
            }
            TunnelMode::Dynamic => TunnelTarget::Dynamic,
        };
        Ok(TunnelConfig {
            enabled: self.enabled,
            description: self.description.trim().into(),
            rule: TunnelRule { bind, target },
        })
    }

    fn view(&self, index: usize) -> Element<'_, Message> {
        let edit = move |edit| Message::Edit(Edit::Tunnel(index, edit));
        let mut fields = column![
            row![
                checkbox(self.enabled)
                    .label("Enabled")
                    .on_toggle(move |value| edit(TunnelEdit::Enabled(value))),
                pick_list(
                    [TunnelMode::Local, TunnelMode::Remote, TunnelMode::Dynamic],
                    Some(self.mode),
                    move |mode| edit(TunnelEdit::Mode(mode))
                ),
                text_input("Description (optional)", &self.description)
                    .on_input(move |value| edit(TunnelEdit::Description(value))),
                button("Remove").on_press(Message::Edit(Edit::RemoveTunnel(index))),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            row![
                text_input("Listen address (localhost)", &self.bind)
                    .on_input(move |value| edit(TunnelEdit::Bind(value))),
                text_input("Listen port", &self.port)
                    .on_input(move |value| edit(TunnelEdit::Port(value)))
                    .width(105),
            ]
            .spacing(8),
        ]
        .spacing(6);
        if self.mode != TunnelMode::Dynamic {
            fields = fields.push(
                row![
                    text_input("Service hostname or IP", &self.host)
                        .on_input(move |value| edit(TunnelEdit::Host(value))),
                    text_input("Service port", &self.target_port)
                        .on_input(move |value| edit(TunnelEdit::TargetPort(value)))
                        .width(105),
                ]
                .spacing(8),
            );
        }
        fields.into()
    }
}

fn bounded(value: String, max: usize) -> String {
    if value.len() <= max {
        value
    } else {
        String::new()
    }
}

fn port(value: &str) -> Option<u16> {
    let value = value.trim();
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|&port| port != 0)
}
