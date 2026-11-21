//! Command review and run-scoped sudo authentication for an attached terminal.

use std::sync::{Arc, Mutex};

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
use engine::password::Endpoint;

mod manual;

const MAX_PREFLIGHT_RETRIES: u8 = 3;

#[derive(Clone, Copy)]
pub enum Source {
    Vault,
    Manual,
    Ticket,
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
    AuthenticatePlan(u64, Source),
    SkipPlanAuthentication(u64),
    Challenge(u64),
    Use(u64),
    ManualReady(u64, bool),
    #[cfg(not(target_os = "windows"))]
    ManualEdited(u64),
    Finished(u64, Result<Completed, &'static str>),
}

#[derive(Clone)]
pub struct Completed {
    status: engine::Status,
    terminal: Arc<Mutex<Option<engine::password::Terminal>>>,
}

impl Completed {
    pub(super) fn new(
        status: engine::Status,
        terminal: Option<engine::password::Terminal>,
    ) -> Self {
        Self {
            status,
            terminal: Arc::new(Mutex::new(terminal)),
        }
    }

    fn take_terminal(&self) -> Option<engine::password::Terminal> {
        self.terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

pub(super) struct Review {
    command: engine::password::Command,
    authentication: AuthenticationReview,
}

pub(super) struct AuthenticationReview {
    scope: Scope,
    credential: Option<AgentSudoCredential>,
}

impl AuthenticationReview {
    pub(super) fn credential_label(&self) -> Option<&str> {
        self.credential
            .as_ref()
            .map(|credential| credential.label.as_str())
    }

    pub(super) fn destination(&self) -> &str {
        &self.scope.destination
    }
}

impl Review {
    pub(super) fn wire(&self) -> &str {
        self.command.wire()
    }

    pub(super) fn credential_label(&self) -> Option<&str> {
        self.authentication.credential_label()
    }

    pub(super) fn destination(&self) -> &str {
        self.authentication.destination()
    }
}

struct Scope {
    connection: Connection,
    destination: String,
}

struct Connection {
    endpoint: Endpoint,
    ssh: Option<Box<SshTarget>>,
    directory: String,
}

impl Connection {
    fn matches(&self, other: &Self) -> bool {
        self.ssh == other.ssh
            && self.directory == other.directory
            && self.endpoint.matches(&other.endpoint)
    }

    fn login(&self) -> Option<LoginTarget> {
        self.ssh.as_deref().map(login)
    }
}

pub(super) struct Authenticated {
    scope: Scope,
    terminal: engine::password::Terminal,
}

pub(super) struct Pending {
    scope: Scope,
    authorization: Option<Authorization>,
    preflight: bool,
    answer: Option<async_channel::Sender<SensitiveLine>>,
    manual: Option<manual::Prompt>,
}

impl Pending {
    pub(super) fn credential_label(&self) -> Option<&str> {
        match &self.authorization {
            Some(Authorization::Vault(credential)) => Some(&credential.label),
            _ => None,
        }
    }

    pub(super) fn destination(&self) -> &str {
        &self.scope.destination
    }

    pub(super) fn manual_open(&self) -> bool {
        self.manual.is_some()
    }

    fn running_notice(&self) -> &'static str {
        if self.preflight {
            "Validating sudo for this implementation run…"
        } else {
            "Running the approved sudo command…"
        }
    }
}

impl Run {
    pub(super) fn reviewed_password_sudo(&self) -> Option<&Review> {
        match &self.commands.front()?.sudo {
            Some(Sudo::Password(review)) => Some(review),
            _ => None,
        }
    }
}

impl Workspace {
    fn password_target(&self, run: &Run) -> Result<Connection, String> {
        if run.mode != Mode::Run || run.swarm || !matches!(run.purpose, Purpose::Agent(_)) {
            return Err("Password sudo requires a standalone attached agent run.".into());
        }
        #[cfg(unix)]
        if run.local_sudo_available
            && run.supports_native_local_sudo()
            && let Some(RunTarget::Attached(Target::Local { directory, .. })) = &run.target
        {
            self.current_run_pane(run)?;
            directory.require_current()?;
            return Ok(Connection {
                endpoint: Endpoint::Local(directory.clone()),
                ssh: None,
                directory: directory
                    .path()
                    .to_str()
                    .ok_or("The local directory is unavailable.")?
                    .into(),
            });
        }
        let Some(RunTarget::Attached(Target::Ssh {
            opener: original,
            directory,
        })) = &run.target
        else {
            return Err("Password sudo requires an attached SSH connection or a verified local sudo target.".into());
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
        Ok(Connection {
            endpoint: Endpoint::Ssh(current),
            ssh: Some(Box::new(route.clone())),
            directory: directory.clone(),
        })
    }

    pub(super) fn password_sudo_available(&self, run: &Run) -> bool {
        self.password_target(run).is_ok_and(|connection| {
            run.sudo_terminal
                .as_ref()
                .is_none_or(|authenticated| authenticated.scope.connection.matches(&connection))
        })
    }

    pub(super) fn prepare_password_sudo(&self, run: &Run, command: &str) -> Result<Review, String> {
        let authentication = self.prepare_sudo_authentication(run)?;
        Ok(Review {
            command: engine::password::Command::new(
                &authentication.scope.connection.directory,
                command,
                authentication.scope.connection.endpoint.is_local(),
            )?,
            authentication,
        })
    }

    fn prepare_sudo_authentication(&self, run: &Run) -> Result<AuthenticationReview, String> {
        let connection = self.password_target(run)?;
        let login = connection.login();
        let destination = match &connection.endpoint {
            Endpoint::Ssh(opener) => format!(
                "{} · {}",
                mask::prompt(
                    &run.redactor,
                    &login
                        .as_ref()
                        .ok_or("The SSH account is unavailable.")?
                        .label()
                )?,
                opener
                    .verified_host_key()
                    .ok_or("The SSH host key is unavailable.")?
                    .fingerprint(),
            ),
            #[cfg(unix)]
            Endpoint::Local(_) => format!(
                "Local shell · {}",
                mask::prompt(&run.redactor, &connection.directory)?
            ),
        };
        let credential = if let Some(authenticated) = &run.sudo_terminal {
            if !authenticated.scope.connection.matches(&connection) {
                return Err("The sudo terminal no longer matches the current target.".into());
            }
            None
        } else {
            login
                .as_ref()
                .zip(self.vault.as_ref())
                .and_then(|(login, vault)| vault.agent_sudo_credential(login).ok())
                .map(|mut credential| {
                    credential.label = mask::prompt(&run.redactor, &credential.label)?;
                    Ok::<_, String>(credential)
                })
                .transpose()?
        };
        Ok(AuthenticationReview {
            scope: Scope {
                connection,
                destination,
            },
            credential,
        })
    }

    pub(super) fn offer_plan_sudo_preflight(
        &self,
        run: &mut Run,
        outcome: &engine::Outcome,
    ) -> Result<bool, String> {
        let Some(RunTarget::Attached(target)) = &run.target else {
            return Ok(false);
        };
        let plan_probe = run.uses_plan_sudo_probe();
        #[cfg(unix)]
        if run.supports_native_local_sudo() && !plan_probe {
            run.local_sudo_available = local_probe_has_sudo(outcome, target.directory());
        }
        if !plan_probe {
            return Ok(false);
        }
        let snapshot = engine::planning_probe::Snapshot::capture(target, outcome, &|text| {
            mask::prompt(&run.redactor, text)
        })?;
        #[cfg(unix)]
        if run.supports_native_local_sudo() {
            run.local_sudo_available = snapshot.has_sudo_as_non_root();
        }
        if !snapshot.needs_password_sudo() {
            return Ok(false);
        }
        self.planning_probe_target(run)?;
        run.sudo_preflight = Some(self.prepare_sudo_authentication(run)?);
        run.next(
            Phase::SudoPreflight,
            "Review sudo authentication for this implementation run.",
        );
        Ok(true)
    }

    fn check_password_sudo(
        &self,
        run: &Run,
        scope: &Scope,
        authorization: Option<&Authorization>,
    ) -> Result<(), String> {
        self.agent_dispatch_policy(run)?.check()?;
        let connection = self.password_target(run)?;
        if !scope.connection.matches(&connection) {
            return Err("The sudo approval no longer matches the current target.".into());
        }
        self.agent_transport(run)?;
        if let Some(Authorization::Vault(credential)) = authorization {
            self.vault
                .as_ref()
                .ok_or("The vault is no longer available.")?
                .check_agent_sudo_credential(
                    &scope
                        .connection
                        .login()
                        .ok_or("The SSH sudo account is unavailable.")?,
                    credential,
                )?;
        }
        Ok(())
    }

    pub(super) fn revalidate_password_sudo(
        &self,
        run: &Run,
        pending: &Pending,
    ) -> Result<(), String> {
        self.check_password_sudo(run, &pending.scope, pending.authorization.as_ref())
    }

    pub(super) fn revalidate_sudo_terminal(
        &self,
        run: &Run,
        authenticated: &Authenticated,
    ) -> Result<(), String> {
        let connection = self.password_target(run)?;
        if !authenticated.scope.connection.matches(&connection) {
            return Err("The sudo terminal no longer matches the current target.".into());
        }
        Ok(())
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
            authentication,
        } = *review;
        if mask::prompt(&run.redactor, command.wire())? != command.wire() {
            return Err("The effective sudo command contains text that requires masking.".into());
        }
        self.launch_sudo_operation(id, run, authentication, source, Some((command, proposal)))
    }

    fn launch_sudo_operation(
        &self,
        id: window::Id,
        run: &mut Run,
        authentication: AuthenticationReview,
        source: Source,
        command: Option<(engine::password::Command, super::Command)>,
    ) -> Result<Task<Desktop>, String> {
        let AuthenticationReview { scope, credential } = authentication;
        let preflight = command.is_none();
        let target = (preflight || run.supports_native_local_sudo())
            .then(|| self.planning_probe_target(run).map(Arc::new))
            .transpose()?;
        let authorization = match (source, &run.sudo_terminal) {
            (Source::Vault, None) => Some(Authorization::Vault(
                credential.ok_or("The reviewed vault credential is unavailable.")?,
            )),
            (Source::Manual, None) => Some(Authorization::Manual),
            (Source::Ticket, Some(authenticated))
                if !preflight && authenticated.scope.connection.matches(&scope.connection) =>
            {
                None
            }
            _ => return Err("The sudo approval no longer matches the current run.".into()),
        };
        self.check_password_sudo(run, &scope, authorization.as_ref())?;
        let credential = match &authorization {
            Some(Authorization::Vault(credential)) => Some(credential.clone()),
            _ => None,
        };
        let endpoint = scope.connection.endpoint.clone();
        let policy = self.agent_dispatch_policy(run)?;
        let terminal = run
            .sudo_terminal
            .take()
            .map(|authenticated| authenticated.terminal);
        let reuse = terminal.is_some();
        let (answer, answers) = async_channel::bounded(1);
        run.password = Some(Pending {
            scope,
            authorization,
            preflight,
            answer: (!reuse).then_some(answer),
            manual: None,
        });
        run.record(if preflight {
            "Approved sudo authentication for the implementation run.".into()
        } else {
            "Approved sudo command. Command output is withheld.".into()
        });
        let epoch = if reuse {
            run.next(Phase::SudoRunning, "Running the approved sudo command…")
        } else {
            run.next(
                Phase::SudoChallenge,
                "Validating sudo in the run's private terminal…",
            )
        };
        let command = command.map(|(command, proposal)| {
            run.final_evidence.start(epoch, &proposal);
            command
        });
        let cancel = run.cancel.clone();
        let events = stream::once(async move {
            let (send, receive) = async_channel::bounded(2);
            let worker = send.clone();
            if std::thread::Builder::new()
                .name("kortty-agent-sudo".into())
                .spawn(move || {
                    let result = policy
                        .check()
                        .map_err(|_| "Managed policy blocks the reviewed sudo operation.")
                        .and_then(|()| {
                            let authorized = move || {
                                credential.as_ref().is_none_or(AgentSudoCredential::active)
                                    && target
                                        .as_ref()
                                        .is_none_or(|target| target.check_worker().is_ok())
                            };
                            let challenge = || worker.try_send(Message::Challenge(epoch)).is_ok();
                            if let Some(command) = command {
                                command.run_with_terminal(
                                    endpoint, terminal, cancel, authorized, answers, challenge,
                                )
                            } else {
                                engine::password::Terminal::authenticate(
                                    endpoint,
                                    move || !cancel.requested() && authorized(),
                                    answers,
                                    challenge,
                                )
                                .map(|terminal| {
                                    let status = if terminal.is_some() {
                                        engine::Status::Exited(0)
                                    } else {
                                        engine::Status::Cancelled
                                    };
                                    (status, terminal)
                                })
                            }
                        })
                        .map(|(status, terminal)| Completed::new(status, terminal));
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
            Message::AuthenticatePlan(epoch, source)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPreflight
                    && !run.paused
                    && !run.cancel.requested()
                    && matches!(source, Source::Manual | Source::Vault) =>
            {
                let review = run
                    .sudo_preflight
                    .take()
                    .ok_or("The sudo review expired.")?;
                run.sudo_preflight_attempts += 1;
                return self.launch_sudo_operation(id, run, review, source, None);
            }
            Message::SkipPlanAuthentication(epoch)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPreflight
                    && !run.paused
                    && !run.cancel.requested() =>
            {
                self.planning_probe_target(run)?;
                run.sudo_preflight
                    .take()
                    .ok_or("The sudo review expired.")?;
                return self.agent_turn(id, run);
            }
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
                if let Some(pending) = &run.password
                    && pending.authorization.is_some()
                {
                    if let Err(error) = self.revalidate_password_sudo(run, pending) {
                        run.stop(&error);
                    } else {
                        run.phase = Phase::SudoPassword;
                        run.notice =
                            "The run's private terminal is waiting for its sudo password.".into();
                    }
                }
            }
            Message::Use(epoch)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPassword
                    && !run.cancel.requested()
                    && !run.paused =>
            {
                if run.password.as_ref().is_some_and(|pending| {
                    matches!(pending.authorization, Some(Authorization::Manual))
                }) {
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
                    password_submitted(run);
                }
            }
            Message::ManualReady(epoch, submitted)
                if epoch == run.epoch
                    && run.phase == Phase::SudoPassword
                    && !run.cancel.requested()
                    && run.password.as_ref().is_some_and(Pending::manual_open) =>
            {
                if !submitted || run.paused {
                    run.stop("The sudo password prompt was cancelled.");
                } else if let Err(error) = self.submit_manual_sudo_line(run) {
                    run.stop(&error);
                } else {
                    password_submitted(run);
                }
            }
            #[cfg(not(target_os = "windows"))]
            Message::ManualEdited(epoch) if epoch == run.epoch => {}
            Message::Finished(epoch, result)
                if epoch == run.epoch
                    && matches!(
                        run.phase,
                        Phase::SudoChallenge | Phase::SudoPassword | Phase::SudoRunning
                    ) =>
            {
                let pending = run.password.take();
                if run.cancel.requested()
                    || result
                        .as_ref()
                        .is_ok_and(|result| result.status == engine::Status::Cancelled)
                {
                    run.next(Phase::Cancelled, "The owned sudo operation was cancelled.");
                    return Ok(Task::none());
                }
                let preflight = pending.as_ref().is_some_and(|pending| pending.preflight);
                if preflight
                    && matches!(&result, Err(error) if *error == engine::password::AUTHENTICATION_REJECTED)
                    && run.sudo_preflight_attempts <= MAX_PREFLIGHT_RETRIES
                {
                    self.planning_probe_target(run)?;
                    run.sudo_preflight = Some(self.prepare_sudo_authentication(run)?);
                    run.next(
                        Phase::SudoPreflight,
                        "Sudo authentication was rejected. Review the retry or continue to model review.",
                    );
                    return Ok(Task::none());
                }
                let result = result?;
                match (preflight, result.status) {
                    (true, _) => {}
                    (false, engine::Status::Exited(code)) => {
                        run.final_evidence.record_password(epoch, code);
                        run.record(format!("Sudo exit status {code}. Command output withheld."));
                    }
                    _ => {
                        run.record("The sudo command ended without a confirmed exit status.".into())
                    }
                }
                if result.status != engine::Status::Exited(0) {
                    let reused = pending
                        .as_ref()
                        .is_some_and(|pending| pending.authorization.is_none());
                    return Err(if reused {
                        "The sudo command failed. If the sudo ticket expired, start a new run."
                    } else {
                        "The sudo command did not complete successfully."
                    }
                    .into());
                }
                let scope = pending
                    .ok_or("The sudo operation no longer owns its target.")?
                    .scope;
                let terminal = result
                    .take_terminal()
                    .ok_or("The run's sudo terminal closed.")?;
                let authenticated = Authenticated { scope, terminal };
                self.revalidate_sudo_terminal(run, &authenticated)?;
                run.sudo_terminal = Some(authenticated);
                if preflight {
                    run.record("Sudo authentication completed for this run.".into());
                    return self.agent_turn(id, run);
                }
                for reviewed in &mut run.commands {
                    if mask::prompt(&run.redactor, reviewed.text())? != reviewed.text() {
                        return Err("A queued command contains text that requires masking.".into());
                    }
                    reviewed.proposal.purpose =
                        mask::prompt(&run.redactor, &reviewed.proposal.purpose)?;
                }
                if run.commands.is_empty() {
                    return self.agent_turn(id, run);
                }
                run.next(
                    Phase::Approval,
                    "Review the next exact command and its destination.",
                );
                if let Some(task) = self.continue_agent_commands(id, run)? {
                    return Ok(task);
                }
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
        let Some(Authorization::Vault(credential)) = &pending.authorization else {
            return Err("The sudo password source does not match the review.".into());
        };
        let (line, overlay) = self
            .vault
            .as_ref()
            .ok_or("The vault is no longer available.")?
            .agent_sudo_line_with_overlay(
                &pending
                    .scope
                    .connection
                    .login()
                    .ok_or("The SSH sudo account is unavailable.")?,
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
                        && pending
                            .authorization
                            .as_ref()
                            .is_some_and(Authorization::active)
                        && pending
                            .answer
                            .as_ref()
                            .is_some_and(|answer| !answer.is_closed())
                        && self.revalidate_password_sudo(run, pending).is_ok()
                })
        })
    }
}

fn password_submitted(run: &mut Run) {
    run.phase = Phase::SudoRunning;
    run.notice = run
        .password
        .as_ref()
        .expect("owned sudo operation")
        .running_notice()
        .into();
}

#[cfg(unix)]
fn local_probe_has_sudo(outcome: &engine::Outcome, directory: &str) -> bool {
    if outcome.status != engine::Status::Exited(0) || outcome.stdout.truncated() {
        return false;
    }
    let output = outcome.stdout.text();
    let mut fields = output.lines().skip(2);
    let non_root = fields
        .next()
        .and_then(|uid| uid.parse::<u32>().ok())
        .is_some_and(|uid| uid != 0);
    non_root
        && fields.next() == Some(directory)
        && matches!(fields.next(), Some("/usr/bin/sudo" | "/bin/sudo"))
        && fields.next().is_none()
}

fn login(route: &SshTarget) -> LoginTarget {
    LoginTarget {
        host: route.host().into(),
        port: route.port(),
        username: route.username().into(),
    }
}
