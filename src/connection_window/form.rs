use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input, tooltip};
use iced::{Alignment, Element, Length};
use kortty_terminal::TerminalProfile;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::{
    Auth, AuthMethod, Connection, MAX_TUNNELS, SavedLogin, TunnelConfig, TunnelRule, TunnelTarget,
};
use crate::session_security::{IsolationChoice, IsolationLevel, resolve_isolation};
use crate::ssh_dialog::{
    KeepAliveEdit, KeyChoice, LoginTarget, Password, StartupField, StartupFields, normalize_host,
};
use crate::terminal_effects::{Effect, editor::EffectDraft};

use super::color::ColorDraft;
use super::view::ViewChoices;
use super::{CredentialChoice, Message, os_badge, terminal_overrides};

mod geometry;

#[derive(Clone, Debug)]
pub enum Edit {
    Page(Page),
    Name(String),
    Tag(String),
    Emulation(TerminalProfile),
    Incognito(bool),
    Isolation(IsolationChoice),
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Connection,
    Settings,
    Tunnels,
    JumpHost,
    Logging,
    WindowGeometry,
}

impl Page {
    const ALL: [Self; 6] = [
        Self::Connection,
        Self::Settings,
        Self::Tunnels,
        Self::JumpHost,
        Self::Logging,
        Self::WindowGeometry,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Settings => "Terminal Settings",
            Self::Tunnels => "SSH Tunnels",
            Self::JumpHost => "Jump Server",
            Self::Logging => "Terminal Logging",
            Self::WindowGeometry => "Window Geometry",
        }
    }
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
    TemporaryLifetime(String),
    TemporaryAutomatic(bool),
    RemoveTemporaryKey,
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
    TemporaryKey,
    Interactive,
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ask => "Ask for password",
            Self::Vault => "Vault password",
            Self::Key => "SSH private key",
            Self::StoredKey => "Registered SSH key",
            Self::TemporaryKey => "Temporary SSH key",
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
    page: Page,
    id: u64,
    name: String,
    tag: String,
    profile: TerminalProfile,
    incognito: bool,
    isolation: IsolationChoice,
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
    temporary: TemporaryDraft,
}

struct TemporaryDraft {
    credential: Option<String>,
    minutes: String,
    automatic: bool,
    pending: Option<u64>,
    key: Option<Password>,
}

impl Default for TemporaryDraft {
    fn default() -> Self {
        Self {
            credential: None,
            minutes: "60".into(),
            automatic: false,
            pending: None,
            key: None,
        }
    }
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
            page: Page::default(),
            id: 0,
            name: String::new(),
            tag: String::new(),
            profile: TerminalProfile::Xterm,
            incognito: false,
            isolation: IsolationChoice::default(),
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
            page: Page::default(),
            id: if duplicate { 0 } else { connection.id },
            name: if duplicate {
                format!("{} (copy)", connection.name)
            } else {
                connection.name.clone()
            },
            tag: connection.tag.clone(),
            profile: connection.profile().unwrap_or(TerminalProfile::Xterm),
            incognito: connection.incognito,
            isolation: IsolationChoice(connection.isolation_override()),
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
            login: Login::from_saved(&connection.login, duplicate),
            jump: connection
                .jump
                .as_ref()
                .map(|login| Login::from_saved(login, duplicate)),
            tunnels: connection
                .tunnels
                .iter()
                .map(TunnelDraft::from_config)
                .collect(),
            changed: duplicate,
        }
    }

    pub(super) fn begin_temporary_paste(&mut self, request: u64) -> bool {
        if self.login.method != Method::TemporaryKey || self.login.endpoint().is_err() {
            return false;
        }
        self.login.temporary.pending = Some(request);
        true
    }

    pub(super) fn finish_temporary_paste(&mut self, request: u64, key: Password) -> bool {
        let draft = &mut self.login.temporary;
        if draft.pending != Some(request) {
            return false;
        }
        draft.pending = None;
        let value = key.as_str().trim();
        if value.is_empty() || value.len() > kortty_vault::MAX_TEMPORARY_SSH_KEY_BYTES {
            return false;
        }
        self.changed = true;
        draft.key = Some(key);
        true
    }

    pub(super) fn pending_temporary_key(&self) -> Option<&Password> {
        self.login
            .temporary
            .key
            .as_ref()
            .filter(|_| self.login.method == Method::TemporaryKey)
    }

    pub(super) fn os_badge_id(&self) -> Option<&str> {
        self.os_badge.as_deref()
    }

    pub fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Page(page) => {
                self.page = page;
                return;
            }
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
            isolation: self.isolation.0.unwrap_or_default(),
            inherit_isolation: self.isolation.0.is_none(),
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
        inherited: Option<(&str, IsolationLevel)>,
    ) -> Element<'a, Message> {
        use crate::workspace::dialog_design;
        use iced::widget::{container, keyed_column, scrollable, space};

        let tabs = Page::ALL.into_iter().fold(row![], |tabs, page| {
            let selected = self.page == page;
            tabs.push(
                button(text(page.label()).size(13))
                    .padding([8, 12])
                    .style(move |theme, status| dialog_design::tab(theme, status, selected))
                    .on_press(Message::Edit(Edit::Page(page))),
            )
        });
        let index = Page::ALL
            .iter()
            .position(|page| *page == self.page)
            .expect("every editor page has a tab");
        let arrows = row![
            space::horizontal(),
            text(format!("{} / {}", index + 1, Page::ALL.len())).size(13),
            button(text("◂").size(13))
                .style(button::secondary)
                .padding([4, 8])
                .on_press_maybe(
                    index
                        .checked_sub(1)
                        .map(|previous| Message::Edit(Edit::Page(Page::ALL[previous])))
                ),
            button(text("▸").size(13))
                .style(button::secondary)
                .padding([4, 8])
                .on_press_maybe(
                    Page::ALL
                        .get(index + 1)
                        .map(|next| Message::Edit(Edit::Page(*next)))
                ),
        ]
        .align_y(Alignment::Center)
        .spacing(4);
        let content = match self.page {
            Page::Connection => self.connection_page(choices, custom_badges, group),
            Page::Settings => self.settings_page(choices, inherited),
            Page::Tunnels => self.tunnels_page(),
            Page::JumpHost => column![
                checkbox(self.jump.is_some())
                    .label("Use a jump host")
                    .on_toggle(|value| Message::Edit(Edit::UseJump(value))),
                self.jump.as_ref().map(|jump| jump.view(true, choices)),
            ]
            .spacing(10)
            .into(),
            Page::Logging => self
                .logging
                .view(!self.incognito, true)
                .map(|edit| Message::Edit(Edit::Logging(edit))),
            Page::WindowGeometry => self
                .window_geometry
                .view()
                .map(|edit| Message::Edit(Edit::WindowGeometry(edit))),
        };
        let page = container(
            scrollable(container(content).width(Length::Fill).padding(20))
                .height(Length::Fill)
                .width(Length::Fill),
        )
        .style(dialog_design::page);
        column![
            arrows,
            tabs,
            keyed_column![(self.page, page)].height(Length::Fill),
            self.connection().err().map(|error| text(error).size(13)),
        ]
        .spacing(4)
        .height(Length::Fill)
        .into()
    }

    fn connection_page<'a>(
        &'a self,
        choices: &ViewChoices<'_>,
        custom_badges: &'a Catalog,
        group: Element<'a, Message>,
    ) -> Element<'a, Message> {
        column![
            row![
                text("Name").width(105),
                text_input("Connection name", &self.name)
                    .on_input(|value| Message::Edit(Edit::Name(value)))
                    .width(250),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            self.login.view(false, choices),
            row![
                text("Tag").width(105),
                text_input("Tag (optional)", &self.tag)
                    .on_input(|value| Message::Edit(Edit::Tag(value)))
                    .width(250),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            group,
            os_badge::picker(
                os_badge::Badge::resolve(self.os_badge_id(), custom_badges),
                custom_badges,
                self.badge_picker_open,
                Some(Message::Edit(Edit::ToggleOsBadgePicker)),
                None,
                |badge| Message::Edit(Edit::OsBadge(badge)),
            ),
            row![
                text("Terminal emulation").width(144),
                pick_list(TerminalProfile::ALL, Some(self.profile), |value| {
                    Message::Edit(Edit::Emulation(value))
                }),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            self.startup
                .view(168.0)
                .map(|(field, value)| Message::Edit(Edit::Startup(field, value))),
            self.startup
                .keepalive_view(168.0)
                .map(|edit| Message::Edit(Edit::KeepAlive(edit))),
        ]
        .spacing(10)
        .into()
    }

    fn settings_page<'a>(
        &'a self,
        choices: &ViewChoices<'_>,
        inherited: Option<(&str, IsolationLevel)>,
    ) -> Element<'a, Message> {
        let (level, source) = resolve_isolation(
            choices.isolation.0,
            inherited,
            self.isolation.0,
            None,
            false,
            choices.isolation.1,
        );
        let paste_choice = |mode| PasteChoice {
            mode,
            default: choices.default_paste_warning,
        };
        let paste_choices: Vec<_> = std::iter::once(None)
            .chain(PasteWarningMode::ALL.map(Some))
            .map(paste_choice)
            .collect();
        let mut editor = column![
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
            column![
                text("Session isolation").size(16),
                terminal_overrides::isolation_row(
                    "Isolation",
                    pick_list(
                        IsolationChoice::ALL,
                        Some(self.isolation),
                        |value| Message::Edit(Edit::Isolation(value))
                    ).width(Length::Fill),
                ),
                text(format!("Effective isolation · {level} · {source}")).size(13),
                self.terminal.strict_mode_view().map(|edit| Message::Edit(Edit::Terminal(edit))),
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
            .spacing(6),
            self.terminal.view(choices).map(|edit| Message::Edit(Edit::Terminal(edit))),
        ]
        .spacing(8);
        if let Some(effects) = choices.terminal_effects {
            editor = editor.push(
                column![
                    text("Terminal Effect").size(16),
                    self.terminal_effect.view(
                        effects,
                        |effect| Message::Edit(Edit::TerminalEffect(effect)),
                        |speed| Message::Edit(Edit::TerminalEffectSpeed(speed)),
                    ),
                ]
                .spacing(6),
            );
        }
        editor.into()
    }

    fn tunnels_page(&self) -> Element<'_, Message> {
        column![button("Add tunnel").on_press_maybe(
            (self.tunnels.len() < MAX_TUNNELS).then_some(Message::Edit(Edit::AddTunnel))
        )]
        .extend(
            self.tunnels
                .iter()
                .enumerate()
                .map(|(index, tunnel)| tunnel.view(index)),
        )
        .spacing(8)
        .into()
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
            temporary: TemporaryDraft::default(),
        }
    }

    fn from_saved(login: &SavedLogin, duplicate: bool) -> Self {
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
            Auth::TemporaryKey {
                credential,
                minutes,
                automatic,
            } => {
                form.method = Method::TemporaryKey;
                form.temporary.credential = if duplicate { None } else { credential.clone() };
                form.temporary.minutes = minutes.to_string();
                form.temporary.automatic = *automatic;
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
        if matches!(
            edit,
            LoginEdit::Host(_) | LoginEdit::Port(_) | LoginEdit::Username(_) | LoginEdit::Method(_)
        ) {
            self.temporary.pending = None;
            self.temporary.key = None;
        }
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
            LoginEdit::TemporaryLifetime(minutes) => self.temporary.minutes = bounded(minutes, 4),
            LoginEdit::TemporaryAutomatic(enabled) => self.temporary.automatic = enabled,
            LoginEdit::RemoveTemporaryKey => {
                self.temporary.credential = None;
                self.temporary.key = None;
                self.temporary.pending = None;
            }
        }
    }

    fn endpoint(&self) -> Result<LoginTarget, &'static str> {
        let endpoint = LoginTarget {
            host: normalize_host(&self.host).map_err(|_| "Enter a hostname or IP address.")?,
            port: port(&self.port).ok_or("Enter a port from 1 to 65535.")?,
            username: self.username.trim().into(),
        };
        endpoint
            .validate_saved()
            .map_err(|_| "Enter a valid SSH username.")?;
        Ok(endpoint)
    }

    fn saved(&self) -> Result<SavedLogin, &'static str> {
        let endpoint = self.endpoint()?;
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
            Method::TemporaryKey => Auth::TemporaryKey {
                credential: self.temporary.credential.clone(),
                minutes: self
                    .temporary
                    .minutes
                    .parse()
                    .ok()
                    .filter(|minutes| (1..=1440).contains(minutes))
                    .ok_or("Enter a temporary SSH key lifetime from 1 to 1440 minutes.")?,
                automatic: self.temporary.automatic,
            },
        };
        Ok(SavedLogin {
            endpoint,
            auth,
            keyboard_interactive: self.keyboard_interactive && self.method != Method::Interactive,
        })
    }

    fn view<'a>(&'a self, jump: bool, choices: &ViewChoices<'_>) -> Element<'a, Message> {
        let edit = move |edit| Message::Edit(Edit::Login(jump, edit));
        let mut methods = vec![
            Method::Ask,
            Method::Vault,
            Method::Key,
            Method::StoredKey,
            Method::Interactive,
        ];
        if (!jump && choices.temporary_keys) || self.method == Method::TemporaryKey {
            methods.push(Method::TemporaryKey);
        }
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
            pick_list(methods, Some(self.method), move |method| edit(
                LoginEdit::Method(method)
            )),
        ]
        .spacing(8);
        match self.method {
            Method::Ask | Method::Interactive => {}
            Method::Vault => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passwords, &self.password, false);
                fields = fields.push(
                    pick_list(choices, selected, move |choice| {
                        edit(LoginEdit::Password(choice))
                    })
                    .placeholder("Unlock the vault to select a password"),
                );
            }
            Method::Key => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passphrases, &self.passphrase, true);
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
                        choices.keys.to_vec(),
                        KeyChoice::selected(choices.keys, self.stored_key),
                        move |key| edit(LoginEdit::StoredKey(key)),
                    )
                    .placeholder("Select a registered SSH key"),
                );
            }
            Method::TemporaryKey => {
                let key = &self.temporary;
                let can_write = choices.temporary_keys && choices.vault_unlocked && !jump;
                let can_set_options = can_write && (key.credential.is_none() || key.key.is_some());
                fields = fields
                    .push(
                        text(if key.key.is_some() {
                            "Private key pasted. Save to verify and store it."
                        } else if key.credential.is_some() {
                            "Private key saved in the credential vault."
                        } else {
                            "Paste a private key for this SSH account."
                        })
                        .size(13),
                    )
                    .push(
                        row![
                            text_input("1–1440", &key.minutes).width(88).on_input_maybe(
                                can_set_options.then_some(move |minutes| edit(
                                    LoginEdit::TemporaryLifetime(minutes)
                                ))
                            ),
                            text("minutes"),
                            button("Paste private key")
                                .on_press_maybe(can_write.then_some(Message::PasteTemporaryKey)),
                            button("Remove key").on_press_maybe(
                                (choices.vault_unlocked
                                    && (key.credential.is_some() || key.key.is_some()))
                                .then_some(edit(LoginEdit::RemoveTemporaryKey)),
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                    )
                    .push(
                        checkbox(key.automatic)
                            .label("Select the saved key automatically until it expires")
                            .on_toggle_maybe(can_set_options.then_some(move |enabled| {
                                edit(LoginEdit::TemporaryAutomatic(enabled))
                            })),
                    );
                if !choices.vault_unlocked {
                    fields =
                        fields.push(button("Open Credential Vault…").on_press(Message::OpenVault));
                } else if key.credential.is_some() && key.key.is_none() {
                    fields = fields
                        .push(text("Paste a replacement to change the key’s lifetime.").size(13));
                }
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

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn switching_pages_preserves_the_draft_without_making_it_dirty() {
        let mut draft = Draft::new();
        draft.update(Edit::Page(Page::JumpHost));
        assert!(!draft.changed);
        draft.update(Edit::UseJump(true));
        draft.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        draft.update(Edit::Page(Page::Connection));
        draft.update(Edit::Name("Build host".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("build.example".into())));
        draft.update(Edit::Page(Page::Logging));

        let connection = draft.connection().unwrap();
        assert_eq!(connection.name, "Build host");
        assert_eq!(connection.login.endpoint.host, "build.example");
        assert_eq!(connection.jump.unwrap().endpoint.host, "jump.example");
    }

    #[test]
    fn temporary_keys_survive_unrelated_edits_but_require_a_new_key_on_duplicate() {
        let mut draft = Draft::new();
        draft.update(Edit::Name("Fixture".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("ssh.example".into())));
        draft.update(Edit::Login(false, LoginEdit::Method(Method::TemporaryKey)));
        let mut saved = draft.connection().unwrap();
        let reference = "a".repeat(32);
        saved.login.auth = Auth::TemporaryKey {
            credential: Some(reference.clone()),
            minutes: 30,
            automatic: true,
        };

        let mut edited = Draft::from_profile(&saved, None, false);
        edited.update(Edit::Tag("Development".into()));
        assert_eq!(edited.connection().unwrap().login.auth, saved.login.auth);

        let mut duplicate = Draft::from_profile(&saved, None, true);
        assert!(matches!(
            duplicate.connection().unwrap().login.auth,
            Auth::TemporaryKey {
                credential: None,
                ..
            }
        ));
        assert!(duplicate.begin_temporary_paste(10));
        duplicate.update(Edit::Login(false, LoginEdit::Username("other".into())));
        assert!(!duplicate.finish_temporary_paste(10, Password::new("fixture".into())));
        assert!(duplicate.pending_temporary_key().is_none());
    }
}
