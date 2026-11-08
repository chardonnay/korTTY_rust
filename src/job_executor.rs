use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Receiver as CompletionReceiver;
use kortty_jobs::{Cancellation, JournalRecording, RunId, RunOutcome, RunRequest};
use kortty_ssh::{ConnectError, SshSession, TrustStore};

use crate::scheduler_window::SshCommand;
use crate::ssh_connection;

mod batch;
mod connection;
mod remote;
pub(crate) mod virtual_terminal;

pub(crate) use connection::{BatchAuth, Credentials, JobAuth, SavedAuth};
pub(crate) type ArchivePassword = Result<Option<kortty_vault::Secret>, &'static str>;
type BorrowedArchivePassword<'a> = Result<Option<&'a str>, &'static str>;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const CANCELLATION_POLL: Duration = Duration::from_millis(250);

/// The terminal result for one scheduler ticket. Details are synthesized locally.
#[derive(Clone, Debug)]
pub struct Completion {
    pub id: RunId,
    pub outcome: RunOutcome,
    pub detail: Option<String>,
    pub recording: Option<JournalRecording>,
}

/// Starts one unattended SSH execution and sends exactly one terminal result.
pub fn spawn(
    request: RunRequest<SshCommand>,
    recording_directory: Option<PathBuf>,
    snippets_path: Option<PathBuf>,
    file_transfer_allowed: bool,
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
                        file_transfer_allowed,
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
                        file_transfer_allowed,
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
        }
    }

    fn append_detail(&mut self, message: &str) {
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
    file_transfer_allowed: bool,
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
    let result = if request.action.remote.is_some() {
        remote::run(
            request,
            file_transfer_allowed,
            saved_auth,
            archive_password,
            trust_store,
        )
    } else {
        run_command(
            request,
            recording_directory,
            snippets_path,
            saved_auth,
            trust_store,
        )
    };
    result.unwrap_or_else(|failure| Completion::from_result(request.id, Err(failure)))
}

fn run_command(
    request: &RunRequest<SshCommand>,
    recording_directory: Option<PathBuf>,
    snippets_path: Option<&Path>,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    let action = &request.action;
    let cancellation = &request.cancellation;
    check_cancelled(cancellation)?;
    let command = action
        .command_for_exec(snippets_path)
        .map_err(Failure::blocked)?;
    let saved_auth = saved_auth.map_err(Failure::blocked)?;
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
    let trust =
        trust_store().map_err(|_| Failure::failed("Could not access SSH host-key trust store"))?;
    let (target, jumps) = connection::route(action, saved_auth)?;
    check_cancelled(cancellation)?;
    let cancelled = || cancellation.is_requested();
    let tcp =
        ssh_connection::connect_transport(&jumps.first().unwrap_or(&target).endpoint, cancelled);
    check_cancelled(cancellation)?;
    let opened = SshSession::connect_exec_preconnected_cancellable(
        tcp.map_err(|error| connect_failure(error.into()))?,
        target,
        jumps,
        trust,
        &command,
        options.map(|options| ("xterm-256color", options.size())),
        cancelled,
    );
    check_cancelled(cancellation)?;
    let mut session = opened.map_err(connect_failure)?;
    Ok(if let (Some(options), Some(capture)) = (options, capture) {
        virtual_terminal::supervise(request.id, &mut session, cancellation, options, capture)
    } else {
        Completion::from_result(request.id, supervise(&mut session, cancellation))
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

fn supervise(session: &mut SshSession, cancellation: &Cancellation) -> Result<u32, Failure> {
    let abort = session.abort_handle();
    let stdout = session.take_reader().ok_or_else(missing_stream)?;
    let stderr = session.take_stderr().ok_or_else(missing_stream)?;
    let mut stdin = session.take_writer().ok_or_else(missing_stream)?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let (report, reports) = mpsc::sync_channel(3);

    let drained = thread::scope(|scope| {
        let result = (|| {
            for reader in [stdout, stderr] {
                let report = report.clone();
                thread::Builder::new()
                    .name("kortty-job-output".into())
                    .spawn_scoped(scope, move || {
                        let _ = report.send(discard(reader));
                    })
                    .map_err(|_| Failure::failed("Could not start SSH output drain"))?;
            }
            thread::Builder::new()
                .name("kortty-job-eof".into())
                .spawn_scoped(scope, move || {
                    let _ = report.send(stdin.close_input().is_ok());
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

fn discard(mut reader: impl Read) -> bool {
    io::copy(&mut reader, &mut io::sink()).is_ok()
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
        let completed = spawn(request, None, None, true, JobAuth::Manual, Ok(None))
            .recv_blocking()
            .unwrap();
        assert_eq!(completed.id, id);
        assert_eq!(completed.outcome, RunOutcome::Cancelled);
        assert_eq!(completed.detail.as_deref(), Some("SSH job cancelled"));
    }

    #[test]
    fn virtual_terminal_policy_blocks_before_loading_credentials() {
        let (_, mut request) = scheduled_request();
        std::sync::Arc::make_mut(&mut request.action).virtual_terminal =
            Some(virtual_terminal::Options::default());
        let completed = spawn(request, None, None, true, JobAuth::Manual, Ok(None))
            .recv_blocking()
            .unwrap();
        assert_eq!(completed.outcome, RunOutcome::Blocked);
        assert!(completed.recording.is_none());
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
            let completed = run(&request, None, None, false, Ok(None), Ok(None), || {
                panic!("a denied transfer reached the SSH trust store")
            });
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
            let completed = run(&request, None, None, true, Ok(None), password, || {
                panic!("An encrypted ZIP without a password reached SSH setup")
            });
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
        let completed = run(&request, None, None, false, Ok(None), Ok(None), || {
            panic!("An unconfirmed deletion must not open the SSH trust store")
        });
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
        let result = run_command(&request, None, None, Ok(None), || {
            panic!("invalid script must stop before trust lookup")
        });
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
        let result = run_command(&request, None, Some(&profile.path()), Ok(None), || {
            panic!("missing snippet must stop before trust lookup")
        });
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

        let result = run_command(&request, None, Some(&path), Ok(None), || {
            panic!("missing default reached the SSH trust lookup")
        });
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
            command: "exit 0".into(),
            script: None,
            snippet: None,
            remote: None,
            virtual_terminal: None,
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
