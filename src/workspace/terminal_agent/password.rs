//! One-use review and authorization for an attached SSH sudo password.

use iced::futures::{StreamExt as _, stream};
use kortty_ssh::SensitiveLine;

use crate::ssh_connection::ExecOpener;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_logging::RedactionSecrets;
use crate::vault_window::AgentSudoCredential;
use crate::workspace::SshTarget;

use super::{
    Desktop, IsolationLevel, Mode, Phase, Purpose, Run, RunTarget, SessionId, Sudo, Target, Task,
    Workspace, engine, mask, window,
};

#[cfg(target_os = "windows")]
mod manual;

#[derive(Clone, Copy)]
pub enum Source {
    Vault,
    Manual,
}

enum Authorization {
    Vault(AgentSudoCredential),
    Manual,
}

impl Authorization {
    fn active(&self) -> bool {
        match self {
            Self::Vault(credential) => credential.active(),
            Self::Manual => true,
        }
    }
}

#[derive(Clone)]
pub enum Message {
    Approve(u64, Source),
    Challenge(u64),
    Use(u64),
    #[cfg(target_os = "windows")]
    ManualReady(u64, bool),
    Finished(u64, Result<engine::Status, &'static str>),
}

pub(super) struct Review {
    command: engine::password::Command,
    scope: Scope,
    credential: Option<AgentSudoCredential>,
}

impl Review {
    pub(super) fn wire(&self) -> &str {
        self.command.wire()
    }

    pub(super) fn credential_label(&self) -> Option<&str> {
        self.credential
            .as_ref()
            .map(|credential| credential.label.as_str())
    }

    pub(super) fn destination(&self) -> &str {
        &self.scope.destination
    }
}

struct Scope {
    route: SshTarget,
    login: LoginTarget,
    opener: ExecOpener,
    directory: String,
    destination: String,
}

pub(super) struct Pending {
    scope: Scope,
    authorization: Authorization,
    answer: Option<async_channel::Sender<SensitiveLine>>,
    #[cfg(target_os = "windows")]
    manual: Option<manual::Prompt>,
}

impl Pending {
    pub(super) fn credential_label(&self) -> Option<&str> {
        match &self.authorization {
            Authorization::Vault(credential) => Some(&credential.label),
            Authorization::Manual => None,
        }
    }

    pub(super) fn destination(&self) -> &str {
        &self.scope.destination
    }

    pub(super) fn manual_open(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.manual.is_some()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }
}

pub(super) const MANUAL_AVAILABLE: bool = cfg!(target_os = "windows");

impl Run {
    pub(super) fn reviewed_password_sudo(&self) -> Option<&Review> {
        match &self.commands.front()?.sudo {
            Some(Sudo::Password(review)) => Some(review),
            _ => None,
        }
    }
}

impl Workspace {
    fn password_target<'a>(
        &'a self,
        run: &'a Run,
    ) -> Result<(ExecOpener, &'a SshTarget, &'a str), String> {
        if run.mode != Mode::Run || run.swarm || !matches!(run.purpose, Purpose::Agent(_)) {
            return Err("Password sudo requires a standalone attached SSH agent run.".into());
        }
        let Some(RunTarget::Attached(Target::Ssh {
            opener: original,
            directory,
        })) = &run.target
        else {
            return Err("Password sudo requires an attached SSH connection.".into());
        };
        let (owner, pane) = self.current_run_pane(run)?;
        let route = pane.ssh.as_ref().ok_or("The SSH account is unavailable.")?;
        let Some(current) = pane.session.exec_opener() else {
            return Err("The authenticated SSH command channel is unavailable.".into());
        };
        let minimum = self.clipboard.policy.minimum_isolation;
        if route.isolation < minimum
            || (route.isolation == IsolationLevel::Sandbox
                && pane.session.sandbox_backend().is_none())
            || (matches!(current, ExecOpener::Direct(_))
                != (route.isolation == IsolationLevel::None))
            || route.blocked_server(self).is_some()
            || current.is_closed()
            || current.verified_host_key().is_none()
            || !original.same_connection(&current)
            || self.agent_remote_path(owner, run.binding.live()?.session)? != *directory
        {
            return Err("The reviewed SSH route or directory is no longer available.".into());
        }
        Ok((current, route, directory))
    }

    pub(super) fn password_sudo_available(&self, run: &Run) -> bool {
        self.password_target(run).is_ok_and(|(_, route, _)| {
            MANUAL_AVAILABLE
                || self
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.agent_sudo_credential(&login(route)).is_ok())
        })
    }

    pub(super) fn prepare_password_sudo(&self, run: &Run, command: &str) -> Result<Review, String> {
        let (opener, route, directory) = self.password_target(run)?;
        let login = login(route);
        let destination = format!(
            "{} · {}",
            mask::prompt(&run.redactor, &login.label())?,
            opener
                .verified_host_key()
                .ok_or("The SSH host key is unavailable.")?
                .fingerprint(),
        );
        let credential = self
            .vault
            .as_ref()
            .ok_or("Unlock the vault to use password sudo.")
            .and_then(|vault| vault.agent_sudo_credential(&login));
        let credential = match credential {
            Ok(mut credential) => {
                credential.label = mask::prompt(&run.redactor, &credential.label)?;
                Some(credential)
            }
            Err(_) if MANUAL_AVAILABLE => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Review {
            command: engine::password::Command::new(directory, command)?,
            scope: Scope {
                route: route.clone(),
                login,
                opener,
                directory: directory.to_owned(),
                destination,
            },
            credential,
        })
    }

    fn check_password_sudo(
        &self,
        run: &Run,
        scope: &Scope,
        authorization: &Authorization,
    ) -> Result<(), String> {
        self.agent_dispatch_policy(run)?.check()?;
        let (opener, route, directory) = self.password_target(run)?;
        if route != &scope.route
            || directory != scope.directory
            || !opener.same_connection(&scope.opener)
        {
            return Err("The sudo approval no longer matches the current SSH connection.".into());
        }
        self.agent_transport(run)?;
        match authorization {
            Authorization::Vault(credential) => self
                .vault
                .as_ref()
                .ok_or("The vault is no longer available.")?
                .check_agent_sudo_credential(&scope.login, credential)?,
            Authorization::Manual if !MANUAL_AVAILABLE => {
                return Err("Native password entry is unavailable on this platform.".into());
            }
            Authorization::Manual => {}
        }
        Ok(())
    }

    pub(super) fn revalidate_password_sudo(
        &self,
        run: &Run,
        pending: &Pending,
    ) -> Result<(), String> {
        self.check_password_sudo(run, &pending.scope, &pending.authorization)
    }

    fn launch_password_sudo(
        &self,
        id: window::Id,
        run: &mut Run,
        source: Source,
    ) -> Result<Task<Desktop>, String> {
        let Some(super::ReviewedCommand {
            proposal,
            sudo: Some(Sudo::Password(review)),
        }) = run.commands.pop_front()
        else {
            return Err("The reviewed sudo command is unavailable.".into());
        };
        let Review {
            command,
            scope,
            credential,
        } = *review;
        let authorization = match source {
            Source::Vault => Authorization::Vault(
                credential.ok_or("The reviewed vault credential is unavailable.")?,
            ),
            Source::Manual => Authorization::Manual,
        };
        self.check_password_sudo(run, &scope, &authorization)?;
        let credential = match &authorization {
            Authorization::Vault(credential) => Some(credential.clone()),
            Authorization::Manual => None,
        };
        let opener = scope.opener.clone();
        let policy = self.agent_dispatch_policy(run)?;
        let (answer, answers) = async_channel::bounded(1);
        run.password = Some(Pending {
            scope,
            authorization,
            answer: Some(answer),
            #[cfg(target_os = "windows")]
            manual: None,
        });
        run.record("Approved SSH sudo command. Command output is withheld.".into());
        let epoch = run.next(
            Phase::SudoChallenge,
            "Waiting for the reviewed SSH sudo challenge…",
        );
        run.final_evidence.start(epoch, &proposal);
        let cancel = run.cancel.clone();
        let events = stream::once(async move {
            let (send, receive) = async_channel::bounded(2);
            let worker = send.clone();
            if std::thread::Builder::new()
                .name("kortty-agent-sudo".into())
                .spawn(move || {
                    let result = policy
                        .check()
                        .map_err(|_| "Managed policy blocks the reviewed SSH sudo command.")
                        .and_then(|()| {
                            command.run(
                                opener,
                                cancel,
                                move || credential.as_ref().is_none_or(AgentSudoCredential::active),
                                answers,
                                || worker.try_send(Message::Challenge(epoch)).is_ok(),
                            )
                        });
                    let _ = worker.try_send(Message::Finished(epoch, result));
                })
                .is_err()
            {
                let _ = send.try_send(Message::Finished(
                    epoch,
                    Err("Could not start the owned SSH sudo worker."),
                ));
            }
            receive
        })
        .flatten();
        Ok(Task::run(events, move |message| {
            Desktop::TerminalAgent(id, super::Message::Password(message))
        }))
    }

    pub(super) fn password_sudo_action(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        match message {
            Message::Approve(epoch, source)
                if epoch == run.epoch
                    && run.phase == Phase::Approval
                    && !run.paused
                    && !run.cancel.requested()
                    && run.reviewed_password_sudo().is_some() =>
            {
                return self.launch_password_sudo(id, run, source);
            }
            Message::Challenge(epoch)
                if epoch == run.epoch
                    && run.phase == Phase::SudoChallenge
                    && !run.cancel.requested() =>
            {
                if let Some(pending) = &run.password {
                    if let Err(error) = self.revalidate_password_sudo(run, pending) {
                        run.stop(&error);
                    } else {
                        run.phase = Phase::SudoPassword;
                        run.notice =
                            "The reviewed SSH command is waiting for its sudo password.".into();
                    }
                }
            }
            Message::Use(epoch)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPassword
                    && !run.cancel.requested()
                    && !run.paused =>
            {
                #[cfg(target_os = "windows")]
                if run
                    .password
                    .as_ref()
                    .is_some_and(|pending| matches!(pending.authorization, Authorization::Manual))
                {
                    return match self.open_manual_sudo_prompt(id, run) {
                        Ok(task) => Ok(task),
                        Err(error) => {
                            run.stop(&error);
                            Ok(Task::none())
                        }
                    };
                }
                if let Err(error) = self.submit_sudo_line(run) {
                    run.stop(&error);
                } else {
                    run.phase = Phase::SudoRunning;
                    run.notice = "Running the approved SSH sudo command…".into();
                }
            }
            #[cfg(target_os = "windows")]
            Message::ManualReady(epoch, submitted)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPassword
                    && !run.cancel.requested()
                    && run.password.as_ref().is_some_and(Pending::manual_open) =>
            {
                if !submitted || run.paused {
                    run.stop("The native sudo password prompt was cancelled.");
                } else if let Err(error) = self.submit_manual_sudo_line(run) {
                    run.stop(&error);
                } else {
                    run.phase = Phase::SudoRunning;
                    run.notice = "Running the approved SSH sudo command…".into();
                }
            }
            Message::Finished(epoch, result)
                if epoch == run.epoch
                    && matches!(
                        run.phase,
                        Phase::SudoChallenge | Phase::SudoPassword | Phase::SudoRunning
                    ) =>
            {
                run.password = None;
                if run.cancel.requested() || result == Ok(engine::Status::Cancelled) {
                    run.next(
                        Phase::Cancelled,
                        "The owned SSH sudo operation was cancelled.",
                    );
                    return Ok(Task::none());
                }
                match result? {
                    engine::Status::Exited(code) => {
                        run.final_evidence.record_password(epoch, code);
                        run.record(format!(
                            "SSH sudo exit status {code}. Command output withheld."
                        ));
                    }
                    _ => run.record(
                        "The SSH sudo command ended without a confirmed exit status.".into(),
                    ),
                }
                if result != Ok(engine::Status::Exited(0)) {
                    return Err("The SSH sudo command did not complete successfully.".into());
                }
                if run.commands.is_empty() {
                    return self.agent_turn(id, run);
                }
                run.next(
                    Phase::Approval,
                    "Review the next exact command and its destination.",
                );
            }
            _ => {}
        }
        Ok(Task::none())
    }

    fn submit_sudo_line(&self, run: &mut Run) -> Result<(), String> {
        let pending = run
            .password
            .as_ref()
            .ok_or("The sudo password request expired.")?;
        self.revalidate_password_sudo(run, pending)?;
        let Authorization::Vault(credential) = &pending.authorization else {
            return Err("The sudo password source does not match the review.".into());
        };
        let (line, overlay) = self
            .vault
            .as_ref()
            .ok_or("The vault is no longer available.")?
            .agent_sudo_line_with_overlay(
                &pending.scope.login,
                credential,
                run.learned_sudo.as_ref(),
            )?;
        Self::send_sudo_line(run, line, overlay)
    }

    fn send_sudo_line(
        run: &mut Run,
        line: SensitiveLine,
        overlay: RedactionSecrets,
    ) -> Result<(), String> {
        let redactor = run
            .redactor
            .with_overlay(&overlay)
            .map_err(|_| "The reviewed sudo password could not be safely masked.")?;
        let pending = run
            .password
            .as_mut()
            .ok_or("The sudo password request expired.")?;
        let sender = pending
            .answer
            .take()
            .ok_or("The sudo password request was already used.")?;
        if sender.is_closed() {
            return Err("The owned SSH sudo input is closed.".into());
        }
        sender
            .try_send(line)
            .map_err(|_| "The owned SSH sudo input is closed.")?;
        run.redactor = redactor;
        run.learned_sudo = Some(overlay);
        Ok(())
    }

    pub(in crate::workspace) fn agent_password_notice_current(
        &self,
        id: window::Id,
        epoch: u64,
        session: SessionId,
        stream: u64,
    ) -> bool {
        self.terminal_agents.runs.get(&id).is_some_and(|run| {
            run.phase == Phase::SudoPassword
                && run.epoch == epoch
                && !run.closing
                && !run.cancel.requested()
                && run.binding.live().is_ok_and(|binding| {
                    binding.session == session && binding.stream == stream && !binding.private
                })
                && run.password.as_ref().is_some_and(|pending| {
                    !pending.manual_open()
                        && pending.authorization.active()
                        && pending
                            .answer
                            .as_ref()
                            .is_some_and(|answer| !answer.is_closed())
                        && self.revalidate_password_sudo(run, pending).is_ok()
                })
        })
    }
}

fn login(route: &SshTarget) -> LoginTarget {
    LoginTarget {
        host: route.host().into(),
        port: route.port(),
        username: route.username().into(),
    }
}
