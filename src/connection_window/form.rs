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
use crate::connection_store::{Auth, AuthMethod, Connection, SavedLogin};
use crate::session_security::{IsolationChoice, IsolationLevel, resolve_isolation};
use crate::ssh_dialog::{
    KeepAliveEdit, KeyChoice, LoginTarget, Password, StartupField, StartupFields, normalize_host,
};
use crate::terminal_effects::{Effect, editor::EffectDraft};
use crate::workspace::dialog_design::{self, keyboard_table};

use super::color::ColorDraft;
use super::view::ViewChoices;
use super::{CredentialChoice, Message, os_badge, terminal_overrides, tunnels};

mod geometry;
mod tabs;

const LOGIN_LABEL_WIDTH: f32 = 168.0;

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
                | Self::Tunnels
                | Self::JumpHost
                | Self::Logging
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
            Self::Ask => "Ask for password",
            Self::Vault => "Vault password",
            Self::Key => "SSH private key",
            Self::StoredKey => "Registered SSH key",
            Self::TemporaryKey => "Temporary SSH key",
            Self::Interactive => "Keyboard-interactive",
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
    tab_color: ColorDraft,
    os_badge: Option<String>,
    badge_picker_open: bool,
    badge_cursor: Option<String>,
    paste_warning: Option<PasteWarningMode>,
    terminal: terminal_overrides::Draft,
    window_geometry: geometry::Draft,
    login: Login,
    jump: Login,
    jump_enabled: bool,
    tunnels: tunnels::List,
    pub changed: bool,
}

struct EditorIds {
    menu: Id,
    focus_scope: Id,
    page_scope: Id,
    page_scroll: Id,
    emulation: Id,
    encoding: Id,
    folder: Id,
    keepalive: Id,
    badge_list: Id,
}

impl EditorIds {
    fn new() -> Self {
        Self {
            menu: Id::unique(),
            focus_scope: Id::unique(),
            page_scope: Id::unique(),
            page_scroll: Id::unique(),
            emulation: Id::unique(),
            encoding: Id::unique(),
            folder: Id::unique(),
            keepalive: Id::unique(),
            badge_list: Id::unique(),
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
    temporary: TemporaryDraft,
    method_picker: Id,
    credential_picker: Id,
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
            page: Page::default(),
            ids: EditorIds::new(),
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
            badge_cursor: None,
            paste_warning: None,
            terminal: terminal_overrides::Draft::new(&Default::default()),
            window_geometry: geometry::Draft::new(None),
            login: Login::new(),
            jump: Login::new(),
            jump_enabled: false,
            tunnels: tunnels::List::new(Vec::new()),
            changed: false,
        }
    }

    pub fn from_profile(connection: &Connection, os_badge: Option<&str>, duplicate: bool) -> Self {
        Self {
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
            tab_color: ColorDraft::new(connection.tab_color, None),
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
                .map_or_else(Login::new, |login| Login::from_saved(login, duplicate)),
            jump_enabled: connection.jump.is_some(),
            tunnels: tunnels::List::new(connection.tunnels.clone()),
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
            Edit::Logging(edit) => {
                if !self.logging.accepts_editor_edit(!self.incognito, &edit) {
                    return;
                }
                self.logging.update(edit);
            }
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
        use iced::widget::{container, keyed_column, scrollable};

        let content = match self.page {
            Page::Connection => self.connection_page(choices, custom_badges, group),
            Page::Settings => self.settings_page(choices, inherited),
            Page::Tunnels => self.tunnels.view(),
            Page::JumpHost => self.jump_page(choices),
            Page::Logging => self
                .logging
                .editor_view(!self.incognito, choices.logging_policy)
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
            self.connection().err().map(|error| text(error).size(13)),
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
        (self.page == Page::Tunnels).then_some(())?;
        self.tunnels.begin(request, self.ids.focus_scope.clone())
    }

    pub(super) fn commit_tunnel_editor(&mut self, editor: &tunnels::Editor) {
        if self.page == Page::Tunnels {
            self.changed |= self.tunnels.commit(editor, &self.ids.focus_scope);
        }
    }

    pub(super) fn select_tunnel(&mut self, index: usize) -> iced::Task<Message> {
        if self.page == Page::Tunnels {
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

    pub(super) fn reveal_ai_skill(&self, id: u64) -> iced::Task<Message> {
        if self.page == Page::Ai {
            self.terminal.reveal_ai_skill(id)
        } else {
            iced::Task::none()
        }
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
                    .on_input(|value| Message::Edit(Edit::Name(value)))
                    .width(250),
            ),
            self.login.endpoint_fields(false, true),
            login_row("Connection protocol", text("SSH over TCP/IP").size(14)),
            login_row(
                "Terminal emulation",
                pick_list(TerminalProfile::ALL, Some(self.profile), |value| {
                    Message::Edit(Edit::Emulation(value))
                })
                .width(300)
                .keyboard_navigation(self.ids.emulation.clone()),
            ),
            login_row(
                "Text encoding",
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
                    .on_input(|value| Message::Edit(Edit::Tag(value)))
                    .width(250),
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
                .editor_view(LOGIN_LABEL_WIDTH)
                .map(|(field, value)| Message::Edit(Edit::Startup(field, value))),
            self.startup
                .keepalive_view(LOGIN_LABEL_WIDTH, self.ids.keepalive.clone())
                .map(|edit| Message::Edit(Edit::KeepAlive(edit))),
            login_row(
                "Host key check",
                text("Verify before authentication").size(14)
            ),
            rule::horizontal(1),
            self.login.authentication_fields(false, choices, true),
        ]
        .spacing(10)
        .into()
    }

    fn jump_page(&self, choices: &ViewChoices<'_>) -> Element<'_, Message> {
        let enabled = self.jump_enabled;
        let toggle = checkbox(enabled)
            .label("Use a jump host")
            .on_toggle(|value| Message::Edit(Edit::UseJump(value)));
        column![
            keyboard_table::action(
                toggle.into(),
                Some(Message::Edit(Edit::UseJump(!enabled))),
                false
            ),
            rule::horizontal(1),
            self.jump.endpoint_fields(true, enabled),
            self.jump.authentication_fields(true, choices, enabled),
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
            self.terminal.view(choices, false).map(|edit| Message::Edit(Edit::Terminal(edit))),
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
}

fn login_row<'a>(
    label: &'static str,
    field: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(14).width(LOGIN_LABEL_WIDTH), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
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
            method_picker: Id::unique(),
            credential_picker: Id::unique(),
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
            login_row(
                if jump { "Jump host" } else { "Host" },
                row![
                    text_input("Hostname or IP", &self.host).on_input_maybe(
                        enabled.then_some(move |value| edit(LoginEdit::Host(value)))
                    ),
                    text("Port").size(14),
                    port,
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            ),
            login_row(
                "Username",
                text_input("Username", &self.username)
                    .width(250)
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
    ) -> Element<'a, Message> {
        if !enabled {
            return self.disabled_authentication(choices);
        }
        let edit = move |edit| Message::Edit(Edit::Login(jump, edit));
        let method_picker = |methods: Vec<_>| {
            pick_list(methods, Some(self.method), move |method| {
                edit(LoginEdit::Method(method))
            })
            .width(300)
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
            login_row("Authentication", method_picker(methods))
        } else {
            login_row(
                "Authentication",
                self.authentication_options(choices.temporary_keys),
            )
        };
        let mut fields = column![method].spacing(10);
        if !jump {
            let source = match self.method {
                Method::Ask | Method::Vault => Some(login_row(
                    "Password source",
                    method_picker(vec![Method::Ask, Method::Vault]),
                )),
                Method::Key | Method::StoredKey => Some(login_row(
                    "SSH key source",
                    method_picker(vec![Method::Key, Method::StoredKey]),
                )),
                Method::TemporaryKey | Method::Interactive => None,
            };
            fields = fields.push(source);
        }
        match self.method {
            Method::Ask | Method::Interactive => {}
            Method::Vault => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passwords, &self.password, false);
                fields = fields.push(login_row(
                    "Saved credentials",
                    pick_list(choices, selected, move |choice| {
                        edit(LoginEdit::Password(choice))
                    })
                    .placeholder("Unlock the vault to select a password")
                    .width(300)
                    .keyboard_navigation(self.credential_picker.clone()),
                ));
            }
            Method::Key => {
                let (choices, selected) =
                    CredentialChoice::options(choices.passphrases, &self.passphrase, true);
                fields = fields
                    .push(login_row(
                        "Private key file",
                        text_input("Absolute private key path", &self.key_path)
                            .on_input(move |value| edit(LoginEdit::KeyPath(value))),
                    ))
                    .push(login_row(
                        "Key passphrase",
                        pick_list(choices, selected, move |choice| {
                            edit(LoginEdit::Passphrase(choice))
                        })
                        .width(300)
                        .keyboard_navigation(self.credential_picker.clone()),
                    ));
            }
            Method::StoredKey => {
                fields = fields.push(login_row(
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
                        text(if key.key.is_some() {
                            "Private key pasted. Save to verify and store it."
                        } else if key.credential.is_some() {
                            "Private key saved in the credential vault."
                        } else {
                            "Paste a private key for this SSH account."
                        })
                        .size(13),
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
        if self.method != Method::Interactive {
            let follow_up = checkbox(self.keyboard_interactive)
                .label("Also allow keyboard-interactive")
                .on_toggle(move |enabled| edit(LoginEdit::KeyboardFollowUp(enabled)));
            fields = fields.push(login_row(
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

    fn disabled_authentication<'a>(&self, choices: &ViewChoices<'_>) -> Element<'a, Message> {
        let disabled = |label, value: &str| login_row(label, text_input("", value).width(300));
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
            Method::Key => Some(disabled("Private key file", &self.key_path)),
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
            (self.method != Method::Interactive).then(|| login_row(
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
            .text_size(14)
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
