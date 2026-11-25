use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    Id, button, checkbox, column, pick_list, radio, row, rule, text, text_input, tooltip,
};
use iced::{Alignment, Element, Length};
use kortty_terminal::TerminalProfile;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::protocol::Kind as ProtocolKind;
use crate::connection_store::{
    Auth, AuthMethod, AuthPreferences, Connection, SavedLogin, TerminalStyle,
    TerminalStylePreference,
};
use crate::session_security::{IsolationChoice, IsolationLevel, resolve_isolation};
use crate::ssh_dialog::{
    KeepAliveEdit, KeyChoice, LoginTarget, Password, StartupField, StartupFields, normalize_host,
};
use crate::terminal_effects::{Effect, editor::EffectDraft};
use crate::workspace::dialog_design::{self, keyboard_table};

use super::color::ColorDraft;
use super::color_picker::{self, Target as ColorTarget};
use super::custom_color;
use super::view::ViewChoices;
use super::{CredentialChoice, Message, os_badge, terminal_overrides, tunnels};

mod authentication;
mod geometry;
mod key_file;
pub(super) mod protocol;
mod tabs;
mod terminal_settings;

pub(crate) use key_file::pick as pick_private_key;

const LOGIN_LABEL_WIDTH: f32 = 148.0;

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
    UseOwnTerminalSettings(Option<Box<TerminalStyle>>),
    ShowAdditionalTerminalSettings(bool),
    UseTabColor(bool),
    ColorPicker(color_picker::Edit),
    ToggleOsBadgePicker,
    OsBadge(Option<String>),
    PasteWarning(Option<PasteWarningMode>),
    Terminal(terminal_overrides::Edit),
    WindowGeometry(geometry::Edit),
    UseJump(bool),
    Login(bool, LoginEdit),
    EnableTunnels(bool),
    RemoveSelectedTunnel,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Connection,
    Settings,
    Tunnels,
    JumpHost,
    Logging,
    Journal,
    WindowGeometry,
    Ai,
}

impl Page {
    fn keyboard_navigation(self) -> bool {
        matches!(
            self,
            Self::Connection
                | Self::Settings
                | Self::Tunnels
                | Self::JumpHost
                | Self::Logging
                | Self::Journal
                | Self::WindowGeometry
                | Self::Ai
        )
    }

    const ALL: [Self; 8] = [
        Self::Connection,
        Self::Settings,
        Self::Tunnels,
        Self::JumpHost,
        Self::Logging,
        Self::Journal,
        Self::WindowGeometry,
        Self::Ai,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Settings => "Terminal Settings",
            Self::Tunnels => "SSH Tunnels",
            Self::JumpHost => "Jump Server",
            Self::Logging => "Terminal Logging",
            Self::Journal => "Journal",
            Self::WindowGeometry => "Window Geometry",
            Self::Ai => "AI",
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
    PasswordSource(Option<CredentialChoice>),
    InlinePassword(Password),
    InlinePassphrase(Password),
    KeySource(Option<KeyChoice>),
    KeyPath(String),
    StoredKey(KeyChoice),
    Passphrase(CredentialChoice),
    TemporaryLifetime(String),
    TemporaryAutomatic(bool),
    RemoveTemporaryKey,
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
            Self::Ask => "Password",
            Self::Vault => "Vault password",
            Self::Key => "SSH private key",
            Self::StoredKey => "Registered SSH key",
            Self::TemporaryKey => "Temporary SSH key",
            Self::Interactive => "Keyboard-interactive",
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
    protocols: protocol::State,
    page: Page,
    ids: EditorIds,
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
    own_terminal_settings: bool,
    has_own_terminal_draft: bool,
    retained_terminal_style: Option<Box<TerminalStyle>>,
    show_additional_terminal_settings: bool,
    tab_color: ColorDraft,
    color_picker: color_picker::Picker,
    os_badge: Option<String>,
    badge_picker_open: bool,
    badge_cursor: Option<String>,
    paste_warning: Option<PasteWarningMode>,
    terminal: terminal_overrides::Draft,
    window_geometry: geometry::Draft,
    login: Login,
    jump: Login,
    jump_enabled: bool,
    key_picker: Option<(Arc<()>, bool)>,
    tunnels: tunnels::List,
    pub changed: bool,
}

struct EditorIds {
    protocol: Id,
    menu: Id,
    focus_scope: Id,
    page_scope: Id,
    page_scroll: Id,
    emulation: Id,
    encoding: Id,
    folder: Id,
    keepalive: Id,
    badge_list: Id,
    behavior_highlight: Id,
    behavior_paste: Id,
    behavior_isolation: Id,
    behavior_effect: Id,
}

impl EditorIds {
    fn new() -> Self {
        Self {
            protocol: Id::unique(),
            menu: Id::unique(),
            focus_scope: Id::unique(),
            page_scope: Id::unique(),
            page_scroll: Id::unique(),
            emulation: Id::unique(),
            encoding: Id::unique(),
            folder: Id::unique(),
            keepalive: Id::unique(),
            badge_list: Id::unique(),
            behavior_highlight: Id::unique(),
            behavior_paste: Id::unique(),
            behavior_isolation: Id::unique(),
            behavior_effect: Id::unique(),
        }
    }
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
    inline_password: Password,
    inline_passphrase: Password,
    temporary: TemporaryDraft,
    method_picker: Id,
    credential_picker: Id,
    password_source_picker: Id,
    key_source_picker: Id,
    key_browse: Id,
    inline_password_field: Id,
    inline_passphrase_field: Id,
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

impl Draft {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn terminal_effect(&self) -> Option<Effect> {
        self.terminal_effect.effect
    }

    pub fn new() -> Self {
        Self {
            protocols: protocol::State::new(),
            page: Page::default(),
            ids: EditorIds::new(),
            id: 0,
            name: String::new(),
            tag: String::new(),
            profile: TerminalProfile::Xterm,
            incognito: false,
            isolation: IsolationChoice::default(),
            startup: StartupFields::new(
                kortty_ssh::ConnectOptions::new(15, 4).expect("valid new-profile defaults"),
            ),
            logging: crate::terminal_logging::Form::new(Default::default()),
            terminal_effect: EffectDraft::default(),
            command_timestamps: None,
            own_terminal_settings: false,
            has_own_terminal_draft: false,
            retained_terminal_style: None,
            show_additional_terminal_settings: false,
            tab_color: ColorDraft::new(None, None),
            color_picker: color_picker::Picker::default(),
            os_badge: None,
            badge_picker_open: false,
            badge_cursor: None,
            paste_warning: None,
            terminal: terminal_overrides::Draft::new(&Default::default()),
            window_geometry: geometry::Draft::new(None),
            login: Login::new(),
            jump: Login::new_jump(),
            jump_enabled: false,
            key_picker: None,
            tunnels: tunnels::List::new(Vec::new()),
            changed: false,
        }
    }

    pub fn from_profile(connection: &Connection, os_badge: Option<&str>, duplicate: bool) -> Self {
        let style =
            TerminalStyle::from_terminal(&connection.terminal, connection.command_timestamps);
        let own_terminal_settings = !style.is_default()
            || matches!(
                connection.terminal.style_preference,
                Some(TerminalStylePreference::Own),
            );
        let retained_terminal_style = if own_terminal_settings {
            (!style.is_default()).then(|| Box::new(style))
        } else if let Some(TerminalStylePreference::Global(retained)) =
            &connection.terminal.style_preference
        {
            Some(retained.clone())
        } else {
            None
        };
        Self {
            protocols: protocol::State::new(),
            page: Page::default(),
            ids: EditorIds::new(),
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
            own_terminal_settings,
            has_own_terminal_draft: own_terminal_settings,
            retained_terminal_style,
            show_additional_terminal_settings: false,
            tab_color: ColorDraft::new(connection.tab_color, None),
            color_picker: color_picker::Picker::default(),
            os_badge: os_badge.map(str::to_owned),
            badge_picker_open: false,
            badge_cursor: None,
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
                .map_or_else(Login::new_jump, |login| Login::from_saved(login, duplicate)),
            jump_enabled: connection.jump.is_some(),
            key_picker: None,
            tunnels: tunnels::List::new(connection.tunnels.clone()),
            changed: duplicate,
        }
    }

    pub(super) fn begin_temporary_paste(&mut self, request: u64) -> bool {
        if self.protocol() != ProtocolKind::Ssh
            || self.login.method != Method::TemporaryKey
            || self.login.endpoint().is_err()
        {
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

    pub(super) fn pending_inline_credentials(&self) -> super::inline_credentials::Pending {
        let pending = |value: &Password| (!value.as_str().is_empty()).then(|| value.clone());
        super::inline_credentials::Pending {
            password: pending(&self.login.inline_password),
            passphrase: pending(&self.login.inline_passphrase),
            jump_password: self
                .jump_enabled
                .then(|| pending(&self.jump.inline_password))
                .flatten(),
            jump_passphrase: self
                .jump_enabled
                .then(|| pending(&self.jump.inline_passphrase))
                .flatten(),
        }
    }

    pub(super) fn os_badge_id(&self) -> Option<&str> {
        self.os_badge.as_deref()
    }

    pub fn update(&mut self, edit: Edit) {
        if matches!(
            &edit,
            Edit::Login(..)
                | Edit::UseJump(_)
                | Edit::Startup(..)
                | Edit::KeepAlive(_)
                | Edit::EnableTunnels(_)
                | Edit::RemoveSelectedTunnel
        ) {
            if self.protocol() != ProtocolKind::Ssh {
                return;
            }
            self.protocols.dirty.insert(ProtocolKind::Ssh);
        }
        if matches!(&edit, Edit::Page(page) if *page != self.page)
            || matches!(&edit, Edit::UseJump(_) | Edit::Login(..))
        {
            self.key_picker = None;
        }
        match edit {
            Edit::Page(page) => {
                if self.page != page {
                    self.color_picker.close();
                }
                self.page = page;
                return;
            }
            Edit::Name(value) => self.name = bounded(value, 160),
            Edit::Tag(value) => self.tag = bounded(value, 96),
            Edit::Emulation(profile) => {
                self.profile = profile;
                let _ = self
                    .protocols
                    .serial
                    .update(crate::serial_dialog::Message::Profile(profile));
            }
            Edit::Incognito(enabled) => self.incognito = enabled,
            Edit::Isolation(level) => {
                self.isolation = level;
                self.protocols.local.set_protocol_isolation(level.0);
            }
            Edit::Startup(field, value) => self.startup.update(field, value),
            Edit::KeepAlive(edit) => self.startup.update_keepalive(edit),
            Edit::Logging(edit) => {
                if !self.logging.accepts_editor_edit(!self.incognito, &edit) {
                    return;
                }
                self.logging.update(edit);
            }
            Edit::TerminalEffect(effect) => self.terminal_effect.effect = effect,
            Edit::TerminalEffectSpeed(value) => self.terminal_effect.set_speed(value),
            Edit::CommandTimestamps(visibility) if self.own_terminal_settings => {
                self.command_timestamps = visibility
            }
            Edit::UseOwnTerminalSettings(defaults) => {
                let own = defaults.is_some();
                if self.own_terminal_settings == own {
                    return;
                }
                self.color_picker.close();
                self.own_terminal_settings = own;
                if !self.has_own_terminal_draft
                    && let Some(mut defaults) = defaults
                {
                    self.command_timestamps = defaults.command_timestamps;
                    // Inherit the global busy-terminal rule until the editor chooses an override.
                    defaults.confirm_close = None;
                    self.terminal.seed_style(&defaults);
                    self.has_own_terminal_draft = true;
                }
            }
            Edit::ShowAdditionalTerminalSettings(show) => {
                self.show_additional_terminal_settings = show;
                return;
            }
            Edit::UseTabColor(enabled) => {
                self.color_picker.close();
                self.tab_color.set_enabled(enabled);
            }
            Edit::ColorPicker(edit) => {
                if matches!(edit, color_picker::Edit::Open(target, _) if !self.allows_color(target))
                {
                    return;
                }
                if let Some((target, color)) = self.color_picker.update(edit) {
                    self.apply_color(target, color);
                }
                return;
            }
            Edit::ToggleOsBadgePicker => {
                self.badge_picker_open = !self.badge_picker_open;
                self.badge_cursor = None;
                return;
            }
            Edit::OsBadge(badge) if self.badge_picker_open => {
                self.badge_picker_open = false;
                self.badge_cursor = None;
                if self.os_badge == badge {
                    return;
                }
                self.os_badge = badge;
            }
            Edit::PasteWarning(mode) => self.paste_warning = mode,
            Edit::Terminal(edit) => {
                if edit.edits_style() && !self.own_terminal_settings {
                    return;
                }
                self.changed |= self.terminal.update(edit);
                return;
            }
            Edit::WindowGeometry(edit) => {
                self.changed |= self.window_geometry.update(edit);
                return;
            }
            Edit::UseJump(enabled) => {
                if self.jump_enabled == enabled {
                    return;
                }
                self.jump_enabled = enabled;
            }
            Edit::Login(jump, edit) => {
                let login = if jump {
                    if !self.jump_enabled {
                        return;
                    }
                    &mut self.jump
                } else {
                    &mut self.login
                };
                if matches!(&edit, LoginEdit::Method(method) if login.method == *method) {
                    return;
                }
                login.update(edit);
            }
            Edit::EnableTunnels(enabled) if self.page == Page::Tunnels => {
                self.changed |= self.tunnels.enable_all(enabled);
                return;
            }
            Edit::RemoveSelectedTunnel if self.page == Page::Tunnels => {
                self.changed |= self.tunnels.remove_selected();
                return;
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
            .jump_enabled
            .then(|| self.jump.saved())
            .transpose()
            .map_err(|error| format!("Jump host. {error}"))?;
        let mut terminal = self.terminal.value_with_style(self.own_terminal_settings)?;
        terminal.style_preference = if self.own_terminal_settings {
            TerminalStyle::from_terminal(&terminal, self.command_timestamps)
                .is_default()
                .then_some(TerminalStylePreference::Own)
        } else {
            self.retained_terminal_style
                .clone()
                .map(TerminalStylePreference::Global)
        };
        let connection = Connection {
            id: self.id,
            name: name.into(),
            tag: self.tag.trim().into(),
            login,
            jump,
            emulation: self.profile.term_name().into(),
            tunnels: self.tunnels.configs().to_vec(),
            incognito: self.incognito,
            isolation: self.isolation.0.unwrap_or_default(),
            inherit_isolation: self.isolation.0.is_none(),
            startup: self.startup.options().map_err(|error| error.to_string())?,
            logging: self.logging.config().map_err(|error| error.to_string())?,
            terminal_effect: self.terminal_effect.selection(),
            command_timestamps: self
                .own_terminal_settings
                .then_some(self.command_timestamps)
                .flatten(),
            tab_color,
            paste_warning: self.paste_warning,
            window_geometry: self.window_geometry.value()?,
            terminal,
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
        use iced::widget::{container, keyed_column, scrollable};

        let content = match self.page {
            Page::Connection => self.connection_page(choices, custom_badges, group),
            Page::Settings => self.settings_page(choices, inherited),
            Page::Tunnels if self.protocol() != ProtocolKind::Ssh => {
                text("SSH tunnels are retained for SSH connections.").into()
            }
            Page::Tunnels => self.tunnels.view(),
            Page::JumpHost => self.jump_page(choices),
            Page::Logging => self
                .logging
                .editor_view(!self.incognito, choices.logging_policy, &self.name)
                .map(|action| match action {
                    crate::terminal_logging::EditorAction::Edit(edit) => {
                        Message::Edit(Edit::Logging(edit))
                    }
                    crate::terminal_logging::EditorAction::Browse => {
                        Message::BrowseTerminalLogFolder
                    }
                }),
            Page::Journal => self
                .terminal
                .journal_view(choices)
                .map(|edit| Message::Edit(Edit::Terminal(edit))),
            Page::WindowGeometry => self
                .window_geometry
                .view()
                .map(|edit| Message::Edit(Edit::WindowGeometry(edit))),
            Page::Ai => self
                .terminal
                .ai_view(choices, Some(self.id))
                .map(|edit| Message::Edit(Edit::Terminal(edit))),
        };
        let mut scroll = scrollable(container(content).width(Length::Fill).padding(20))
            .height(Length::Fill)
            .width(Length::Fill);
        if self.page.keyboard_navigation() {
            scroll = scroll.id(self.ids.page_scroll.clone());
        }
        let mut page = container(scroll).style(dialog_design::page);
        if self.page.keyboard_navigation() {
            page = page.id(self.ids.page_scope.clone());
        }
        column![
            tabs::arrows(self.page),
            tabs::view(self.page, self.ids.menu.clone()),
            keyed_column![(self.page, page)].height(Length::Fill),
        ]
        .spacing(4)
        .height(Length::Fill)
        .into()
    }

    pub(super) fn tab_shortcut(
        &self,
        key: &iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
    ) -> Option<Message> {
        use iced::keyboard::{Key, Modifiers, key::Named};
        if self.page.keyboard_navigation()
            && *key == Key::Named(Named::Tab)
            && (modifiers - Modifiers::SHIFT).is_empty()
        {
            return Some(Message::FocusEditor(modifiers.shift()));
        }
        tabs::shortcut(self.page, key, modifiers)
    }

    pub(super) fn focus_scope(&self) -> Option<Id> {
        self.page
            .keyboard_navigation()
            .then(|| self.ids.focus_scope.clone())
    }

    pub(super) fn folder_picker_id(&self) -> Id {
        self.ids.folder.clone()
    }

    pub(super) fn begin_tunnel_editor(&self, request: tunnels::Request) -> Option<tunnels::Editor> {
        (self.page == Page::Tunnels && self.protocol() == ProtocolKind::Ssh).then_some(())?;
        self.tunnels.begin(request, self.ids.focus_scope.clone())
    }

    pub(super) fn commit_tunnel_editor(&mut self, editor: &tunnels::Editor) {
        if self.page == Page::Tunnels && self.protocol() == ProtocolKind::Ssh {
            self.changed |= self.tunnels.commit(editor, &self.ids.focus_scope);
        }
    }

    pub(super) fn select_tunnel(&mut self, index: usize) -> iced::Task<Message> {
        if self.page == Page::Tunnels && self.protocol() == ProtocolKind::Ssh {
            self.tunnels.select(index)
        } else {
            iced::Task::none()
        }
    }

    pub(super) fn start_terminal_log_folder_picker(
        &mut self,
        root: &Path,
    ) -> Option<(Arc<()>, PathBuf)> {
        self.logging
            .start_folder_picker(self.page == Page::Logging && !self.incognito, root)
    }

    pub(super) fn finish_terminal_log_folder_picker(
        &mut self,
        ticket: &Arc<()>,
        path: Option<&Path>,
    ) {
        let eligible = self.page == Page::Logging && !self.incognito;
        if let Some(edit) = self.logging.finish_folder_picker(ticket, eligible, path) {
            self.update(Edit::Logging(edit));
        }
    }

    pub(super) fn navigate_badge_picker(&mut self, reference: String) -> iced::Task<Message> {
        if self.page != Page::Connection || !self.badge_picker_open {
            return iced::Task::none();
        }
        let reveal = os_badge::reveal(self.ids.badge_list.clone(), &reference);
        self.badge_cursor = Some(reference);
        reveal
    }

    pub(super) fn focus_editor(&self, backwards: bool) -> iced::Task<Message> {
        use iced::advanced::widget::operation::{focusable, scope};
        use iced_runtime::{Action, task};

        let Some(root) = self.focus_scope() else {
            return iced::Task::none();
        };
        let move_focus = if backwards {
            Action::widget(scope(root, focusable::focus_previous()))
        } else {
            Action::widget(scope(root, focusable::focus_next()))
        };
        task::effect(move_focus).chain(crate::workspace::dialog_design::scroll::reveal_focused(
            self.ids.page_scope.clone(),
            self.ids.page_scroll.clone(),
        ))
    }

    pub(super) fn select_page(&mut self, page: Page) -> iced::Task<Message> {
        use iced::advanced::widget::operation::{focusable, scope};
        use iced_runtime::{Action, task};

        let changed = self.page != page;
        self.update(Edit::Page(page));
        if !changed {
            return iced::Task::none();
        }
        // A newly selected page must not inherit another page's scroll or focus.
        task::effect(Action::widget(scope(
            self.ids.focus_scope.clone(),
            focusable::unfocus(),
        )))
        .chain(iced::widget::operation::snap_to(
            self.ids.page_scroll.clone(),
            iced::widget::operation::RelativeOffset::START,
        ))
    }

    pub(super) fn reveal_ai_skill(&self, id: u64) -> iced::Task<Message> {
        if self.page == Page::Ai {
            self.terminal.reveal_ai_skill(id)
        } else {
            iced::Task::none()
        }
    }

    pub(super) fn style_color_focus(&self, edit: &color_picker::Edit) -> iced::Task<Message> {
        if self.page == Page::Settings
            && !matches!(edit, color_picker::Edit::Open(target, _) if !self.allows_color(*target))
        {
            self.color_picker.focus(edit)
        } else {
            iced::Task::none()
        }
    }

    fn allows_color(&self, target: ColorTarget) -> bool {
        self.page == Page::Settings
            && match target {
                ColorTarget::Foreground | ColorTarget::Background => self.own_terminal_settings,
                ColorTarget::Tab => self.tab_color.enabled(),
            }
    }

    fn apply_color(&mut self, target: ColorTarget, color: Option<crate::terminal_theme::Rgb>) {
        if !self.allows_color(target) {
            return;
        }
        let value = color.map_or_else(String::new, |color| color.to_string());
        self.changed |= match target {
            ColorTarget::Foreground | ColorTarget::Background => self
                .terminal
                .update(terminal_overrides::Edit::Color(target as usize, value)),
            ColorTarget::Tab => color.is_some() && self.tab_color.set_input(value),
        };
    }

    pub(super) fn begin_custom_color(&mut self) -> Option<custom_color::Editor> {
        let (target, current) = self.color_picker.begin_custom()?;
        self.allows_color(target)
            .then(|| custom_color::Editor::new(target, current))
    }

    pub(super) fn finish_custom_color(
        &mut self,
        editor: &custom_color::Editor,
        apply: bool,
        remember: bool,
    ) -> iced::Task<Message> {
        let target = editor.target();
        if !self.allows_color(target) {
            return iced::Task::none();
        }
        if apply && let Some(color) = editor.color() {
            if remember {
                self.color_picker.save_custom(target, color);
            }
            self.apply_color(target, Some(color));
        }
        self.color_picker.focus_trigger(target)
    }

    fn connection_page<'a>(
        &'a self,
        choices: &ViewChoices<'_>,
        custom_badges: &'a Catalog,
        group: Element<'a, Message>,
    ) -> Element<'a, Message> {
        column![
            login_row(
                "Name",
                text_input("Connection name", &self.name)
                    .size(13)
                    .on_input(|value| Message::Edit(Edit::Name(value)))
                    .width(430),
            ),
            self.login
                .endpoint_fields(false, self.protocol() == ProtocolKind::Ssh),
            login_row(
                "Protocol",
                pick_list(
                    ProtocolKind::ALL
                        .into_iter()
                        .filter(|kind| *kind != ProtocolKind::Serial
                            || choices.serial_allowed
                            || self.protocol() == *kind)
                        .collect::<Vec<_>>(),
                    Some(self.protocol()),
                    Message::Protocol,
                )
                .width(160)
                .text_size(13)
                .keyboard_navigation(self.ids.protocol.clone())
            ),
            self.protocols
                .local
                .protocol_fields(self.protocol() == ProtocolKind::Local)
                .map(Message::Local),
            self.protocols
                .serial
                .protocol_fields(self.protocol() == ProtocolKind::Serial)
                .map(Message::Serial),
            login_row(
                "Terminal emulation",
                pick_list(TerminalProfile::ALL, Some(self.profile), |value| {
                    Message::Edit(Edit::Emulation(value))
                })
                .width(430)
                .text_size(13)
                .keyboard_navigation(self.ids.emulation.clone()),
            ),
            login_row(
                "Character encoding",
                iced::widget::container(
                    self.terminal
                        .encoding_picker(Some(self.ids.encoding.clone()))
                        .map(|edit| { Message::Edit(Edit::Terminal(edit)) })
                )
                .width(300),
            ),
            login_row("Folder", group),
            login_row(
                "Tag",
                text_input("Tag (optional)", &self.tag)
                    .size(13)
                    .on_input(|value| Message::Edit(Edit::Tag(value)))
                    .width(430),
            ),
            login_row(
                "OS symbol",
                iced::widget::container(os_badge::picker_control(
                    os_badge::Badge::resolve(self.os_badge_id(), custom_badges),
                    custom_badges,
                    self.badge_picker_open,
                    Some(Message::Edit(Edit::ToggleOsBadgePicker)),
                    |badge| Message::Edit(Edit::OsBadge(badge)),
                    Some(os_badge::KeyboardPicker {
                        scroll: self.ids.badge_list.clone(),
                        cursor: self.badge_cursor.as_deref(),
                    }),
                ))
                .width(300),
            ),
            rule::horizontal(1),
            self.startup
                .editor_view(LOGIN_LABEL_WIDTH, self.protocol() == ProtocolKind::Ssh)
                .map(|(field, value)| Message::Edit(Edit::Startup(field, value))),
            (self.protocol() == ProtocolKind::Ssh).then(|| self
                .startup
                .keepalive_view(LOGIN_LABEL_WIDTH, self.ids.keepalive.clone())
                .map(|edit| Message::Edit(Edit::KeepAlive(edit)))),
            login_row(
                "Host key check",
                text("Verify before authentication").size(13)
            ),
            rule::horizontal(1),
            self.login.authentication_fields(
                false,
                choices,
                self.protocol() == ProtocolKind::Ssh,
                self.key_picker.is_none()
            ),
            (self.protocol() != ProtocolKind::Ssh
                && !choices.vault_unlocked
                && (!self.pending_inline_credentials().is_empty()
                    || self.pending_temporary_key().is_some()))
            .then(|| button("Open Credential Vault…").on_press(Message::OpenVault)),
        ]
        .spacing(10)
        .into()
    }

    fn jump_page(&self, choices: &ViewChoices<'_>) -> Element<'_, Message> {
        let enabled = self.jump_enabled && self.protocol() == ProtocolKind::Ssh;
        let toggle = checkbox(self.jump_enabled)
            .label("Enable Jump Server / Auto-Hop")
            .text_size(13)
            .on_toggle_maybe(
                (self.protocol() == ProtocolKind::Ssh)
                    .then_some(|value| Message::Edit(Edit::UseJump(value))),
            );
        column![
            keyboard_table::action(
                toggle.into(),
                Some(Message::Edit(Edit::UseJump(!enabled))),
                false
            ),
            rule::horizontal(1),
            iced::widget::container(column![
                self.jump.endpoint_fields(true, enabled),
                self.jump.authentication_fields(true, choices, enabled, self.key_picker.is_none()),
            ].spacing(10)).max_width(410),
            text("The connection uses the jump server’s credentials to open a tunnel and verify the target host’s key. The jump server’s host key is confirmed on first use.")
                .size(12).style(text::secondary),
        ]
        .spacing(15)
        .into()
    }
}

fn login_row<'a>(
    label: &'static str,
    field: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(13).width(LOGIN_LABEL_WIDTH), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn login_field<'a>(
    jump: bool,
    label: &'static str,
    field: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    if !jump {
        return login_row(label, field);
    }
    row![text(label).size(13).width(98), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

impl Login {
    fn new_jump() -> Self {
        Self {
            username: String::new(),
            ..Self::new()
        }
    }

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
            inline_password: Password::default(),
            inline_passphrase: Password::default(),
            temporary: TemporaryDraft::default(),
            method_picker: Id::unique(),
            credential_picker: Id::unique(),
            password_source_picker: Id::unique(),
            key_source_picker: Id::unique(),
            key_browse: Id::unique(),
            inline_password_field: Id::unique(),
            inline_passphrase_field: Id::unique(),
        }
    }

    fn from_saved(login: &SavedLogin, duplicate: bool) -> Self {
        let mut form = Self::new();
        form.host = login.endpoint.host.clone();
        form.port = login.endpoint.port.to_string();
        form.username = login.endpoint.username.clone();
        let preferences = &login.preferences;
        form.password = preferences.password.clone().unwrap_or_default();
        form.key_path.clone_from(&preferences.key_file);
        form.passphrase = preferences.passphrase.clone().unwrap_or_default();
        form.keyboard_interactive = login.keyboard_interactive
            || (login.auth.method() == AuthMethod::KeyboardInteractive
                && preferences.keyboard_interactive);
        match &login.auth {
            Auth::Prompt => form.password.clear(),
            Auth::KeyboardInteractive => form.method = Method::Interactive,
            Auth::VaultPassword(id) => {
                form.method = Method::Vault;
                form.password = id.clone();
            }
            Auth::KeyFile { path, passphrase } => {
                form.method = Method::Key;
                form.stored_key = None;
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
                if credential.is_some() || *method == AuthMethod::Password {
                    form.password = credential.clone().unwrap_or_default();
                }
                if key.is_some() || *method == AuthMethod::PublicKey {
                    form.stored_key = *key;
                }
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
            LoginEdit::Host(_)
                | LoginEdit::Port(_)
                | LoginEdit::Username(_)
                | LoginEdit::Method(_)
                | LoginEdit::PasswordSource(_)
                | LoginEdit::KeySource(_)
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
            LoginEdit::PasswordSource(credential) => {
                if !matches!(self.method, Method::Ask | Method::Vault) {
                    return;
                }
                if let Some(credential) = credential {
                    self.method = Method::Vault;
                    self.update(LoginEdit::Password(credential));
                } else {
                    self.method = Method::Ask;
                    self.password.clear();
                    self.inline_password = Password::default();
                }
            }
            LoginEdit::KeySource(key) => {
                if !matches!(self.method, Method::Key | Method::StoredKey) {
                    return;
                }
                self.stored_key = key.as_ref().map(|key| key.id);
                self.method = if key.is_some() {
                    Method::StoredKey
                } else {
                    Method::Key
                };
            }
            LoginEdit::Password(credential) => {
                self.password = credential.id;
                self.inline_password = Password::default();
                if !credential.username.is_empty() {
                    self.username = credential.username;
                }
            }
            LoginEdit::KeyPath(value) => self.key_path = bounded(value, 4096),
            LoginEdit::StoredKey(key) => self.stored_key = Some(key.id),
            LoginEdit::Passphrase(credential) => {
                self.passphrase = credential.id;
                self.inline_passphrase = Password::default();
            }
            LoginEdit::InlinePassword(value) => {
                if matches!(self.method, Method::Ask | Method::Vault)
                    && value.as_str().len() <= 65_536
                {
                    self.inline_password = value;
                }
            }
            LoginEdit::InlinePassphrase(value) => {
                if self.method == Method::Key && value.as_str().len() <= 65_536 {
                    self.inline_passphrase = value;
                }
            }
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
            Method::Vault
                if self.password.is_empty() && self.inline_password.as_str().is_empty() =>
            {
                return Err("Select a vault password.");
            }
            Method::Vault if self.password.is_empty() => Auth::Prompt,
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
        let mut preferences = AuthPreferences {
            password: (!self.password.is_empty()).then(|| self.password.clone()),
            key_file: self.key_path.clone(),
            passphrase: (!self.passphrase.is_empty()).then(|| self.passphrase.clone()),
            keyboard_interactive: self.method == Method::Interactive && self.keyboard_interactive,
        };
        preferences.omit_active(&auth);
        Ok(SavedLogin {
            preferences,
            endpoint,
            auth,
            keyboard_interactive: self.keyboard_interactive && self.method != Method::Interactive,
        })
    }

    fn endpoint_fields(&self, jump: bool, enabled: bool) -> Element<'_, Message> {
        let edit = move |edit| Message::Edit(Edit::Login(jump, edit));
        let port = dialog_design::number_input(
            text_input("22", &self.port).on_input_maybe(enabled.then_some(LoginEdit::Port)),
            &self.port,
            1..=65535,
            1,
            enabled.then_some(LoginEdit::Port),
            54.0,
        )
        .map(edit);
        column![
            login_field(
                jump,
                if jump { "Jump server host" } else { "Host" },
                row![
                    text_input("Hostname or IP", &self.host)
                        .size(13)
                        .width(if jump {
                            Length::Fill
                        } else {
                            Length::Fixed(156.0)
                        })
                        .on_input_maybe(
                            enabled.then_some(move |value| edit(LoginEdit::Host(value)))
                        ),
                    text("Port").size(13),
                    port,
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            ),
            login_field(
                jump,
                "Username",
                text_input("Username", &self.username)
                    .size(13)
                    .width(if jump { 280 } else { 430 })
                    .on_input_maybe(
                        enabled.then_some(move |value| edit(LoginEdit::Username(value)))
                    ),
            ),
        ]
        .spacing(10)
        .into()
    }

    fn authentication_fields<'a>(
        &'a self,
        jump: bool,
        choices: &ViewChoices<'_>,
        enabled: bool,
        browse_enabled: bool,
    ) -> Element<'a, Message> {
        if !enabled {
            return self.disabled_authentication(jump, choices);
        }
        let edit = move |edit| Message::Edit(Edit::Login(jump, edit));
        let method_picker = |methods: Vec<_>| {
            pick_list(methods, Some(self.method), move |method| {
                edit(LoginEdit::Method(method))
            })
            .width(if jump { 280 } else { 300 })
            .text_size(13)
            .keyboard_navigation(self.method_picker.clone())
        };
        let method = if jump {
            let mut methods = vec![
                Method::Ask,
                Method::Vault,
                Method::Key,
                Method::StoredKey,
                Method::Interactive,
            ];
            if self.method == Method::TemporaryKey {
                methods.push(Method::TemporaryKey);
            }
            login_field(jump, "Authentication", method_picker(methods))
        } else {
            login_row(
                "Authentication",
                self.authentication_options(choices.temporary_keys),
            )
        };
        let mut fields = column![method].spacing(10);
        if !jump {
            fields = fields.push(self.saved_authentication_sources(choices, true, browse_enabled));
        }
        match self.method {
            Method::Ask | Method::Interactive => {}
            Method::Vault if jump => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passwords, &self.password, false);
                fields = fields.push(login_field(
                    jump,
                    "Saved credentials",
                    pick_list(choices, selected, move |choice| {
                        edit(LoginEdit::Password(choice))
                    })
                    .placeholder("Unlock the vault to select a password")
                    .width(300)
                    .keyboard_navigation(self.credential_picker.clone()),
                ));
            }
            Method::Key if jump => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passphrases, &self.passphrase, true);
                fields = fields
                    .push(login_field(
                        jump,
                        if jump { "Key file" } else { "Private key file" },
                        self.key_file_field(jump, true, browse_enabled),
                    ))
                    .push(login_field(
                        jump,
                        "Key passphrase",
                        pick_list(choices, selected, move |choice| {
                            edit(LoginEdit::Passphrase(choice))
                        })
                        .width(300)
                        .keyboard_navigation(self.credential_picker.clone()),
                    ));
            }
            Method::StoredKey if jump => {
                fields = fields.push(login_field(
                    jump,
                    "Registered SSH key",
                    pick_list(
                        choices.keys.to_vec(),
                        KeyChoice::selected(choices.keys, self.stored_key),
                        move |key| edit(LoginEdit::StoredKey(key)),
                    )
                    .placeholder("Select a registered SSH key")
                    .width(300)
                    .keyboard_navigation(self.credential_picker.clone()),
                ));
            }
            Method::Vault | Method::Key | Method::StoredKey => {}
            Method::TemporaryKey => {
                let key = &self.temporary;
                let can_write = choices.temporary_keys && choices.vault_unlocked && !jump;
                let can_set_options = can_write && (key.credential.is_none() || key.key.is_some());
                let paste = can_write.then_some(Message::PasteTemporaryKey);
                let remove = (choices.vault_unlocked
                    && (key.credential.is_some() || key.key.is_some()))
                .then_some(edit(LoginEdit::RemoveTemporaryKey));
                let automatic = checkbox(key.automatic)
                    .label("Select the saved key automatically until it expires")
                    .on_toggle_maybe(
                        can_set_options
                            .then_some(move |enabled| edit(LoginEdit::TemporaryAutomatic(enabled))),
                    );
                fields = fields
                    .push(login_row(
                        "Temporary key",
                        text(self.temporary_key_feedback(choices.temporary_key_vault)).size(13),
                    ))
                    .push(login_row(
                        "Expiration",
                        row![
                            text_input("1–1440", &key.minutes).width(88).on_input_maybe(
                                can_set_options.then_some(move |minutes| edit(
                                    LoginEdit::TemporaryLifetime(minutes)
                                ))
                            ),
                            text("minutes"),
                            keyboard_table::action(
                                button("Paste private key")
                                    .on_press_maybe(paste.clone())
                                    .into(),
                                paste,
                                true
                            ),
                            keyboard_table::action(
                                button("Remove key").on_press_maybe(remove.clone()).into(),
                                remove,
                                true
                            ),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .wrap(),
                    ))
                    .push(login_row(
                        "",
                        keyboard_table::action(
                            automatic.into(),
                            can_set_options
                                .then_some(edit(LoginEdit::TemporaryAutomatic(!key.automatic))),
                            false,
                        ),
                    ));
                if !choices.vault_unlocked {
                    fields = fields.push(login_row(
                        "",
                        keyboard_table::action(
                            button("Open Credential Vault…")
                                .on_press(Message::OpenVault)
                                .into(),
                            Some(Message::OpenVault),
                            true,
                        ),
                    ));
                } else if key.credential.is_some() && key.key.is_none() {
                    fields = fields.push(login_row(
                        "",
                        text("Paste a replacement to change the key’s lifetime.").size(13),
                    ));
                }
            }
        }
        if jump && matches!(self.method, Method::Ask | Method::Vault) {
            fields = fields.push(login_field(
                true,
                "Password",
                self.inline_password_field(true, true),
            ));
        }
        if jump && self.method == Method::Key {
            fields = fields.push(login_field(
                true,
                "Passphrase",
                self.inline_passphrase_field(true, true),
            ));
        }
        if jump && !choices.vault_unlocked && self.method != Method::TemporaryKey {
            fields = fields.push(login_field(
                true,
                "",
                keyboard_table::action(
                    button("Open Credential Vault…")
                        .on_press(Message::OpenVault)
                        .into(),
                    Some(Message::OpenVault),
                    true,
                ),
            ));
        }
        if self.method != Method::Interactive {
            let follow_up = checkbox(self.keyboard_interactive)
                .label("Also allow keyboard-interactive")
                .text_size(13)
                .on_toggle(move |enabled| edit(LoginEdit::KeyboardFollowUp(enabled)));
            fields = fields.push(login_field(
                jump,
                "",
                keyboard_table::action(
                    follow_up.into(),
                    Some(edit(LoginEdit::KeyboardFollowUp(
                        !self.keyboard_interactive,
                    ))),
                    false,
                ),
            ));
        }
        fields.into()
    }

    fn disabled_authentication<'a>(
        &'a self,
        jump: bool,
        choices: &ViewChoices<'_>,
    ) -> Element<'a, Message> {
        if !jump {
            let selected = match self.method {
                Method::Vault => Method::Ask,
                Method::StoredKey => Method::Key,
                method => method,
            };
            let mut methods = vec![("Password", Method::Ask), ("SSH key", Method::Key)];
            if choices.temporary_keys || selected == Method::TemporaryKey {
                methods.push(("Temporary SSH key", Method::TemporaryKey));
            }
            methods.push(("Keyboard-interactive", Method::Interactive));
            let options = iced::widget::row(methods.into_iter().map(|(label, method)| {
                row![
                    text(if method == selected { "◉" } else { "○" })
                        .size(16)
                        .style(text::secondary),
                    text(label).size(13).style(text::secondary)
                ]
                .spacing(5)
                .align_y(Alignment::Center)
                .into()
            }))
            .spacing(12)
            .wrap();
            return column![
                login_row("Authentication", options),
                self.saved_authentication_sources(choices, false, false),
                login_row(
                    "",
                    checkbox(self.keyboard_interactive)
                        .label("Also allow keyboard-interactive")
                        .text_size(13)
                ),
            ]
            .spacing(10)
            .into();
        }
        let disabled = |label, value: &str| {
            login_field(
                jump,
                label,
                text_input("", value).width(if jump { 280 } else { 300 }),
            )
        };
        let credential = choices
            .passwords
            .iter()
            .find(|choice| choice.id == self.password);
        let key = KeyChoice::selected(choices.keys, self.stored_key);
        let fields = match self.method {
            Method::Ask | Method::Vault => Some(disabled(
                "Saved credentials",
                credential.map_or("Ask when connecting", |choice| choice.label.as_str()),
            )),
            Method::Key => Some(login_field(
                jump,
                if jump { "Key file" } else { "Private key file" },
                self.key_file_field(jump, false, false),
            )),
            Method::StoredKey => Some(disabled(
                "Registered SSH key",
                key.as_ref()
                    .map_or("Select a registered SSH key", |choice| choice.name.as_str()),
            )),
            Method::TemporaryKey => Some(disabled("Expiration", &self.temporary.minutes)),
            Method::Interactive => None,
        };
        column![
            disabled("Authentication", &self.method.to_string()),
            fields,
            (self.method != Method::Interactive).then(|| login_field(
                jump,
                "",
                checkbox(self.keyboard_interactive).label("Also allow keyboard-interactive")
            )),
        ]
        .spacing(10)
        .into()
    }

    fn authentication_options(&self, temporary_keys: bool) -> Element<'_, Message> {
        use iced::keyboard::{Key, key::Named};
        use keyboard_table::TableKey;

        let selected = match self.method {
            Method::Vault => Method::Ask,
            Method::StoredKey => Method::Key,
            method => method,
        };
        let option = |label, value, method| {
            radio(label, value, Some(selected), |_| {
                let next = if value == selected {
                    self.method
                } else {
                    method
                };
                Message::Edit(Edit::Login(false, LoginEdit::Method(next)))
            })
            .text_size(13)
            .size(16)
        };
        let password = if self.password.is_empty() {
            Method::Ask
        } else {
            Method::Vault
        };
        let key = if self.stored_key.is_some() {
            Method::StoredKey
        } else {
            Method::Key
        };
        let temporary_keys = temporary_keys || self.method == Method::TemporaryKey;
        let mut methods = vec![
            ("Password", Method::Ask, password),
            ("SSH key", Method::Key, key),
        ];
        if temporary_keys {
            methods.push((
                "Temporary SSH key",
                Method::TemporaryKey,
                Method::TemporaryKey,
            ));
        }
        methods.push((
            "Keyboard-interactive",
            Method::Interactive,
            Method::Interactive,
        ));
        let current = methods
            .iter()
            .position(|(_, family, _)| *family == selected)
            .unwrap_or(0);
        let content = iced::widget::row(
            methods
                .iter()
                .map(|&(label, family, method)| option(label, family, method).into()),
        )
        .spacing(12)
        .align_y(Alignment::Center)
        .wrap()
        .into();
        keyboard_table::view(content, move |key, modifiers, _| {
            if !modifiers.is_empty() {
                return TableKey::Ignored;
            }
            let index = match key {
                Key::Named(Named::ArrowDown | Named::ArrowRight) => (current + 1) % methods.len(),
                Key::Named(Named::ArrowUp | Named::ArrowLeft) => {
                    (current + methods.len() - 1) % methods.len()
                }
                Key::Named(Named::Space) => return TableKey::Handled(None),
                _ => return TableKey::Ignored,
            };
            TableKey::Handled(Some(Message::Edit(Edit::Login(
                false,
                LoginEdit::Method(methods[index].2),
            ))))
        })
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
    fn new_profile_defaults_match_java_without_rewriting_existing_profiles() {
        let mut draft = Draft::new();
        draft.update(Edit::Name("New profile".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("host.example".into())));
        let mut saved = draft.connection().unwrap();
        assert_eq!(saved.startup.seconds(), 15);
        assert_eq!(saved.startup.attempts(), 4);
        assert_eq!(saved.login.endpoint.username, "root");
        saved.startup = kortty_ssh::ConnectOptions::default();
        let existing = Draft::from_profile(&saved, None, false)
            .connection()
            .unwrap();
        assert_eq!(existing.startup, saved.startup);

        let mut edited = Draft::from_profile(&saved, None, false);
        edited.update(Edit::UseJump(true));
        edited.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        let pending = edited.connection().unwrap().jump.unwrap();
        assert!(pending.endpoint.username.is_empty());
        assert!(pending.endpoint.validate().is_err());
        edited.update(Edit::Login(true, LoginEdit::Username("jump-user".into())));
        assert_eq!(
            edited.connection().unwrap().jump.unwrap().endpoint.username,
            "jump-user"
        );
    }

    #[test]
    fn ssh_editor_port_stepper_routes_edits_to_the_correct_login() {
        use iced::advanced::widget::operation::focusable;
        use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
        use iced::keyboard::{Key, key::Named};
        use iced::{Event, Font, Renderer, Size, mouse};
        use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

        use crate::settings_search::test_input;

        let login = Login::new();
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let size = Size::new(520.0, 350.0);
        for jump in [false, true] {
            let mut view = login.endpoint_fields(jump, true);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let layout = Layout::new(&node);
            let mut fields = layout.children();
            let host = fields.next().unwrap().children().nth(1).unwrap();
            let username = fields.next().unwrap().children().nth(1).unwrap();
            assert_eq!(host.bounds().x, username.bounds().x);
            let spinner = host.children().nth(2).unwrap();
            let buttons = spinner.children().nth(1).unwrap();
            let cursor =
                mouse::Cursor::Available(buttons.children().next().unwrap().bounds().center());
            let mut messages = Vec::new();
            for event in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                view.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(event),
                    layout,
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &node.bounds(),
                );
            }
            assert!(matches!(
                messages.as_slice(),
                [Message::Edit(Edit::Login(actual, LoginEdit::Port(port)))]
                    if *actual == jump && port == "23"
            ));
            let mut interface = UserInterface::build(
                login.endpoint_fields(jump, true),
                size,
                Cache::default(),
                &mut renderer,
            );
            for _ in 0..2 {
                test_input::apply(
                    &mut interface,
                    &renderer,
                    task::effect(Action::widget(focusable::focus_next())),
                );
            }
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::ArrowDown),
                None,
            );
            assert!(matches!(
                messages.as_slice(),
                [Message::Edit(Edit::Login(actual, LoginEdit::Port(port)))]
                    if *actual == jump && port == "21"
            ));
        }
    }

    #[test]
    fn disabling_the_jump_host_preserves_its_draft_and_ignores_disabled_edits() {
        let mut draft = Draft::new();
        draft.update(Edit::Name("Fixture".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("target.example".into())));
        draft.update(Edit::UseJump(true));
        draft.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        draft.update(Edit::Login(true, LoginEdit::Username("jump-user".into())));
        draft.update(Edit::Login(true, LoginEdit::Port("2223".into())));
        draft.update(Edit::UseJump(false));
        draft.update(Edit::Login(true, LoginEdit::Port("22".into())));
        assert!(draft.connection().unwrap().jump.is_none());
        draft.update(Edit::UseJump(true));
        let jump = draft.connection().unwrap().jump.unwrap();
        assert_eq!(jump.endpoint.host, "jump.example");
        assert_eq!(jump.endpoint.port, 2223);
    }

    #[test]
    fn reselecting_the_current_authentication_keeps_a_pending_temporary_key() {
        let mut draft = Draft::new();
        draft.update(Edit::Login(false, LoginEdit::Host("ssh.example".into())));
        draft.update(Edit::Login(false, LoginEdit::Method(Method::TemporaryKey)));
        draft.changed = false;
        assert!(draft.begin_temporary_paste(9));
        draft.update(Edit::Login(false, LoginEdit::Method(Method::TemporaryKey)));
        assert!(!draft.changed);
        assert!(draft.finish_temporary_paste(9, Password::new("fixture".into())));
    }

    #[test]
    fn switching_pages_preserves_the_draft_without_making_it_dirty() {
        let mut draft = Draft::new();
        draft.update(Edit::Page(Page::JumpHost));
        assert!(!draft.changed);
        draft.update(Edit::UseJump(true));
        draft.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        draft.update(Edit::Login(true, LoginEdit::Username("jump-user".into())));
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
