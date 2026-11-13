use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};

use iced::widget::{
    self, button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use kortty_ssh::{ConnectOptions, KeepAlive};
use kortty_terminal::TerminalProfile;
use kortty_vault::Secret;
use serde::{Deserialize, Serialize};

use crate::connection_store::{Auth, Connection, SavedLogin};
use crate::session_security::IsolationLevel;
use crate::ssh_worker::sandbox_available;

mod temporary_key;
pub use temporary_key::TemporaryKey;

const INLINE_LABEL_WIDTH: f32 = 115.0;
const MAX_HOST_INPUT_BYTES: usize = 256;
const MAX_USERNAME_BYTES: usize = 255;
const MAX_PASSWORD_BYTES: usize = 4096;
const MAX_KEY_PATH_BYTES: usize = 4096;

type ServerAddress = (String, u16);

/// An SSH connection request awaiting worker-side DNS and key-file handling.
#[derive(Debug)]
pub struct Request {
    pub target: LoginRequest,
    pub jump: Option<LoginRequest>,
    pub profile: TerminalProfile,
    pub isolation: IsolationLevel,
    pub options: ConnectOptions,
}

#[derive(Debug)]
pub struct LoginRequest {
    pub endpoint: LoginTarget,
    pub auth: Authentication,
    pub keyboard_interactive: bool,
}

impl LoginRequest {
    pub(crate) fn keyboard_options(&self) -> KeyboardOptions {
        KeyboardOptions {
            primary: matches!(self.auth, Authentication::KeyboardInteractive),
            follow_up: self.keyboard_interactive,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct KeyboardOptions {
    primary: bool,
    follow_up: bool,
}

impl KeyboardOptions {
    pub(crate) fn enabled(self) -> bool {
        self.primary || self.follow_up
    }
}

pub enum Authentication {
    KeyboardInteractive,
    Password(Password),
    PrivateKey(PathBuf),
    KeyWithPassphrase(PathBuf, Password),
    TemporaryKey(TemporaryKey),
    VaultPassword(String),
    VaultKey(PathBuf, String),
    StoredKey { id: u64, explicit: bool },
    ResolvedKey(Box<crate::ssh_key_catalog::KeyRecord>, Option<Password>),
}

impl fmt::Debug for Authentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::KeyboardInteractive => "KeyboardInteractive",
            Self::Password(_) => "Password([redacted])",
            Self::PrivateKey(_) => "PrivateKey([redacted])",
            Self::KeyWithPassphrase(..) => "KeyWithPassphrase([redacted])",
            Self::TemporaryKey(_) => "TemporaryKey([redacted])",
            Self::VaultPassword(_) | Self::VaultKey(..) => "Vault reference",
            Self::StoredKey { .. } | Self::ResolvedKey(..) => "Registered SSH key",
        })
    }
}

/// A password whose debug representation never exposes its contents.
#[derive(Clone, Default)]
pub struct Password(Secret);

impl Password {
    pub fn new(value: String) -> Self {
        Self(Secret::new(value))
    }

    pub fn from_secret(value: Secret) -> Self {
        Self(value)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Moves the credential to the SSH worker's zeroizing credential.
    pub fn into_string(mut self) -> String {
        std::mem::take(&mut *self.0)
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Password,
    PrivateKey,
    StoredKey,
    VaultPassword,
    TemporaryKey,
    KeyboardInteractive,
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Password => "Password",
            Self::PrivateKey => "SSH private key",
            Self::StoredKey => "Registered SSH key",
            Self::VaultPassword => "Vault password",
            Self::TemporaryKey => "Temporary SSH key",
            Self::KeyboardInteractive => "Keyboard-interactive",
        })
    }
}

#[derive(Clone)]
pub enum LoginMessage {
    Host(String),
    Port(String),
    Username(String),
    Authentication(Method),
    KeyboardFollowUp(bool),
    Password(Password),
    Passphrase(Password),
    KeyFile(String),
    StoredKey(KeyChoice),
    VaultPassword(String),
    TemporaryLifetime(String),
}

impl fmt::Debug for LoginMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Edit events may contain pasted text in any field. Log their types only.
        formatter.write_str(match self {
            Self::Host(_) => "Host",
            Self::Port(_) => "Port",
            Self::Username(_) => "Username",
            Self::Authentication(_) => "Authentication",
            Self::KeyboardFollowUp(_) => "KeyboardFollowUp",
            Self::Password(_) => "Password([redacted])",
            Self::Passphrase(_) => "Passphrase([redacted])",
            Self::KeyFile(_) => "KeyFile([redacted])",
            Self::StoredKey(_) => "StoredKey",
            Self::VaultPassword(_) => "VaultPassword([redacted])",
            Self::TemporaryLifetime(_) => "TemporaryLifetime",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyChoice {
    pub(crate) id: u64,
    pub(crate) name: String,
}

impl KeyChoice {
    pub(crate) fn selected(choices: &[Self], id: Option<u64>) -> Option<Self> {
        let id = id?;
        Some(
            choices
                .iter()
                .find(|choice| choice.id == id)
                .cloned()
                .unwrap_or_else(|| Self {
                    id,
                    name: "Registered key unavailable".into(),
                }),
        )
    }
}

impl fmt::Display for KeyChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Target(LoginMessage),
    Jump(LoginMessage),
    UseJump(bool),
    Profile(TerminalProfile),
    Isolation(IsolationLevel),
    Startup(StartupField, String),
    Logging(crate::terminal_logging::Edit),
    Connect,
    OpenVault,
    PasteTemporaryKey,
    Cancel,
}

pub struct SshDialog {
    target: LoginForm,
    jump: Option<LoginForm>,
    profile: TerminalProfile,
    isolation: IsolationLevel,
    minimum_isolation: IsolationLevel,
    startup: StartupFields,
    logging: crate::terminal_logging::Form,
}

#[derive(Clone, Copy, Debug)]
pub enum StartupField {
    Timeout,
    Attempts,
}

pub(crate) struct StartupFields {
    seconds: String,
    attempts: String,
    keepalive: KeepAliveMode,
    keepalive_seconds: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeepAliveMode(Option<bool>);

impl fmt::Display for KeepAliveMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            None => "Use the global setting",
            Some(true) => "Enabled",
            Some(false) => "Disabled",
        })
    }
}

#[derive(Clone, Debug)]
pub enum KeepAliveEdit {
    Mode(KeepAliveMode),
    Interval(String),
}

impl StartupFields {
    pub(crate) fn new(options: ConnectOptions) -> Self {
        Self {
            seconds: options.seconds().to_string(),
            attempts: options.attempts().to_string(),
            keepalive: KeepAliveMode(options.keepalive().map(KeepAlive::enabled)),
            keepalive_seconds: options
                .keepalive()
                .unwrap_or_default()
                .seconds()
                .to_string(),
        }
    }

    pub(crate) fn update(&mut self, field: StartupField, value: String) {
        let (target, limit) = match field {
            StartupField::Timeout => (&mut self.seconds, 3),
            StartupField::Attempts => (&mut self.attempts, 2),
        };
        if value.len() <= limit && value.bytes().all(|byte| byte.is_ascii_digit()) {
            *target = value;
        }
    }

    pub(crate) fn options(&self) -> Result<ConnectOptions, ValidationError> {
        let seconds = self.seconds.parse().map_err(|_| ValidationError::Startup)?;
        let attempts = self
            .attempts
            .parse()
            .map_err(|_| ValidationError::Startup)?;
        let keepalive = self
            .keepalive
            .0
            .map(|enabled| {
                let seconds = self
                    .keepalive_seconds
                    .parse()
                    .map_err(|_| ValidationError::KeepAlive)?;
                KeepAlive::new(enabled, seconds).map_err(|_| ValidationError::KeepAlive)
            })
            .transpose()?;
        ConnectOptions::new(seconds, attempts)
            .map(|options| options.with_keepalive(keepalive))
            .map_err(|_| ValidationError::Startup)
    }

    pub(crate) fn update_keepalive(&mut self, edit: KeepAliveEdit) {
        match edit {
            KeepAliveEdit::Mode(mode) => self.keepalive = mode,
            KeepAliveEdit::Interval(value)
                if value.len() <= 3 && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                self.keepalive_seconds = value;
            }
            KeepAliveEdit::Interval(_) => {}
        }
    }

    pub(crate) fn keepalive_view(&self, label_width: f32) -> Element<'_, KeepAliveEdit> {
        let modes = [
            KeepAliveMode(None),
            KeepAliveMode(Some(true)),
            KeepAliveMode(Some(false)),
        ];
        column![
            labeled(
                label_width,
                "SSH keepalive",
                pick_list(modes, Some(self.keepalive), KeepAliveEdit::Mode)
            ),
            self.keepalive.0.is_some().then(|| labeled(
                label_width,
                "Keepalive interval",
                row![
                    text_input("5–600", &self.keepalive_seconds)
                        .width(72)
                        .on_input(KeepAliveEdit::Interval),
                    text("seconds").size(13),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )),
        ]
        .spacing(6)
        .into()
    }

    pub(crate) fn view(&self, label_width: f32) -> Element<'_, (StartupField, String)> {
        column![
            labeled(
                label_width,
                "Timeout",
                row![
                    text_input("1–300", &self.seconds)
                        .width(72)
                        .on_input(|value| (StartupField::Timeout, value)),
                    text("seconds").size(13),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            ),
            labeled(
                label_width,
                "Connection attempts",
                row![
                    text_input("0–20", &self.attempts)
                        .width(72)
                        .on_input(|value| (StartupField::Attempts, value)),
                    text("0 = no retries").size(13),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            ),
        ]
        .spacing(6)
        .into()
    }
}

struct LoginForm {
    host_field: widget::Id,
    credential_field: widget::Id,
    identity_locked: bool,
    host: String,
    host_edited: bool,
    port: String,
    username: String,
    method: Method,
    keyboard_interactive: bool,
    password: Password,
    password_too_long: bool,
    key_file: String,
    passphrase: Password,
    passphrase_too_long: bool,
    stored_password: Option<String>,
    stored_passphrase: Option<String>,
    stored_key: Option<u64>,
    explicit_key: bool,
    explicit_vault: bool,
    temporary_key: Option<temporary_key::Form>,
}

impl SshDialog {
    pub(crate) fn restore_keyboard_options(
        &mut self,
        target: KeyboardOptions,
        jump: Option<KeyboardOptions>,
    ) {
        self.target.restore_keyboard_options(target);
        if let Some((form, options)) = self.jump.as_mut().zip(jump) {
            form.restore_keyboard_options(options);
        }
    }

    pub fn new(profile: TerminalProfile) -> Self {
        Self {
            target: LoginForm::new(),
            jump: None,
            profile,
            isolation: IsolationLevel::None,
            minimum_isolation: IsolationLevel::None,
            startup: StartupFields::new(ConnectOptions::default()),
            logging: crate::terminal_logging::Form::new(Default::default()),
        }
    }

    /// Creates a fresh form for a saved route. Credentials must be entered again.
    pub fn with_target(
        profile: TerminalProfile,
        endpoint: &LoginTarget,
        jump: Option<&LoginTarget>,
    ) -> Self {
        Self {
            target: LoginForm::for_endpoint(endpoint),
            jump: jump.map(LoginForm::for_endpoint),
            profile,
            isolation: IsolationLevel::None,
            minimum_isolation: IsolationLevel::None,
            startup: StartupFields::new(ConnectOptions::default()),
            logging: crate::terminal_logging::Form::new(Default::default()),
        }
    }

    pub fn from_saved(profile: TerminalProfile, connection: &Connection) -> Self {
        Self {
            target: LoginForm::for_saved(&connection.login),
            jump: connection.jump.as_ref().map(LoginForm::for_saved),
            profile,
            isolation: connection.isolation,
            minimum_isolation: IsolationLevel::None,
            startup: StartupFields::new(connection.startup),
            logging: crate::terminal_logging::Form::new(connection.logging.clone()),
        }
    }

    pub(crate) fn with_fixed_route(mut self) -> Self {
        self.target.identity_locked = true;
        if let Some(jump) = &mut self.jump {
            jump.identity_locked = true;
        }
        self
    }

    pub(crate) fn enable_teamwork_temporary_key(&mut self, selected: bool) {
        self.target
            .temporary_key
            .get_or_insert_with(Default::default);
        if selected {
            self.target.method = Method::TemporaryKey;
        }
    }

    pub(crate) fn temporary_key_selected(&self) -> bool {
        self.target.method == Method::TemporaryKey
    }

    pub(crate) fn begin_temporary_key_paste(&mut self, request: u64) -> bool {
        if self.temporary_key_selected()
            && let Some(form) = &mut self.target.temporary_key
        {
            form.begin_paste(request);
            return true;
        }
        false
    }

    pub(crate) fn finish_temporary_key_paste(&mut self, request: u64, value: Password) {
        if self.temporary_key_selected()
            && let Some(form) = &mut self.target.temporary_key
        {
            form.finish_paste(request, value);
        }
    }

    pub(crate) fn expects_temporary_key_paste(&self, request: u64) -> bool {
        self.temporary_key_selected()
            && self
                .target
                .temporary_key
                .as_ref()
                .is_some_and(|form| form.expects_paste(request))
    }

    pub(crate) fn restore_temporary_key(&mut self, key: TemporaryKey) {
        if let Some(form) = &mut self.target.temporary_key {
            self.target.method = Method::TemporaryKey;
            form.restore(key);
        }
    }

    pub(crate) fn with_startup(mut self, options: ConnectOptions) -> Self {
        self.set_startup(options);
        self
    }

    pub(crate) fn set_startup(&mut self, options: ConnectOptions) {
        self.startup = StartupFields::new(options);
    }

    pub(crate) fn set_logging(&mut self, config: crate::terminal_logging::Config) {
        self.logging = crate::terminal_logging::Form::new(config);
    }

    pub(crate) fn logging_config(&self) -> std::io::Result<crate::terminal_logging::Config> {
        self.logging.config()
    }

    pub(crate) fn logging_view(&self, eligible: bool) -> Element<'_, Message> {
        self.logging.view(eligible, false).map(Message::Logging)
    }

    pub(crate) fn with_isolation(
        mut self,
        requested: IsolationLevel,
        minimum: IsolationLevel,
    ) -> Self {
        self.minimum_isolation = minimum;
        self.isolation = self.isolation.max(requested).max(minimum);
        self
    }

    pub(crate) fn with_launch_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = isolation;
        self.minimum_isolation = isolation;
        self
    }

    pub fn focus(&self) -> Task<Message> {
        let field = if self.target.identity_locked {
            &self.target.credential_field
        } else {
            &self.target.host_field
        };
        widget::operation::focus(field.clone())
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Target(message) => self.target.update(message),
            Message::Jump(message) => {
                if let Some(jump) = &mut self.jump {
                    jump.update(message);
                }
            }
            Message::UseJump(true) if !self.target.identity_locked => {
                self.jump.get_or_insert_with(LoginForm::new);
            }
            Message::UseJump(false) if !self.target.identity_locked => self.jump = None,
            Message::Profile(profile) => self.profile = profile,
            Message::Startup(field, value) => self.startup.update(field, value),
            Message::Logging(edit) => self.logging.update(edit),
            Message::Isolation(level)
                if level >= self.minimum_isolation && supported_isolation(level) =>
            {
                self.isolation = level;
            }
            Message::Cancel => {
                self.target.clear_password();
                if let Some(jump) = &mut self.jump {
                    jump.clear_password();
                }
            }
            Message::Connect
            | Message::OpenVault
            | Message::PasteTemporaryKey
            | Message::Isolation(_)
            | Message::UseJump(_) => {}
        }
    }

    pub fn take_request(&mut self) -> Result<Request, ValidationError> {
        let options = self.startup.options()?;
        self.logging
            .config()
            .map_err(|_| ValidationError::Logging)?;
        let endpoint = self.target.validate()?;
        let jump_endpoint = self.jump.as_ref().map(LoginForm::validate).transpose()?;
        let jump = self
            .jump
            .as_mut()
            .zip(jump_endpoint)
            .map(|(form, endpoint)| form.take_login(endpoint));
        Ok(Request {
            target: self.target.take_login(endpoint),
            jump,
            profile: self.profile,
            isolation: self.isolation,
            options,
        })
    }

    pub fn can_connect(&self) -> bool {
        self.startup.options().is_ok()
            && self.logging.config().is_ok()
            && self.target.validate().is_ok()
            && self
                .jump
                .as_ref()
                .is_none_or(|jump| jump.validate().is_ok())
            && supported_isolation(self.isolation)
    }

    pub fn target_endpoint(&self) -> Option<LoginTarget> {
        self.target.endpoint().ok()
    }

    pub(crate) fn server_addresses(&self) -> Option<(ServerAddress, Option<ServerAddress>)> {
        let target = self.target.address().ok()?;
        let jump = self
            .jump
            .as_ref()
            .map(LoginForm::address)
            .transpose()
            .ok()?;
        Some((target, jump))
    }

    pub fn vault_password(&self) -> Option<&str> {
        self.target.stored_password.as_deref()
    }

    pub fn explicit_vault(&self) -> Option<(LoginTarget, String)> {
        if self.target.explicit_vault && self.target.method == Method::VaultPassword {
            return Some((self.target_endpoint()?, self.vault_password()?.to_owned()));
        }
        None
    }

    pub fn profile(&self) -> TerminalProfile {
        self.profile
    }

    pub fn saved_draft(&self, name: &str) -> std::io::Result<Connection> {
        let login = self.target.saved_login()?;
        let draft = Connection {
            id: 0,
            name: if name.trim().is_empty() {
                format!("{}@{}", login.endpoint.username, login.endpoint.host)
            } else {
                name.trim().to_owned()
            },
            tag: String::new(),
            login,
            jump: self.jump.as_ref().map(LoginForm::saved_login).transpose()?,
            emulation: self.profile.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: self.isolation,
            startup: self.startup.options().map_err(std::io::Error::other)?,
            logging: self.logging.config()?,
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        };
        draft.validate()?;
        Ok(draft)
    }

    pub fn identity_view(&self, label_width: f32) -> Element<'_, Message> {
        self.target
            .identity_view(self.can_connect(), Message::Target, label_width)
    }

    pub fn authentication_view(
        &self,
        label_width: f32,
        keys: &[KeyChoice],
    ) -> Element<'_, Message> {
        self.target
            .authentication_view(self.can_connect(), Message::Target, label_width, keys)
    }

    pub fn options_view(&self, label_width: f32, keys: &[KeyChoice]) -> Element<'_, Message> {
        let can_connect = self.can_connect();
        let jump = self.jump.as_ref().map(LoginForm::validate).transpose();
        let mut content = column![
            self.startup
                .view(label_width)
                .map(|(field, value)| Message::Startup(field, value)),
            checkbox(self.jump.is_some())
                .label("Use a jump host")
                .on_toggle_maybe((!self.target.identity_locked).then_some(Message::UseJump)),
        ]
        .spacing(8);
        if let Some(form) = &self.jump {
            content = content.push(text("Jump host").size(17)).push(form.view(
                can_connect,
                Message::Jump,
                label_width,
                keys,
            ));
            if form.host_edited
                && let Err(error) = jump
            {
                content = content.push(text(error.message()).size(13));
            }
        }
        let isolation: Vec<_> = IsolationLevel::ALL
            .into_iter()
            .filter(|level| *level >= self.minimum_isolation && supported_isolation(*level))
            .collect();
        content = content.push(labeled(
            label_width,
            "Session isolation",
            pick_list(isolation, Some(self.isolation), Message::Isolation),
        ));
        if !supported_isolation(self.isolation) {
            content = content.push(text("SSH sandbox is unavailable on this system.").size(13));
        }
        content.into()
    }

    pub fn view_for(
        &self,
        sftp: bool,
        logging_allowed: bool,
        keys: &[KeyChoice],
    ) -> Element<'_, Message> {
        let can_connect = self.can_connect();
        let status = self.validation_notice();
        let mut content = column![
            text(if sftp {
                "SFTP connection"
            } else {
                "SSH connection"
            })
            .size(22),
            self.target
                .view(can_connect, Message::Target, INLINE_LABEL_WIDTH, keys),
        ];
        if !sftp {
            content = content.push(labeled(
                INLINE_LABEL_WIDTH,
                "Emulation",
                pick_list(TerminalProfile::ALL, Some(self.profile), Message::Profile),
            ));
        }
        content = content.push(self.options_view(INLINE_LABEL_WIDTH, keys));
        if !sftp {
            content = content.push(self.logging_view(logging_allowed));
        }
        let mut controls = row![
            button("Connect").on_press_maybe(can_connect.then_some(Message::Connect)),
            button("Cancel").on_press(Message::Cancel),
        ]
        .spacing(8);
        if self.uses_vault() {
            controls = controls.push(button("Vault…").on_press(Message::OpenVault));
        }
        let content = content
            .push(controls)
            .push(text(status.unwrap_or_default()).size(13))
            .spacing(8)
            .padding(16)
            .max_width(650);
        scrollable(container(content).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    pub fn uses_vault(&self) -> bool {
        self.target.uses_vault() || self.jump.as_ref().is_some_and(LoginForm::uses_vault)
    }

    pub(crate) fn restore_explicit_key(&mut self, jump: bool, endpoint: &LoginTarget, id: u64) {
        let form = if jump {
            self.jump.as_mut()
        } else {
            Some(&mut self.target)
        };
        if let Some(form) = form
            && form.endpoint().as_ref() == Ok(endpoint)
        {
            form.select_registered_key(id, true);
        }
    }

    pub fn validation_notice(&self) -> Option<&'static str> {
        self.startup
            .options()
            .err()
            .map(ValidationError::message)
            .or_else(|| {
                self.logging
                    .config()
                    .err()
                    .map(|_| ValidationError::Logging.message())
            })
            .or_else(|| {
                self.target
                    .host_edited
                    .then(|| self.target.validate().err().map(ValidationError::message))
                    .flatten()
            })
    }
}

fn supported_isolation(level: IsolationLevel) -> bool {
    level != IsolationLevel::Sandbox || sandbox_available()
}

impl LoginForm {
    fn new() -> Self {
        Self {
            host_field: widget::Id::unique(),
            credential_field: widget::Id::unique(),
            identity_locked: false,
            host: String::new(),
            host_edited: false,
            port: String::from("22"),
            username: String::from("root"),
            method: Method::Password,
            keyboard_interactive: false,
            password: Password::default(),
            password_too_long: false,
            key_file: String::new(),
            passphrase: Password::default(),
            passphrase_too_long: false,
            stored_password: None,
            stored_passphrase: None,
            stored_key: None,
            explicit_key: false,
            explicit_vault: false,
            temporary_key: None,
        }
    }

    fn for_endpoint(endpoint: &LoginTarget) -> Self {
        Self {
            host: bounded(endpoint.host.clone(), MAX_HOST_INPUT_BYTES),
            host_edited: true,
            port: endpoint.port.to_string(),
            username: bounded(endpoint.username.clone(), MAX_USERNAME_BYTES),
            ..Self::new()
        }
    }

    fn for_saved(login: &SavedLogin) -> Self {
        let mut form = Self::for_endpoint(&login.endpoint);
        form.keyboard_interactive = login.keyboard_interactive;
        match &login.auth {
            Auth::Prompt => {}
            Auth::KeyboardInteractive => form.method = Method::KeyboardInteractive,
            Auth::VaultPassword(id) => {
                form.method = Method::VaultPassword;
                form.stored_password = Some(id.clone());
            }
            Auth::KeyFile { path, passphrase } => {
                form.method = Method::PrivateKey;
                form.key_file = path.to_string_lossy().into_owned();
                form.stored_passphrase = passphrase.clone();
            }
            Auth::StoredKey(id) => form.select_registered_key(*id, false),
            Auth::Configured {
                method,
                credential,
                key,
            } => {
                use crate::connection_store::AuthMethod;
                form.stored_password = credential.clone();
                form.stored_key = *key;
                form.method = match method {
                    AuthMethod::Password if credential.is_some() => Method::VaultPassword,
                    AuthMethod::Password => Method::Password,
                    AuthMethod::PublicKey => Method::StoredKey,
                    AuthMethod::KeyboardInteractive => Method::KeyboardInteractive,
                };
            }
        }
        form
    }

    fn restore_keyboard_options(&mut self, options: KeyboardOptions) {
        if options.primary {
            self.method = Method::KeyboardInteractive;
        } else if self.method == Method::KeyboardInteractive {
            self.method = Method::Password;
        }
        self.keyboard_interactive = options.follow_up;
    }

    fn update(&mut self, message: LoginMessage) {
        match message {
            LoginMessage::Host(value) if !self.identity_locked => {
                if self.host != value {
                    self.clear_references();
                }
                self.host = bounded(value, MAX_HOST_INPUT_BYTES);
                self.host_edited = true;
            }
            LoginMessage::Port(value) if !self.identity_locked => {
                if self.port != value {
                    self.clear_references();
                }
                self.port = bounded(value, 16);
            }
            LoginMessage::Username(value) if !self.identity_locked => {
                if self.username != value {
                    self.clear_references();
                }
                self.username = bounded(value, MAX_USERNAME_BYTES + 4);
            }
            LoginMessage::Authentication(method) => {
                if (method == Method::VaultPassword && self.stored_password.is_none())
                    || (method == Method::TemporaryKey && self.temporary_key.is_none())
                {
                    return;
                }
                self.method = method;
                self.clear_password();
            }
            LoginMessage::KeyboardFollowUp(enabled) => self.keyboard_interactive = enabled,
            LoginMessage::Password(password) if self.method == Method::Password => {
                self.password_too_long = password.0.len() > MAX_PASSWORD_BYTES;
                self.password = if self.password_too_long {
                    Password::default()
                } else {
                    password
                };
            }
            LoginMessage::KeyFile(value) if self.method == Method::PrivateKey => {
                if self.key_file != value {
                    self.stored_passphrase = None;
                }
                self.key_file = bounded(value, MAX_KEY_PATH_BYTES);
            }
            LoginMessage::StoredKey(choice) if self.method == Method::StoredKey => {
                self.select_registered_key(choice.id, true);
            }
            LoginMessage::VaultPassword(id) if !id.is_empty() => {
                self.clear_password();
                self.stored_password = Some(id);
                self.explicit_vault = true;
                self.method = Method::VaultPassword;
            }
            LoginMessage::Passphrase(passphrase) if self.method == Method::PrivateKey => {
                self.passphrase_too_long = passphrase.0.len() > MAX_PASSWORD_BYTES;
                self.passphrase = if self.passphrase_too_long {
                    Password::default()
                } else {
                    passphrase
                };
            }
            LoginMessage::TemporaryLifetime(minutes) if self.method == Method::TemporaryKey => {
                if let Some(form) = &mut self.temporary_key {
                    form.set_minutes(minutes);
                }
            }
            _ => {}
        }
    }

    fn take_login(&mut self, endpoint: LoginTarget) -> LoginRequest {
        let auth = match self.method {
            Method::KeyboardInteractive => Authentication::KeyboardInteractive,
            Method::Password => Authentication::Password(std::mem::take(&mut self.password)),
            Method::VaultPassword => {
                Authentication::VaultPassword(self.stored_password.clone().unwrap_or_default())
            }
            Method::StoredKey => Authentication::StoredKey {
                id: self.stored_key.unwrap_or_default(),
                explicit: self.explicit_key,
            },
            Method::TemporaryKey => Authentication::TemporaryKey(
                self.temporary_key
                    .as_mut()
                    .and_then(temporary_key::Form::take)
                    .expect("temporary key was validated"),
            ),
            Method::PrivateKey => {
                let path = PathBuf::from(&self.key_file);
                if !self.passphrase.0.is_empty() {
                    Authentication::KeyWithPassphrase(path, std::mem::take(&mut self.passphrase))
                } else if let Some(id) = &self.stored_passphrase {
                    Authentication::VaultKey(path, id.clone())
                } else {
                    Authentication::PrivateKey(path)
                }
            }
        };
        LoginRequest {
            endpoint,
            auth,
            keyboard_interactive: self.keyboard_interactive
                && self.method != Method::KeyboardInteractive,
        }
    }

    fn view(
        &self,
        can_connect: bool,
        wrap: fn(LoginMessage) -> Message,
        label_width: f32,
        keys: &[KeyChoice],
    ) -> Element<'_, Message> {
        column![
            self.identity_view(can_connect, wrap, label_width),
            self.authentication_view(can_connect, wrap, label_width, keys),
        ]
        .spacing(8)
        .into()
    }

    fn identity_view(
        &self,
        can_connect: bool,
        wrap: fn(LoginMessage) -> Message,
        label_width: f32,
    ) -> Element<'_, Message> {
        let connect = can_connect.then_some(Message::Connect);
        let endpoint = row![
            text_input("Hostname or IP address", &self.host)
                .id(self.host_field.clone())
                .on_input_maybe(
                    (!self.identity_locked).then_some(move |value| wrap(LoginMessage::Host(value)))
                )
                .on_submit_maybe(connect.clone())
                .width(Length::Fill),
            text("Port"),
            text_input("22", &self.port)
                .on_input_maybe(
                    (!self.identity_locked).then_some(move |value| wrap(LoginMessage::Port(value)))
                )
                .on_submit_maybe(connect.clone())
                .width(80),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            labeled(label_width, "Host", endpoint),
            labeled(
                label_width,
                "Username",
                text_input("SSH username", &self.username)
                    .on_input_maybe(
                        (!self.identity_locked)
                            .then_some(move |value| wrap(LoginMessage::Username(value)))
                    )
                    .on_submit_maybe(connect)
                    .width(Length::Fill),
            ),
        ]
        .spacing(8)
        .into()
    }

    fn authentication_view(
        &self,
        can_connect: bool,
        wrap: fn(LoginMessage) -> Message,
        label_width: f32,
        keys: &[KeyChoice],
    ) -> Element<'_, Message> {
        let connect = can_connect.then_some(Message::Connect);
        let credential = match self.method {
            Method::Password => labeled(
                label_width,
                "Password",
                text_input("SSH password", &self.password.0)
                    .id(self.credential_field.clone())
                    .secure(true)
                    .on_input(move |value| wrap(LoginMessage::Password(Password::new(value))))
                    .on_submit_maybe(connect.clone())
                    .width(Length::Fill),
            ),
            Method::PrivateKey => labeled(
                label_width,
                "Key file",
                column![
                    text_input("Absolute path", &self.key_file)
                        .id(self.credential_field.clone())
                        .on_input(move |value| wrap(LoginMessage::KeyFile(value)))
                        .on_submit_maybe(connect.clone())
                        .width(Length::Fill),
                    text_input(
                        if self.stored_passphrase.is_some() {
                            "Blank uses the vault passphrase"
                        } else {
                            "Private key passphrase (optional)"
                        },
                        &self.passphrase.0,
                    )
                    .secure(true)
                    .on_input(move |value| wrap(LoginMessage::Passphrase(Password::new(value))))
                    .on_submit_maybe(connect.clone())
                    .width(Length::Fill),
                ]
                .spacing(4),
            ),
            Method::StoredKey => labeled(
                label_width,
                "Registered key",
                pick_list(
                    keys.to_vec(),
                    KeyChoice::selected(keys, self.stored_key),
                    move |key| wrap(LoginMessage::StoredKey(key)),
                )
                .placeholder("Select a registered SSH key")
                .width(Length::Fill),
            ),
            Method::VaultPassword => labeled(
                label_width,
                "Password",
                text("Uses the selected credential from the unlocked vault.").size(13),
            ),
            Method::TemporaryKey => {
                let form = self
                    .temporary_key
                    .as_ref()
                    .expect("enabled temporary key form");
                labeled(
                    label_width,
                    "Temporary key",
                    column![
                        row![
                            text_input("1–1440", &form.minutes)
                                .id(self.credential_field.clone())
                                .width(72)
                                .on_input(move |value| wrap(LoginMessage::TemporaryLifetime(
                                    value
                                ))),
                            text("minutes").size(13),
                            button("Paste private key").on_press(Message::PasteTemporaryKey),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        text(form.status()).size(13),
                    ]
                    .spacing(4),
                )
            }
            Method::KeyboardInteractive => {
                text("The server will request responses after its host key is verified.")
                    .size(13)
                    .into()
            }
        };
        let mut methods = vec![
            Method::Password,
            Method::PrivateKey,
            Method::StoredKey,
            Method::KeyboardInteractive,
        ];
        if self.stored_password.is_some() {
            methods.push(Method::VaultPassword);
        }
        if self.temporary_key.is_some() {
            methods.push(Method::TemporaryKey);
        }
        let mut form = column![
            labeled(
                label_width,
                "Authentication",
                pick_list(methods, Some(self.method), move |method| wrap(
                    LoginMessage::Authentication(method)
                ),),
            ),
            credential,
        ]
        .spacing(8);
        if self.method != Method::KeyboardInteractive {
            form = form.push(
                checkbox(self.keyboard_interactive)
                    .label("Also allow keyboard-interactive")
                    .on_toggle(move |enabled| wrap(LoginMessage::KeyboardFollowUp(enabled))),
            );
        }
        form.into()
    }

    fn clear_password(&mut self) {
        self.password = Password::default();
        self.password_too_long = false;
        self.passphrase = Password::default();
        self.passphrase_too_long = false;
        if let Some(form) = &mut self.temporary_key {
            form.clear();
        }
    }

    fn clear_references(&mut self) {
        self.stored_password = None;
        self.stored_passphrase = None;
        self.stored_key = None;
        self.explicit_key = false;
        self.explicit_vault = false;
        if self.method == Method::VaultPassword {
            self.method = Method::Password;
        }
    }

    fn select_registered_key(&mut self, id: u64, explicit: bool) {
        self.clear_password();
        self.stored_key = Some(id);
        self.explicit_key = explicit;
        self.method = Method::StoredKey;
    }

    fn uses_vault(&self) -> bool {
        matches!(self.method, Method::VaultPassword | Method::StoredKey)
            || (self.method == Method::PrivateKey && self.stored_passphrase.is_some())
    }

    fn address(&self) -> Result<ServerAddress, ValidationError> {
        let host = normalize_host(&self.host)?;
        let port = self.port.trim();
        let port = port
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| port.parse::<u16>().ok())
            .flatten()
            .filter(|&port| port != 0)
            .ok_or(ValidationError::Port)?;
        Ok((host, port))
    }

    fn endpoint(&self) -> Result<LoginTarget, ValidationError> {
        let (host, port) = self.address()?;
        let username = self.username.trim();
        if !valid_username(username) {
            return Err(ValidationError::Username);
        }
        Ok(LoginTarget {
            host,
            port,
            username: username.to_owned(),
        })
    }

    fn validate(&self) -> Result<LoginTarget, ValidationError> {
        let endpoint = self.endpoint()?;
        match self.method {
            Method::Password if self.password_too_long => {
                return Err(ValidationError::PasswordTooLong);
            }
            Method::Password if self.password.0.is_empty() => {
                return Err(ValidationError::Password);
            }
            Method::VaultPassword if self.stored_password.is_none() => {
                return Err(ValidationError::Password);
            }
            Method::PrivateKey if self.passphrase_too_long => {
                return Err(ValidationError::PasswordTooLong);
            }
            Method::StoredKey if self.stored_key.is_none_or(|id| id == 0) => {
                return Err(ValidationError::RegisteredKey);
            }
            Method::TemporaryKey
                if self
                    .temporary_key
                    .as_ref()
                    .and_then(temporary_key::Form::key)
                    .is_none() =>
            {
                return Err(ValidationError::TemporaryKey);
            }
            Method::PrivateKey
                if self.key_file.len() > MAX_KEY_PATH_BYTES
                    || unsafe_text(&self.key_file)
                    || !Path::new(&self.key_file).is_absolute() =>
            {
                return Err(ValidationError::PrivateKey);
            }
            _ => {}
        }
        Ok(endpoint)
    }

    fn saved_login(&self) -> std::io::Result<SavedLogin> {
        let endpoint = self.validate().map_err(std::io::Error::other)?;
        let auth = match self.method {
            Method::Password => Auth::Prompt,
            Method::KeyboardInteractive => Auth::KeyboardInteractive,
            Method::VaultPassword => {
                Auth::VaultPassword(self.stored_password.clone().unwrap_or_default())
            }
            Method::PrivateKey => Auth::KeyFile {
                path: PathBuf::from(&self.key_file),
                passphrase: self.stored_passphrase.clone(),
            },
            Method::StoredKey => Auth::StoredKey(self.stored_key.unwrap_or_default()),
            Method::TemporaryKey => {
                return Err(std::io::Error::other(
                    "A temporary SSH key cannot be saved.",
                ));
            }
        };
        Ok(SavedLogin {
            endpoint,
            auth,
            keyboard_interactive: self.keyboard_interactive
                && self.method != Method::KeyboardInteractive,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
}

impl LoginTarget {
    pub(crate) fn servers<'a>(
        &'a self,
        jump: Option<&'a Self>,
    ) -> impl Iterator<Item = (&'a str, u16)> {
        jump.into_iter()
            .chain(std::iter::once(self))
            .map(|login| (login.host.as_str(), login.port))
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.validate_saved()?;
        if self.username.is_empty() {
            return Err(ValidationError::Username);
        }
        Ok(())
    }

    pub(crate) fn validate_saved(&self) -> Result<(), ValidationError> {
        normalize_host(&self.host)?;
        if self.port == 0 {
            return Err(ValidationError::Port);
        }
        if (!self.username.is_empty() && !valid_username(&self.username))
            || self.username.trim() != self.username
        {
            return Err(ValidationError::Username);
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{}@{host}:{}", self.username, self.port)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Host,
    Port,
    Username,
    Password,
    PasswordTooLong,
    PrivateKey,
    RegisteredKey,
    TemporaryKey,
    Startup,
    KeepAlive,
    Logging,
}

impl ValidationError {
    fn message(self) -> &'static str {
        match self {
            Self::Host => "Enter an ASCII hostname or an IPv4 or IPv6 address.",
            Self::Port => "Enter a port from 1 to 65535.",
            Self::Username => "Enter an SSH username without control characters.",
            Self::Password => "Enter an SSH password.",
            Self::PasswordTooLong => "The password exceeds 4096 bytes.",
            Self::PrivateKey => {
                "Enter the absolute path to an Ed25519, NIST ECDSA, or RSA key in OpenSSH or supported PEM format."
            }
            Self::RegisteredKey => "Select a registered SSH key.",
            Self::TemporaryKey => "Paste a temporary SSH private key before connecting.",
            Self::Startup => {
                "Enter a timeout from 1 to 300 seconds and 0 to 20 connection attempts."
            }
            Self::KeepAlive => "Enter an SSH keepalive interval from 5 to 600 seconds.",
            Self::Logging => "Check the Terminal Logging folder, size and retention values.",
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ValidationError {}

pub(crate) fn normalize_host(input: &str) -> Result<String, ValidationError> {
    let host = input.trim();
    if let Some(address) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return address
            .parse::<Ipv6Addr>()
            .map(|address| address.to_string())
            .map_err(|_| ValidationError::Host);
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.len() > 253
        || name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || !name.split('.').all(valid_dns_label)
    {
        return Err(ValidationError::Host);
    }
    // Preserve a DNS root dot so resolver search suffixes cannot change the name.
    Ok(host.to_ascii_lowercase())
}

fn valid_dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    bytes.len() <= 63
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

pub(crate) fn valid_username(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_USERNAME_BYTES && !unsafe_text(value)
}

pub(crate) fn unsafe_text(value: &str) -> bool {
    value.chars().any(|ch| {
        ch.is_control() || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

fn bounded(value: String, limit: usize) -> String {
    if value.len() <= limit {
        value
    } else {
        String::new()
    }
}

fn labeled<'a, M: 'a>(
    label_width: f32,
    label: &'a str,
    field: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    row![text(label).width(label_width), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_ssh_keepalive_edit_validates_and_can_return_to_inheritance() {
        let mut fields = StartupFields::new(ConnectOptions::default());
        assert_eq!(fields.options().unwrap().keepalive(), None);
        fields.update_keepalive(KeepAliveEdit::Mode(KeepAliveMode(Some(false))));
        fields.update_keepalive(KeepAliveEdit::Interval("120".into()));
        assert_eq!(
            fields.options().unwrap().keepalive(),
            Some(KeepAlive::new(false, 120).unwrap())
        );
        fields.update_keepalive(KeepAliveEdit::Interval("601".into()));
        assert_eq!(fields.options().unwrap_err(), ValidationError::KeepAlive);
        fields.update_keepalive(KeepAliveEdit::Mode(KeepAliveMode(None)));
        assert_eq!(fields.options().unwrap().keepalive(), None);
    }

    fn password(value: &str) -> Message {
        Message::Target(LoginMessage::Password(Password::new(value.to_owned())))
    }

    #[test]
    fn password_moves_without_trimming_or_debug_output() {
        let mut dialog = SshDialog::new(TerminalProfile::Vt220);
        dialog.update(Message::Target(LoginMessage::Host(
            "  Example.COM.  ".into(),
        )));
        dialog.update(Message::Target(LoginMessage::Port(" 2222 ".into())));
        dialog.update(Message::Target(LoginMessage::Username("  alice  ".into())));
        let edit = password("  geheim🔒 ");
        assert!(!format!("{edit:?}").contains("geheim"));
        dialog.update(edit);
        let request = dialog.take_request().unwrap();
        assert_eq!(request.target.endpoint.host, "example.com.");
        assert_eq!(request.target.endpoint.port, 2222);
        assert_eq!(request.target.endpoint.username, "alice");
        assert_eq!(request.profile, TerminalProfile::Vt220);
        assert!(!format!("{request:?}").contains("geheim"));
        let Authentication::Password(password) = request.target.auth else {
            panic!("password expected")
        };
        assert!(!format!("{password:?}").contains("geheim"));
        assert_eq!(password.into_string(), "  geheim🔒 ");
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
    }

    #[test]
    fn absolute_key_paths_are_validated_without_reading_them() {
        let mut dialog = SshDialog::new(TerminalProfile::Xterm);
        dialog.update(Message::Target(LoginMessage::Host("localhost".into())));
        dialog.update(password("secret-from-password-mode"));
        dialog.update(Message::Target(LoginMessage::Authentication(
            Method::PrivateKey,
        )));
        dialog.update(Message::Target(LoginMessage::KeyFile(
            "~/.ssh/id_ed25519".into(),
        )));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::PrivateKey
        );
        #[cfg(windows)]
        let path = r"C:\directory-that-need-not-exist\kortty-key";
        #[cfg(not(windows))]
        let path = "/directory-that-need-not-exist/kortty-key";
        let edit = Message::Target(LoginMessage::KeyFile(path.into()));
        assert!(!format!("{edit:?}").contains("kortty-key"));
        dialog.update(edit);
        let request = dialog.take_request().unwrap();
        assert!(!format!("{request:?}").contains("kortty-key"));
        assert!(
            matches!(request.target.auth, Authentication::PrivateKey(file) if file == Path::new(path))
        );
        dialog.update(Message::Target(LoginMessage::Authentication(
            Method::Password,
        )));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
    }

    #[test]
    fn host_accepts_ipv6_and_normalized_dns_without_resolving() {
        for (input, expected) in [
            (" [2001:0DB8:0:0::1] ", "2001:db8::1"),
            ("::1", "::1"),
            ("127.0.0.1", "127.0.0.1"),
            ("XN--BCHER-KVA.example", "xn--bcher-kva.example"),
            ("jump-box.invalid", "jump-box.invalid"),
        ] {
            assert_eq!(normalize_host(input).unwrap(), expected);
        }
        for input in [
            "",
            "ssh://localhost",
            "user@example.com",
            "localhost:2222",
            "[::1]:2222",
            "[127.0.0.1]",
            "127.1",
            "256.0.0.1",
            "a..example",
            "-a.example",
            "bücher.example",
            "host\nother",
        ] {
            assert_eq!(
                normalize_host(input),
                Err(ValidationError::Host),
                "{input:?}"
            );
        }
    }

    #[test]
    fn port_user_and_password_errors_cannot_reuse_previous_input() {
        let mut dialog = SshDialog::new(TerminalProfile::Xterm);
        dialog.update(Message::Target(LoginMessage::Host("localhost".into())));
        dialog.update(password("short"));
        for port in ["0", "65536", "+22", "ssh", ""] {
            dialog.update(Message::Target(LoginMessage::Port(port.into())));
            assert_eq!(dialog.take_request().unwrap_err(), ValidationError::Port);
        }
        dialog.update(Message::Target(LoginMessage::Port("65535".into())));
        dialog.update(Message::Target(LoginMessage::Username(
            "user\nother".into(),
        )));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Username
        );
        dialog.update(Message::Target(LoginMessage::Username("user".into())));
        dialog.update(Message::Target(LoginMessage::Password(Password::new(
            "x".repeat(MAX_PASSWORD_BYTES + 1),
        ))));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::PasswordTooLong
        );
        dialog.update(password("new"));
        dialog.update(Message::Cancel);
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
    }

    #[test]
    fn reconnect_prefills_the_endpoint_without_credentials() {
        let mut dialog = SshDialog::with_target(
            TerminalProfile::Vt100,
            &LoginTarget {
                host: "[::1]".into(),
                port: 2022,
                username: "runner".into(),
            },
            None,
        );
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
        dialog.update(password("fresh"));
        let request = dialog.take_request().unwrap();
        assert_eq!(
            (
                request.target.endpoint.host.as_str(),
                request.target.endpoint.port,
                request.target.endpoint.username.as_str(),
                request.profile
            ),
            ("::1", 2022, "runner", TerminalProfile::Vt100)
        );
    }

    #[test]
    fn jump_requires_separate_credentials_and_never_consumes_an_invalid_route() {
        let mut dialog = SshDialog::with_target(
            TerminalProfile::Xterm,
            &LoginTarget {
                host: "internal.example.".into(),
                port: 22,
                username: "target-user".into(),
            },
            Some(&LoginTarget {
                host: "jump.example".into(),
                port: 2222,
                username: "jump-user".into(),
            }),
        );
        dialog.update(password("target-test-secret"));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
        dialog.update(Message::Jump(LoginMessage::Password(Password::new(
            "jump-test-secret".into(),
        ))));
        let request = dialog.take_request().unwrap();
        assert!(!format!("{request:?}").contains("test-secret"));
        let jump = request.jump.unwrap();
        assert_eq!(request.target.endpoint.host, "internal.example.");
        assert_eq!(jump.endpoint.username, "jump-user");
        assert!(matches!(request.target.auth,
            Authentication::Password(value) if value.0.as_str() == "target-test-secret"));
        assert!(matches!(jump.auth,
            Authentication::Password(value) if value.0.as_str() == "jump-test-secret"));

        dialog.update(Message::UseJump(false));
        dialog.update(password("direct-test-secret"));
        assert!(dialog.take_request().unwrap().jump.is_none());
    }

    #[test]
    fn changing_a_saved_route_drops_the_vault_reference() {
        let endpoint = LoginTarget {
            host: "approved.test".into(),
            port: 22,
            username: "operator".into(),
        };
        let saved = Connection {
            id: 1,
            name: "Saved test".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint,
                auth: Auth::VaultPassword("reference".into()),
                keyboard_interactive: false,
            },
            jump: None,
            emulation: TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: IsolationLevel::None,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        };
        let mut dialog = SshDialog::from_saved(TerminalProfile::Xterm, &saved);
        assert!(matches!(dialog.take_request().unwrap().target.auth,
            Authentication::VaultPassword(id) if id == "reference"));
        dialog.update(Message::Target(LoginMessage::Host("changed.test".into())));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
        dialog.update(Message::Target(LoginMessage::Authentication(
            Method::VaultPassword,
        )));
        assert_eq!(
            dialog.take_request().unwrap_err(),
            ValidationError::Password
        );
    }
}
