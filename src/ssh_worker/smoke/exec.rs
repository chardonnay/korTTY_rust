//! Opt-in test against the same explicitly authorized host as the direct exec test.

use std::net::SocketAddr;
use std::sync::Arc;

use kortty::ssh_runtime::{mux::PasswordEvent, password};
use kortty_ssh::{ConnectOptions, Endpoint, HostKey, KeepAlive, SensitiveLine};

use super::*;
use crate::ssh_connection::ExecOpener;
use crate::terminal_agent::{Cancel, Status, Target};

#[path = "exec/folder_usage.rs"]
mod folder_usage;

#[cfg(unix)]
#[path = "../../../crates/ssh/tests/support/liveness_link.rs"]
mod liveness_link;

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE and explicit KORTTY_SSH_EXEC_* host fixtures"]
fn isolated_worker_exec_preserves_shell_and_separates_output() {
    let mut worker = fixture_worker();
    let opener = worker.opener().unwrap();
    let mut shell_input = worker.take_writer().unwrap();
    let mut shell_output = worker.take_reader().unwrap();
    let target = Target::Ssh {
        opener: ExecOpener::Worker(opener.clone()),
        directory: "/tmp".into(),
    };
    let outcome = target
        .clone()
        .execute(
            "printf 'exec-out'; printf 'exec-err' >&2; exit 23",
            &Cancel::default(),
        )
        .unwrap();
    assert_eq!(outcome.status, Status::Exited(23));
    assert_eq!(&*outcome.stdout.text(), "exec-out");
    assert_eq!(&*outcome.stderr.text(), "exec-err");
    assert_shell_usable(&mut shell_input, &mut shell_output, "after-exec");

    let mut channel = opener
        .open_exec("printf ready; sleep 1; printf finished", || false)
        .unwrap();
    let _ = channel.take_writer().unwrap().close_input();
    let mut reader = channel.take_reader().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut ready = Vec::new();
    let mut buffer = [0; wire::CHUNK - 3];
    while ready.len() < b"ready".len() {
        let mut header = [0; 3];
        read_exact_before(&mut reader, &mut header, deadline);
        assert_eq!(header[0], 0, "the marker belongs to stdout");
        let count = u16::from_be_bytes([header[1], header[2]]) as usize;
        assert!((1..=buffer.len()).contains(&count));
        read_exact_before(&mut reader, &mut buffer[..count], deadline);
        ready.extend_from_slice(&buffer[..count]);
    }
    assert!(ready.starts_with(b"ready"));
    let abort = channel.abort_handle();
    abort.abort();
    assert!(abort.is_closed());
    drop(channel);
    assert_shell_usable(&mut shell_input, &mut shell_output, "after-cancel");

    // The command is finite even if the remote server does not terminate an
    // already-started process immediately when the exec channel closes.
    thread::sleep(Duration::from_millis(1200));
    let outcome = target
        .execute("printf reopened", &Cancel::default())
        .unwrap();
    assert_eq!(outcome.status, Status::Exited(0));
    assert_eq!(&*outcome.stdout.text(), "reopened");
    worker.close().unwrap();
    assert!(opener.is_closed());
}

fn fixture_worker() -> Session {
    fixture_worker_mode(false)
}

fn fixture_worker_mode(sftp: bool) -> Session {
    fixture_worker_with_options(sftp, ConnectOptions::default())
}

fn fixture_worker_with_options(sftp: bool, options: ConnectOptions) -> Session {
    let address: SocketAddr = required("ADDRESS").parse().unwrap();
    fixture_worker_at(sftp, options, address)
}

fn fixture_worker_at(sftp: bool, options: ConnectOptions, address: SocketAddr) -> Session {
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let request = Request {
        target: LoginRequest {
            endpoint: LoginTarget {
                host: address.ip().to_string(),
                port: address.port(),
                username: required("USER"),
            },
            auth: Authentication::PrivateKey(PathBuf::from(required("PRIVATE_KEY"))),
            keyboard_interactive: false,
        },
        jump: None,
        profile: TerminalProfile::Xterm,
        isolation: IsolationLevel::Process,
        options,
    };
    let pins = TrustStore::from_pins(vec![(
        endpoint,
        HostKey::from_openssh(&required("HOST_KEY")).unwrap(),
    )])
    .unwrap();
    Session::dial_executable(
        request,
        GRID,
        sftp,
        &DialCancellation::default(),
        &worker_path(),
        &pins,
        None,
    )
    .unwrap()
}

#[cfg(unix)]
#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE and explicit KORTTY_SSH_EXEC_* OpenSSH fixtures"]
fn isolated_worker_reports_liveness_timeout_from_its_owned_connection() {
    use std::sync::atomic::Ordering;

    let upstream: SocketAddr = required("ADDRESS").parse().unwrap();
    assert!(
        upstream.ip().is_loopback(),
        "use an owned loopback OpenSSH fixture"
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let link = Arc::new(liveness_link::Link::default());
    let relay = Arc::clone(&link);
    let proxy = thread::spawn(move || relay.run(listener, upstream));
    let mut worker = fixture_worker_at(false, ConnectOptions::default(), address);
    let mut input = worker.take_writer().unwrap();
    let mut output = worker.take_reader().unwrap();
    assert_shell_usable(&mut input, &mut output, "before-liveness");
    thread::sleep(Duration::from_secs(4));
    assert_shell_usable(&mut input, &mut output, "after-liveness");
    link.blackhole.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(11);
    let error = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "isolated liveness deadline was not enforced"
        );
        match output.read_timeout(&mut [0; 1024], remaining) {
            Err(error) => break error,
            Ok(0) => panic!("the isolated connection ended without its liveness result"),
            Ok(_) => {}
        }
    };
    assert!(
        error.to_string().contains("SSH server stopped responding"),
        "{error}"
    );
    worker.close().unwrap();
    proxy.join().unwrap().unwrap();
}

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE and explicit KORTTY_SSH_EXEC_* host fixtures"]
fn isolated_worker_keepalive_preserves_shell_and_sftp() {
    let options = ConnectOptions::default().with_keepalive(Some(KeepAlive::new(true, 5).unwrap()));
    let mut worker = fixture_worker_with_options(false, options);
    let mut input = worker.take_writer().unwrap();
    let mut output = worker.take_reader().unwrap();
    assert_shell_usable(&mut input, &mut output, "before-keepalive");
    thread::sleep(Duration::from_secs(6));
    let mut lease = worker.opener().unwrap().open_sftp(|| false).unwrap();
    let mut sftp =
        kortty_ssh::sftp::Sftp::new(lease.take_reader().unwrap(), lease.take_writer().unwrap())
            .unwrap();
    assert_eq!(
        sftp.stat(".").unwrap().kind(),
        kortty_ssh::sftp::FileKind::Directory
    );
    drop(sftp);
    drop(lease);
    assert_shell_usable(&mut input, &mut output, "after-keepalive");
    worker.close().unwrap();
}

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE and an OpenSSH SFTP fixture"]
fn isolated_parallel_sftp_channels_cancel_without_closing_the_original_route() {
    use kortty_ssh::{MAX_SFTP_LEASES, sftp::Sftp};

    for standalone in [false, true] {
        let mut worker = fixture_worker_mode(standalone);
        let mut input = worker.take_writer().unwrap();
        let mut output = worker.take_reader().unwrap();
        let mut connection = crate::ssh_connection::Connected::Worker(Box::new(worker));
        let opener = connection.sftp_opener().unwrap();
        let count = MAX_SFTP_LEASES - usize::from(standalone);
        let mut channels = (0..count)
            .map(|_| {
                let mut channel = opener.open_cancellable(|| false).unwrap();
                let client = Sftp::new(
                    channel.take_reader().unwrap(),
                    channel.take_writer().unwrap(),
                )
                .unwrap();
                (channel, client)
            })
            .collect::<Vec<_>>();
        let (cancelled, mut cancelled_client) = channels.pop().unwrap();
        let abort = cancelled.abort_handle();
        let home = cancelled_client.canonicalize(b".").unwrap();
        abort.abort();
        assert!(cancelled_client.canonicalize(b".").is_err());
        for (_, client) in &mut channels {
            assert_eq!(client.canonicalize(b".").unwrap(), home);
        }
        let mut replacement = opener.open_cancellable(|| false).unwrap();
        let mut replacement_client = Sftp::new(
            replacement.take_reader().unwrap(),
            replacement.take_writer().unwrap(),
        )
        .unwrap();
        assert_eq!(replacement_client.canonicalize(b".").unwrap(), home);
        if standalone {
            let mut primary = Sftp::new(output, input).unwrap();
            assert_eq!(primary.canonicalize(b".").unwrap(), home);
        } else {
            assert_shell_usable(&mut input, &mut output, "after-parallel-sftp-cancel");
        }
        connection.close().unwrap();
    }
}

#[test]
#[ignore = "requires an explicit OpenSSH fixture with the private privileged helper and mock sudo"]
fn isolated_worker_privileged_file_round_trip_on_attached_and_standalone_routes() {
    let path = std::env::var("KORTTY_SSH_FILE_FIXTURE_PATH")
        .expect("supply the canonical path of the owned privileged edit fixture");
    assert_eq!(
        path, "/fixtures/test.txt",
        "use the disposable fixture path"
    );

    for standalone in [false, true] {
        let mut worker = fixture_worker_mode(standalone);
        let opener = worker.exec_opener().unwrap();
        assert_eq!(
            opener.verified_host_key(),
            Some(&HostKey::from_openssh(&required("HOST_KEY")).unwrap())
        );
        let route = ExecOpener::Worker(opener.clone());
        if standalone {
            assert!(worker.opener().is_none());
            let mut primary = kortty_ssh::sftp::Sftp::new(
                worker.take_reader().unwrap(),
                worker.take_writer().unwrap(),
            )
            .unwrap();
            let home = primary.canonicalize(b".").unwrap();
            round_trip_privileged_file(&route, &path);
            assert_eq!(primary.canonicalize(b".").unwrap(), home);
        } else {
            let mut input = worker.take_writer().unwrap();
            let mut output = worker.take_reader().unwrap();
            assert_shell_usable(&mut input, &mut output, "before-privileged-edit");
            round_trip_privileged_file(&route, &path);
            assert_shell_usable(&mut input, &mut output, "after-privileged-edit");
        }
        worker.close().unwrap();
        assert!(opener.is_closed());
    }
}

fn round_trip_privileged_file(opener: &ExecOpener, path: &str) {
    use crate::sftp_window::privileged_test::{Auth, read, write};

    const BEFORE: &str = "before\n";
    const EDITED: &str = "kortty file fixture · café 東京\n";
    let password = || Auth::Password(SensitiveLine::new("kortty-file-fixture").unwrap());
    let allowed = || true;
    let initial = read(opener.clone(), path, Auth::Passwordless, allowed).unwrap();
    assert_eq!(initial.path(), path);
    assert_eq!(&**initial.text(), BEFORE);

    assert!(
        write(
            opener.clone(),
            &initial,
            Arc::from("bad password must not publish"),
            Auth::Password(SensitiveLine::new("wrong-fixture-password").unwrap()),
            allowed,
        )
        .is_err()
    );
    let authenticated = read(opener.clone(), path, password(), allowed).unwrap();
    assert_eq!(&**authenticated.text(), BEFORE);
    let saved = write(
        opener.clone(),
        &authenticated,
        Arc::from(EDITED),
        password(),
        allowed,
    )
    .unwrap();
    assert_eq!(&**saved.text(), EDITED);

    let verified = read(opener.clone(), path, Auth::Passwordless, allowed).unwrap();
    assert_eq!(&**verified.text(), EDITED);
    assert!(
        write(
            opener.clone(),
            &initial,
            Arc::from("stale version must not publish"),
            Auth::Passwordless,
            allowed,
        )
        .is_err()
    );
    write(
        opener.clone(),
        &verified,
        Arc::from(BEFORE),
        Auth::Passwordless,
        allowed,
    )
    .unwrap();
    let restored = read(opener.clone(), path, password(), allowed).unwrap();
    assert_eq!(&**restored.text(), BEFORE);
}

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE and explicit KORTTY_SSH_EXEC_* host fixtures"]
fn isolated_worker_password_exec_commits_one_line_and_never_relays_output() {
    assert_password_exec(fixture_worker());
}

pub(in crate::ssh_worker) fn assert_password_exec(mut worker: Session) {
    let opener = worker.opener().unwrap();
    let mut shell_input = worker.take_writer().unwrap();
    let mut shell_output = worker.take_reader().unwrap();
    let nonce = [19; 16];
    let marker = password::marker(nonce);
    let next = |channel: &crate::ssh_worker::client::PasswordChannel| {
        channel.next(Duration::from_secs(10)).unwrap().unwrap()
    };
    let open = |wire: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match opener.open_password_exec(wire, nonce, || false) {
                Ok(channel) => break channel,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(40)),
                Err(error) => panic!("Could not open password test lease: {error}"),
            }
        }
    };
    let stdout_only = open(&format!("printf '%s' '{marker}'; exit 17"));
    assert_eq!(
        next(&stdout_only),
        PasswordEvent::Finished(password::Status::Exited(17))
    );
    drop(stdout_only);

    // The mock accepts exactly one line and EOF, then deliberately echoes the fixture.
    // No real sudo is invoked, and a stalled peer cannot leave a test process behind.
    let script = format!(
        "printf '%s' '{}' >&2; sleep 0.1; printf '%s' '{}' >&2; IFS= read -r value || exit 70; test \"$value\" = 'synthetic-only' || exit 71; IFS= read -r extra && exit 72; printf '%s' \"$value\"; printf '%s' \"$value\" >&2; exit 29",
        &marker[..14],
        &marker[14..],
    );
    let wire = format!(
        "exec /usr/bin/timeout 6 /bin/sh -c '{}'",
        script.replace('\'', "'\\''")
    );
    let channel = open(&wire);
    assert_eq!(next(&channel), PasswordEvent::Challenge);
    channel
        .stage(SensitiveLine::new("synthetic-only").unwrap(), || true)
        .unwrap();
    assert_eq!(next(&channel), PasswordEvent::Staged);
    channel.commit(|| true).unwrap();
    assert_eq!(
        next(&channel),
        PasswordEvent::Finished(password::Status::Exited(29))
    );
    drop(channel);

    let cancelled = open(&wire);
    assert_eq!(next(&cancelled), PasswordEvent::Challenge);
    cancelled
        .stage(SensitiveLine::new("synthetic-only").unwrap(), || true)
        .unwrap();
    assert_eq!(next(&cancelled), PasswordEvent::Staged);
    drop(cancelled);
    assert_shell_usable(&mut shell_input, &mut shell_output, "after-password-cancel");
    // Allow the finite mock to exit even on a server that retains the process after close.
    thread::sleep(Duration::from_millis(6100));
    worker.close().unwrap();
}

fn required(suffix: &str) -> String {
    std::env::var(format!("KORTTY_SSH_EXEC_{suffix}"))
        .expect("provide the explicit live SSH fixture")
}

fn read_exact_before(reader: &mut Reader, mut buffer: &mut [u8], deadline: Instant) {
    while !buffer.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "isolated exec marker timed out");
        let count = reader.read_timeout(buffer, left).unwrap();
        assert_ne!(count, 0, "isolated exec closed before its marker");
        buffer = &mut buffer[count..];
    }
}

fn assert_shell_usable(input: &mut Writer, output: &mut Reader, label: &str) {
    writeln!(
        input,
        "HISTFILE=/dev/null; printf '%s%s\\n' 'kortty-worker-' '{label}'"
    )
    .unwrap();
    input.flush().unwrap();
    let wanted = format!("kortty-worker-{label}");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut received = Vec::new();
    let mut buffer = [0; 2048];
    while !received
        .windows(wanted.len())
        .any(|window| window == wanted.as_bytes())
    {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the original isolated SSH shell did not respond"
        );
        let count = output.read_timeout(&mut buffer, left).unwrap();
        assert_ne!(count, 0, "the original shell closed with its exec channel");
        received.extend_from_slice(&buffer[..count]);
        assert!(received.len() < 64 * 1024);
    }
}
