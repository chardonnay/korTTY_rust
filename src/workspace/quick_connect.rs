//! Owner-scoped native quick connection window.

use std::collections::VecDeque;
use std::fmt;

use iced::{Size, Task, window};
use kortty_pty::{LocalShell, ShellIntegrationSnippet};
use kortty_ssh::ConnectOptions;
use kortty_terminal::TerminalProfile;

#[cfg(test)]
use crate::connection_store::Auth;
use crate::connection_store::{Connection, Store};
use crate::profile;
use crate::serial_dialog::{self, SerialDialog};
use crate::session_security::LocalSettings;
use crate::ssh_dialog::{self, SshDialog};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    IbmKind, IsolationLevel, Message as Desktop, PendingIbm, PendingSerial, SerialConnection,
    Workspace, project_access, split_connection::SplitTarget,
};

const TITLE: &str = "Quick Connect";
mod view;

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
    ShowAppearance,
    #[cfg(windows)]
    LocalShell(LocalShell),
    ShellIntegration(bool),
    Ssh(ssh_dialog::Message),
    Serial(serial_dialog::Message),
    IbmAddress(String),
    IbmDevice(IbmKind),
    IbmName(String),
    IbmPlaintext(bool),
    ShowOptions,
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
    selected: Option<u64>,
    group_tab: bool,
    group: Option<String>,
    protocol: Protocol,
    ssh: SshDialog,
    serial: SerialDialog,
    ibm: PendingIbm,
    font: Option<u8>,
    minimum_isolation: IsolationLevel,
    options: bool,
    appearance: bool,
    local_shell: LocalShell,
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
            selected: None,
            group_tab: false,
            group: None,
            protocol: Protocol::Ssh,
            ssh: blank_ssh(profile, minimum_isolation),
            serial: SerialDialog::new(serial_id, SerialConnection::default(), profile),
            ibm: new_ibm(IbmKind::Tn3270e),
            font: None,
            minimum_isolation,
            options: false,
            appearance: false,
            local_shell: LocalShell::default(),
            shell_integration: false,
            save: false,
            name: String::new(),
            saving: false,
            status: None,
        }
    }

    fn choose(&mut self, id: u64) -> bool {
        let Some((connection, profile)) = self
            .store
            .as_ref()
            .and_then(|store| store.get(id))
            .and_then(|connection| Some((connection, connection.profile()?)))
        else {
            return false;
        };
        self.ssh = SshDialog::from_saved(profile, connection)
            .with_isolation(IsolationLevel::None, self.minimum_isolation);
        self.selected = Some(id);
        self.protocol = Protocol::Ssh;
        self.save = false;
        self.name.clear();
        self.status = None;
        true
    }

    fn selected(&self) -> Option<&Connection> {
        self.store.as_ref()?.get(self.selected?)
    }

    fn can_connect(&self) -> bool {
        if self.saving {
            return false;
        }
        if self.group_tab {
            return self.group.is_some();
        }
        match self.protocol {
            Protocol::Ssh | Protocol::Sftp => self.ssh.can_connect(),
            Protocol::Local => true,
            Protocol::Serial => self.serial.validated().is_ok(),
            Protocol::Tn3270 | Protocol::Tn5250 => self.ibm.connection().is_some(),
        }
    }

    fn group_members(&self, group: &str) -> usize {
        self.store.as_ref().map_or(0, |store| {
            store
                .connections()
                .iter()
                .filter(|connection| store.group_of(connection.id) == Some(group))
                .count()
        })
    }
}

impl Workspace {
    fn quick_shell_integration(
        &self,
        profile: TerminalProfile,
    ) -> Result<LocalSettings, &'static str> {
        if cfg!(windows) {
            return Err("Use the shell integration snippets to set up shells on Windows.");
        }
        if !self.window_settings.prompt_navigation_enabled
            || !super::shell_integration::supports_marks(profile)
        {
            return Err(
                "Select an emulation that reads command marks and enable them in Settings → Terminal.",
            );
        }
        let mut settings = self
            .security
            .local_settings(None, self.clipboard.policy.minimum_isolation)
            .map_err(|_| "Local session settings are unavailable.")?;
        if settings.isolation == IsolationLevel::Sandbox {
            return Err("A sandboxed shell uses its own isolated startup environment.");
        }
        if ShellIntegrationSnippet::local_default().is_none() {
            return Err("Automatic setup is available for local bash, zsh and fish.");
        }
        settings.shell_integration = true;
        Ok(settings)
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
        self.refresh_ssh_key_choices();
        let Some(profile) = self.windows.get(&owner).map(|window| window.local_profile) else {
            return Task::none();
        };
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        let (id, open) = self.open_placed_dialog(
            DialogKind::QuickConnect,
            window::Settings {
                size: Size::new(775.0, 610.0),
                min_size: Some(Size::new(700.0, 400.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let dialog = Dialog::new(
            id,
            owner,
            split,
            self.allocate(),
            profile,
            self.clipboard.policy.minimum_isolation,
        );
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
        Task::batch([open.map(|_| Desktop::Noop).chain(focus), load])
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
        Some(Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(owner),
        ]))
    }

    pub(super) fn quick_connect_owner_closed(&mut self, owner: window::Id) -> Task<Desktop> {
        if let Some(id) = self.quick_connect_for_owner(owner) {
            self.quick_connect = None;
            return self.close_placed_dialog(id);
        }
        Task::none()
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
        if !self.isolation_required() && self.clipboard.policy.serial_allowed {
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
        let Some(dialog) = self.quick_connect.as_mut().filter(|dialog| dialog.id == id) else {
            return Task::none();
        };
        let owner = dialog.owner;
        if dialog.saving && !matches!(message, Message::Saved(_)) {
            return Task::none();
        }
        match message {
            Message::Loaded(Ok(store)) => {
                if dialog.store.is_none() {
                    dialog.store = Some(store);
                }
            }
            Message::Loaded(Err(error)) => dialog.status = Some(error),
            Message::Saved(Ok((store, id))) => {
                dialog.saving = false;
                dialog.selected = Some(id);
                dialog.save = false;
                self.update_connection_store(store);
                return self.quick_submit();
            }
            Message::Saved(Err(error)) => {
                dialog.saving = false;
                dialog.status = Some(error);
            }
            Message::Query(query) if query.len() <= 240 => dialog.query = query,
            Message::Select(connection) if dialog.choose(connection) => {
                return dialog
                    .ssh
                    .focus()
                    .map(move |message| wrap(id, Message::Ssh(message)));
            }
            Message::Recent(connection) if split => {
                if dialog.choose(connection) {
                    if dialog.can_connect() {
                        return self.quick_submit();
                    }
                    return dialog
                        .ssh
                        .focus()
                        .map(move |message| wrap(id, Message::Ssh(message)));
                }
            }
            Message::Recent(connection) => {
                if dialog
                    .store
                    .as_ref()
                    .is_some_and(|store| store.get(connection).is_some())
                {
                    let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
                    return Task::batch([close, self.open_recent_connection(owner, connection)]);
                }
            }
            Message::GroupTab(group) if !split => dialog.group_tab = group,
            Message::Group(group) if !split && dialog.group_members(&group) > 0 => {
                dialog.group = Some(group)
            }
            Message::Protocol(protocol) if allowed.contains(&protocol) => {
                dialog.protocol = protocol;
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
            Message::Profile(profile) => {
                dialog.ssh.update(ssh_dialog::Message::Profile(profile));
                let _ = dialog
                    .serial
                    .update(serial_dialog::Message::Profile(profile));
            }
            Message::Font(font)
                if (profile::MIN_TERMINAL_FONT..=profile::MAX_TERMINAL_FONT).contains(&font) =>
            {
                dialog.font = Some(font);
            }
            Message::ShowAppearance => dialog.appearance = !dialog.appearance,
            #[cfg(windows)]
            Message::LocalShell(shell)
                if dialog.protocol == Protocol::Local && !dialog.group_tab =>
            {
                dialog.local_shell = shell;
            }
            Message::ShellIntegration(enabled) if dialog.protocol == Protocol::Local => {
                dialog.shell_integration = enabled;
            }
            Message::Ssh(ssh_dialog::Message::Connect) if dialog.protocol.is_ssh() => {
                return self.quick_submit();
            }
            Message::Ssh(ssh_dialog::Message::OpenVault) => return self.show_vault(owner),
            Message::Ssh(message) => dialog.ssh.update(message),
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
            Message::ShowOptions => dialog.options = !dialog.options,
            Message::SaveConnection(save) => dialog.save = save,
            Message::ConnectionName(name) if name.len() <= 160 => dialog.name = name,
            Message::Reset => dialog.font = None,
            Message::Connect => return self.quick_submit(),
            Message::LoadProject if !split => {
                let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
                return Task::batch([
                    close,
                    self.update(Desktop::Project(owner, project_access::Message::Show)),
                ]);
            }
            Message::Manage if !split => {
                let close = self.close_quick_connect(id).unwrap_or_else(Task::none);
                return Task::batch([close, self.show_connections(owner)]);
            }
            Message::Focus => return window::gain_focus(id),
            Message::Cancel => return self.close_quick_connect(id).unwrap_or_else(Task::none),
            _ => {}
        }
        Task::none()
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
            return self.quick_connect_owner_closed(owner);
        }
        let incognito = dialog.protocol == Protocol::Ssh
            && dialog.selected().is_some_and(|profile| profile.incognito);
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
                draft.incognito = dialog.selected().is_some_and(|profile| profile.incognito);
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
        let Some(dialog) = self.quick_connect.take() else {
            return Task::none();
        };
        let start = match dialog.protocol {
            Protocol::Ssh | Protocol::Sftp => self.start_quick_ssh(
                owner,
                dialog.ssh,
                super::ssh_access::QuickLaunch {
                    saved: dialog.selected,
                    sftp: dialog.protocol == Protocol::Sftp,
                    incognito,
                    font: dialog.font,
                    split: dialog.split,
                    ..Default::default()
                },
            ),
            Protocol::Local => {
                let profile = dialog.ssh.profile();
                let settings = if dialog.local_shell.is_default() {
                    Ok(dialog
                        .shell_integration
                        .then(|| self.quick_shell_integration(profile).ok())
                        .flatten())
                } else {
                    self.local_settings_for_shell(dialog.local_shell).map(Some)
                };
                match settings
                    .and_then(|settings| self.store_local_session(profile, dialog.font, settings))
                {
                    Ok(session) => {
                        if let Some(target) = dialog.split {
                            self.start_split_worker(owner, target, session);
                        } else {
                            self.add_tab(owner, session);
                        }
                    }
                    Err(error) => {
                        self.notice(owner, format!("Could not start the local shell ({error})"))
                    }
                }
                Task::none()
            }
            Protocol::Serial => {
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.pending_serial = Some(PendingSerial {
                        form: dialog.serial,
                        replace: None,
                        reconnect: None,
                        split: dialog.split,
                        font: dialog.font,
                    });
                }
                self.connect_serial(owner)
            }
            Protocol::Tn3270 | Protocol::Tn5250 => {
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.pending_ibm = Some(PendingIbm {
                        font: dialog.font,
                        ..dialog.ibm
                    });
                }
                self.connect_ibm(owner)
            }
        };
        Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(owner),
            start,
        ])
    }

    pub(super) fn open_recent_connection(&mut self, owner: window::Id, id: u64) -> Task<Desktop> {
        let Some(connection) = self.saved_connection(id) else {
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
        for connection in store
            .connections()
            .iter()
            .filter(|connection| store.group_of(connection.id) == Some(group))
        {
            if needs_entered_password(connection)
                || (connection.incognito && !self.clipboard.policy.incognito_allowed)
            {
                skipped.push(connection.name.as_str());
            } else {
                members.push_back(connection.id);
            }
        }
        let task = self.advance_quick_group(owner, members);
        if !skipped.is_empty() {
            self.notice(
                owner,
                format!(
                    "Skipped connections requiring a password or blocked by policy: {}",
                    skipped.join(", ")
                ),
            );
        }
        task
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
        let form = SshDialog::from_saved(profile, &connection).with_isolation(
            IsolationLevel::None,
            self.clipboard.policy.minimum_isolation,
        );
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
        assert_eq!(dialog.font, None);
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
        use iced::widget::pane_grid::{Axis, Node};
        use std::time::{Duration, Instant};

        let (mut workspace, owner, source) = split_fixture();
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
            Message::Connect,
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }

        let window = &workspace.windows[&owner];
        let pending = window.pending_split_connection.unwrap();
        assert_eq!(window.tab().unwrap().grid.len(), 2);
        workspace.wait_for_test_split(owner);

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
