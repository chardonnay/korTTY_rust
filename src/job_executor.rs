use std::io;
#[cfg(test)]
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Receiver as CompletionReceiver;
use kortty_jobs::{Cancellation, JournalRecording, RunId, RunOutcome, RunRequest};
use kortty_ssh::{ConnectError, SshSession, TrustStore};

use crate::clipboard_policy::{ClipboardPolicy, ServerAccess};
use crate::scheduler_window::{SshCommand, SudoMode};
use crate::sftp_window::worker::{Connection as SftpConnection, Worker as SftpWorker};
use crate::ssh_connection;

pub(crate) mod agent;
pub(crate) mod automation;
mod batch;
mod connection;
mod remote;
pub(crate) mod rsync;
mod sudo;
pub(crate) mod virtual_terminal;

pub(crate) use connection::{BatchAuth, Credentials, JobAuth, SavedAuth};
pub(crate) use remote::sync::preview as preview_sftp_sync;
pub(crate) use remote::{BrowserArchiveManifest, scan_browser_archive};
pub(crate) type ArchivePassword = Result<Option<kortty_vault::Secret>, &'static str>;
type BorrowedArchivePassword<'a> = Result<Option<&'a str>, &'static str>;

pub(crate) fn swarm_completion_summary(
    targets: &[kortty_jobs::SwarmTargetReport],
    outcome: RunOutcome,
) -> String {
    let (mut success, mut failed, mut blocked, mut cancelled) = (0, 0, 0, 0);
    for target in targets {
        match target.outcome {
            RunOutcome::Success => success += 1,
            RunOutcome::Failed { .. } => failed += 1,
            RunOutcome::Blocked => blocked += 1,
            RunOutcome::Cancelled => cancelled += 1,
        }
    }
    let mut detail = format!(
        "{success} of {} targets succeeded · {failed} failed · {blocked} blocked · {cancelled} cancelled",
        targets.len(),
    );
    if outcome != RunOutcome::Cancelled {
        detail.push_str(" · Open the Swarm report for target details");
    }
    detail
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const CANCELLATION_POLL: Duration = Duration::from_millis(250);

pub(crate) fn open_directory_browser(
    credentials: Credentials,
    cancelled: impl Fn() -> bool,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<SftpWorker, &'static str> {
    let (target, jumps) = credentials.into_route().map_err(|failure| failure.detail)?;
    let trust = trust_store().map_err(|_| "Could not access SSH host-key trust store")?;
    let tcp =
        ssh_connection::connect_transport(&jumps.first().unwrap_or(&target).endpoint, &cancelled)
            .map_err(|error| connect_failure(error.into()).detail)?;
    let session = SshSession::connect_subsystem_preconnected_cancellable(
        tcp, target, jumps, trust, "sftp", &cancelled,
    )
    .map_err(|error| connect_failure(error).detail)?;
    if cancelled() {
        return Err("Remote directory selection cancelled");
    }
    SftpWorker::start(
        SftpConnection::Standalone(ssh_connection::Connected::Direct(Box::new(session))),
        crate::clipboard_policy::RemoteEditorPolicy::Deny,
    )
    .map_err(|_| "Could not start the SFTP directory browser")
}

/// The terminal result for one scheduler ticket. Details are synthesized locally.
#[derive(Clone, Debug)]
pub struct Completion {
    pub id: RunId,
    pub outcome: RunOutcome,
    pub detail: Option<String>,
    pub recording: Option<JournalRecording>,
    pub swarm_report: Option<Box<kortty_jobs::SwarmReport>>,
}

#[derive(Clone)]
pub(crate) struct ExecutionPolicy {
    pub(crate) file_transfer_allowed: bool,
    pub(crate) sudo_allowed: bool,
    pub(crate) server_access: ServerAccess,
    pub(crate) automation: Option<automation::Plan>,
    pub(crate) sync: Option<crate::scheduler_window::sync::RunContext>,
    pub(crate) privileged: Option<crate::scheduler_window::privileged::RunContext>,
    pub(crate) group_scope: Option<std::sync::Arc<SshCommand>>,
}

impl ExecutionPolicy {
    fn has_target_groups(&self, action: &SshCommand) -> bool {
        self.group_scope.is_some() || action.target_groups.is_some()
    }

    fn require_target_groups(&self, action: &SshCommand) -> Result<(), Failure> {
        self.group_scope
            .as_deref()
            .unwrap_or(action)
            .require_installed_target_groups()
            .map_err(Failure::blocked)
    }

    fn check_elevation(&self, action: &SshCommand) -> Result<(), Failure> {
        action.validate_elevation().map_err(Failure::blocked)?;
        if action.sudo != SudoMode::Disabled && !self.sudo_allowed {
            return Err(Failure::blocked("Managed policy forbids scheduler sudo."));
        }
        Ok(())
    }

    pub(crate) fn any_route_allowed(&self, action: &SshCommand) -> bool {
        if action.connection.is_some() {
            action
                .targets()
                .any(|reference| connection::reference_allowed(reference, &self.server_access))
        } else {
            connection::check_action_route(action, &self.server_access).is_ok()
        }
    }

    #[cfg(test)]
    fn unrestricted(file_transfer_allowed: bool) -> Self {
        Self {
            file_transfer_allowed,
            sudo_allowed: true,
            server_access: ServerAccess::default(),
            automation: None,
            sync: None,
            privileged: None,
            group_scope: None,
        }
    }
}

/// Starts one unattended SSH execution and sends exactly one terminal result.
pub(crate) fn spawn(
    request: RunRequest<SshCommand>,
    recording_directory: Option<PathBuf>,
    snippets_path: Option<PathBuf>,
    policy: ExecutionPolicy,
    auth: JobAuth,
    archive_password: ArchivePassword,
) -> CompletionReceiver<Completion> {
    let (completion, result) = async_channel::bounded(1);
    let id = request.id;
    let finished = completion.clone();
    let worker = thread::Builder::new()
        .name("kortty-job".into())
        .spawn(move || {
            let completion = catch_unwind(AssertUnwindSafe(|| {
                let archive_password = archive_password
                    .as_ref()
                    .map(|secret| secret.as_ref().map(|secret| secret.as_str()))
                    .map_err(|&error| error);
                let single = |saved| {
                    run(
                        &request,
                        recording_directory,
                        snippets_path.as_deref(),
                        &policy,
                        saved,
                        archive_password,
                        ssh_connection::trust_store,
                    )
                };
                match auth {
                    JobAuth::Manual => single(Ok(None)),
                    JobAuth::Saved(saved) => single(saved.map(|credentials| Some(*credentials))),
                    JobAuth::Batch(saved) => batch::run(
                        &request,
                        snippets_path.as_deref(),
                        &policy,
                        saved,
                        archive_password,
                        ssh_connection::trust_store,
                    ),
                }
            }))
            .unwrap_or_else(|_| {
                Completion::from_result(id, Err(Failure::failed("SSH job worker failed")))
            });
            let _ = finished.try_send(completion);
        });
    if worker.is_err() {
        let _ = completion.try_send(Completion::from_result(
            id,
            Err(Failure::failed("Could not start SSH job worker")),
        ));
    }
    result
}

struct Failure {
    outcome: RunOutcome,
    detail: &'static str,
}

impl Failure {
    const fn failed(detail: &'static str) -> Self {
        Self {
            outcome: RunOutcome::Failed { exit_code: None },
            detail,
        }
    }

    const fn blocked(detail: &'static str) -> Self {
        Self {
            outcome: RunOutcome::Blocked,
            detail,
        }
    }
}

impl Completion {
    fn from_result(id: RunId, result: Result<u32, Failure>) -> Self {
        let (outcome, detail) = match result {
            Ok(0) => (RunOutcome::Success, None),
            Ok(code) => {
                let exit_code = i32::try_from(code).ok();
                (
                    RunOutcome::Failed { exit_code },
                    exit_code
                        .is_none()
                        .then(|| format!("Remote SSH exit code {code}")),
                )
            }
            Err(failure) => (failure.outcome, Some(failure.detail.to_owned())),
        };
        Self {
            id,
            outcome,
            detail,
            recording: None,
            swarm_report: None,
        }
    }

    pub(crate) fn append_detail(&mut self, message: &str) {
        if let Some(detail) = &mut self.detail {
            detail.push_str(" · ");
            detail.push_str(message);
        } else {
            self.detail = Some(message.to_owned());
        }
    }
}

fn check_cancelled(cancellation: &Cancellation) -> Result<(), Failure> {
    if cancellation.is_requested() {
        Err(Failure {
            outcome: RunOutcome::Cancelled,
            detail: "SSH job cancelled",
        })
    } else {
        Ok(())
    }
}

fn run(
    request: &RunRequest<SshCommand>,
    recording_directory: Option<PathBuf>,
    snippets_path: Option<&Path>,
    policy: &ExecutionPolicy,
    saved_auth: SavedAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Completion {
    if !request.action.additional_connections.is_empty() {
        return Completion::from_result(
            request.id,
            Err(Failure::blocked(
                "Multi-target SSH authentication is unavailable",
            )),
        );
    }
    let mut journal_failed = false;
    let file_action = request.action.remote.as_ref();
    let journal = policy.automation.as_ref().and_then(|plan| {
        if file_action.is_some() && request.cancellation.is_requested() {
            return None;
        }
        let credentials = saved_auth.as_ref().ok().and_then(Option::as_ref);
        let archive_secret = match file_action {
            Some(crate::scheduler_window::RemoteAction::Archive(_)) => {
                archive_password.ok().flatten()
            }
            _ => None,
        };
        match plan.start(
            &request.action,
            credentials,
            archive_secret,
            &ClipboardPolicy::load(),
        ) {
            Ok(journal) => journal,
            Err(_) => {
                journal_failed = true;
                None
            }
        }
    });
    let result = check_cancelled(&request.cancellation)
        .and_then(|()| policy.require_target_groups(&request.action))
        .and_then(|()| policy.check_elevation(&request.action))
        .and_then(|()| connection::check_action_route(&request.action, &policy.server_access))
        .and_then(|()| {
            if file_action.is_some() {
                remote::run(request, policy, saved_auth, archive_password, trust_store)
            } else {
                run_command(
                    request,
                    recording_directory,
                    snippets_path,
                    policy,
                    saved_auth,
                    trust_store,
                    policy.automation.as_ref().zip(journal.as_ref()),
                )
                .map(|completion| (completion, None))
            }
        });
    let (mut completion, file_note) =
        result.unwrap_or_else(|failure| (Completion::from_result(request.id, Err(failure)), None));
    if let Some((plan, journal)) = policy.automation.as_ref().zip(journal) {
        let artifact = if file_action.is_some() {
            plan.finish_file(
                journal,
                &completion,
                file_note.as_deref().or(completion.detail.as_deref()),
            )
        } else {
            plan.finish_command(journal, completion.outcome)
        };
        journal_failed |= !artifact.complete;
        plan.completed(artifact);
    }
    if journal_failed {
        completion.append_detail("Could not complete the target session journal");
    }
    completion
}

fn run_command(
    request: &RunRequest<SshCommand>,
    recording_directory: Option<PathBuf>,
    snippets_path: Option<&Path>,
    policy: &ExecutionPolicy,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
    journal: Option<(
        &automation::Plan,
        &crate::session_journal::automation::Recorder,
    )>,
) -> Result<Completion, Failure> {
    let action = &request.action;
    let cancellation = &request.cancellation;
    check_cancelled(cancellation)?;
    let command = action
        .command_for_exec(snippets_path)
        .map_err(Failure::blocked)?;
    let mut saved_auth = saved_auth.map_err(Failure::blocked)?;
    let password = saved_auth
        .as_mut()
        .and_then(Credentials::take_sudo_password);
    let invocation = sudo::prepare(
        &command,
        action.sudo,
        password.as_ref().map(|secret| secret.as_str()),
    )?;
    let options = action.virtual_terminal;
    let capture = options
        .map(|_| {
            let directory = recording_directory.as_deref().ok_or_else(|| {
                Failure::blocked("Application policy forbids virtual terminal recordings")
            })?;
            virtual_terminal::Capture::create(directory, request.id)
                .map_err(|_| Failure::failed("Could not create the virtual terminal recording"))
        })
        .transpose()?;
    let (target, jumps) = connection::route(action, saved_auth, &policy.server_access)?;
    let trust =
        trust_store().map_err(|_| Failure::failed("Could not access SSH host-key trust store"))?;
    check_cancelled(cancellation)?;
    policy.require_target_groups(action)?;
    let cancelled = || cancellation.is_requested();
    let tcp =
        ssh_connection::connect_transport(&jumps.first().unwrap_or(&target).endpoint, cancelled);
    check_cancelled(cancellation)?;
    policy.require_target_groups(action)?;
    let tcp = tcp.map_err(|error| connect_failure(error.into()))?;
    let pty = options.map(|options| ("xterm-256color", options.size()));
    let opened = if policy.has_target_groups(action) {
        SshSession::connect_preconnected_guarded(
            tcp,
            (target, jumps),
            trust,
            kortty_ssh::StartupChannel::Exec {
                command: &invocation.command,
                pty,
            },
            cancelled,
            || {
                policy.require_target_groups(action).map_err(|failure| {
                    io::Error::new(io::ErrorKind::PermissionDenied, failure.detail)
                })
            },
        )
    } else {
        SshSession::connect_exec_preconnected_cancellable(
            tcp,
            target,
            jumps,
            trust,
            &invocation.command,
            pty,
            cancelled,
        )
    };
    check_cancelled(cancellation)?;
    if opened.is_err() {
        policy.require_target_groups(action)?;
    }
    let mut session = opened.map_err(connect_failure)?;
    let journal = journal
        .filter(|(plan, _)| plan.can_capture(&ClipboardPolicy::load()))
        .map(|(_, journal)| journal);
    if let Some(journal) = journal {
        if action.sudo == SudoMode::Disabled {
            journal.command(&command);
        } else {
            journal.command(&format!("sudo {command}"));
        }
    }
    Ok(if let (Some(options), Some(capture)) = (options, capture) {
        virtual_terminal::supervise(
            request.id,
            &mut session,
            cancellation,
            options,
            capture,
            journal,
        )
    } else {
        Completion::from_result(
            request.id,
            supervise(&mut session, cancellation, journal, invocation.password),
        )
    })
}

fn connect_failure(error: ConnectError) -> Failure {
    match error {
        ConnectError::UnknownHostKey { .. } => {
            Failure::blocked("SSH host key requires approval in an interactive connection")
        }
        ConnectError::ChangedHostKey { .. } => Failure::blocked("SSH host key has changed"),
        ConnectError::RequestRejected { request: "pty-req" } => {
            Failure::failed("SSH server refused the virtual terminal PTY")
        }
        ConnectError::RequestRejected {
            request: "subsystem",
        } => Failure::failed("SSH server refused the SFTP subsystem"),
        ConnectError::RequestRejected { .. } => {
            Failure::failed("SSH server refused the command request")
        }
        ConnectError::Io(error) if error.kind() == io::ErrorKind::TimedOut => {
            Failure::failed("SSH connection or channel setup timed out")
        }
        ConnectError::Io(_) | ConnectError::Protocol(_) => {
            Failure::failed("Could not establish an authenticated SSH channel")
        }
    }
}

fn supervise(
    session: &mut SshSession,
    cancellation: &Cancellation,
    journal: Option<&crate::session_journal::automation::Recorder>,
    password: Option<sudo::Password<'_>>,
) -> Result<u32, Failure> {
    let abort = session.abort_handle();
    let stdout = session.take_reader().ok_or_else(missing_stream)?;
    let stderr = session.take_stderr().ok_or_else(missing_stream)?;
    enum Input<'a> {
        Plain(kortty_ssh::SshWriter),
        Sensitive(kortty_ssh::SensitiveExecInput, sudo::Input<'a>),
    }
    let (prompt, input) = match password {
        Some(password) => {
            let (prompt, input) = password.split();
            let writer = session
                .take_sensitive_exec_input()
                .ok_or_else(missing_stream)?;
            (Some(prompt), Input::Sensitive(writer, input))
        }
        None => (
            None,
            Input::Plain(session.take_writer().ok_or_else(missing_stream)?),
        ),
    };
    let outputs = [
        sudo::Output::new(stdout, None),
        sudo::Output::new(stderr, prompt),
    ];
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let (report, reports) = mpsc::sync_channel(3);

    let drained = thread::scope(|scope| {
        let result = (|| {
            for (index, reader) in outputs.into_iter().enumerate() {
                let report = report.clone();
                thread::Builder::new()
                    .name("kortty-job-output".into())
                    .spawn_scoped(scope, move || {
                        let _ = report.send(automation::drain(reader, journal, index == 1));
                    })
                    .map_err(|_| Failure::failed("Could not start SSH output drain"))?;
            }
            thread::Builder::new()
                .name("kortty-job-eof".into())
                .spawn_scoped(scope, move || {
                    let result = match input {
                        Input::Plain(mut writer) => writer.close_input(),
                        Input::Sensitive(writer, input) => match input.take_line(cancellation) {
                            Ok(Some(line)) => writer.send(line),
                            Ok(None) => writer.close(),
                            Err(error) => Err(error),
                        },
                    };
                    let _ = report.send(result.is_ok());
                })
                .map_err(|_| Failure::failed("Could not close SSH command input"))?;

            wait_for_io(&reports, cancellation, deadline)
        })();
        if result.is_err() {
            abort.abort();
        }
        result
    });
    let exit_code = session.exit_code();
    let closed = session.close();
    check_cancelled(cancellation)?;
    let drained = drained?;

    // A command can exit before it observes our empty stdin. An authenticated
    // exit status still records the actual outcome in that case.
    match exit_code {
        Some(code) => Ok(code),
        None if !drained || closed.is_err() => Err(Failure::failed("SSH command transport failed")),
        None => Err(Failure::failed("SSH command ended without an exit status")),
    }
}

fn missing_stream() -> Failure {
    Failure::failed("SSH command streams were unavailable")
}

fn wait_for_io(
    reports: &Receiver<bool>,
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<bool, Failure> {
    let mut remaining = 3;
    let mut complete = true;
    while remaining != 0 {
        check_cancelled(cancellation)?;
        let Some(time_left) = deadline.checked_duration_since(Instant::now()) else {
            return Err(Failure::failed("SSH command exceeded the one-hour limit"));
        };
        match reports.recv_timeout(time_left.min(CANCELLATION_POLL)) {
            Ok(success) => {
                remaining -= 1;
                complete &= success;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Failure::failed("SSH command I/O worker ended unexpectedly"));
            }
        }
    }
    Ok(complete)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::time::SystemTime;

    use kortty_jobs::{Job, JobGraph, JobId, Scheduler};
    use kortty_ssh::{Authentication, Connect, Endpoint, TrustMode, TrustStore, WindowSize};

    use super::*;

    #[cfg(unix)]
    mod openssh_script;

    #[test]
    fn cancelled_ticket_completes_without_attempting_the_connection() {
        let (scheduler, request) = scheduled_request();
        let id = request.id;
        scheduler.cancel(id).unwrap();
        let completed = spawn(
            request,
            None,
            None,
            ExecutionPolicy::unrestricted(true),
            JobAuth::Manual,
            Ok(None),
        )
        .recv_blocking()
        .unwrap();
        assert_eq!(completed.id, id);
        assert_eq!(completed.outcome, RunOutcome::Cancelled);
        assert_eq!(completed.detail.as_deref(), Some("SSH job cancelled"));
    }

    #[test]
    fn server_policy_blocks_manual_shell_and_sftp_before_key_or_trust() {
        let policy = blocked_server_policy("127.0.0.1:22");
        for sftp in [false, true] {
            let (_, mut request) = scheduled_request();
            if sftp {
                let mut action = serde_json::to_value(request.action.as_ref()).unwrap();
                action["command"] = serde_json::json!("");
                action["remote"] = serde_json::json!({ "kind": "mkdir", "path": "/blocked" });
                request.action = std::sync::Arc::new(serde_json::from_value(action).unwrap());
            }
            let completed = run(&request, None, None, &policy, Ok(None), Ok(None), || {
                panic!("blocked endpoint must not open the SSH trust store")
            });
            assert_eq!(completed.outcome, RunOutcome::Blocked);
            assert!(completed.detail.unwrap().contains("Managed policy blocks"));
        }
    }

    pub(super) fn blocked_server_policy(pattern: &str) -> ExecutionPolicy {
        let policy = crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['{pattern}']"
        )).unwrap();
        ExecutionPolicy {
            automation: None,
            sync: None,
            privileged: None,
            group_scope: None,
            file_transfer_allowed: true,
            sudo_allowed: policy.scheduler_sudo_allowed,
            server_access: policy.server_access().clone(),
        }
    }

    #[test]
    fn virtual_terminal_policy_blocks_before_loading_credentials() {
        let (_, mut request) = scheduled_request();
        std::sync::Arc::make_mut(&mut request.action).virtual_terminal =
            Some(virtual_terminal::Options::default());
        let completed = spawn(
            request,
            None,
            None,
            ExecutionPolicy::unrestricted(true),
            JobAuth::Manual,
            Ok(None),
        )
        .recv_blocking()
        .unwrap();
        assert_eq!(completed.outcome, RunOutcome::Blocked);
        assert!(completed.recording.is_none());
    }

    #[test]
    fn scheduler_sudo_denial_blocks_before_ssh_setup() {
        let (_, mut request) = scheduled_request();
        std::sync::Arc::make_mut(&mut request.action).sudo = SudoMode::Passwordless;
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.features]\nscheduler-sudo = 'deny'",
        )
        .unwrap();
        let policy = ExecutionPolicy {
            sudo_allowed: policy.scheduler_sudo_allowed,
            ..ExecutionPolicy::unrestricted(true)
        };
        let completed = run(&request, None, None, &policy, Ok(None), Ok(None), || {
            panic!("denied sudo must stop before trust lookup")
        });
        assert_eq!(completed.outcome, RunOutcome::Blocked);
        assert_eq!(
            completed.detail.as_deref(),
            Some("Managed policy forbids scheduler sudo.")
        );
    }

    #[test]
    fn scheduled_file_transfer_policy_blocks_before_loading_credentials() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("untouched-file");
        use serde_json::json;
        for remote in [
            json!({"kind":"upload", "remote":"/remote/file", "local":path}),
            json!({"kind":"download", "remote":"/remote/file", "local":path}),
            json!({"kind":"upload", "remote":"/remote/folder", "local":path, "folder":true}),
            json!({"kind":"download", "remote":"/remote/folder", "local":path, "folder":true}),
            json!({"kind":"archive_zip", "sources":["/remote/file"], "target":"/remote/output.zip", "level":6}),
            json!({"kind":"archive", "format":"zip_password", "sources":["/remote/file"], "target":"/remote/output.zip", "level":6, "password_credential":"0123456789abcdef0123456789abcdef"}),
        ] {
            let (_, mut request) = scheduled_request();
            let mut action = serde_json::to_value(request.action.as_ref()).unwrap();
            action["command"] = serde_json::Value::String(String::new());
            action["remote"] = remote;
            request.action = std::sync::Arc::new(serde_json::from_value(action).unwrap());
            let completed = run(
                &request,
                None,
                None,
                &ExecutionPolicy::unrestricted(false),
                Ok(None),
                Ok(None),
                || panic!("a denied transfer reached the SSH trust store"),
            );
            assert_eq!(completed.outcome, RunOutcome::Blocked);
            assert_eq!(
                completed.detail.as_deref(),
                Some("Application policy forbids scheduled file transfers")
            );
            assert!(!path.exists());
        }
    }

    #[test]
    fn missing_archive_password_blocks_before_trust_or_ssh_authentication() {
        let (_, mut request) = scheduled_request();
        let mut action = serde_json::to_value(request.action.as_ref()).unwrap();
        action["command"] = serde_json::Value::String(String::new());
        action["remote"] = serde_json::json!({
            "kind": "archive",
            "format": "zip_password",
            "sources": ["/source"],
            "target": "/archive.zip",
            "level": 6,
            "password_credential": "0123456789abcdef0123456789abcdef",
        });
        request.action = std::sync::Arc::new(serde_json::from_value(action).unwrap());
        for password in [
            Ok(None),
            Ok(Some("  ")),
            Err("The archive vault is locked."),
        ] {
            let completed = run(
                &request,
                None,
                None,
                &ExecutionPolicy::unrestricted(true),
                Ok(None),
                password,
                || panic!("An encrypted ZIP without a password reached SSH setup"),
            );
            assert_eq!(completed.outcome, RunOutcome::Blocked);
            assert!(completed.detail.unwrap().contains("vault"));
        }
    }

    #[test]
    fn scheduled_delete_rejects_unconfirmed_path_before_loading_credentials() {
        let (_, mut request) = scheduled_request();
        let mut action = serde_json::to_value(request.action.as_ref()).unwrap();
        action["command"] = serde_json::Value::String(String::new());
        action["remote"] = serde_json::json!({
            "kind": "delete", "path": "/safe/stale", "confirmed_path": "/other",
        });
        request.action = std::sync::Arc::new(serde_json::from_value(action).unwrap());
        let completed = run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(false),
            Ok(None),
            Ok(None),
            || panic!("An unconfirmed deletion must not open the SSH trust store"),
        );
        assert_eq!(completed.outcome, RunOutcome::Blocked);
        assert_eq!(
            completed.detail.as_deref(),
            Some("Repeat the exact remote path to authorize deletion")
        );
    }

    #[test]
    fn invalid_script_blocks_before_ssh_setup() {
        let (_, mut request) = scheduled_request();
        let mut action = serde_json::to_value(request.action.as_ref()).unwrap();
        action["command"] = serde_json::Value::String(String::new());
        action["script"] =
            serde_json::json!({ "language": "bash", "body": "bad\0script", "arguments": [] });
        request.action = std::sync::Arc::new(serde_json::from_value(action).unwrap());
        let result = run_command(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(true),
            Ok(None),
            || panic!("invalid script must stop before trust lookup"),
            None,
        );
        assert!(matches!(result, Err(failure) if failure.outcome == RunOutcome::Blocked));
    }

    #[test]
    fn missing_snippet_blocks_before_ssh_setup() {
        let profile = crate::snippets::tests::Profile::new();
        let (_, mut request) = scheduled_request();
        let action = std::sync::Arc::make_mut(&mut request.action);
        action.command.clear();
        action.snippet = Some(crate::scheduler_window::SnippetLink {
            id: "b12222125615152b3fcfb1c0eb6c7492"
                .to_owned()
                .try_into()
                .unwrap(),
            arguments: Vec::new(),
        });
        let result = run_command(
            &request,
            None,
            Some(&profile.path()),
            &ExecutionPolicy::unrestricted(true),
            Ok(None),
            || panic!("missing snippet must stop before trust lookup"),
            None,
        );
        assert!(
            matches!(result, Err(failure) if failure.outcome == RunOutcome::Blocked && failure.detail == "The linked snippet is missing")
        );
    }

    #[test]
    fn linked_snippet_without_a_required_default_never_reaches_trust_or_authentication() {
        use crate::snippets::{Input, Library, Variable};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.path();
        let mut library = Library::open(path.clone()).unwrap();
        let id = library
            .save(
                None,
                Input {
                    name: "Ticket task".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "printf '%s\\n' '${TICKET}'".into(),
                },
            )
            .unwrap();
        library
            .save_variable(
                None,
                Variable {
                    name: "ticket".into(),
                    value: "  ".into(),
                },
            )
            .unwrap();
        let (_, mut request) = scheduled_request();
        let action = std::sync::Arc::make_mut(&mut request.action);
        action.command.clear();
        action.snippet = Some(crate::scheduler_window::SnippetLink {
            id,
            arguments: Vec::new(),
        });

        let result = run_command(
            &request,
            None,
            Some(&path),
            &ExecutionPolicy::unrestricted(true),
            Ok(None),
            || panic!("missing default reached the SSH trust lookup"),
            None,
        );
        assert!(
            matches!(result, Err(failure) if failure.outcome == RunOutcome::Blocked && failure.detail.contains("needs a value"))
        );
    }

    #[test]
    fn startup_cancellation_closes_a_silent_peer_before_sending_a_command() {
        struct TrustFixture(PathBuf);

        impl Drop for TrustFixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-job-trust-{:016x}",
            u64::from_le_bytes(nonce)
        ));
        std::fs::create_dir(&path).unwrap();
        let trust = TrustFixture(path);
        let (scheduler, request) = scheduled_request();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let id = request.id;
        let peer = thread::spawn(move || {
            // Test peer: reads the initial exchange without sending anything.
            let (peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            let mut peer = BufReader::new(peer);
            let mut identification = Vec::new();
            peer.read_until(b'\n', &mut identification).unwrap();
            assert!(identification.starts_with(b"SSH-2.0-"));

            // Sunset sends KEXINIT before receiving the peer identification.
            // Drain that packet before cancelling so TCP timing cannot make
            // already-queued handshake bytes look like later traffic.
            let mut length = [0; 4];
            peer.read_exact(&mut length).unwrap();
            let length = u32::from_be_bytes(length) as usize;
            assert!((6..=35_000).contains(&length));
            let mut packet = vec![0; length];
            peer.read_exact(&mut packet).unwrap();
            assert_eq!(packet[1], 20); // SSH_MSG_KEXINIT

            scheduler.cancel(id).unwrap();
            assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        });
        let start = Instant::now();
        let result = SshSession::connect_exec_cancellable(
            Connect {
                address,
                endpoint: Endpoint::new("127.0.0.1", address.port()).unwrap(),
                username: "test-only".into(),
                authentication: Authentication::password("test-only".into()),
                term: String::new(),
                size: WindowSize {
                    columns: 0,
                    rows: 0,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            TrustStore::new(trust.0.join("known_hosts"), TrustMode::Strict),
            &[],
            "exit 0",
            || request.cancellation.is_requested(),
        );
        peer.join().unwrap();
        assert!(matches!(
            result,
            Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::Interrupted
        ));
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn timed_out_io_and_large_exit_status_have_explicit_results() {
        let (_, request) = scheduled_request();
        let (_report, reports) = mpsc::sync_channel(3);
        let outcome = wait_for_io(&reports, &request.cancellation, Instant::now());
        let timed_out = Completion::from_result(request.id, outcome.map(|_| 0));
        assert_eq!(timed_out.outcome, RunOutcome::Failed { exit_code: None });
        assert_eq!(
            timed_out.detail.as_deref(),
            Some("SSH command exceeded the one-hour limit")
        );

        let large = Completion::from_result(request.id, Ok(u32::MAX));
        assert_eq!(large.outcome, RunOutcome::Failed { exit_code: None });
        assert_eq!(
            large.detail.as_deref(),
            Some("Remote SSH exit code 4294967295")
        );
    }

    pub(super) fn scheduled_request() -> (Scheduler<SshCommand>, RunRequest<SshCommand>) {
        let id = JobId::new("ssh-job").unwrap();
        let action = SshCommand {
            host: "127.0.0.1".into(),
            port: 22,
            username: "test-only".into(),
            key_file: PathBuf::new(),
            connection: None,
            additional_connections: Vec::new(),
            target_groups: None,
            command: "exit 0".into(),
            working_directory: None,
            script: None,
            snippet: None,
            remote: None,
            agent: None,
            sudo: SudoMode::Disabled,
            virtual_terminal: None,
            automation_journal: None,
            desktop_notices: Default::default(),
            webhook_targets: Vec::new(),
        };
        let graph = JobGraph::new(vec![Job::new(id.clone(), "SSH job", action).unwrap()]).unwrap();
        let mut scheduler = Scheduler::new(graph);
        let request = scheduler
            .start_manual(&id, SystemTime::now())
            .unwrap()
            .started
            .pop()
            .unwrap();
        (scheduler, request)
    }
}
