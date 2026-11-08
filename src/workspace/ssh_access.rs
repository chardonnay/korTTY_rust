use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use iced::futures::{StreamExt, stream};
use iced::widget::{button, checkbox, column, row, text};
use iced::{Element, Task, window};
use kortty_ssh::{
    Authentication, ConnectError, ConnectOptions, Endpoint, HostKey, HostKeyStatus, StartupChannel,
    TrustStore,
};
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use crate::connection_store::{Connection, Store};
use crate::session::{Session, SessionId, Status, ssh_size};
use crate::session_security::IsolationLevel;
use crate::ssh_connection::{self, Connected, trust_store};
use crate::ssh_dialog::{self, KeyboardOptions, LoginRequest, LoginTarget, SshDialog};
use crate::ssh_worker::{self, CancelOnDrop, DialCancellation};
use crate::terminal_effects::Selection;
use crate::vault_window::Kind;

use super::{
    Message as WorkspaceMessage, Workspace, initial_grid,
    recently_closed::{ReopenToken, SshOutcome},
    split_connection::SplitTarget,
};

mod challenge;
mod reconnect;
mod registered_keys;
mod reopen;
use reconnect::ReconnectLaunch;
pub(super) use registered_keys::KeySnapshots;
use reopen::ReopenLaunch;
pub(super) use reopen::{ReopenSshOrigin, SavedSshProfile};
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshTarget {
    #[serde(flatten)]
    login: LoginTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jump: Option<LoginTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved: Option<u64>,
    #[serde(default, skip_serializing_if = "IsolationLevel::is_none")]
    pub(super) isolation: IsolationLevel,
}

impl SshTarget {
    pub(crate) fn matches_imported_route(&self, other: &Self) -> bool {
        self.login == other.login && self.jump == other.jump && self.isolation == other.isolation
    }

    pub(crate) fn from_imported_route(
        login: LoginTarget,
        jump: Option<LoginTarget>,
        isolation: IsolationLevel,
    ) -> io::Result<Self> {
        let target = Self {
            login,
            jump,
            isolation,
            saved: None,
        };
        target.validate()?;
        Ok(target)
    }

    pub fn validate(&self) -> io::Result<()> {
        for login in std::iter::once(&self.login).chain(self.jump.iter()) {
            login
                .validate()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            Endpoint::new(&login.host, login.port)?;
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        self.login.label()
    }

    pub(super) fn host(&self) -> &str {
        &self.login.host
    }

    pub(super) fn port(&self) -> u16 {
        self.login.port
    }

    pub(super) fn username(&self) -> &str {
        &self.login.username
    }

    pub(super) fn saved_id(&self) -> Option<u64> {
        self.saved
    }

    fn form(&self, profile: TerminalProfile) -> SshDialog {
        SshDialog::with_target(profile, &self.login, self.jump.as_ref())
    }

    pub(super) fn matches(&self, connection: &Connection) -> bool {
        connection.matches_route(&self.login, self.jump.as_ref())
    }

    pub(super) fn matches_endpoints(
        &self,
        login: &LoginTarget,
        jump: Option<&LoginTarget>,
    ) -> bool {
        &self.login == login && self.jump.as_ref() == jump
    }

    pub(super) fn same_saved_route(&self, other: &Self) -> bool {
        self.saved.is_some()
            && self.saved == other.saved
            && self.login == other.login
            && self.jump == other.jump
    }
}

pub(super) struct PendingSsh {
    replace: Option<SessionId>,
    split: Option<SplitTarget>,
    reopen: Option<ReopenLaunch>,
    reconnect: Option<ReconnectLaunch>,
    sftp: bool,
    saved: Option<u64>,
    incognito: bool,
    retry: Option<RetryLogin>,
    key_snapshots: KeySnapshots,
    group: VecDeque<u64>,
    font: Option<u8>,
    duplicate_tab: Option<u64>,
    stage: Stage,
}

struct RetryLogin {
    keyboard: (KeyboardOptions, Option<KeyboardOptions>),
    target_key: Option<PathBuf>,
    jump_key: Option<PathBuf>,
    explicit_vault: Option<(LoginTarget, String)>,
    explicit_key: Option<(LoginTarget, u64)>,
    explicit_jump_key: Option<(LoginTarget, u64)>,
    startup: ConnectOptions,
    logging: crate::terminal_logging::Config,
}

impl RetryLogin {
    fn new(
        request: &ssh_dialog::Request,
        explicit_vault: Option<(LoginTarget, String)>,
        logging: crate::terminal_logging::Config,
    ) -> Self {
        fn key_path(login: &LoginRequest) -> Option<PathBuf> {
            match &login.auth {
                ssh_dialog::Authentication::PrivateKey(path)
                | ssh_dialog::Authentication::KeyWithPassphrase(path, _) => Some(path.clone()),
                _ => None,
            }
        }

        fn explicit_key(login: &LoginRequest) -> Option<(LoginTarget, u64)> {
            match login.auth {
                ssh_dialog::Authentication::StoredKey { id, explicit: true } => {
                    Some((login.endpoint.clone(), id))
                }
                _ => None,
            }
        }

        Self {
            keyboard: (
                request.target.keyboard_options(),
                request.jump.as_ref().map(LoginRequest::keyboard_options),
            ),
            target_key: key_path(&request.target),
            jump_key: request.jump.as_ref().and_then(key_path),
            explicit_vault,
            explicit_key: explicit_key(&request.target),
            explicit_jump_key: request.jump.as_ref().and_then(explicit_key),
            startup: request.options,
            logging,
        }
    }

    fn restore(&self, form: &mut SshDialog, endpoint: &LoginTarget) {
        use ssh_dialog::{LoginMessage, Message, Method};

        fn restore_key(
            form: &mut SshDialog,
            key: Option<&Path>,
            wrap: fn(LoginMessage) -> Message,
        ) {
            if let Some(path) = key.and_then(Path::to_str) {
                form.update(wrap(LoginMessage::Authentication(Method::PrivateKey)));
                form.update(wrap(LoginMessage::KeyFile(path.to_owned())));
            }
        }

        form.restore_keyboard_options(self.keyboard.0, self.keyboard.1);
        form.set_startup(self.startup);
        form.set_logging(self.logging.clone());
        restore_key(form, self.target_key.as_deref(), Message::Target);
        restore_key(form, self.jump_key.as_deref(), Message::Jump);
        if let Some((endpoint, id)) = &self.explicit_key {
            form.restore_explicit_key(false, endpoint, *id);
        }
        if let Some((endpoint, id)) = &self.explicit_jump_key {
            form.restore_explicit_key(true, endpoint, *id);
        }
        if let Some((original, credential)) = &self.explicit_vault
            && original == endpoint
        {
            form.update(Message::Target(LoginMessage::VaultPassword(
                credential.clone(),
            )));
        }
    }
}

#[derive(Default)]
pub(super) struct QuickLaunch {
    pub(super) saved: Option<u64>,
    pub(super) sftp: bool,
    pub(super) incognito: bool,
    pub(super) group: VecDeque<u64>,
    pub(super) font: Option<u8>,
    pub(super) split: Option<SplitTarget>,
}

impl PendingSsh {
    pub(super) fn refresh_connection_group(&mut self, store: &Store) {
        if let Stage::Dialing {
            target,
            connection_group,
            ..
        } = &mut self.stage
        {
            *connection_group = target
                .saved_id()
                .filter(|&id| {
                    store
                        .get(id)
                        .is_some_and(|connection| target.matches(connection))
                })
                .and_then(|id| store.group_of(id))
                .and_then(crate::profile::normalize_tab_group);
        }
    }

    pub(super) fn reopen_token(&self) -> Option<ReopenToken> {
        self.reopen.as_ref().map(|reopen| reopen.token)
    }

    fn reopen_form_key(&self) -> Option<(ReopenToken, u64)> {
        self.reopen.as_ref().map(ReopenLaunch::form_key)
    }

    fn form_key(&self) -> Option<FormKey> {
        self.reopen_form_key().map(FormKey::Reopen).or_else(|| {
            self.reconnect
                .as_ref()
                .map(|reconnect| FormKey::Reconnect(reconnect.form_revision))
        })
    }

    pub(super) fn depends_on(&self, session: SessionId) -> bool {
        self.replace == Some(session)
            || self.split.is_some_and(|target| target.source == session)
            || self
                .reconnect
                .as_ref()
                .is_some_and(|reconnect| reconnect.target.source == session)
    }
}

enum Stage {
    Form(Box<SshDialog>),
    Dialing {
        id: u64,
        target: SshTarget,
        profile: TerminalProfile,
        environment_credential: Option<String>,
        logging: Option<Box<crate::terminal_logging::Prepared>>,
        terminal_effect: Option<Selection>,
        command_timestamps: Option<bool>,
        connection_group: Option<String>,
        _cancellation: CancelOnDrop,
        challenge: Option<Box<challenge::Form>>,
    },
    Unknown {
        id: u64,
        target: SshTarget,
        profile: TerminalProfile,
        endpoint: Endpoint,
        offered: HostKey,
    },
    Changed {
        id: u64,
        target: SshTarget,
        profile: TerminalProfile,
        review: ChangedKey,
    },
    Remembering {
        id: u64,
        target: SshTarget,
        profile: TerminalProfile,
        replacing: bool,
    },
}

struct ChangedKey {
    endpoint: Endpoint,
    expected: HostKey,
    offered: HostKey,
    reviewing: bool,
    verified: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Show(window::Id, Option<SessionId>),
    ShowIncognito(window::Id),
    ShowSftp(window::Id),
    Form(window::Id, ssh_dialog::Message),
    ReopenForm(window::Id, ReopenToken, u64, ssh_dialog::Message),
    ReconnectForm(window::Id, u64, ssh_dialog::Message),
    Cancel(window::Id),
    Challenge(window::Id, u64, challenge::Ticket),
    PromptAnswer(window::Id, u64, u64, usize, ssh_dialog::Password),
    PromptSubmit(window::Id, u64, u64),
    Dialed(window::Id, u64, DialResponse),
    Remember(window::Id, u64),
    ReviewChanged(window::Id, u64, bool),
    VerifyChanged(window::Id, u64, bool),
    ReplaceChanged(window::Id, u64),
    Remembered(window::Id, u64, Result<(), String>),
}

#[derive(Clone)]
pub struct DialResponse(Arc<Mutex<Option<Result<Connected, ConnectError>>>>);

impl fmt::Debug for DialResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DialResponse")
            .finish_non_exhaustive()
    }
}

impl Workspace {
    fn allow_ssh_transport(&mut self, window: window::Id, transport: &str) -> bool {
        if self.clipboard.policy.problem.is_some()
            || (self.clipboard.policy.minimum_isolation == IsolationLevel::Sandbox
                && !ssh_worker::sandbox_available())
        {
            return self.allow_in_process_transport(window, transport);
        }
        true
    }

    pub(super) fn update_ssh(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Show(window, replace) => self.show_shell_ssh(window, replace, false),
            Message::ShowIncognito(window) => self.show_shell_ssh(window, None, true),
            Message::ShowSftp(window) => {
                if !self.allow_ssh_transport(window, "SFTP") {
                    return Task::none();
                }
                if let Some(task) = self.open_attached_sftp(window) {
                    return task;
                }
                self.show_sftp_login(window)
            }
            Message::Form(window, message) if self.guarded_ssh_form_key(window).is_none() => {
                self.update_ssh_form(window, message)
            }
            Message::ReopenForm(window, token, revision, message)
                if self.reopen_ssh_form_key(window) == Some((token, revision)) =>
            {
                self.update_ssh_form(window, message)
            }
            Message::Cancel(window) => self.cancel_ssh_login(window),
            Message::ReconnectForm(window, revision, message)
                if self.guarded_ssh_form_key(window) == Some(FormKey::Reconnect(revision)) =>
            {
                self.update_ssh_form(window, message)
            }
            Message::Form(..) | Message::ReopenForm(..) | Message::ReconnectForm(..) => {
                Task::none()
            }
            Message::Dialed(window, id, result) => self.ssh_dialed(window, id, result),
            Message::Challenge(window, id, ticket) => self.ssh_challenge(window, id, ticket),
            Message::PromptAnswer(window, id, round, index, value) => {
                if let Some(Some(form)) = self.ssh_challenge_slot(window, id)
                    && form.round() == round
                {
                    form.answer(index, value);
                }
                Task::none()
            }
            Message::PromptSubmit(window, id, round) => {
                let attempt =
                    self.pending_ssh_mut(window)
                        .and_then(|pending| match &pending.stage {
                            Stage::Dialing {
                                id: active,
                                target,
                                profile,
                                ..
                            } if *active == id => Some((target.clone(), *profile)),
                            _ => None,
                        });
                if let Some((target, profile)) = attempt
                    && let Some(refresh) =
                        self.refresh_registered_key_attempt(window, &target, profile)
                {
                    return refresh;
                }
                if let Some(slot) = self.ssh_challenge_slot(window, id)
                    && let Some(form) = slot
                    && form.round() == round
                    && form.submit()
                {
                    *slot = None;
                }
                Task::none()
            }
            Message::Remember(window, id) => self.remember_ssh(window, id),
            Message::ReviewChanged(window, id, reviewing) => {
                if !self.host_pins_locked()
                    && let Some(review) = self.changed_key(window, id)
                {
                    review.reviewing = reviewing;
                    review.verified = false;
                }
                Task::none()
            }
            Message::VerifyChanged(window, id, verified) => {
                if !self.host_pins_locked()
                    && let Some(review) = self.changed_key(window, id)
                    && review.reviewing
                {
                    review.verified = verified;
                }
                Task::none()
            }
            Message::ReplaceChanged(window, id) => self.replace_changed_ssh(window, id),
            Message::Remembered(window, id, result) => {
                let Some(PendingSsh {
                    stage:
                        Stage::Remembering {
                            id: active,
                            target,
                            profile,
                            replacing,
                        },
                    ..
                }) = self.pending_ssh_mut(window)
                else {
                    return Task::none();
                };
                if *active != id {
                    return Task::none();
                }
                let (target, profile, replacing) = (target.clone(), *profile, *replacing);
                let refresh = if result.is_ok() {
                    self.refresh_known_hosts()
                } else {
                    Task::none()
                };
                let notice = match result {
                    Ok(()) if replacing => {
                        "SSH host key replaced. Review the login and connect again.".into()
                    }
                    Ok(()) => "SSH host key saved. Review the login and connect again.".into(),
                    Err(error) if replacing => {
                        format!("Could not replace the SSH host key ({error})")
                    }
                    Err(error) => format!("Could not trust the SSH host key ({error})"),
                };
                Task::batch([
                    refresh,
                    self.reopen_ssh_form(window, target, profile, notice),
                ])
            }
        }
    }

    fn update_ssh_form(
        &mut self,
        owner: window::Id,
        message: ssh_dialog::Message,
    ) -> Task<WorkspaceMessage> {
        match message {
            ssh_dialog::Message::Connect => self.connect_ssh(owner),
            ssh_dialog::Message::OpenVault => self.show_vault(owner),
            ssh_dialog::Message::Cancel => self.cancel_ssh_login(owner),
            message => {
                if let Some(PendingSsh {
                    stage: Stage::Form(form),
                    ..
                }) = self.pending_ssh_mut(owner)
                {
                    form.update(message);
                }
                Task::none()
            }
        }
    }

    fn cancel_ssh_login(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        if self.project_ssh_target(owner).is_some() {
            self.defer_interrupted_project_reconnects(owner, None);
        }
        let reopen = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.pending_ssh.take())
            .and_then(|pending| pending.reopen_token());
        self.sftp_logins.remove(&owner);
        reopen.map_or_else(Task::none, |token| {
            self.finish_reopen_ssh(owner, token, SshOutcome::Canceled)
        })
    }

    fn show_shell_ssh(
        &mut self,
        window: window::Id,
        replace: Option<SessionId>,
        mut incognito: bool,
    ) -> Task<WorkspaceMessage> {
        if self.reopen_ssh_token(window).is_some() || !self.allow_ssh_transport(window, "SSH") {
            return Task::none();
        }
        self.refresh_ssh_key_choices();
        let (form, saved) = if let Some(id) = replace {
            let Some((tab, _, pane)) = self.reconnectable_ssh(window, id) else {
                return Task::none();
            };
            let Some(target) = pane.ssh.as_ref() else {
                return Task::none();
            };
            let Some(tab) = self.windows[&window].tabs[tab].terminal() else {
                return Task::none();
            };
            incognito = tab.incognito;
            (
                self.ssh_form(target, pane.frame.screen.profile),
                target.saved,
            )
        } else {
            let Some(state) = self.windows.get(&window) else {
                return Task::none();
            };
            (
                SshDialog::new(state.local_profile).with_isolation(
                    IsolationLevel::None,
                    self.clipboard.policy.minimum_isolation,
                ),
                None,
            )
        };
        if incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(window, "Managed policy blocks incognito sessions.".into());
            return Task::none();
        }
        let focus = form
            .focus()
            .map(move |message| wrap(Message::Form(window, message)));
        if let Some(state) = self.windows.get_mut(&window) {
            state.cancel_paste();
            state.pending_ibm = None;
            state.pending_serial = None;
            state.notice = None;
            state.pending_ssh = Some(PendingSsh {
                replace,
                split: None,
                reopen: None,
                reconnect: None,
                sftp: false,
                saved,
                incognito,
                retry: None,
                key_snapshots: Default::default(),
                group: VecDeque::new(),
                font: None,
                duplicate_tab: None,
                stage: Stage::Form(Box::new(form)),
            });
        }
        focus
    }

    fn pending_ssh_mut(&mut self, id: window::Id) -> Option<&mut PendingSsh> {
        if let Some(window) = self.windows.get_mut(&id) {
            window.pending_ssh.as_mut()
        } else {
            self.sftp_logins.get_mut(&id)
        }
    }

    pub(super) fn duplicate_dashboard_ssh(
        &mut self,
        window: window::Id,
        source_tab: u64,
        target: &SshTarget,
        profile: TerminalProfile,
        incognito: bool,
        font: Option<u8>,
    ) -> Task<WorkspaceMessage> {
        if self.reopen_ssh_token(window).is_some()
            || !self.allow_ssh_transport(window, "SSH")
            || (incognito && !self.clipboard.policy.incognito_allowed)
        {
            return Task::none();
        }
        let form = self.ssh_form(target, profile);
        let focus = form
            .focus()
            .map(move |message| wrap(Message::Form(window, message)));
        let Some(state) = self.windows.get_mut(&window) else {
            return Task::none();
        };
        state.pending_ssh = Some(PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: None,
            sftp: false,
            saved: target.saved_id(),
            incognito,
            retry: None,
            key_snapshots: Default::default(),
            group: VecDeque::new(),
            font,
            duplicate_tab: Some(source_tab),
            stage: Stage::Form(Box::new(form)),
        });
        state.cancel_paste();
        focus
    }

    pub(super) fn show_saved_ssh(
        &mut self,
        id: window::Id,
        connection: Connection,
        sftp: bool,
    ) -> Task<WorkspaceMessage> {
        if self.reopen_ssh_token(id).is_some()
            || !self.allow_ssh_transport(id, if sftp { "SFTP" } else { "SSH" })
        {
            return Task::none();
        }
        let incognito = !sftp && connection.incognito;
        if incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(
                id,
                "Managed policy blocks this incognito connection.".into(),
            );
            return Task::none();
        }
        let Some(state) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let Some(profile) = connection.profile() else {
            return Task::none();
        };
        let form = SshDialog::from_saved(profile, &connection).with_isolation(
            IsolationLevel::None,
            self.clipboard.policy.minimum_isolation,
        );
        let focus = form
            .focus()
            .map(move |message| wrap(Message::Form(id, message)));
        state.cancel_paste();
        state.pending_ibm = None;
        state.pending_serial = None;
        state.pending_tunnels = None;
        state.notice = None;
        state.pending_ssh = Some(PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: None,
            sftp,
            saved: Some(connection.id),
            incognito,
            retry: None,
            key_snapshots: Default::default(),
            group: VecDeque::new(),
            font: None,
            duplicate_tab: None,
            stage: Stage::Form(Box::new(form)),
        });
        focus
    }

    pub(super) fn start_same_ssh_split(
        &mut self,
        owner: window::Id,
        target: SplitTarget,
        ssh: &SshTarget,
        profile: TerminalProfile,
        incognito: bool,
        font: Option<u8>,
    ) -> Task<WorkspaceMessage> {
        let form = self.fixed_split_ssh_form(ssh, profile);
        self.start_quick_ssh(
            owner,
            form,
            QuickLaunch {
                saved: ssh.saved_id(),
                incognito,
                font,
                split: Some(target),
                ..Default::default()
            },
        )
    }

    pub(super) fn start_quick_ssh(
        &mut self,
        id: window::Id,
        form: SshDialog,
        launch: QuickLaunch,
    ) -> Task<WorkspaceMessage> {
        self.launch_quick_ssh(id, form, launch, None, true)
    }

    fn launch_quick_ssh(
        &mut self,
        id: window::Id,
        form: SshDialog,
        launch: QuickLaunch,
        reopen: Option<ReopenLaunch>,
        automatic: bool,
    ) -> Task<WorkspaceMessage> {
        let QuickLaunch {
            saved,
            sftp,
            incognito,
            group,
            font,
            split,
        } = launch;
        let token = reopen.as_ref().map(|reopen| reopen.token);
        let form_key = reopen
            .as_ref()
            .map(|reopen| FormKey::Reopen(reopen.form_key()));
        if let Some(token) = token {
            if self
                .windows
                .get(&id)
                .is_none_or(|window| window.pending_ssh.is_some())
            {
                return self.finish_reopen_ssh(id, token, SshOutcome::Canceled);
            }
        } else if self.reopen_ssh_token(id).is_some() {
            return Task::none();
        }
        if !self.allow_ssh_transport(id, if sftp { "SFTP" } else { "SSH" }) {
            return token.map_or_else(Task::none, |token| {
                self.finish_reopen_ssh(id, token, SshOutcome::Unavailable)
            });
        }
        let incognito = incognito && !sftp;
        if incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(
                id,
                "Managed policy blocks this incognito connection.".into(),
            );
            return token.map_or_else(Task::none, |token| {
                self.finish_reopen_ssh(id, token, SshOutcome::Unavailable)
            });
        }
        let ready = automatic && form.can_connect();
        let focus = form
            .focus()
            .map(move |message| wrap(form_message(id, form_key, message)));
        let Some(state) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        state.cancel_paste();
        state.pending_ibm = None;
        state.pending_serial = None;
        state.notice = None;
        state.pending_ssh = Some(PendingSsh {
            replace: None,
            split,
            reopen,
            reconnect: None,
            sftp,
            saved,
            incognito,
            retry: None,
            key_snapshots: Default::default(),
            group,
            font,
            duplicate_tab: None,
            stage: Stage::Form(Box::new(form)),
        });
        if ready { self.connect_ssh(id) } else { focus }
    }

    fn ssh_form(&self, target: &SshTarget, profile: TerminalProfile) -> SshDialog {
        let form = if let Some(connection) = target.saved.and_then(|id| self.saved_connection(id))
            && target.matches(&connection)
        {
            SshDialog::from_saved(profile, &connection)
        } else {
            target.form(profile)
        };
        form.with_isolation(target.isolation, self.clipboard.policy.minimum_isolation)
    }

    fn fixed_split_ssh_form(&self, target: &SshTarget, profile: TerminalProfile) -> SshDialog {
        let floor = target
            .isolation
            .max(self.clipboard.policy.minimum_isolation);
        self.ssh_form(target, profile)
            .with_isolation(floor, floor)
            .with_fixed_route()
    }

    fn show_sftp_login(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.reopen_ssh_token(id).is_some() || !self.allow_ssh_transport(id, "SFTP") {
            return Task::none();
        }
        self.refresh_ssh_key_choices();
        let target = self
            .sftp
            .get(&id)
            .map(|window| window.target())
            .or_else(|| self.focused_session(id).and_then(|pane| pane.ssh.as_ref()));
        let form = target.map_or_else(
            || {
                SshDialog::new(TerminalProfile::Xterm).with_isolation(
                    IsolationLevel::None,
                    self.clipboard.policy.minimum_isolation,
                )
            },
            |target| self.ssh_form(target, TerminalProfile::Xterm),
        );
        let focus = form
            .focus()
            .map(move |message| wrap(Message::Form(id, message)));
        let pending = PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: None,
            sftp: true,
            saved: target.and_then(|target| target.saved),
            incognito: false,
            retry: None,
            key_snapshots: Default::default(),
            group: VecDeque::new(),
            font: None,
            duplicate_tab: None,
            stage: Stage::Form(Box::new(form)),
        };
        if let Some(window) = self.windows.get_mut(&id) {
            window.cancel_paste();
            window.pending_ibm = None;
            window.pending_serial = None;
            window.notice = None;
            window.pending_ssh = Some(pending);
        } else if self.sftp.contains_key(&id) {
            self.sftp_logins.insert(id, pending);
        } else {
            return Task::none();
        }
        focus
    }

    fn connect_ssh(&mut self, window: window::Id) -> Task<WorkspaceMessage> {
        if !self.allow_ssh_transport(window, "SSH") {
            return self.fail_pending_reopen_ssh(window);
        }
        if self.drop_stale_ssh_split(window) || !self.validate_ssh_reconnect(window) {
            return Task::none();
        }
        let split = self
            .pending_ssh_mut(window)
            .and_then(|pending| pending.split);
        let minimum = self.clipboard.policy.minimum_isolation;
        let Some(PendingSsh {
            stage: Stage::Form(form),
            sftp,
            saved,
            replace,
            incognito,
            retry,
            ..
        }) = self.pending_ssh_mut(window)
        else {
            return Task::none();
        };
        let chosen_vault = form.explicit_vault();
        let Ok(log_config) = form.logging_config() else {
            return Task::none();
        };
        let Ok(mut request) = form.take_request() else {
            return Task::none();
        };
        *retry = Some(RetryLogin::new(
            &request,
            chosen_vault.clone(),
            log_config.clone(),
        ));
        let (sftp, saved, replace, incognito) = (*sftp, *saved, *replace, *incognito);
        let mut target = SshTarget {
            login: request.target.endpoint.clone(),
            jump: request.jump.as_ref().map(|login| login.endpoint.clone()),
            saved: None,
            isolation: request.isolation.max(minimum),
        };
        request.isolation = target.isolation;
        if target.isolation == IsolationLevel::Sandbox && !ssh_worker::sandbox_available() {
            self.notice(window, "SSH sandbox is unavailable on this system.".into());
            return self.fail_pending_reopen_ssh(window);
        }
        let guarded_saved = self
            .windows
            .get(&window)
            .and_then(|window| window.pending_ssh.as_ref())
            .and_then(|pending| {
                if let Some(reconnect) = &pending.reconnect {
                    Some(
                        reconnect
                            .saved
                            .as_ref()
                            .map(|saved| saved.connection.clone()),
                    )
                } else {
                    pending
                        .reopen
                        .as_ref()?
                        .saved
                        .as_ref()
                        .map(|saved| saved.current())
                }
            });
        let current_saved = saved.and_then(|id| self.saved_connection_with_group(id));
        let (saved_connection, connection_group) = current_saved
            .map_or((None, None), |(connection, group)| {
                (Some(connection), group)
            });
        let current = guarded_saved.unwrap_or(saved_connection);
        if let Some(refresh) = self.apply_reopen_ssh_config(window, current.as_ref()) {
            return refresh;
        }
        let current = current.filter(|connection| target.matches(connection));
        let connection_group = connection_group.filter(|_| current.is_some());
        if current.is_some() {
            target.saved = saved;
        }
        let incognito = incognito
            || (!sftp
                && replace.is_none()
                && current
                    .as_ref()
                    .is_some_and(|connection| connection.incognito));
        let incognito = if let Some(target) = split {
            match self.split_connection_privacy(window, target, incognito) {
                Ok(value) => value,
                Err(reason) => {
                    self.notice(window, reason.into());
                    return Task::none();
                }
            }
        } else {
            incognito
        };
        if incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(
                window,
                "Managed policy blocks this incognito connection.".into(),
            );
            return self.fail_pending_reopen_ssh(window);
        }
        if incognito && let Some(pending) = self.pending_ssh_mut(window) {
            // A saved connection may become private while its login form is open.
            // Keep that intent even if a changed credential requires a new form.
            pending.incognito = true;
        }
        if !saved_credentials_match(&request, current.as_ref(), chosen_vault.as_ref()) {
            target.saved = None;
            return self.reopen_ssh_form(
                window,
                target,
                request.profile,
                "The saved connection changed. Reopen it or enter credentials for this login."
                    .into(),
            );
        }
        let environment_credential = match &request.target.auth {
            ssh_dialog::Authentication::VaultPassword(id) => Some(id.clone()),
            _ => None,
        };
        let key_snapshots = match self.resolve_ssh_logins(&mut request, current.as_ref()) {
            Ok(keys) => keys,
            Err(error) => {
                use super::project_access::WaitReason;
                let reason = if error == crate::vault_window::VAULT_LOCKED {
                    WaitReason::VaultLocked
                } else {
                    WaitReason::SignIn
                };
                self.defer_project_ssh_dial(window, reason);
                self.notice(window, error.into());
                return Task::none();
            }
        };
        let logging = (!incognito
            && !sftp
            && self.clipboard.policy.problem.is_none()
            && (log_config.enabled() || self.journal_allowed()))
        .then(|| {
            let capture = crate::terminal_logging::Prepared::new(
                log_config,
                self.clipboard.policy.terminal_logging.clone(),
                current
                    .as_ref()
                    .map_or_else(|| target.login.host.clone(), |saved| saved.name.clone()),
                target.login.host.clone(),
                target.login.username.clone(),
            );
            let secrets = capture.secrets();
            for login in std::iter::once(&request.target).chain(request.jump.as_ref()) {
                use ssh_dialog::Authentication;
                match &login.auth {
                    Authentication::Password(value)
                    | Authentication::KeyWithPassphrase(_, value)
                    | Authentication::ResolvedKey(_, Some(value)) => secrets.add(value.as_str()),
                    _ => {}
                }
            }
            Box::new(capture)
        });
        let profile = request.profile;
        let size = ssh_size(
            self.reconnect_ssh_grid(window)
                .map_or_else(|| initial_grid(profile), |(grid, _)| grid),
        );
        let id = self.allocate();
        let worker_label = if incognito {
            "Incognito SSH".into()
        } else if let Some(connection) = &current {
            connection.name.clone()
        } else {
            target.login.host.clone()
        };
        let cancellation = DialCancellation::for_session(worker_label);
        let (events, messages) = async_channel::bounded(4);
        let prompt = challenge::Forwarder {
            events,
            window,
            dial: id,
            cancellation: cancellation.clone(),
            rounds: Arc::default(),
            secrets: logging.as_ref().map(|capture| capture.secrets()),
        };
        if let Some(pending) = self.pending_ssh_mut(window) {
            pending.key_snapshots = key_snapshots;
            let terminal_effect = if sftp
                || pending.replace.is_some()
                || pending.reconnect.is_some()
                || pending.reopen.is_some()
            {
                None
            } else {
                current
                    .as_ref()
                    .and_then(|connection| connection.terminal_effect)
            };
            let command_timestamps = current
                .as_ref()
                .and_then(|connection| connection.command_timestamps);
            pending.stage = Stage::Dialing {
                id,
                target: target.clone(),
                profile,
                environment_credential,
                logging,
                terminal_effect,
                command_timestamps,
                connection_group,
                _cancellation: CancelOnDrop(cancellation),
                challenge: None,
            };
        }
        let events = prompt.events.clone();
        let messages = stream::once(async move {
            let publish = move |result| {
                let _ = events.try_send(Message::Dialed(
                    window,
                    id,
                    DialResponse(Arc::new(Mutex::new(Some(result)))),
                ));
            };
            let publish_result = publish.clone();
            if let Err(error) = thread::Builder::new()
                .name("kortty-ssh-connect".into())
                .spawn(move || publish_result(dial(request, sftp, &prompt, size)))
            {
                publish(Err(error.into()));
            }
            messages
        })
        .flatten();
        Task::run(messages, wrap)
    }

    fn ssh_challenge_slot(
        &mut self,
        window: window::Id,
        id: u64,
    ) -> Option<&mut Option<Box<challenge::Form>>> {
        if self.drop_stale_ssh_split(window) || !self.validate_ssh_reconnect(window) {
            return None;
        }
        let Stage::Dialing {
            id: active,
            challenge,
            ..
        } = &mut self.pending_ssh_mut(window)?.stage
        else {
            return None;
        };
        (*active == id).then_some(challenge)
    }

    fn ssh_challenge(
        &mut self,
        window: window::Id,
        id: u64,
        ticket: challenge::Ticket,
    ) -> Task<WorkspaceMessage> {
        if self.project_ssh_target(window).is_some() {
            let current = self.ssh_challenge_slot(window, id).is_some();
            ticket.cancel();
            if current {
                self.defer_project_ssh_dial(window, super::project_access::WaitReason::SignIn);
            }
            return Task::none();
        }
        if let Some(slot @ None) = self.ssh_challenge_slot(window, id)
            && ticket.active()
        {
            let form = challenge::Form::new(ticket);
            let focus = form.focus();
            *slot = Some(Box::new(form));
            return focus;
        }
        ticket.cancel();
        Task::none()
    }

    fn resolve_ssh_login(&self, login: &mut LoginRequest) -> Result<(), &'static str> {
        use ssh_dialog::Authentication as Input;
        login.auth = match &login.auth {
            Input::VaultPassword(id) => Input::Password(ssh_dialog::Password::from_secret(
                self.vault_credential(id, Kind::Password, &login.endpoint)?,
            )),
            Input::VaultKey(path, id) => Input::KeyWithPassphrase(
                path.clone(),
                ssh_dialog::Password::from_secret(self.vault_credential(
                    id,
                    Kind::KeyPassphrase,
                    &login.endpoint,
                )?),
            ),
            _ => return Ok(()),
        };
        Ok(())
    }

    fn ssh_dialed(
        &mut self,
        window: window::Id,
        id: u64,
        response: DialResponse,
    ) -> Task<WorkspaceMessage> {
        if self.drop_stale_ssh_split(window) || !self.validate_ssh_reconnect(window) {
            return Task::none();
        }
        let Some(PendingSsh {
            replace,
            split,
            sftp,
            incognito,
            font,
            duplicate_tab,
            reopen,
            stage:
                Stage::Dialing {
                    id: active,
                    target,
                    profile,
                    environment_credential,
                    logging,
                    terminal_effect,
                    command_timestamps,
                    connection_group,
                    ..
                },
            ..
        }) = self.pending_ssh_mut(window)
        else {
            return Task::none();
        };
        if *active != id {
            return Task::none();
        }
        let (replace, target, profile, sftp, incognito, font) =
            (*replace, target.clone(), *profile, *sftp, *incognito, *font);
        let split = *split;
        let reopen = reopen.as_ref().map(|reopen| reopen.token);
        let environment_credential = environment_credential.clone();
        let logging = logging.take();
        let terminal_effect = *terminal_effect;
        let command_timestamps = *command_timestamps;
        let connection_group = connection_group.clone();
        let duplicate_tab = *duplicate_tab;
        if reopen.is_some()
            && let Some(refresh) = self.refresh_reopen_ssh_config(window)
        {
            return refresh;
        }
        if let Some(refresh) = self.refresh_registered_key_attempt(window, &target, profile) {
            return refresh;
        }
        let result = response
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(Err(error)) = &result
            && self.defer_project_ssh_dial(
                window,
                super::project_access::WaitReason::from_connect_error(error),
            )
        {
            self.notice(
                window,
                format!(
                    "{} is offline. Automatic reconnect stopped ({error}).",
                    target.label()
                ),
            );
            return Task::none();
        }
        match result {
            Some(Ok(ssh)) if sftp => {
                let recent = if incognito {
                    Task::none()
                } else {
                    self.record_connection_used(target.clone())
                };
                return Task::batch([self.open_sftp_connection(window, target, ssh), recent]);
            }
            Some(Ok(ssh)) => {
                let saved = target.saved;
                let terminal_effect = terminal_effect
                    .filter(|selection| self.allowed_terminal_effect(selection.effect));
                let (grid, preserve_strict) = self
                    .reconnect_ssh_grid(window)
                    .unwrap_or((initial_grid(profile), false));
                let strict = preserve_strict || ssh.sandbox_backend().is_some();
                let forwarding = ssh
                    .tunnel_openers()
                    .map(|(direct, remote)| super::tunnel_access::Forwarding::new(direct, remote));
                let appearance = self.window_settings.terminal_appearance();
                let appearance = terminal_effect.map_or(appearance, |selection| {
                    selection.effect.appearance(appearance)
                });
                let session = match Session::ssh_with_logging(
                    Some(ssh),
                    grid,
                    profile,
                    self.window_settings.prompt_navigation_enabled,
                    appearance,
                    logging,
                    strict,
                ) {
                    Ok(session) => session,
                    Err(error) => {
                        return self.reopen_ssh_form(window, target, profile, error.to_string());
                    }
                };
                let slot = if let Some(old) = replace {
                    let Some((tab, pane, _)) = self.reconnectable_ssh(window, old) else {
                        return Task::none();
                    };
                    Some((tab, pane, old))
                } else {
                    None
                };
                let session_id =
                    self.store_connected_session(session, grid, slot.map(|(_, _, old)| old), font);
                let recent_target = (!incognito
                    && reopen.is_none()
                    && replace.is_none()
                    && split.is_none_or(|target| !target.same_server))
                .then(|| target.clone());
                if let Some(pane) = self.sessions.get_mut(&session_id) {
                    pane.ssh = Some(target);
                    pane.forwarding = forwarding;
                    pane.strict_escapes = strict;
                    pane.credential_environment_source = environment_credential;
                    if let Some(selection) = terminal_effect {
                        pane.effect = Some(super::effects::Active::from_saved(selection));
                    }
                }
                if terminal_effect.is_some() {
                    self.sync_pane_appearance(session_id);
                }
                let group = self
                    .pending_ssh_mut(window)
                    .map_or_else(VecDeque::new, |pending| std::mem::take(&mut pending.group));
                let reconnect = self.pending_ssh_mut(window).and_then(|pending| {
                    Some((
                        pending.reconnect.take()?,
                        std::mem::take(&mut pending.key_snapshots),
                    ))
                });
                if let Some(state) = self.windows.get_mut(&window) {
                    state.pending_ssh = None;
                    state.notice = None;
                }
                if let Some((reconnect, key_snapshots)) = reconnect {
                    return self.start_reconnect_worker(
                        window,
                        reconnect.target,
                        session_id,
                        reconnect.saved,
                        key_snapshots,
                    );
                }
                if let Some((tab, pane, old)) = slot {
                    if let Some(tab) = self
                        .windows
                        .get_mut(&window)
                        .and_then(|window| window.tabs.get_mut(tab))
                        .and_then(super::TabEntry::terminal_mut)
                        && let Some(cell) = tab.grid.get_mut(pane)
                    {
                        *cell = session_id;
                        if tab.tunnel_owner == Some(old) {
                            tab.tunnel_owner = Some(session_id);
                            self.remap_tunnel_predecessors(old, session_id);
                        }
                        self.stop_session(old);
                    } else {
                        self.stop_session(session_id);
                    }
                } else if let Some(target) = split {
                    self.attach_split_connection(window, target, session_id);
                } else {
                    self.add_configured_tab(
                        window,
                        session_id,
                        incognito,
                        command_timestamps,
                        connection_group,
                    );
                    if let Some(source) = duplicate_tab {
                        self.place_duplicate_tab(window, source);
                    }
                }
                let recent = recent_target
                    .filter(|_| self.sessions.contains_key(&session_id))
                    .map_or_else(Task::none, |target| self.record_connection_used(target));
                let tunnels = if let Some(saved) = saved
                    && self.sessions.contains_key(&session_id)
                {
                    self.configure_saved_tunnels(window, session_id, saved)
                } else {
                    Task::none()
                };
                let next = if let Some(token) = reopen {
                    let tab = self.windows.get(&window).and_then(|state| {
                        let (index, _) = state.pane_location(session_id)?;
                        state.tabs.get(index).map(super::TabEntry::id)
                    });
                    let outcome = tab.map_or(SshOutcome::Unavailable, SshOutcome::Opened);
                    self.finish_reopen_ssh(window, token, outcome)
                } else {
                    self.advance_quick_group(window, group)
                };
                return Task::batch([tunnels, next, recent]);
            }
            Some(Err(ConnectError::UnknownHostKey { endpoint, offered })) => {
                if let Some(pending) = self.pending_ssh_mut(window) {
                    pending.stage = Stage::Unknown {
                        id,
                        target,
                        profile,
                        endpoint,
                        offered,
                    };
                }
            }
            Some(Err(ConnectError::ChangedHostKey {
                endpoint,
                expected,
                offered,
            })) => {
                if let Some(pending) = self.pending_ssh_mut(window) {
                    pending.stage = Stage::Changed {
                        id,
                        target,
                        profile,
                        review: ChangedKey {
                            endpoint,
                            expected,
                            offered,
                            reviewing: false,
                            verified: false,
                        },
                    };
                }
            }
            Some(Err(error)) => {
                return self.reopen_ssh_form(window, target, profile, error.to_string());
            }
            None => {}
        }
        Task::none()
    }

    fn drop_stale_ssh_split(&mut self, window: window::Id) -> bool {
        let target = self
            .windows
            .get(&window)
            .and_then(|window| window.pending_ssh.as_ref())
            .and_then(|pending| pending.split);
        if target.is_none_or(|target| self.split_target_pane(window, target).is_some()) {
            return false;
        }
        if let Some(state) = self.windows.get_mut(&window) {
            state.pending_ssh = None;
        }
        self.notice(
            window,
            "The original pane is no longer available for splitting.".into(),
        );
        true
    }

    fn remember_ssh(&mut self, window: window::Id, id: u64) -> Task<WorkspaceMessage> {
        if self.drop_stale_ssh_split(window) || !self.validate_ssh_reconnect(window) {
            return Task::none();
        }
        let Some(PendingSsh {
            stage:
                Stage::Unknown {
                    id: active,
                    target,
                    profile,
                    endpoint,
                    offered,
                },
            ..
        }) = self.pending_ssh_mut(window)
        else {
            return Task::none();
        };
        if *active != id {
            return Task::none();
        }
        let (endpoint, offered) = (endpoint.clone(), offered.clone());
        let (target, profile) = (target.clone(), *profile);
        if let Some(refresh) = self.refresh_reopen_ssh_config(window) {
            return refresh;
        }
        if let Some(refresh) = self.refresh_registered_key_attempt(window, &target, profile) {
            return refresh;
        }
        if let Some(pending) = self.pending_ssh_mut(window) {
            pending.stage = Stage::Remembering {
                id,
                target,
                profile,
                replacing: false,
            };
        }
        Task::future(async move {
            let result =
                trust_store().and_then(|store| match store.remember(&endpoint, &offered)? {
                    HostKeyStatus::Known => Ok(()),
                    _ => Err(io::Error::other(
                        "the SSH host key changed while approval was open",
                    )),
                });
            wrap(Message::Remembered(
                window,
                id,
                result.map_err(|error| error.to_string()),
            ))
        })
    }

    fn changed_key(&mut self, window: window::Id, id: u64) -> Option<&mut ChangedKey> {
        let pending = self.pending_ssh_mut(window)?;
        match &mut pending.stage {
            Stage::Changed {
                id: active, review, ..
            } if *active == id => Some(review),
            _ => None,
        }
    }

    fn replace_changed_ssh(&mut self, window: window::Id, id: u64) -> Task<WorkspaceMessage> {
        if self.host_pins_locked()
            || self.drop_stale_ssh_split(window)
            || !self.validate_ssh_reconnect(window)
        {
            return Task::none();
        }
        let Some(pending) = self.pending_ssh_mut(window) else {
            return Task::none();
        };
        let Stage::Changed {
            id: active,
            target,
            profile,
            review,
        } = &pending.stage
        else {
            return Task::none();
        };
        if *active != id || !review.reviewing || !review.verified {
            return Task::none();
        }
        let (endpoint, expected, offered) = (
            review.endpoint.clone(),
            review.expected.clone(),
            review.offered.clone(),
        );
        let (target, profile) = (target.clone(), *profile);
        if let Some(refresh) = self.refresh_reopen_ssh_config(window) {
            return refresh;
        }
        if let Some(refresh) = self.refresh_registered_key_attempt(window, &target, profile) {
            return refresh;
        }
        if let Some(pending) = self.pending_ssh_mut(window) {
            pending.stage = Stage::Remembering {
                id,
                target,
                profile,
                replacing: true,
            };
        }
        Task::future(async move {
            let result = super::known_hosts::ensure_pin_mutation_allowed()
                .and_then(|()| {
                    if trust_store()?.replace_pin(&endpoint, &expected, &offered)? {
                        Ok(())
                    } else {
                        Err(io::Error::other(
                            "the saved key changed while the review was open",
                        ))
                    }
                })
                .map_err(|error| error.to_string());
            wrap(Message::Remembered(window, id, result))
        })
    }

    fn reopen_ssh_form(
        &mut self,
        window: window::Id,
        mut target: SshTarget,
        profile: TerminalProfile,
        notice: String,
    ) -> Task<WorkspaceMessage> {
        if self.defer_project_ssh_dial(window, super::project_access::WaitReason::ConnectionFailed)
        {
            self.notice(window, notice);
            return Task::none();
        }
        let minimum = self.clipboard.policy.minimum_isolation;
        let Some(pending) = self.pending_ssh_mut(window) else {
            return Task::none();
        };
        let same_server = pending.split.is_some_and(|split| split.same_server);
        let reopen = pending.reopen_token();
        let reconnect = pending.reconnect.is_some();
        let saved_form = pending
            .reopen
            .as_ref()
            .and_then(|reopen| reopen.saved.as_deref())
            .or_else(|| {
                pending
                    .reconnect
                    .as_ref()
                    .and_then(|reconnect| reconnect.saved.as_deref())
            })
            .map(|saved| {
                target.saved = Some(saved.connection.id);
                saved.form(profile, target.isolation.max(minimum))
            });
        let mut form = if let Some(form) = saved_form {
            form
        } else if same_server {
            self.fixed_split_ssh_form(&target, profile)
        } else if reopen.is_some() || reconnect {
            let floor = target.isolation.max(minimum);
            target
                .form(profile)
                .with_isolation(floor, floor)
                .with_fixed_route()
        } else {
            self.ssh_form(&target, profile)
        };
        if let Some(retry) = self
            .pending_ssh_mut(window)
            .and_then(|pending| pending.retry.as_ref())
        {
            retry.restore(&mut form, &target.login);
        }
        let form_key = if let Some(token) = reopen {
            Some(FormKey::Reopen((token, self.allocate())))
        } else if reconnect {
            Some(FormKey::Reconnect(self.allocate()))
        } else {
            None
        };
        let focus = form
            .focus()
            .map(move |message| wrap(form_message(window, form_key, message)));
        if let Some(pending) = self.pending_ssh_mut(window) {
            if let Some(reopen) = &mut pending.reopen
                && let Some(FormKey::Reopen((_, revision))) = form_key
            {
                reopen.form_revision = revision;
            }
            if let Some(reconnect) = &mut pending.reconnect
                && let Some(FormKey::Reconnect(revision)) = form_key
            {
                reconnect.form_revision = revision;
            }
            pending.saved = target.saved;
            pending.key_snapshots = KeySnapshots::default();
            pending.stage = Stage::Form(Box::new(form));
        }
        self.notice(window, notice);
        focus
    }

    fn reconnectable_ssh(
        &self,
        window: window::Id,
        id: SessionId,
    ) -> Option<(usize, iced::widget::pane_grid::Pane, &super::PaneSession)> {
        let (tab, pane) = self.windows.get(&window)?.pane_location(id)?;
        let previous = self.sessions.get(&id)?;
        (previous.ssh.is_some()
            && matches!(
                previous.session.frame().status,
                Status::Disconnected | Status::Exited(_) | Status::Error(_)
            ))
        .then_some((tab, pane, previous))
    }

    pub(super) fn store_disconnected_ssh(
        &mut self,
        target: SshTarget,
        profile: TerminalProfile,
        font: u8,
    ) -> io::Result<SessionId> {
        let grid = initial_grid(profile);
        let session = Session::ssh(
            None,
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
        )?;
        let id = self.store_session(session, grid, font);
        self.sessions
            .get_mut(&id)
            .expect("stored session exists")
            .ssh = Some(target);
        Ok(id)
    }

    pub(super) fn ssh_dialog_view<'a>(
        &self,
        id: window::Id,
        pending: &'a PendingSsh,
    ) -> Element<'a, WorkspaceMessage> {
        let form_key = pending.form_key();
        let cancel = wrap(form_message(id, form_key, ssh_dialog::Message::Cancel));
        match &pending.stage {
            Stage::Form(form) => {
                let form = form
                    .view_for(
                        pending.sftp,
                        !pending.incognito && self.clipboard.policy.problem.is_none(),
                        &self.ssh_key_choices,
                    )
                    .map(move |message| wrap(form_message(id, form_key, message)));
                if pending.incognito {
                    column![text("Incognito session").size(14), form]
                        .spacing(5)
                        .into()
                } else {
                    form
                }
            }
            Stage::Dialing {
                id: dial,
                challenge: Some(form),
                ..
            } => form.view(id, *dial, cancel),
            Stage::Dialing { target, .. } => column![
                text(format!("Connecting to {}…", target.label())),
                button("Cancel").on_press(cancel)
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Stage::Unknown {
                id: request,
                endpoint,
                offered,
                ..
            } => column![
                text(format!("First SSH connection to {endpoint}")).size(20),
                text("Compare the host key fingerprint with a trusted source."),
                text(offered.fingerprint()),
                row![
                    button("Cancel").on_press(cancel),
                    button("Trust host key").on_press(wrap(Message::Remember(id, *request)))
                ]
                .spacing(8),
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Stage::Remembering { .. } => {
                column![text("Saving the SSH host key…")].padding(16).into()
            }
            Stage::Changed {
                id: request,
                review,
                ..
            } => changed_key_view(id, *request, review, self.host_pins_locked(), cancel),
        }
    }
}

fn changed_key_view(
    window: window::Id,
    request: u64,
    review: &ChangedKey,
    locked: bool,
    cancel: WorkspaceMessage,
) -> Element<'static, WorkspaceMessage> {
    let mut content = column![
        text(format!("SSH host key changed for {}", review.endpoint)).size(20),
        text("The connection was stopped before authentication."),
        text(format!("Trusted key · {}", review.expected.algorithm())).size(14),
        text(review.expected.fingerprint()).font(iced::Font::MONOSPACE),
        text(format!("Offered key · {}", review.offered.algorithm())).size(14),
        text(review.offered.fingerprint()).font(iced::Font::MONOSPACE),
    ]
    .padding(16)
    .spacing(9);
    if locked {
        content = content.push(text("Managed policy locks trusted SSH host keys."));
    }
    if review.reviewing && !locked {
        content = content.push(
            checkbox(review.verified)
                .label("I verified the offered SHA-256 fingerprint through a trusted source.")
                .on_toggle(move |checked| wrap(Message::VerifyChanged(window, request, checked))),
        );
        content
            .push(
                row![
                    button("Cancel").on_press(wrap(Message::ReviewChanged(window, request, false))),
                    button("Replace trusted key")
                        .style(button::danger)
                        .on_press_maybe(
                            review
                                .verified
                                .then_some(wrap(Message::ReplaceChanged(window, request)))
                        ),
                ]
                .spacing(8),
            )
            .into()
    } else {
        content
            .push(
                row![
                    button("Close").on_press(cancel),
                    button("Review and Replace…").on_press_maybe(
                        (!locked).then_some(wrap(Message::ReviewChanged(window, request, true)))
                    ),
                ]
                .spacing(8),
            )
            .into()
    }
}

fn dial(
    request: ssh_dialog::Request,
    sftp: bool,
    prompt: &challenge::Forwarder,
    size: kortty_ssh::WindowSize,
) -> Result<Connected, ConnectError> {
    if request.isolation >= IsolationLevel::Process {
        return ssh_worker::Session::dial(
            request,
            size,
            sftp,
            &prompt.cancellation,
            &mut |round| prompt.request(round),
        )
        .map(|session| Connected::Worker(Box::new(session)));
    }
    dial_direct(request, sftp, prompt, trust_store()?, size)
}

fn dial_direct(
    request: ssh_dialog::Request,
    sftp: bool,
    prompt: &challenge::Forwarder,
    trust: TrustStore,
    size: kortty_ssh::WindowSize,
) -> Result<Connected, ConnectError> {
    let ssh_dialog::Request {
        target,
        jump,
        profile,
        options,
        ..
    } = request;
    ssh_connection::dial(
        (
            route_login(target, prompt.clone())?,
            jump.map(|login| route_login(login, prompt.clone()))
                .transpose()?,
        ),
        trust,
        if sftp {
            StartupChannel::Subsystem("sftp")
        } else {
            StartupChannel::Terminal {
                term: profile.term_name(),
                size,
            }
        },
        options,
        None,
        || prompt.cancellation.is_cancelled(),
    )
    .map(|session| Connected::Direct(Box::new(session)))
}

fn route_login(
    login: LoginRequest,
    prompt: challenge::Forwarder,
) -> io::Result<kortty_ssh::SshTarget> {
    use ssh_dialog::Authentication as Input;
    let handler = move |round: &kortty_ssh::KeyboardChallenge| prompt.request(round);
    let primary = match login.auth {
        Input::KeyboardInteractive => None,
        Input::Password(password) => Some(Authentication::password(password.into_string())),
        Input::PrivateKey(path) => Some(ssh_connection::private_key(&path)?),
        Input::KeyWithPassphrase(path, passphrase) => Some(
            ssh_connection::private_key_with_passphrase(&path, passphrase)?,
        ),
        Input::ResolvedKey(record, passphrase) => {
            let mut key = ssh_connection::read_registered_key(&record)?;
            let key = std::mem::take(&mut *key);
            Some(match passphrase {
                Some(passphrase) => {
                    Authentication::private_key_with_passphrase(key, passphrase.into_string())
                }
                None => Authentication::private_key(key),
            })
        }
        Input::VaultPassword(_) | Input::VaultKey(..) | Input::StoredKey { .. } => {
            return Err(io::Error::other("SSH credential was not resolved"));
        }
    };
    Ok(kortty_ssh::SshTarget {
        endpoint: Endpoint::new(&login.endpoint.host, login.endpoint.port)?,
        username: login.endpoint.username,
        authentication: match primary {
            None => Authentication::keyboard_interactive(handler),
            Some(primary) if login.keyboard_interactive => {
                primary.with_keyboard_interactive(handler)
            }
            Some(primary) => primary,
        },
    })
}

fn saved_credentials_match(
    request: &ssh_dialog::Request,
    current: Option<&Connection>,
    explicit_vault: Option<&(LoginTarget, String)>,
) -> bool {
    fn matches(login: &LoginRequest, saved: Option<&crate::connection_store::SavedLogin>) -> bool {
        use crate::connection_store::Auth;
        use ssh_dialog::Authentication as Input;
        let expected = saved.map(|login| &login.auth);
        match &login.auth {
            Input::VaultPassword(id) => {
                expected.and_then(Auth::credential_reference) == Some(id.as_str())
            }
            Input::VaultKey(path, id) => {
                matches!(expected, Some(Auth::KeyFile { path: key, passphrase: Some(reference) }) if path == key && id == reference)
            }
            Input::StoredKey { id, explicit } => {
                *explicit || expected.and_then(Auth::registered_key) == Some(*id)
            }
            _ => true,
        }
    }

    let explicit_match = explicit_vault.is_some_and(|(endpoint, credential)| {
        endpoint == &request.target.endpoint
            && matches!(&request.target.auth, ssh_dialog::Authentication::VaultPassword(id) if id == credential)
    });
    (explicit_match || matches(&request.target, current.map(|connection| &connection.login)))
        && request.jump.as_ref().is_none_or(|jump| {
            matches(
                jump,
                current.and_then(|connection| connection.jump.as_ref()),
            )
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKey {
    Reopen((ReopenToken, u64)),
    Reconnect(u64),
}

fn form_message(
    owner: window::Id,
    form_key: Option<FormKey>,
    message: ssh_dialog::Message,
) -> Message {
    match form_key {
        Some(FormKey::Reopen((token, revision))) => {
            Message::ReopenForm(owner, token, revision, message)
        }
        Some(FormKey::Reconnect(revision)) => Message::ReconnectForm(owner, revision, message),
        None => Message::Form(owner, message),
    }
}

fn wrap(message: Message) -> WorkspaceMessage {
    WorkspaceMessage::Ssh(message)
}
