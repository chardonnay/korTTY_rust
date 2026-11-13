use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use iced::widget::{button, column, row, text};
use iced::{Element, Task, window};
use kortty_ssh::{Endpoint, HostKey, TrustStore};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Connection;
use crate::ssh_connection::trust_store;
use crate::ssh_dialog::{Authentication, LoginTarget, Password, Request};
use crate::terminal_logging;
use crate::vault_window::CommandAuthority;

use super::{
    KeySnapshots, Message, PendingSsh, SavedSshProfile, SshTarget, Stage, Workspace,
    WorkspaceMessage, wrap,
};

const CHANGED: &str =
    "The vault, connection or reviewed SSH host keys changed. Review the sign-in again.";
const UNTRUSTED: &str = "Verify the target and jump host keys with another SSH sign-in method before using a password command.";

pub(super) struct Continuation {
    pub(super) request: Request,
    pub(super) target: SshTarget,
    pub(super) current: Option<Connection>,
    pub(super) sftp: bool,
    pub(super) incognito: bool,
    pub(super) log_config: terminal_logging::Config,
    pub(super) connection_group: Option<String>,
}

struct Command {
    endpoint: LoginTarget,
    jump: bool,
    authority: CommandAuthority,
}

struct Review {
    commands: Vec<Command>,
    endpoints: Vec<Endpoint>,
    pins: Vec<(Endpoint, HostKey)>,
    target: SshTarget,
    saved: Option<SavedSshProfile>,
    incognito: bool,
}

impl Review {
    fn current(&self) -> Result<(), String> {
        self.allowed(&ClipboardPolicy::load())?;
        if self
            .commands
            .iter()
            .any(|command| !command.authority.current())
            || self.saved.as_ref().is_some_and(|saved| !saved.is_current())
            || trust_store()
                .and_then(|store| store.selected_pins(&self.endpoints))
                .map_or(true, |pins| pins != self.pins)
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    fn allowed(&self, policy: &ClipboardPolicy) -> Result<(), String> {
        allowed(policy, &self.target, self.incognito)
    }

    fn run(&self, cancel: &AtomicBool) -> Result<Vec<(bool, Password)>, String> {
        let root = crate::profile::root()
            .map_err(|_| "Could not open the private password program work directory.")?
            .join("password-program-work");
        self.commands
            .iter()
            .map(|command| {
                let password = command.authority.run(&root, cancel, || self.current())?;
                Ok((command.jump, Password::from_secret(password)))
            })
            .collect()
    }
}

fn allowed(policy: &ClipboardPolicy, target: &SshTarget, incognito: bool) -> Result<(), String> {
    if policy.problem.is_some()
        || !target.workflow_route_allowed(policy)
        || (incognito && !policy.incognito_allowed)
    {
        return Err("Managed policy blocks this password command for the selected route.".into());
    }
    Ok(())
}

pub(super) struct Pending {
    pub(super) target: SshTarget,
    ticket: u64,
    review: Arc<Review>,
    request: Option<Continuation>,
    keys: Option<KeySnapshots>,
    cancel: Arc<AtomicBool>,
    running: bool,
}

impl Pending {
    pub(super) fn view(
        &self,
        owner: window::Id,
        cancel: WorkspaceMessage,
    ) -> Element<'_, WorkspaceMessage> {
        let mut details = column![text(self.target.project_route_label()).size(14)].spacing(10);
        for command in &self.review.commands {
            details = details.push(text(command.endpoint.label()).size(14)).push(
                text(command.authority.review())
                    .font(iced::Font::MONOSPACE)
                    .size(12),
            );
        }
        for (endpoint, pin) in &self.review.pins {
            details = details.push(text(format!("{endpoint} · {}", pin.fingerprint())).size(12));
        }
        column![
            text(if self.running {
                "Getting the SSH password…"
            } else {
                "Review SSH password program"
            })
            .size(20),
            text("The program runs on this computer with your account permissions, outside the SSH session's isolation boundary.").size(13),
            iced::widget::scrollable(details).height(iced::Length::FillPortion(1)),
            row![
                button("Cancel").on_press(cancel),
                button("Run and Connect").on_press_maybe(
                    (!self.running)
                        .then_some(wrap(Message::ApprovePasswordCommand(owner, self.ticket)))
                ),
            ]
            .spacing(8)
        ]
        .padding(16)
        .spacing(12)
        .into()
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

type ResultSlot = Result<Vec<(bool, Password)>, String>;

#[derive(Clone)]
pub struct Response(Arc<Mutex<Option<ResultSlot>>>);

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password command response")
    }
}

impl Workspace {
    pub(super) fn stage_password_commands(
        &mut self,
        owner: window::Id,
        continuation: Continuation,
    ) -> Result<Continuation, Task<WorkspaceMessage>> {
        let commands = match self.password_commands(&continuation.request) {
            Ok(commands) if commands.is_empty() => return Ok(continuation),
            Ok(commands) => commands,
            Err(problem) => {
                self.notice(owner, problem);
                return Err(Task::none());
            }
        };
        let review = match self.password_command_review(owner, &continuation, commands) {
            Ok(review) => Arc::new(review),
            Err(problem) => {
                self.notice(owner, problem);
                return Err(self.fail_pending_reopen_ssh(owner));
            }
        };
        let mut continuation = continuation;
        let keys = match self.resolve_ssh_logins_except_commands(
            &mut continuation.request,
            continuation.current.as_ref(),
            &review.commands,
        ) {
            Ok(keys) => keys,
            Err(problem) => {
                self.notice(owner, problem.into());
                return Err(Task::none());
            }
        };
        let ticket = self.allocate();
        if let Some(pending) = self.pending_ssh_mut(owner) {
            pending.stage = Stage::PasswordCommand(Box::new(Pending {
                target: continuation.target.clone(),
                ticket,
                review,
                request: Some(continuation),
                keys: Some(keys),
                cancel: Arc::new(AtomicBool::new(false)),
                running: false,
            }));
        }
        Err(Task::none())
    }

    fn password_commands(&self, request: &Request) -> Result<Vec<Command>, String> {
        let Some(vault) = self.vault.as_ref() else {
            return Ok(Vec::new());
        };
        let mut commands = Vec::new();
        for (jump, login) in [
            (false, Some(&request.target)),
            (true, request.jump.as_ref()),
        ] {
            if let Some(login) = login
                && let Authentication::VaultPassword(id) = &login.auth
                && let Some(authority) = vault.password_command(id, &login.endpoint)?
            {
                commands.push(Command {
                    endpoint: login.endpoint.clone(),
                    jump,
                    authority,
                });
            }
        }
        Ok(commands)
    }

    fn password_command_review(
        &self,
        owner: window::Id,
        continuation: &Continuation,
        commands: Vec<Command>,
    ) -> Result<Review, String> {
        let pending = self.pending_ssh(owner).ok_or(CHANGED)?;
        if pending.control.is_some()
            || pending.teamwork.is_some()
            || pending.reopen.is_some()
            || pending.reconnect.is_some()
            || pending.replace.is_some()
            || pending.split.is_some()
            || self.project_ssh_target(owner).is_some()
        {
            return Err("Password commands require a manual SSH or SFTP sign-in.".into());
        }
        allowed(
            &self.clipboard.policy,
            &continuation.target,
            continuation.incognito,
        )?;
        let endpoints = std::iter::once(&continuation.target.login)
            .chain(continuation.target.jump.iter())
            .map(|login| Endpoint::new(&login.host, login.port))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|_| CHANGED)?;
        let pins = trust_store()
            .and_then(|store| store.selected_pins(&endpoints))
            .map_err(|_| UNTRUSTED)?;
        if endpoints
            .iter()
            .any(|endpoint| !pins.iter().any(|(pin, _)| pin == endpoint))
        {
            return Err(UNTRUSTED.into());
        }
        let saved = continuation
            .current
            .as_ref()
            .map(|connection| {
                SavedSshProfile::from_profile(connection.id)
                    .ok()
                    .flatten()
                    .filter(|saved| saved.connection == *connection)
                    .ok_or(CHANGED)
            })
            .transpose()?;
        Ok(Review {
            commands,
            endpoints,
            pins,
            target: continuation.target.clone(),
            saved,
            incognito: continuation.incognito,
        })
    }

    pub(super) fn approve_password_command(
        &mut self,
        owner: window::Id,
        ticket: u64,
    ) -> Task<WorkspaceMessage> {
        if self.exiting || self.scheduler_draining || self.owner_dialog(owner).is_some() {
            return Task::none();
        }
        let Some(PendingSsh {
            stage: Stage::PasswordCommand(pending),
            ..
        }) = self.pending_ssh(owner)
        else {
            return Task::none();
        };
        if pending.ticket != ticket || pending.running || pending.cancel.load(Ordering::Acquire) {
            return Task::none();
        }
        if let Err(problem) = pending
            .review
            .allowed(&self.clipboard.policy)
            .and_then(|()| pending.review.current())
        {
            let task = self.cancel_ssh_login(owner);
            self.notice(owner, problem);
            return task;
        }
        let Some(PendingSsh {
            stage: Stage::PasswordCommand(pending),
            ..
        }) = self.pending_ssh_mut(owner)
        else {
            return Task::none();
        };
        pending.running = true;
        let review = Arc::clone(&pending.review);
        let cancel = Arc::clone(&pending.cancel);
        let (sender, receiver) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded::<()>(1);
        self.track_session_shutdown(done);
        let worker = thread::Builder::new()
            .name("kortty-ssh-password-command".into())
            .spawn(move || {
                let _ = sender.try_send(review.run(&cancel));
                drop(finished);
            });
        Task::future(async move {
            let result = match worker {
                Ok(_) => receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The password program stopped.".into())),
                Err(_) => Err("Could not start the password program.".into()),
            };
            wrap(Message::PasswordCommandFinished(
                owner,
                ticket,
                Response(Arc::new(Mutex::new(Some(result)))),
            ))
        })
    }

    pub(super) fn password_command_finished(
        &mut self,
        owner: window::Id,
        ticket: u64,
        response: Response,
    ) -> Task<WorkspaceMessage> {
        let Some(PendingSsh {
            stage: Stage::PasswordCommand(pending),
            ..
        }) = self.pending_ssh(owner)
        else {
            return Task::none();
        };
        if pending.ticket != ticket || !pending.running || pending.cancel.load(Ordering::Acquire) {
            return Task::none();
        }
        let result = response
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some(result) = result else {
            return Task::none();
        };
        let result = result.and_then(|passwords| {
            pending.review.allowed(&self.clipboard.policy)?;
            pending.review.current()?;
            let trust = TrustStore::from_pins(pending.review.pins.clone()).map_err(|_| CHANGED)?;
            Ok((passwords, trust))
        });
        let (passwords, trust) = match result {
            Ok(result) => result,
            Err(problem) => {
                let target = pending.target.clone();
                let profile = pending
                    .request
                    .as_ref()
                    .expect("active request")
                    .request
                    .profile;
                return self.reopen_ssh_form(owner, target, profile, problem);
            }
        };
        let Some(PendingSsh {
            stage: Stage::PasswordCommand(pending),
            ..
        }) = self.pending_ssh_mut(owner)
        else {
            return Task::none();
        };
        let Some((mut continuation, keys)) = pending.request.take().zip(pending.keys.take()) else {
            return Task::none();
        };
        if !keys.is_current() {
            return self.reopen_ssh_form(
                owner,
                continuation.target,
                continuation.request.profile,
                CHANGED.into(),
            );
        }
        for (jump, password) in passwords {
            let login = if jump {
                continuation
                    .request
                    .jump
                    .as_mut()
                    .expect("reviewed jump login")
            } else {
                &mut continuation.request.target
            };
            login.auth = Authentication::Password(password);
        }
        self.launch_prepared_ssh(owner, continuation, keys, None, None, Some(trust))
    }

    fn resolve_ssh_logins_except_commands(
        &self,
        request: &mut Request,
        saved: Option<&Connection>,
        commands: &[Command],
    ) -> Result<KeySnapshots, &'static str> {
        let mut skipped = [false; 2];
        for command in commands {
            skipped[usize::from(command.jump)] = true;
        }
        self.resolve_ssh_logins_filtered(request, saved, skipped)
    }

    pub(in crate::workspace) fn cancel_revoked_password_commands(
        &mut self,
    ) -> Task<WorkspaceMessage> {
        let owners: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&id, window)| {
                let Stage::PasswordCommand(pending) = &window.pending_ssh.as_ref()?.stage else {
                    return None;
                };
                pending
                    .review
                    .commands
                    .iter()
                    .any(|command| !command.authority.current())
                    .then_some(id)
            })
            .chain(self.sftp_logins.iter().filter_map(|(&id, login)| {
                let Stage::PasswordCommand(pending) = &login.stage else {
                    return None;
                };
                pending
                    .review
                    .commands
                    .iter()
                    .any(|command| !command.authority.current())
                    .then_some(id)
            }))
            .collect();
        Task::batch(owners.into_iter().map(|id| {
            let cancel = self.cancel_ssh_login(id);
            self.notice(
                id,
                "The vault changed. Review the SSH password command again.".into(),
            );
            cancel
        }))
    }
}

#[cfg(test)]
#[test]
fn ssh_password_command_preserves_session_isolation_and_sftp_transfer_policy() {
    use crate::session_security::IsolationLevel;

    let login = LoginTarget {
        host: "example.test".into(),
        port: 22,
        username: "operator".into(),
    };
    let mut policy = ClipboardPolicy {
        file_transfer_allowed: false,
        minimum_isolation: IsolationLevel::Process,
        ..ClipboardPolicy::default()
    };
    let target =
        SshTarget::from_imported_route(login.clone(), None, IsolationLevel::Process).unwrap();
    assert!(allowed(&policy, &target, false).is_ok());
    let direct = SshTarget::from_imported_route(login, None, IsolationLevel::None).unwrap();
    assert!(allowed(&policy, &direct, false).is_err());
    policy.incognito_allowed = false;
    assert!(allowed(&policy, &target, true).is_err());
}
