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

use crate::connection_store::{Connection, Store, teamwork};
use crate::profile::InitialTerminalSize;
use crate::session::{GridSize, Session, SessionId, Status, ssh_size};
use crate::session_security::IsolationLevel;
use crate::ssh_connection::{self, Connected, trust_store};
use crate::ssh_dialog::{self, KeyboardOptions, LoginRequest, LoginTarget, SshDialog};
use crate::ssh_worker::{self, CancelOnDrop, DialCancellation};
use crate::terminal_effects::Selection;
use crate::vault_window::Kind;

use super::{
    Message as WorkspaceMessage, Workspace,
    recently_closed::{ReopenToken, SshOutcome},
    split_connection::SplitTarget,
};

mod challenge;
mod control_create;
mod password_command;
mod reconnect;
mod registered_keys;
mod reopen;
mod teamwork_auth;
use control_create::ControlLaunch;
pub(super) use control_create::CreateOptions;
use reconnect::ReconnectLaunch;
pub(super) use registered_keys::KeySnapshots;
use reopen::ReopenLaunch;
pub(super) use reopen::{ReopenSshOrigin, SavedSshProfile};
use teamwork_auth::AuthSnapshot;
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
    pub(crate) fn workflow_route_allowed(
        &self,
        policy: &crate::clipboard_policy::ClipboardPolicy,
    ) -> bool {
        self.isolation >= policy.minimum_isolation
            && policy
                .server_access()
                .first_blocked(self.login.servers(self.jump.as_ref()))
                .is_none()
    }

    pub(super) fn blocked_server(&self, workspace: &Workspace) -> Option<String> {
        workspace
            .blocked_ssh_server(&self.login, self.jump.as_ref())
            .map(|(host, port)| super::server_access::blocked_notice(host, port))
    }

    pub(crate) fn sftp_sudo_allowed(
        &self,
        policy: &crate::clipboard_policy::ClipboardPolicy,
    ) -> bool {
        use crate::clipboard_policy::RemoteEditorPolicy;

        policy.problem.is_none()
            && policy.sftp_sudo_edit_allowed
            && policy.file_transfer_allowed
            && policy.remote_editor == RemoteEditorPolicy::Allow
            && self.isolation >= policy.minimum_isolation
            && policy
                .server_access()
                .first_blocked(self.login.servers(self.jump.as_ref()))
                .is_none()
    }

    pub(crate) fn sftp_sudo_login(&self) -> &LoginTarget {
        &self.login
    }

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

    pub(super) fn project_route_label(&self) -> String {
        self.jump.as_ref().map_or_else(
            || self.login.label(),
            |jump| format!("{} via {}", self.login.label(), jump.label()),
        )
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
    control: Option<ControlLaunch>,
    teamwork: Option<TeamworkLaunch>,
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

struct TeamworkLaunch {
    selection: TeamworkSelection,
    route: SshTarget,
    auth: AuthSnapshot,
    form_revision: u64,
}

pub(super) struct TeamworkUnlock {
    owner: window::Id,
    vault: window::Id,
    selection: TeamworkSelection,
    sftp: bool,
    preference: teamwork::DefaultAuth,
    control: Option<CreateOptions>,
}

#[derive(Clone)]
pub(super) struct TeamworkSelection {
    identity: teamwork::Identity,
    store_path: PathBuf,
    connection: Box<Connection>,
    default_auth_eligible: bool,
}

#[derive(Clone, Copy)]
pub(super) struct TeamworkTerminalInput {
    pub(super) paste_line_delay_ms: Option<u16>,
    strict: bool,
}

impl TeamworkSelection {
    pub(super) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(super) fn new(entry: &teamwork::Entry, store_path: PathBuf) -> Self {
        Self {
            identity: entry.identity().clone(),
            store_path,
            connection: Box::new(entry.connection.clone()),
            default_auth_eligible: entry.default_auth_eligible,
        }
    }

    pub(super) fn matches_catalog(&self, workspace: &Workspace) -> bool {
        workspace
            .teamwork_entry(&self.identity, &self.store_path)
            .is_some_and(|entry| {
                entry.connection == *self.connection
                    && entry.default_auth_eligible == self.default_auth_eligible
            })
    }
}

const TEAMWORK_RESELECT: &str =
    "This Teamwork connection changed or is no longer available. Select it again.";

struct RetryLogin {
    keyboard: (KeyboardOptions, Option<KeyboardOptions>),
    target_key: Option<PathBuf>,
    jump_key: Option<PathBuf>,
    temporary_key: Option<ssh_dialog::TemporaryKey>,
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
            temporary_key: match &request.target.auth {
                ssh_dialog::Authentication::TemporaryKey(key) => Some(key.clone()),
                _ => None,
            },
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
        if let Some(key) = &self.temporary_key {
            form.restore_temporary_key(key.clone());
        }
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
    pub(super) fn control_anchor(&self) -> Option<SessionId> {
        self.control.as_ref()?;
        self.replace
    }

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
        self.reopen_form_key()
            .map(FormKey::Reopen)
            .or_else(|| {
                self.reconnect
                    .as_ref()
                    .map(|reconnect| FormKey::Reconnect(reconnect.form_revision))
            })
            .or_else(|| {
                self.teamwork
                    .as_ref()
                    .map(|shared| FormKey::Teamwork(shared.form_revision))
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
    PasswordCommand(Box<password_command::Pending>),
    Dialing {
        id: u64,
        target: SshTarget,
        profile: TerminalProfile,
        grid: GridSize,
        environment_credential: Option<String>,
        logging: Option<Box<crate::terminal_logging::Prepared>>,
        terminal_effect: Option<Selection>,
        command_timestamps: Option<bool>,
        journal_config: Option<crate::session_journal::ConnectionConfig>,
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

impl Stage {
    fn target(&self) -> Option<&SshTarget> {
        match self {
            Self::Form(_) => None,
            Self::PasswordCommand(pending) => Some(&pending.target),
            Self::Dialing { target, .. }
            | Self::Unknown { target, .. }
            | Self::Changed { target, .. }
            | Self::Remembering { target, .. } => Some(target),
        }
    }
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
    TeamworkForm(window::Id, u64, ssh_dialog::Message),
    TemporaryKeyPasted(window::Id, TemporaryKeyPaste, ssh_dialog::Password),
    Cancel(window::Id),
    ApprovePasswordCommand(window::Id, u64),
    PasswordCommandFinished(window::Id, u64, password_command::Response),
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

#[derive(Clone, Copy, Debug)]
pub struct TemporaryKeyPaste {
    form_revision: u64,
    request: u64,
    internal_clipboard: bool,
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
            Message::ApprovePasswordCommand(window, id) => {
                self.approve_password_command(window, id)
            }
            Message::PasswordCommandFinished(window, id, response) => {
                self.password_command_finished(window, id, response)
            }
            Message::ReconnectForm(window, revision, message)
                if self.guarded_ssh_form_key(window) == Some(FormKey::Reconnect(revision)) =>
            {
                self.update_ssh_form(window, message)
            }
            Message::TeamworkForm(window, revision, message)
                if self.guarded_ssh_form_key(window) == Some(FormKey::Teamwork(revision)) =>
            {
                self.update_ssh_form(window, message)
            }
            Message::TemporaryKeyPasted(window, paste, value) => {
                if self.guarded_ssh_form_key(window) != Some(FormKey::Teamwork(paste.form_revision))
                    || !self.pending_ssh_mut(window).is_some_and(|pending| {
                        matches!(&pending.stage, Stage::Form(form)
                            if form.expects_temporary_key_paste(paste.request))
                    })
                {
                    return Task::none();
                }
                if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
                    return cancel;
                }
                let value = if self.clipboard.policy.internal == paste.internal_clipboard {
                    value
                } else {
                    ssh_dialog::Password::default()
                };
                if let Some(PendingSsh {
                    stage: Stage::Form(form),
                    ..
                }) = self.pending_ssh_mut(window)
                {
                    form.finish_temporary_key_paste(paste.request, value);
                }
                Task::none()
            }
            Message::Form(..)
            | Message::ReopenForm(..)
            | Message::ReconnectForm(..)
            | Message::TeamworkForm(..) => Task::none(),
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
                if let Some((target, profile)) = attempt {
                    if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
                        return cancel;
                    }
                    if let Some(refresh) =
                        self.refresh_registered_key_attempt(window, &target, profile)
                    {
                        return refresh;
                    }
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
            ssh_dialog::Message::PasteTemporaryKey => self.paste_teamwork_key(owner),
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

    fn paste_teamwork_key(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        let Some(FormKey::Teamwork(revision)) = self.guarded_ssh_form_key(owner) else {
            return Task::none();
        };
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(owner) {
            return cancel;
        }
        let paste = TemporaryKeyPaste {
            form_revision: revision,
            request: self.allocate(),
            internal_clipboard: self.clipboard.policy.internal,
        };
        let Some(PendingSsh {
            stage: Stage::Form(form),
            ..
        }) = self.pending_ssh_mut(owner)
        else {
            return Task::none();
        };
        if !form.begin_temporary_key_paste(paste.request) {
            return Task::none();
        }
        // This button reads on behalf of the user. OSC 5522 permissions apply to terminal programs.
        self.read_clipboard().map(move |value| {
            wrap(Message::TemporaryKeyPasted(
                owner,
                paste,
                ssh_dialog::Password::new(value.unwrap_or_default()),
            ))
        })
    }

    fn cancel_stale_routed_ssh_login(
        &mut self,
        owner: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        let pending = self.pending_ssh(owner)?;
        let blocked = pending
            .stage
            .target()
            .or_else(|| pending.teamwork.as_ref().map(|shared| &shared.route))
            .and_then(|target| target.blocked_server(self));
        if let Some(notice) = blocked {
            self.defer_project_ssh_dial(owner, super::project_access::WaitReason::Policy);
            let cancel = self.cancel_ssh_login(owner);
            self.notice(owner, notice);
            return Some(cancel);
        }
        if let Some(reason) = self.control_ssh_stale_reason(owner) {
            let cancel = self.cancel_ssh_login(owner);
            self.notice(owner, reason.into());
            return Some(cancel);
        }
        let shared = self
            .windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .teamwork
            .as_ref()?;
        let reason = if !self.teamwork_sync_allowed() {
            "Managed policy blocks Teamwork connections."
        } else if !shared.selection.matches_catalog(self) {
            TEAMWORK_RESELECT
        } else if !shared
            .auth
            .is_current(self, &shared.selection, &shared.route.login)
        {
            "Teamwork authentication changed or is unavailable. Select the connection again."
        } else {
            return None;
        };
        let cancel = self.cancel_ssh_login(owner);
        self.notice(owner, reason.into());
        Some(cancel)
    }

    fn cancel_ssh_login(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        if self.project_ssh_target(owner).is_some() {
            self.defer_interrupted_project_reconnects(owner, None);
        }
        let pending = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.pending_ssh.take());
        if let Some(anchor) = pending.as_ref().and_then(PendingSsh::control_anchor) {
            self.close_control_ssh_anchor(anchor);
        }
        let reopen = pending.and_then(|pending| pending.reopen_token());
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
        if let Some(notice) = self.blocked_ssh_form(&form) {
            self.notice(window, notice);
            return Task::none();
        }
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
                control: None,
                teamwork: None,
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

    fn pending_ssh(&self, id: window::Id) -> Option<&PendingSsh> {
        if let Some(window) = self.windows.get(&id) {
            window.pending_ssh.as_ref()
        } else {
            self.sftp_logins.get(&id)
        }
    }

    fn pending_ssh_mut(&mut self, id: window::Id) -> Option<&mut PendingSsh> {
        if let Some(window) = self.windows.get_mut(&id) {
            window.pending_ssh.as_mut()
        } else {
            self.sftp_logins.get_mut(&id)
        }
    }

    fn project_sized_ssh_pane(&self, window: window::Id) -> Option<&super::PaneSession> {
        let pending = self.pending_ssh(window)?;
        let source = pending
            .reconnect
            .as_ref()
            .map(|reconnect| reconnect.target.source)
            .or(pending.replace)?;
        let pane = self.sessions.get(&source)?;
        (!pane.project_initial_size.is_default()).then_some(pane)
    }

    fn live_ssh_grid(&self, window: window::Id) -> Option<(GridSize, bool)> {
        self.reconnect_ssh_grid(window).or_else(|| {
            let pane = self.project_sized_ssh_pane(window)?;
            Some((pane.grid, pane.reconnect_strict_escapes()))
        })
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
        if let Some(notice) = target.blocked_server(self) {
            self.notice(window, notice);
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
            control: None,
            teamwork: None,
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
        self.stage_saved_ssh(id, connection, sftp, None)
    }

    pub(super) fn show_saved_ssh_in_new_window(
        &mut self,
        owner: window::Id,
        connection: Connection,
    ) -> Task<WorkspaceMessage> {
        if !self.allow_ssh_transport(owner, "SSH") || !self.allow_saved_server(owner, &connection) {
            return Task::none();
        }
        if connection.incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(
                owner,
                "Managed policy blocks this incognito connection.".into(),
            );
            return Task::none();
        }
        let (id, opened) = if let Some(geometry) = connection.window_geometry {
            let (id, opened) = self.open_window_at_placement(geometry.placement());
            self.windows
                .get_mut(&id)
                .expect("new connection window")
                .geometry
                .mark_connection_specific();
            let opened = if geometry.maximized {
                opened.chain(window::maximize(id, true))
            } else {
                opened
            };
            (id, opened)
        } else {
            self.open_window(false)
        };
        let login = self.show_saved_ssh(id, connection, false);
        opened.chain(login)
    }

    fn stage_saved_ssh(
        &mut self,
        id: window::Id,
        connection: Connection,
        sftp: bool,
        control: Option<ControlLaunch>,
    ) -> Task<WorkspaceMessage> {
        if self.reopen_ssh_token(id).is_some()
            || !self.allow_ssh_transport(id, if sftp { "SFTP" } else { "SSH" })
        {
            return Task::none();
        }
        if !self.allow_saved_server(id, &connection) {
            return Task::none();
        }
        let incognito = connection.incognito
            || control
                .as_ref()
                .is_some_and(|launch| launch.options.incognito);
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
        let form = SshDialog::from_saved(profile, &connection);
        let form = if let Some(launch) = &control {
            form.with_launch_isolation(launch.floor).with_fixed_route()
        } else {
            form.with_isolation(
                IsolationLevel::None,
                self.clipboard.policy.minimum_isolation,
            )
        };
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
            control,
            teamwork: None,
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

    pub(super) fn show_teamwork_ssh(
        &mut self,
        owner: window::Id,
        selection: TeamworkSelection,
        sftp: bool,
    ) -> Task<WorkspaceMessage> {
        self.open_teamwork_ssh(owner, selection, sftp, true, None)
    }

    fn open_teamwork_ssh(
        &mut self,
        owner: window::Id,
        selection: TeamworkSelection,
        sftp: bool,
        offer_unlock: bool,
        control: Option<CreateOptions>,
    ) -> Task<WorkspaceMessage> {
        if self.exiting
            || !self.teamwork_sync_allowed()
            || self.reopen_ssh_token(owner).is_some()
            || !self.allow_ssh_transport(owner, if sftp { "SFTP" } else { "SSH" })
            || !self.windows.contains_key(&owner)
        {
            return Task::none();
        }
        if !selection.matches_catalog(self) {
            self.notice(owner, TEAMWORK_RESELECT.into());
            return Task::none();
        }
        let connection = &selection.connection;
        if !self.allow_saved_server(owner, connection) {
            return Task::none();
        }
        let incognito = connection.incognito || control.is_some_and(|options| options.incognito);
        if incognito && !self.clipboard.policy.incognito_allowed {
            self.notice(
                owner,
                "Managed policy blocks this incognito connection.".into(),
            );
            return Task::none();
        }
        if connection.validate().is_err()
            || connection
                .jump
                .as_ref()
                .is_some_and(|jump| jump.endpoint.username.is_empty())
        {
            self.notice(
                owner,
                "This Teamwork connection requires a valid route with login usernames.".into(),
            );
            return Task::none();
        }
        if offer_unlock {
            if let Some(vault) = self.current_vault_unlock() {
                return window::gain_focus(vault);
            }
            if self.vault_id.is_none()
                && selection.default_auth_eligible
                && matches!(
                    self.teamwork_auth_default(),
                    teamwork::DefaultAuth::Password { .. }
                )
                && self.vault_unlock_available()
            {
                let open = self.show_vault_unlock(owner);
                if let Some(vault) = self.vault_unlock_for_owner(owner) {
                    self.teamwork_unlock = Some(TeamworkUnlock {
                        owner,
                        vault,
                        selection,
                        sftp,
                        preference: self.teamwork_auth_default().clone(),
                        control,
                    });
                    return open;
                }
            }
        }
        let (login, auth) = AuthSnapshot::choose(self, &selection);
        if login.username.is_empty() {
            self.notice(
                owner,
                "This Teamwork target needs a username from its source or an available authentication default.".into(),
            );
            return Task::none();
        }
        let Some(profile) = connection.profile() else {
            return Task::none();
        };
        let Ok(route) = SshTarget::from_imported_route(
            login,
            connection.jump.as_ref().map(|jump| jump.endpoint.clone()),
            connection.isolation,
        ) else {
            self.notice(
                owner,
                "This Teamwork connection has an invalid SSH route.".into(),
            );
            return Task::none();
        };
        let floor = route
            .isolation
            .max(self.clipboard.policy.minimum_isolation)
            .max(
                control
                    .and_then(|options| options.isolation)
                    .unwrap_or_default(),
            );
        let mut form = route
            .form(profile)
            .with_startup(connection.startup)
            .with_isolation(floor, floor)
            .with_fixed_route();
        auth.apply_to(&mut form, &route.login);
        let form_revision = self.allocate();
        let key = Some(FormKey::Teamwork(form_revision));
        let focus = form
            .focus()
            .map(move |message| wrap(form_message(owner, key, message)));
        self.refresh_ssh_key_choices();
        let Some(state) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        state.cancel_paste();
        state.pending_ibm = None;
        state.pending_serial = None;
        state.pending_tunnels = None;
        state.notice = auth.notice();
        state.pending_ssh = Some(PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: None,
            control: control.map(|options| ControlLaunch {
                options,
                floor,
                saved: None,
                vault: None,
            }),
            teamwork: Some(TeamworkLaunch {
                selection,
                route,
                auth,
                form_revision,
            }),
            sftp,
            saved: None,
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

    pub(super) fn resume_teamwork_after_vault(
        &mut self,
        closed: window::Id,
    ) -> Task<WorkspaceMessage> {
        let Some(pending) = self
            .teamwork_unlock
            .take_if(|pending| pending.vault == closed)
        else {
            return Task::none();
        };
        if self.exiting || !self.windows.contains_key(&pending.owner) {
            return Task::none();
        }
        if self.teamwork_auth_default() != &pending.preference {
            self.notice(
                pending.owner,
                "Teamwork authentication changed. Select the connection again.".into(),
            );
            return Task::none();
        }
        if pending.control.is_some()
            && (!self.clipboard.policy.control_allowed()
                || !self
                    .vault
                    .as_ref()
                    .is_some_and(crate::vault_window::VaultWindow::is_unlocked))
        {
            return Task::none();
        }
        let focus = self.open_teamwork_ssh(
            pending.owner,
            pending.selection,
            pending.sftp,
            false,
            pending.control,
        );
        if pending.control.is_some() {
            return self
                .start_control_ssh(pending.owner, focus)
                .map_or_else(|_| Task::none(), |(_, task)| task);
        }
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
        if let Some(notice) = self.blocked_ssh_form(&form) {
            self.notice(id, notice);
            return token.map_or_else(Task::none, |token| {
                self.finish_reopen_ssh(id, token, SshOutcome::Unavailable)
            });
        }
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
            control: None,
            teamwork: None,
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
        let project = self.project_remote_target(id);
        let target = project.or_else(|| {
            self.sftp
                .get(&id)
                .map(|window| window.target())
                .or_else(|| self.focused_session(id).and_then(|pane| pane.ssh.as_ref()))
        });
        if let Some(notice) = target.and_then(|target| target.blocked_server(self)) {
            self.notice(id, notice);
            return Task::none();
        }
        let form = target.map_or_else(
            || {
                SshDialog::new(TerminalProfile::Xterm).with_isolation(
                    IsolationLevel::None,
                    self.clipboard.policy.minimum_isolation,
                )
            },
            |target| {
                if project.is_some() {
                    self.fixed_split_ssh_form(target, TerminalProfile::Xterm)
                } else {
                    self.ssh_form(target, TerminalProfile::Xterm)
                }
            },
        );
        let focus = form
            .focus()
            .map(move |message| wrap(Message::Form(id, message)));
        let pending = PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: None,
            control: None,
            teamwork: None,
            sftp: true,
            saved: target.and_then(|target| target.saved),
            incognito: self
                .sftp
                .get(&id)
                .map(|window| window.incognito())
                .or_else(|| self.windows.get(&id)?.tab().map(|tab| tab.incognito))
                .unwrap_or_default(),
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
        } else if self.sftp.contains_key(&id) || self.project_remote_target(id).is_some() {
            self.sftp_logins.insert(id, pending);
        } else {
            return Task::none();
        }
        focus
    }

    pub(in crate::workspace) fn show_project_sftp_login(
        &mut self,
        id: window::Id,
    ) -> Task<WorkspaceMessage> {
        if self.project_remote_target(id).is_none() || self.sftp_logins.contains_key(&id) {
            return Task::none();
        }
        if let Some(error) = self.project_remote_denial(id) {
            self.notice(id, error);
            return Task::none();
        }
        self.show_sftp_login(id)
    }

    fn connect_ssh(&mut self, window: window::Id) -> Task<WorkspaceMessage> {
        let task = self.prepare_ssh_dial(window);
        if let Err(error) = self.attach_control_ssh_tab(window) {
            let canceled = self.cancel_ssh_login(window);
            self.notice(window, format!("Could not open the SSH tab ({error})."));
            return canceled;
        }
        task
    }

    fn ssh_needs_terminal_secrets(&self, incognito: bool, sftp: bool, logging: bool) -> bool {
        let policy = &self.clipboard.policy;
        !sftp
            && policy.problem.is_none()
            && ((self.ai_features_enabled()
                && (policy.ai_chat_allowed
                    || policy.ai_agent_allowed()
                    || policy.ai_planning_allowed()
                    || policy.ai_swarm_allowed()))
                || (!incognito
                    && (logging
                        || self.journal_allowed()
                        || policy.control_allowed() && policy.mcp_server_allowed)))
    }

    fn ssh_terminal_capture(
        &self,
        incognito: bool,
        config: crate::terminal_logging::Config,
        name: String,
        target: &SshTarget,
    ) -> crate::terminal_logging::Prepared {
        // AI still needs an in-memory credential mask in a private terminal.
        // Never let that mask enable persistent logging for an incognito pane.
        crate::terminal_logging::Prepared::new(
            if incognito {
                Default::default()
            } else {
                config
            },
            self.clipboard.policy.terminal_logging.clone(),
            name,
            target.login.host.clone(),
            target.login.username.clone(),
        )
    }

    fn prepare_ssh_dial(&mut self, window: window::Id) -> Task<WorkspaceMessage> {
        if !self.allow_ssh_transport(window, "SSH") {
            return self.fail_pending_reopen_ssh(window);
        }
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            return cancel;
        }
        let project = self.project_remote_target(window).cloned();
        if let Some(error) = self.project_remote_denial(window) {
            let cancel = self.cancel_ssh_login(window);
            self.notice(window, error);
            return cancel;
        }
        let blocked = self
            .pending_ssh(window)
            .and_then(|pending| match &pending.stage {
                Stage::Form(form) => self.blocked_ssh_form(form),
                _ => None,
            });
        if let Some(notice) = blocked {
            self.defer_project_ssh_dial(window, super::project_access::WaitReason::Policy);
            self.notice(window, notice);
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
            teamwork,
            control,
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
        let minimum = control
            .as_ref()
            .map_or(minimum, |launch| minimum.max(launch.floor));
        let minimum = if let Some(shared) = teamwork {
            if !shared.route.matches_endpoints(
                &request.target.endpoint,
                request.jump.as_ref().map(|login| &login.endpoint),
            ) {
                *retry = None;
                let route = shared.route.clone();
                return self.reopen_ssh_form(
                    window,
                    route,
                    request.profile,
                    "Review the unchanged Teamwork route before connecting.".into(),
                );
            }
            minimum.max(shared.route.isolation)
        } else {
            minimum
        };
        let shared_name = teamwork
            .as_ref()
            .map(|shared| shared.selection.connection.name.clone());
        let shared_keepalive = teamwork
            .as_ref()
            .and_then(|shared| shared.selection.connection.startup.keepalive());
        *retry = Some(RetryLogin::new(
            &request,
            chosen_vault.clone(),
            log_config.clone(),
        ));
        let saved = if teamwork.is_some() { None } else { *saved };
        let (sftp, replace, incognito) = (*sftp, *replace, *incognito);
        let mut target = SshTarget {
            login: request.target.endpoint.clone(),
            jump: request.jump.as_ref().map(|login| login.endpoint.clone()),
            saved: None,
            isolation: request.isolation.max(minimum),
        };
        if let Some(project) = project.as_ref()
            && (!project.matches_endpoints(&target.login, target.jump.as_ref())
                || target.isolation != project.isolation.max(minimum))
        {
            let cancel = self.cancel_ssh_login(window);
            self.notice(
                window,
                "The reviewed SFTP route changed. Review the project tab again.".into(),
            );
            return cancel;
        }
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
        let control_saved = self
            .windows
            .get(&window)
            .and_then(|window| window.pending_ssh.as_ref())
            .and_then(|pending| pending.control.as_ref())
            .and_then(|launch| launch.saved.as_ref());
        let current_saved = if let Some(control_saved) = control_saved {
            let Some(current) = control_saved.current() else {
                return self
                    .cancel_stale_routed_ssh_login(window)
                    .unwrap_or_else(Task::none);
            };
            Some(current)
        } else {
            saved.and_then(|id| self.saved_connection_with_group(id))
        };
        let (saved_connection, connection_group) = current_saved
            .map_or((None, None), |(connection, group)| {
                (Some(connection), group)
            });
        let current = guarded_saved.unwrap_or(saved_connection);
        if let Some(refresh) = self.apply_reopen_ssh_config(window, current.as_ref()) {
            return refresh;
        }
        let current = current.filter(|connection| target.matches(connection));
        let keepalive = current
            .as_ref()
            .and_then(|connection| connection.startup.keepalive())
            .or(shared_keepalive)
            .unwrap_or_else(|| self.window_settings.ssh_keepalive());
        request.options = request.options.with_keepalive(Some(keepalive));
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
        let continuation = password_command::Continuation {
            request,
            target,
            current,
            sftp,
            incognito,
            log_config,
            connection_group,
        };
        let mut continuation = match self.stage_password_commands(window, continuation) {
            Ok(continuation) => continuation,
            Err(task) => return task,
        };
        let request = &mut continuation.request;
        let environment_credential = match &request.target.auth {
            ssh_dialog::Authentication::VaultPassword(id) => Some(id.clone()),
            _ => None,
        };
        let key_snapshots = match self.resolve_ssh_logins(request, continuation.current.as_ref()) {
            Ok(keys) => keys,
            Err(error) => {
                use super::project_access::WaitReason;
                if error == crate::vault_window::VAULT_LOCKED
                    && let Some(unlock) = self.unlock_for_control_ssh(window)
                {
                    return unlock;
                }
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
        self.launch_prepared_ssh(
            window,
            continuation,
            key_snapshots,
            environment_credential,
            shared_name,
            None,
        )
    }

    fn launch_prepared_ssh(
        &mut self,
        window: window::Id,
        continuation: password_command::Continuation,
        key_snapshots: KeySnapshots,
        environment_credential: Option<String>,
        shared_name: Option<String>,
        trust: Option<TrustStore>,
    ) -> Task<WorkspaceMessage> {
        let password_command::Continuation {
            request,
            target,
            current,
            sftp,
            incognito,
            log_config,
            connection_group,
        } = continuation;
        let launch_name = current
            .as_ref()
            .map(|connection| connection.name.clone())
            .or(shared_name)
            .unwrap_or_else(|| target.login.host.clone());
        let logging = self
            .ssh_needs_terminal_secrets(incognito, sftp, log_config.enabled())
            .then(|| {
                let capture =
                    self.ssh_terminal_capture(incognito, log_config, launch_name.clone(), &target);
                let secrets = capture.secrets();
                for login in std::iter::once(&request.target).chain(request.jump.as_ref()) {
                    use ssh_dialog::Authentication;
                    match &login.auth {
                        Authentication::Password(value)
                        | Authentication::KeyWithPassphrase(_, value)
                        | Authentication::ResolvedKey(_, Some(value)) => {
                            secrets.add(value.as_str())
                        }
                        _ => {}
                    }
                }
                Box::new(capture)
            });
        let profile = request.profile;
        let overrides = current
            .as_ref()
            .map(|connection| &connection.terminal)
            .or_else(|| {
                let teamwork = self.pending_ssh(window)?.teamwork.as_ref()?;
                Some(&teamwork.selection.connection().terminal)
            });
        let project_pane = self.project_sized_ssh_pane(window);
        let grid = if let Some(pane) = project_pane.filter(|pane| pane.project_initial_ssh_pending)
        {
            self.configured_initial_grid(profile, overrides, pane.project_initial_size)
        } else {
            self.live_ssh_grid(window)
                .map(|(grid, _)| grid)
                .unwrap_or_else(|| {
                    self.configured_initial_grid(profile, overrides, InitialTerminalSize::default())
                })
        };
        let size = ssh_size(grid);
        let id = self.allocate();
        let worker_label = if incognito {
            "Incognito SSH".into()
        } else {
            launch_name
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
                || (pending.replace.is_some() && pending.control.is_none())
                || pending.reconnect.is_some()
                || pending.reopen.is_some()
            {
                None
            } else {
                current
                    .as_ref()
                    .and_then(|connection| connection.terminal_effect)
                    .or_else(|| {
                        pending
                            .teamwork
                            .as_ref()?
                            .selection
                            .connection
                            .terminal_effect
                    })
            };
            let command_timestamps = current
                .as_ref()
                .and_then(|connection| connection.command_timestamps)
                .or_else(|| {
                    pending
                        .teamwork
                        .as_ref()?
                        .selection
                        .connection
                        .command_timestamps
                });
            pending.stage = Stage::Dialing {
                id,
                target: target.clone(),
                profile,
                grid,
                environment_credential,
                logging,
                terminal_effect,
                command_timestamps,
                journal_config: current
                    .as_ref()
                    .map(|connection| connection.terminal.journal)
                    .or_else(|| {
                        pending
                            .teamwork
                            .as_ref()
                            .map(|teamwork| teamwork.selection.connection().terminal.journal)
                    }),
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
                .spawn(move || publish_result(dial(request, sftp, &prompt, size, trust)))
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
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            ticket.cancel();
            return cancel;
        }
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
            reconnect,
            control,
            teamwork,
            stage:
                Stage::Dialing {
                    id: active,
                    target,
                    profile,
                    grid,
                    environment_credential,
                    logging,
                    terminal_effect,
                    command_timestamps,
                    journal_config,
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
        let dial_grid = *grid;
        let control = control.is_some();
        let teamwork_terminal = teamwork.as_ref().map(|teamwork| {
            let options = &teamwork.selection.connection().terminal;
            TeamworkTerminalInput {
                strict: options.strict_terminal_mode == Some(true),
                paste_line_delay_ms: options.paste_line_delay_ms,
            }
        });
        let saved_journal = *journal_config;
        let split = *split;
        let reopen = reopen.as_ref().map(|reopen| reopen.token);
        let reconnect_source = reconnect.as_ref().map(|reconnect| reconnect.target.source);
        let environment_credential = environment_credential.clone();
        let logging = logging.take();
        let terminal_effect = *terminal_effect;
        let command_timestamps = *command_timestamps;
        let connection_group = connection_group.clone();
        let duplicate_tab = *duplicate_tab;
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            return cancel;
        }
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
            Some(Ok(ssh)) if sftp && self.project_remote_target(window).is_some() => {
                return self.open_project_remote_tool(window, target, ssh);
            }
            Some(Ok(ssh)) if sftp => {
                let recent = if incognito {
                    Task::none()
                } else {
                    self.record_connection_used(target.clone())
                };
                return Task::batch([
                    self.open_sftp_connection(window, target, ssh, incognito),
                    recent,
                ]);
            }
            Some(Ok(ssh)) => {
                let slot = if let Some(old) = replace {
                    let located = if control {
                        self.windows
                            .get(&window)
                            .and_then(|window| window.pane_location(old))
                    } else {
                        self.reconnectable_ssh(window, old)
                            .map(|(tab, pane, _)| (tab, pane))
                    };
                    let Some((tab, pane)) = located else {
                        return Task::none();
                    };
                    Some((tab, pane, old))
                } else {
                    None
                };
                let project_source = slot.map(|(_, _, old)| old).or(reconnect_source);
                let teamwork_terminal = teamwork_terminal.or_else(|| {
                    let source = project_source.or_else(|| {
                        split
                            .filter(|split| split.same_server)
                            .map(|split| split.source)
                    })?;
                    let pane = self.sessions.get(&source)?;
                    if pane.ssh.as_ref() != Some(&target) {
                        return None;
                    }
                    pane.teamwork_terminal_input
                });
                let project_terminal_appearance = project_source
                    .and_then(|old| self.sessions.get(&old))
                    .and_then(|pane| pane.project_terminal_appearance.clone());
                let project_effect = project_terminal_appearance
                    .as_ref()
                    .and_then(|_| project_source.and_then(|old| self.pane_effect(old)));
                let saved = target.saved;
                let terminal_effect = terminal_effect
                    .filter(|selection| self.allowed_terminal_effect(selection.effect));
                let (grid, preserve_strict) =
                    self.live_ssh_grid(window).unwrap_or((dial_grid, false));
                let forwarding = ssh
                    .tunnel_openers()
                    .map(|(direct, remote)| super::tunnel_access::Forwarding::new(direct, remote));
                let overrides = self.saved_terminal_preferences.terminal(&target);
                let saved_strict = overrides
                    .and_then(|settings| settings.strict_terminal_mode)
                    .or_else(|| {
                        teamwork_terminal.and_then(|settings| settings.strict.then_some(true))
                    });
                let strict =
                    saved_strict.unwrap_or(preserve_strict || ssh.sandbox_backend().is_some());
                let journal_config = saved_journal
                    .or_else(|| {
                        replace
                            .and_then(|id| self.sessions.get(&id))
                            .filter(|pane| {
                                target.saved_id().is_none() && pane.ssh.as_ref() == Some(&target)
                            })
                            .map(|pane| pane.journal_config)
                    })
                    .unwrap_or_default();
                let appearance = project_terminal_appearance.as_deref().map_or_else(
                    || self.terminal_base_appearance(overrides),
                    |project| self.project_base_appearance(project),
                );
                let appearance = terminal_effect
                    .or(project_effect.map(|active| active.selection))
                    .map_or(appearance, |selection| {
                        selection.effect.appearance(appearance)
                    });
                let session = match Session::ssh_with_logging(
                    Some(ssh),
                    grid,
                    profile,
                    self.window_settings.prompt_navigation_enabled,
                    appearance,
                    logging,
                    crate::session::StreamOptions {
                        strict_escapes: Some(strict),
                        encoding: overrides.and_then(|settings| settings.encoding),
                        scrollback: self.terminal_scrollback(overrides),
                    },
                ) {
                    Ok(session) => session,
                    Err(error) => {
                        return self.reopen_ssh_form(window, target, profile, error.to_string());
                    }
                };
                let project_initial_size = slot
                    .and_then(|(_, _, old)| self.sessions.get(&old))
                    .map_or_else(InitialTerminalSize::default, |pane| {
                        pane.project_initial_size
                    });
                let session_id =
                    self.store_connected_session(session, grid, slot.map(|(_, _, old)| old), font);
                let session_id = if let Some((_, _, anchor)) = slot.filter(|_| control) {
                    self.replace_control_ssh_session(anchor, session_id);
                    anchor
                } else {
                    session_id
                };
                let recent_target = (!incognito
                    && reopen.is_none()
                    && (replace.is_none() || control)
                    && split.is_none_or(|target| !target.same_server))
                .then(|| target.clone());
                if let Some(pane) = self.sessions.get_mut(&session_id) {
                    pane.ssh = Some(target);
                    pane.project_initial_size = project_initial_size;
                    pane.project_terminal_appearance = project_terminal_appearance;
                    pane.journal_config = journal_config;
                    pane.forwarding = forwarding;
                    pane.strict_escapes = strict;
                    pane.saved_strict_escapes = saved_strict.is_some();
                    pane.teamwork_terminal_input = teamwork_terminal;
                    pane.credential_environment_source = environment_credential;
                    if let Some(selection) = terminal_effect {
                        pane.effect = Some(super::effects::Active::from_saved(selection));
                    } else if project_effect.is_some() {
                        pane.effect = project_effect;
                    }
                }
                self.sync_pane_appearance(session_id);
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
                    let origin = (control && !incognito)
                        .then(|| {
                            self.sessions
                                .get(&session_id)
                                .and_then(super::recently_closed::Origin::capture)
                        })
                        .flatten();
                    if let Some(tab) = self
                        .windows
                        .get_mut(&window)
                        .and_then(|window| window.tabs.get_mut(tab))
                        .and_then(super::TabEntry::terminal_mut)
                        && let Some(cell) = tab.grid.get_mut(pane)
                    {
                        *cell = session_id;
                        let remap = old != session_id && tab.tunnel_owner == Some(old);
                        if remap {
                            tab.tunnel_owner = Some(session_id);
                        }
                        if control {
                            tab.reopen_origin = origin;
                        }
                        if remap {
                            self.remap_tunnel_predecessors(old, session_id);
                        }
                        if control {
                            self.session_restore.changed(false);
                        }
                        if old != session_id {
                            self.rebind_coding_agent_alias(old, session_id);
                            self.stop_session(old);
                        }
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
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            return cancel;
        }
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
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            return cancel;
        }
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
        if let Some(cancel) = self.cancel_stale_routed_ssh_login(window) {
            return cancel;
        }
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
        let control_floor = pending
            .control
            .as_ref()
            .map(|launch| launch.floor.max(minimum));
        let shared_floor = pending.teamwork.as_ref().map(|shared| {
            let isolation = target.isolation;
            target = shared.route.clone();
            let floor = target.isolation.max(minimum);
            target.isolation = isolation.max(floor);
            floor
        });
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
        } else if let Some(floor) = shared_floor {
            target
                .form(profile)
                .with_isolation(target.isolation, floor)
                .with_fixed_route()
        } else if same_server || self.project_remote_target(window).is_some() {
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
        if let Some(floor) = control_floor {
            form = form.with_isolation(floor, floor).with_fixed_route();
        }
        if let Some(pending) = self.pending_ssh_mut(window) {
            if let Some(retry) = &pending.retry {
                if let Some(shared) = &pending.teamwork {
                    shared.auth.enable_retry(&mut form);
                }
                retry.restore(&mut form, &target.login);
            } else if let Some(shared) = &pending.teamwork {
                shared.auth.apply_to(&mut form, &target.login);
            }
        }
        let form_key = if let Some(token) = reopen {
            Some(FormKey::Reopen((token, self.allocate())))
        } else if reconnect {
            Some(FormKey::Reconnect(self.allocate()))
        } else if shared_floor.is_some() {
            Some(FormKey::Teamwork(self.allocate()))
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
            if let Some(shared) = &mut pending.teamwork
                && let Some(FormKey::Teamwork(revision)) = form_key
            {
                shared.form_revision = revision;
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
            && !previous.control_ssh_provisional
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
        initial_size: InitialTerminalSize,
    ) -> io::Result<SessionId> {
        let grid = self.configured_initial_grid(
            profile,
            self.saved_terminal_preferences.terminal(&target),
            initial_size,
        );
        let id = self.store_disconnected_ssh_in_grid(target, profile, font, grid)?;
        let pane = self.sessions.get_mut(&id).expect("restored SSH pane");
        pane.project_initial_size = initial_size;
        pane.project_initial_ssh_pending = !initial_size.is_default();
        Ok(id)
    }

    fn store_disconnected_ssh_in_grid(
        &mut self,
        target: SshTarget,
        profile: TerminalProfile,
        font: u8,
        grid: GridSize,
    ) -> io::Result<SessionId> {
        let session = Session::ssh(
            None,
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.terminal_base_appearance(self.saved_terminal_preferences.terminal(&target)),
        )?;
        session.set_scrollback_config(
            self.terminal_scrollback(self.saved_terminal_preferences.terminal(&target)),
        );
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
            Stage::PasswordCommand(pending) => pending.view(id, cancel),
            Stage::Form(form) => {
                let form = form
                    .view_for(
                        pending.sftp,
                        !pending.incognito && self.clipboard.policy.problem.is_none(),
                        &self.ssh_key_choices,
                    )
                    .map(move |message| wrap(form_message(id, form_key, message)));
                if let Some(shared) = &pending.teamwork {
                    let privacy = if pending.incognito {
                        " · Incognito"
                    } else {
                        ""
                    };
                    column![
                        text(format!(
                            "Teamwork · {}{privacy}",
                            shared.selection.connection.name
                        ))
                        .size(14),
                        form
                    ]
                    .spacing(5)
                    .into()
                } else if pending.incognito {
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
    trust: Option<TrustStore>,
) -> Result<Connected, ConnectError> {
    if request.isolation >= IsolationLevel::Process {
        let mut prompts = |round: &kortty_ssh::KeyboardChallenge| prompt.request(round);
        let result = match trust {
            Some(trust) => ssh_worker::Session::dial_with_trust(
                request,
                size,
                sftp,
                &prompt.cancellation,
                &mut prompts,
                &trust,
            ),
            None => {
                ssh_worker::Session::dial(request, size, sftp, &prompt.cancellation, &mut prompts)
            }
        };
        return result.map(|session| Connected::Worker(Box::new(session)));
    }
    dial_direct(
        request,
        sftp,
        prompt,
        trust.map_or_else(trust_store, Ok)?,
        size,
    )
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
        Input::TemporaryKey(key) => {
            let mut bytes = key.copy_for_attempt()?;
            Some(Authentication::private_key(std::mem::take(&mut *bytes)))
        }
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
    Teamwork(u64),
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
        Some(FormKey::Teamwork(revision)) => Message::TeamworkForm(owner, revision, message),
        None => Message::Form(owner, message),
    }
}

fn wrap(message: Message) -> WorkspaceMessage {
    WorkspaceMessage::Ssh(message)
}
