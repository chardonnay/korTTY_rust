use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Output as ProcessOutput;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kortty_ssh::sftp::Sftp;
use kortty_ssh::{ConnectOptions, HostKey, HostKeyStatus, TrustMode, TrustStore};
use kortty_terminal::TerminalProfile;

use super::*;
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::{Authentication, LoginRequest, LoginTarget};

use crate::ssh_test_fixture as support;
use crate::ssh_test_peer as keyboard_peer;

mod side_channels;

const TIMEOUT: Duration = Duration::from_secs(12);
const INITIAL: WindowSize = WindowSize {
    columns: 80,
    rows: 24,
    pixel_width: 0,
    pixel_height: 0,
};

pub(crate) struct Fixture {
    directory: support::TempDirectory,
    listener: TcpListener,
    user: String,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let directory = support::TempDirectory::new("openssh-worker");
        for name in ["host", "rotated", "client"] {
            let result = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(directory.0.join(name))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let user = Command::new("id").arg("-un").output().unwrap();
        assert!(user.status.success());
        Self {
            directory,
            listener,
            user: String::from_utf8(user.stdout).unwrap().trim().into(),
        }
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new("127.0.0.1", self.listener.local_addr().unwrap().port()).unwrap()
    }

    fn key(&self, name: &str) -> HostKey {
        let line = fs::read_to_string(self.directory.0.join(format!("{name}.pub"))).unwrap();
        HostKey::from_openssh(line.trim()).unwrap()
    }

    pub(crate) fn request(&self, key: PathBuf) -> Request {
        request(&self.listener, self.user.clone(), key)
    }

    fn serve(&self, host: &str) -> JoinHandle<ProcessOutput> {
        self.serve_with_extra(host, "")
    }

    pub(crate) fn serve_with_extra(&self, host: &str, extra: &str) -> JoinHandle<ProcessOutput> {
        let config = self.directory.0.join(format!("{host}.conf"));
        let host_key = self.directory.0.join(host);
        let authorized = self.directory.0.join("client.pub");
        fs::write(
            &config,
            format!(
                "HostKey \"{}\"\nAuthorizedKeysFile \"{}\"\nAllowUsers {}\n\
                 PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
                 UsePAM no\nStrictModes yes\nLogLevel DEBUG2\nPrintMotd no\nPrintLastLog no\n\
                 PermitTTY yes\nAllowTcpForwarding yes\nGatewayPorts no\nSubsystem sftp internal-sftp\n{extra}",
                host_key.display(),
                authorized.display(),
                self.user,
            ),
        )
        .unwrap();
        let listener = self.listener.try_clone().unwrap();
        thread::spawn(move || {
            let stream = accept(&listener);
            let input: OwnedFd = stream.into();
            let output = input.try_clone().unwrap();
            Command::new("/usr/sbin/sshd")
                .args(["-ie", "-f"])
                .arg(config)
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(output))
                .stderr(Stdio::piped())
                .output()
                .unwrap()
        })
    }

    fn trust(&self) -> TrustStore {
        TrustStore::new(self.directory.0.join("known_hosts"), TrustMode::Strict)
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory.0
    }

    pub(crate) fn pinned_trust(&self) -> TrustStore {
        let trust = self.trust();
        assert_eq!(
            trust.remember(&self.endpoint(), &self.key("host")).unwrap(),
            HostKeyStatus::Known
        );
        trust
    }
}

fn request(listener: &TcpListener, username: String, key: PathBuf) -> Request {
    Request {
        target: LoginRequest {
            endpoint: LoginTarget {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                username,
            },
            auth: Authentication::PrivateKey(key),
            keyboard_interactive: false,
        },
        jump: None,
        profile: TerminalProfile::Xterm,
        isolation: IsolationLevel::Process,
        options: Default::default(),
    }
}

pub(crate) fn executable() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("KORTTY_SSH_TEST_EXE").expect("set KORTTY_SSH_TEST_EXE to the built app"),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "invalid app executable"
    );
    path
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "SSH worker did not connect");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("could not accept SSH worker: {error}"),
        }
    }
}

pub(crate) fn dial(
    request: Request,
    sftp: bool,
    executable: &Path,
    trust: &TrustStore,
) -> Result<Session, ConnectError> {
    Session::dial_executable(
        request,
        INITIAL,
        sftp,
        &DialCancellation::default(),
        executable,
        trust,
        None,
    )
}

fn server_log(server: JoinHandle<ProcessOutput>) -> String {
    String::from_utf8_lossy(&server.join().unwrap().stderr).into_owned()
}

fn assert_no_auth(server: JoinHandle<ProcessOutput>) {
    let log = server_log(server);
    assert!(log.contains("Connection from"), "{log}");
    assert!(!log.contains("userauth-request"), "{log}");
}

fn bounded_io<T: Send + 'static>(
    session: &mut Session,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (send, receive) = mpsc::channel();
    let task = thread::spawn(move || send.send(work()).ok());
    let value = receive.recv_timeout(TIMEOUT);
    if value.is_err() {
        session.close().unwrap();
    }
    task.join().unwrap();
    value.expect("worker I/O did not complete")
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_verifies_pins_and_runs_a_separate_shell_process() {
    worker_pins_and_shell(IsolationLevel::Process);
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_session_process_window_ends_only_the_selected_worker() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    trust
        .remember(&fixture.endpoint(), &fixture.key("host"))
        .unwrap();
    let executable = executable();
    let start = |label: &str| {
        let server = fixture.serve("host");
        let session = Session::dial_executable(
            fixture.request(fixture.directory.0.join("client")),
            INITIAL,
            false,
            &DialCancellation::for_session(label.into()),
            &executable,
            &trust,
            None,
        )
        .unwrap();
        (session, server)
    };
    let (mut selected, selected_server) = start("selected process test");
    let (mut other, other_server) = start("other process test");
    let rows = monitor::sample();
    let selected_row = rows
        .iter()
        .find(|row| row.label == "selected process test")
        .unwrap();
    let other_row = rows
        .iter()
        .find(|row| row.label == "other process test")
        .unwrap();
    let pid = selected_row.pid.unwrap();
    assert_ne!(pid, other_row.pid.unwrap());
    assert_ne!(pid, std::process::id());
    assert!(!selected_row.sandbox);
    assert!(selected_row.resident_bytes.is_some_and(|bytes| bytes > 0));
    assert!(selected_row.cpu_time.is_some());

    monitor::end(selected_row.id).unwrap();
    assert!(selected.abort_handle().is_closed());
    assert!(!other.abort_handle().is_closed());
    assert!(
        monitor::sample()
            .iter()
            .all(|row| row.id != selected_row.id)
    );
    let mut reader = selected.take_reader().unwrap();
    bounded_io(&mut selected, move || {
        reader.read_to_end(&mut Vec::new()).ok()
    });
    selected.close().unwrap();
    drop(selected);
    assert_reaped(pid);
    monitor::end(selected_row.id).unwrap();
    assert!(!other.abort_handle().is_closed());
    other
        .take_writer()
        .unwrap()
        .write_all(b"printf '%s%s\\n' 'other-worker-' 'survived'; exit 29\n")
        .unwrap();
    let mut reader = other.take_reader().unwrap();
    let output = bounded_io(&mut other, move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        output
    });
    assert!(
        output
            .lines()
            .any(|line| line.trim() == "other-worker-survived")
    );
    assert_eq!(other.exit_code(), Some(29));
    other.close().unwrap();
    assert!(server_log(selected_server).contains("Accepted publickey"));
    assert!(server_log(other_server).contains("Accepted publickey"));
}

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing to the built app"]
fn process_worker_uses_requested_startup_timeout_without_retrying_the_ssh_handshake() {
    worker_startup_timeout(IsolationLevel::Process);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing into the packaged macOS app"]
fn sandbox_worker_macos_uses_requested_startup_timeout_without_retrying_the_ssh_handshake() {
    worker_startup_timeout(IsolationLevel::Sandbox);
}

fn worker_startup_timeout(isolation: IsolationLevel) {
    let directory = support::TempDirectory::new("worker-startup-deadline");
    let key = directory.0.join("unused-key");
    fs::write(&key, "test fixture").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let incoming = listener.try_clone().unwrap();
    // Test double: a real TCP peer that does not send an SSH identification.
    let peer = thread::spawn(move || {
        let mut stream = accept(&incoming);
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let mut request = request(&listener, "fixture".into(), key);
    request.isolation = isolation;
    request.options = ConnectOptions::new(1, 3).unwrap();
    let trust = TrustStore::new(directory.0.join("known_hosts"), TrustMode::Strict);
    let started = Instant::now();
    let result = dial(request, false, &executable(), &trust);
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
    );
    assert!(
        (Duration::from_millis(900)..Duration::from_secs(5)).contains(&elapsed),
        "{elapsed:?}"
    );
    assert!(peer.join().unwrap().starts_with(b"SSH-2.0-"));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

fn worker_pins_and_shell(isolation: IsolationLevel) {
    let executable = executable();
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let endpoint = fixture.endpoint();
    let host = fixture.key("host");
    let rotated = fixture.key("rotated");
    let request = |key| {
        let mut request = fixture.request(key);
        request.isolation = isolation;
        request.options = ConnectOptions::new(5, 3).unwrap();
        request
    };
    let malformed = fixture.directory.0.join("malformed-key");
    fs::write(&malformed, "malformed test key").unwrap();

    let server = fixture.serve("host");
    assert!(matches!(
        dial(request(malformed.clone()), false, &executable, &trust),
        Err(ConnectError::UnknownHostKey { endpoint: actual, offered })
            if actual == endpoint && offered == host
    ));
    assert_no_auth(server);
    assert!(
        matches!(fixture.listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        trust.remember(&endpoint, &host).unwrap(),
        HostKeyStatus::Known
    );

    let server = fixture.serve("rotated");
    assert!(matches!(
        dial(request(malformed), false, &executable, &trust),
        Err(ConnectError::ChangedHostKey { endpoint: actual, expected, offered })
            if actual == endpoint && expected == host && offered == rotated
    ));
    assert_no_auth(server);
    assert!(
        matches!(fixture.listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    assert_eq!(trust.check(&endpoint, &host).unwrap(), HostKeyStatus::Known);

    let server = fixture.serve("host");
    let rejected = dial(
        request(fixture.directory.0.join("rotated")),
        false,
        &executable,
        &trust,
    );
    assert!(rejected.is_err());
    let log = server_log(server);
    assert!(
        log.contains("userauth-request") && !log.contains("Accepted publickey"),
        "{log}"
    );
    assert!(
        matches!(fixture.listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );

    let server = fixture.serve("host");
    let request = request(fixture.directory.0.join("client"));
    let mut worker = dial(request, false, &executable, &trust).unwrap();
    #[cfg(target_os = "macos")]
    if isolation == IsolationLevel::Sandbox {
        assert_eq!(worker.sandbox_backend(), Some("App Sandbox"));
    }
    let pid = {
        let mut child = lock(&worker.abort.0.child);
        let child = child.as_mut().unwrap();
        assert!(!child.exited().unwrap());
        child.id()
    };
    assert_ne!(pid, std::process::id());
    worker
        .resize(WindowSize {
            rows: 41,
            columns: 113,
            ..INITIAL
        })
        .unwrap();
    let mut writer = worker.take_writer().unwrap();
    writer
        .write_all(b"printf '\\n%s:%s\\n' 'worker-size' \"$(stty size)\"; exit 23\n")
        .unwrap();
    let mut reader = worker.take_reader().unwrap();
    let bytes = bounded_io(&mut worker, move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let output = String::from_utf8_lossy(&bytes);
    assert!(output.contains("worker-size:41 113"), "{output}");
    assert_eq!(worker.exit_code(), Some(23));
    worker.close().unwrap();
    assert!(lock(&worker.abort.0.child).is_none());
    assert_reaped(pid);
    assert!(server_log(server).contains("Accepted publickey"));
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_authenticates_with_an_encrypted_ecdsa_key() {
    const PASSPHRASE: &str = "test-only worker key passphrase";
    let fixture = Fixture::new();
    let key = fixture.directory.0.join("client-ecdsa");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ecdsa", "-b", "384", "-N", PASSPHRASE, "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    fs::copy(
        key.with_extension("pub"),
        fixture.directory.0.join("client.pub"),
    )
    .unwrap();
    let trust = fixture.trust();
    trust
        .remember(&fixture.endpoint(), &fixture.key("host"))
        .unwrap();
    let mut request = fixture.request(key.clone());
    request.target.auth =
        Authentication::KeyWithPassphrase(key, crate::ssh_dialog::Password::new(PASSPHRASE.into()));
    let server = fixture.serve("host");
    let mut session = dial(request, false, &executable(), &trust).unwrap();
    let mut writer = session.take_writer().unwrap();
    writer
        .write_all(b"printf '%s%s\\n' 'worker-ecdsa-' 'ok'; exit 21\n")
        .unwrap();
    let mut reader = session.take_reader().unwrap();
    let output = bounded_io(&mut session, move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        output
    });
    assert!(output.lines().any(|line| line.trim() == "worker-ecdsa-ok"));
    assert_eq!(session.exit_code(), Some(21));
    session.close().unwrap();
    assert!(server_log(server).contains("Accepted publickey"));
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_runs_standalone_sftp() {
    worker_standalone_sftp(IsolationLevel::Process);
}

fn worker_standalone_sftp(isolation: IsolationLevel) {
    let executable = executable();
    let fixture = Fixture::new();
    let trust = fixture.trust();
    trust
        .remember(&fixture.endpoint(), &fixture.key("host"))
        .unwrap();
    let server = fixture.serve("host");
    let mut request = fixture.request(fixture.directory.0.join("client"));
    request.isolation = isolation;
    let mut worker = dial(request, true, &executable, &trust).unwrap();
    let remote = fixture.directory.0.join("test.bin");
    let path = remote.to_str().unwrap().as_bytes().to_vec();
    let input = worker.take_reader().unwrap();
    let output = worker.take_writer().unwrap();
    let bytes = b"\0kortty worker SFTP roundtrip\xff";
    bounded_io(&mut worker, move || {
        let mut sftp = Sftp::new(input, output).unwrap();
        assert_eq!(
            sftp.upload_new(&path, &bytes[..]).unwrap(),
            bytes.len() as u64
        );
        let mut received = Vec::new();
        sftp.download(&path, &mut received).unwrap();
        assert_eq!(received, bytes);
    });
    assert_eq!(fs::read(remote).unwrap(), bytes);
    worker.close().unwrap();
    assert!(server_log(server).contains("Accepted publickey"));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing into the packaged macOS app"]
fn openssh_sandbox_worker_macos_shell_sftp_and_pins() {
    worker_pins_and_shell(IsolationLevel::Sandbox);
    worker_standalone_sftp(IsolationLevel::Sandbox);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing into the packaged macOS app"]
fn openssh_sandbox_worker_macos_connects_via_pinned_jump() {
    let jump = Fixture::new();
    let target = Fixture::new();
    let trust = TrustStore::from_pins(vec![
        (jump.endpoint(), jump.key("host")),
        (target.endpoint(), target.key("rotated")),
    ])
    .unwrap();
    let mut request = target.request(target.directory.0.join("client"));
    request.isolation = IsolationLevel::Sandbox;
    request.jump = Some(jump.request(jump.directory.0.join("client")).target);
    let first = jump.serve("host");
    let last = target.serve("rotated");
    let mut worker = dial(request, false, &executable(), &trust).unwrap();
    assert_eq!(worker.sandbox_backend(), Some("App Sandbox"));
    let mut writer = worker.take_writer().unwrap();
    writer
        .write_all(b"HISTFILE=/dev/null; printf '\\n%s%s\\n' 'jump-' 'target'; exit 17\n")
        .unwrap();
    let mut reader = worker.take_reader().unwrap();
    let output = bounded_io(&mut worker, move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        String::from_utf8_lossy(&output).into_owned()
    });
    assert!(output.contains("jump-target"), "{output}");
    assert_eq!(worker.exit_code(), Some(17));
    worker.close().unwrap();
    let first = server_log(first);
    assert!(first.contains("Accepted publickey"));
    let peer = first
        .lines()
        .find_map(|line| line.strip_prefix("Connection from "))
        .and_then(|line| line.split_once(" on "))
        .map(|(origin, _)| origin)
        .expect("OpenSSH must log the actual TCP peer");
    assert!(first.contains(&format!("originator {peer}, target 127.0.0.1")));
    assert!(server_log(last).contains("Accepted publickey"));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing into the packaged macOS app"]
fn openssh_sandbox_worker_macos_rejects_a_modified_helper_before_dialing() {
    let original = executable();
    let app = original.ancestors().nth(3).unwrap();
    let fixture = support::TempDirectory::new("ssh-worker-modified-bundle");
    let copy = fixture.0.join("kortty.app");
    assert!(
        Command::new("/usr/bin/ditto")
            .arg(app)
            .arg(&copy)
            .status()
            .unwrap()
            .success()
    );
    let helper_info = copy.join("Contents/Helpers/kortty-ssh-worker.app/Contents/Info.plist");
    let mut contents = fs::read(&helper_info).unwrap();
    contents.push(b'\n');
    fs::write(helper_info, contents).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let trust = TrustStore::new(fixture.0.join("known_hosts"), TrustMode::Strict);
    let mut request = request(
        &listener,
        "kortty-test".into(),
        fixture.0.join("unused-key"),
    );
    request.isolation = IsolationLevel::Sandbox;
    assert!(matches!(
        dial(request, false, &copy.join("Contents/MacOS/kortty"), &trust),
        Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing to the built app"]
fn worker_start_cancellation_kills_and_reaps_the_child() {
    worker_start_cancellation(IsolationLevel::Process);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing into the packaged macOS app"]
fn openssh_sandbox_worker_macos_cancels_with_the_peer_open() {
    worker_start_cancellation(IsolationLevel::Sandbox);
}

fn worker_start_cancellation(isolation: IsolationLevel) {
    let executable = executable();
    let fixture = support::TempDirectory::new("ssh-worker-cancel");
    let key = fixture.0.join("unused-test-key");
    fs::write(&key, "test credential").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let trust = TrustStore::new(fixture.0.join("known_hosts"), TrustMode::Strict);
    let mut request = request(&listener, "kortty-test".into(), key);
    request.isolation = isolation;
    #[cfg(target_os = "macos")]
    let leak = (isolation == IsolationLevel::Sandbox).then(|| {
        let path = fixture.0.join("never-inherit");
        fs::write(&path, b"disposable descriptor probe").unwrap();
        let file = fs::File::open(&path).unwrap();
        rustix::io::fcntl_setfd(&file, rustix::io::FdFlags::empty()).unwrap();
        let path = path.canonicalize().unwrap();
        assert!(find_open_file(std::process::id(), &path));
        (file, path)
    });
    let cancellation = DialCancellation::default();
    let pending = cancellation.clone();
    let guard = CancelOnDrop(cancellation.clone());
    let (send, receive) = mpsc::channel();
    let dial = thread::spawn(move || {
        let result =
            Session::dial_executable(request, INITIAL, false, &pending, &executable, &trust, None);
        send.send(result)
            .unwrap_or_else(|_| panic!("missing test receiver"));
    });
    // Test peer intentionally stops before exchanging an SSH identification string.
    let _stalled = accept(&listener);
    let pid = {
        let pending = lock(&cancellation.0);
        let abort = pending.active.as_ref().unwrap();
        lock(&abort.0.child).as_ref().unwrap().id()
    };
    assert_ne!(pid, std::process::id());
    #[cfg(target_os = "macos")]
    if let Some((_file, path)) = leak {
        assert!(
            !find_open_file(pid, &path),
            "worker inherited a desktop file"
        );
    }
    drop(guard);
    let result = receive.recv_timeout(TIMEOUT).unwrap();
    assert!(
        matches!(result, Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
    );
    dial.join().unwrap();
    assert_reaped(pid);
}

#[cfg(target_os = "macos")]
fn find_open_file(pid: u32, path: &Path) -> bool {
    let result = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-Fn", "--"])
        .arg(path)
        .output()
        .unwrap();
    match result.status.code() {
        Some(0) => String::from_utf8_lossy(&result.stdout)
            .lines()
            .any(|entry| {
                entry
                    .strip_prefix('n')
                    .is_some_and(|name| Path::new(name) == path)
            }),
        Some(1) if result.stdout.is_empty() && result.stderr.is_empty() => false,
        _ => panic!("could not inspect fixture descriptors: {result:?}"),
    }
}

fn assert_reaped(pid: u32) {
    use rustix::process::{Pid, WaitOptions, waitpid};

    let pid = Pid::from_raw(pid.try_into().unwrap()).unwrap();
    assert_eq!(
        waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
        rustix::io::Errno::CHILD,
    );
}

#[test]
#[ignore = "requires a built kortty, OpenSSH ssh-keygen and a test Python with Paramiko 4.0.0"]
fn keyboard_interactive_worker_round_trip() {
    keyboard_worker(IsolationLevel::Process);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
#[ignore = "requires a packaged macOS app or Landlock ABI 6+ Linux, and the SSH test peer"]
fn keyboard_interactive_worker_sandbox_round_trip() {
    #[cfg(target_os = "linux")]
    assert!(sandbox_available(), "SSH worker sandbox unavailable");
    keyboard_worker(IsolationLevel::Sandbox);
}

fn keyboard_worker(isolation: IsolationLevel) {
    let executable = executable();
    let fixture = Fixture::new();
    let allowed = fs::read_to_string(fixture.directory.0.join("client.pub")).unwrap();
    let allowed = allowed.split_whitespace().nth(1).unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/ssh/tests/support/keyboard_peer.py");
    for mode in [
        "direct",
        "combined",
        "password-first",
        "unknown",
        "changed",
        "cancel",
    ] {
        let mut peer =
            keyboard_peer::start(&script, mode, &fixture.directory.0.join("host"), allowed);
        let endpoint = peer.endpoint();
        let (auth, keyboard_interactive) = match mode {
            "combined" => (
                Authentication::PrivateKey(fixture.directory.0.join("client")),
                true,
            ),
            "password-first" => (
                Authentication::Password(crate::ssh_dialog::Password::new(
                    "password-method-test-value".into(),
                )),
                true,
            ),
            _ => (Authentication::KeyboardInteractive, false),
        };
        let request = Request {
            target: LoginRequest {
                endpoint: LoginTarget {
                    host: "127.0.0.1".into(),
                    port: peer.address.port(),
                    username: keyboard_peer::USER.into(),
                },
                auth,
                keyboard_interactive,
            },
            jump: None,
            profile: TerminalProfile::Xterm,
            isolation,
            options: Default::default(),
        };
        let pins = match mode {
            "unknown" => Vec::new(),
            "changed" => vec![(endpoint.clone(), fixture.key("rotated"))],
            _ => vec![(endpoint.clone(), fixture.key("host"))],
        };
        let trust = TrustStore::from_pins(pins).unwrap();
        let mut count = 0;
        let result = Session::dial_executable(
            request,
            INITIAL,
            false,
            &DialCancellation::default(),
            &executable,
            &trust,
            Some(&mut |round| {
                count += 1;
                assert_eq!(round.endpoint, endpoint);
                assert_eq!(round.username, keyboard_peer::USER);
                let answers: &[&str] = match (mode, count) {
                    ("cancel", 1) => return Err(io::ErrorKind::Interrupted.into()),
                    (_, 1) if round.prompts.is_empty() => &[],
                    (_, 2) => {
                        assert_eq!(round.prompts[0].text, "Code: ");
                        assert!(!round.prompts[0].echo);
                        assert!(round.prompts[1].echo);
                        &["ä123", ""]
                    }
                    (_, 3) => &["yes"],
                    _ => panic!("unexpected challenge {round:?}"),
                };
                Ok(answers
                    .iter()
                    .map(|answer| KeyboardResponse::new((*answer).into()))
                    .collect())
            }),
        );
        match (mode, result) {
            ("unknown", Err(ConnectError::UnknownHostKey { .. }))
            | ("changed", Err(ConnectError::ChangedHostKey { .. })) => {
                assert_eq!(count, 0);
                assert_eq!(peer.responses(), 0);
            }
            ("cancel", Err(ConnectError::Io(error))) => {
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                assert_eq!(count, 1);
                assert_eq!(peer.responses(), 0);
            }
            (_, Ok(mut session)) => {
                assert_eq!(
                    session.sandbox_backend(),
                    (isolation == IsolationLevel::Sandbox).then_some(SANDBOX_BACKEND)
                );
                let mut reader = session.take_reader().unwrap();
                let received = bounded_io(&mut session, move || {
                    let mut received = String::new();
                    reader.read_to_string(&mut received).unwrap();
                    received
                });
                assert_eq!(received, "keyboard-interactive-ok\n");
                assert_eq!(count, 3);
                assert_eq!(peer.responses(), 3);
                session.close().unwrap();
            }
            (_, Err(error)) => panic!("unexpected {mode} error: {error}"),
        }
    }
}
