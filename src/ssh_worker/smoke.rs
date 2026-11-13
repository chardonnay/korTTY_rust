use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kortty_terminal::TerminalProfile;

use crate::session_security::IsolationLevel;
use crate::ssh_dialog::{Authentication, LoginRequest, LoginTarget, Password};

use super::*;

pub(super) mod exec;

const GRID: WindowSize = WindowSize {
    columns: 80,
    rows: 24,
    pixel_width: 0,
    pixel_height: 0,
};

fn worker_path() -> PathBuf {
    std::env::var_os("KORTTY_SSH_TEST_EXE")
        .map(PathBuf::from)
        .expect("set KORTTY_SSH_TEST_EXE to the built kortty executable")
        .canonicalize()
        .expect("the kortty executable must exist")
}

fn request(port: u16) -> Request {
    Request {
        target: LoginRequest {
            endpoint: LoginTarget {
                host: "127.0.0.1".into(),
                port,
                username: "worker-probe".into(),
            },
            auth: Authentication::Password(Password::new("disposable test password".into())),
            keyboard_interactive: false,
        },
        jump: None,
        profile: TerminalProfile::Xterm,
        isolation: IsolationLevel::Process,
        options: Default::default(),
    }
}

#[test]
#[ignore = "requires the actual kortty executable"]
fn worker_pipe_smoke_rejects_refused_loopback() {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let refused = TcpStream::connect(address).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);

    match Session::dial_executable(
        request(address.port()),
        GRID,
        false,
        &DialCancellation::default(),
        &worker_path(),
        &TrustStore::from_pins(Vec::new()).unwrap(),
        None,
    ) {
        Err(ConnectError::Io(error)) => {
            // The worker sends the real OS error through its response pipe.
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), refused.to_string());
        }
        Err(error) => panic!("expected the worker's I/O error, got {error}"),
        Ok(_) => panic!("unexpected SSH connection on a closed loopback port"),
    }
}

#[test]
#[ignore = "requires the actual kortty executable"]
fn worker_exits_when_its_parent_process_dies() {
    const STALL_PORT: &str = "KORTTY_SSH_TEST_STALL_PORT";
    if let Ok(port) = std::env::var(STALL_PORT) {
        let _ = Session::dial_executable(
            request(port.parse().unwrap()),
            GRID,
            false,
            &DialCancellation::default(),
            &worker_path(),
            &TrustStore::from_pins(Vec::new()).unwrap(),
            None,
        );
        return;
    }

    struct Stop(Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut parent = Stop(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "ssh_worker::smoke::worker_exits_when_its_parent_process_dies",
            ])
            .env("KORTTY_SSH_TEST_EXE", worker_path())
            .env(STALL_PORT, port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut peer = loop {
        match listener.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "SSH worker never connected");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("test listener failed: {error}"),
        }
    };
    // This deliberately stalled test peer never sends an SSH banner. The
    // transport would otherwise remain in the startup handshake until timeout.
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    parent.0.kill().unwrap();
    parent.0.wait().unwrap();
    match peer.read_to_end(&mut Vec::new()) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("SSH worker outlived its parent: {error}"),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
#[test]
fn unavailable_sandbox_refuses_before_connecting_or_spawning() {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let mut request = request(socket.local_addr().unwrap().port());
    request.isolation = IsolationLevel::Sandbox;
    let result = Session::dial_executable(
        request,
        GRID,
        false,
        &DialCancellation::default(),
        std::path::Path::new("unused-test-executable"),
        &TrustStore::from_pins(Vec::new()).unwrap(),
        None,
    );
    assert!(matches!(
        result,
        Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::Unsupported
    ));
    assert_eq!(
        socket.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[cfg(windows)]
#[test]
fn failed_windows_sandbox_launch_leaves_the_approved_endpoint_untouched() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut request = request(listener.local_addr().unwrap().port());
    request.isolation = IsolationLevel::Sandbox;
    let executable = std::env::current_exe()
        .unwrap()
        .with_file_name("missing-worker.exe");
    assert!(!executable.exists());
    assert!(
        Session::dial_executable(
            request,
            GRID,
            false,
            &DialCancellation::default(),
            &executable,
            &TrustStore::from_pins(Vec::new()).unwrap(),
            None,
        )
        .is_err()
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires the actual packaged kortty executable and an unelevated Windows desktop user"]
fn windows_sandbox_relay_reaches_the_approved_peer_and_cancels() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut request = request(listener.local_addr().unwrap().port());
    request.isolation = IsolationLevel::Sandbox;
    let cancellation = DialCancellation::default();
    let cancel = CancelOnDrop(cancellation.clone());
    let executable = worker_path();
    let worker = thread::spawn(move || {
        Session::dial_executable(
            request,
            GRID,
            false,
            &cancellation,
            &executable,
            &TrustStore::from_pins(Vec::new()).unwrap(),
            None,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut peer = loop {
        match listener.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if worker.is_finished() {
                    match worker.join().unwrap() {
                        Ok(_) => panic!("the worker reported an SSH session without a peer"),
                        Err(error) => panic!("the confined worker failed before dialing: {error}"),
                    }
                }
                assert!(Instant::now() < deadline, "confined worker never connected");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("test listener failed: {error}"),
        }
    };
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    // Test double for a peer stalling before key exchange. Both streams
    // use the real worker and the actual OS relay.
    peer.write_all(b"SSH-2.0-kortty-fixture\r\n").unwrap();
    let mut banner = [0; 8];
    peer.read_exact(&mut banner).unwrap();
    assert_eq!(&banner, b"SSH-2.0-");
    drop(cancel);
    match peer.read_to_end(&mut Vec::new()) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("relay outlived cancellation: {error}"),
    }
    // Keep the peer alive to check that cancelling the desktop's relay also
    // interrupts its pending TCP read without a close from the other side.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "relay threads did not stop promptly"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        worker.join().unwrap(),
        Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::Interrupted
    ));
}

#[cfg(windows)]
#[test]
#[ignore = "requires the packaged Windows app and a disposable OpenSSH fixture"]
fn windows_sandbox_relay_authenticates_shell_and_sftp_and_rejects_a_changed_host_key() {
    use kortty_ssh::{HostKey, sftp::Sftp};

    let required = |key| std::env::var(key).unwrap_or_else(|_| panic!("set {key} for the fixture"));
    let (shell_command, exit_code): (&[u8], u32) =
        match std::env::var("KORTTY_WINDOWS_RELAY_SERVER_SHELL").as_deref() {
            Ok("cmd") => (b"echo relay-shell-rea^dy&exit 0\r\n", 0),
            Ok("posix") | Err(std::env::VarError::NotPresent) => {
                (b"printf '\\n%s%s\\n' 'relay-shell-' 'ready'; exit 23\n", 23)
            }
            _ => panic!("set KORTTY_WINDOWS_RELAY_SERVER_SHELL to cmd or posix"),
        };
    let fixture = PathBuf::from(required("KORTTY_WINDOWS_RELAY_FIXTURE"));
    let address: std::net::SocketAddr = required("KORTTY_WINDOWS_RELAY_ADDRESS").parse().unwrap();
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let username = required("KORTTY_WINDOWS_RELAY_USER");
    let host_key = |file| {
        let key = std::fs::read_to_string(fixture.join(file)).unwrap();
        HostKey::from_openssh(key.trim()).unwrap()
    };
    let dial = |pin, sftp| {
        let mut request = request(address.port());
        request.isolation = IsolationLevel::Sandbox;
        request.target.endpoint.host = address.ip().to_string();
        request.target.endpoint.username.clone_from(&username);
        request.target.auth = Authentication::PrivateKey(fixture.join("client"));
        Session::dial_executable(
            request,
            GRID,
            sftp,
            &DialCancellation::default(),
            &worker_path(),
            &TrustStore::from_pins(vec![(endpoint.clone(), host_key(pin))]).unwrap(),
            None,
        )
    };

    let terminal = dial("host.pub", false).unwrap();
    if std::env::var_os("KORTTY_WINDOWS_RELAY_TEST_FORWARDING").is_some() {
        assert!(address.ip().is_loopback());
        verify_windows_relay_forwarding(terminal, shell_command, exit_code);
    } else {
        verify_windows_relay_shell(terminal, shell_command, exit_code);
    }

    let mut worker = dial("host.pub", true).unwrap();
    assert_eq!(worker.sandbox_backend(), Some("LPAC + Job"));
    let mut sftp = Sftp::new(worker.take_reader().unwrap(), worker.take_writer().unwrap()).unwrap();
    let cwd = sftp.canonicalize(".").unwrap();
    assert!(
        cwd.starts_with(b"/")
            || matches!(cwd.as_slice(), [drive, b':', b'/', ..] if drive.is_ascii_alphabetic()),
        "expected an absolute remote working directory, got {cwd:?}"
    );
    if let Ok(path) = std::env::var("KORTTY_WINDOWS_RELAY_SFTP_FILE") {
        let expected = std::fs::read(fixture.join("sftp-sample")).unwrap();
        assert!(!expected.is_empty());
        let mut actual = Vec::new();
        assert_eq!(
            sftp.download(path, &mut actual).unwrap(),
            expected.len() as u64
        );
        assert_eq!(actual, expected);
    }
    drop(sftp);
    worker.close().unwrap();

    assert!(matches!(
        dial("rotated.pub", false),
        Err(ConnectError::ChangedHostKey { endpoint: actual, expected, offered })
            if actual == endpoint && expected == host_key("rotated.pub") && offered == host_key("host.pub")
    ));

    let destination_port = required("KORTTY_WINDOWS_RELAY_JUMP_TARGET_PORT")
        .parse()
        .unwrap();
    let destination = Endpoint::new("127.0.0.1", destination_port).unwrap();
    let mut request = request(destination_port);
    request.isolation = IsolationLevel::Sandbox;
    request.target.endpoint.username.clone_from(&username);
    request.target.auth = Authentication::PrivateKey(fixture.join("client"));
    request.jump = Some(LoginRequest {
        endpoint: LoginTarget {
            host: address.ip().to_string(),
            port: address.port(),
            username,
        },
        auth: Authentication::PrivateKey(fixture.join("client")),
        keyboard_interactive: false,
    });
    let pins = vec![
        (endpoint, host_key("host.pub")),
        (destination, host_key("rotated.pub")),
    ];
    let worker = Session::dial_executable(
        request,
        GRID,
        false,
        &DialCancellation::default(),
        &worker_path(),
        &TrustStore::from_pins(pins).unwrap(),
        None,
    )
    .unwrap();
    verify_windows_relay_shell(worker, shell_command, exit_code);
}

#[cfg(windows)]
fn verify_windows_relay_forwarding(worker: Session, shell_command: &[u8], exit_code: u32) {
    use kortty_ssh::{MAX_FORWARD_STREAMS, MAX_REMOTE_FORWARD_LISTENERS, sftp::Sftp};

    use crate::ssh_connection::DirectOpener;
    use crate::ssh_tunnel::{Capacity, Destination, Listener};

    let loopback = "127.0.0.1:0".parse().unwrap();
    let service = TcpListener::bind(loopback).unwrap();
    service.set_nonblocking(true).unwrap();
    let opener = worker.opener().unwrap();
    let registrations = (0..MAX_REMOTE_FORWARD_LISTENERS)
        .map(|_| opener.bind(loopback, || false).unwrap())
        .collect::<Vec<_>>();
    let (listener, _updates) = Listener::start(
        DirectOpener::Worker(opener.clone()),
        loopback,
        Destination::Fixed(
            Endpoint::new("127.0.0.1", service.local_addr().unwrap().port()).unwrap(),
        ),
        Capacity::default(),
    )
    .unwrap();
    let mut streams = Vec::with_capacity(MAX_FORWARD_STREAMS);
    for index in 0..MAX_FORWARD_STREAMS {
        let mut client = TcpStream::connect(listener.bound_address()).unwrap();
        let mut server = accept_forward_service(&service);
        server.set_nonblocking(false).unwrap();
        for socket in [&client, &server] {
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
        }
        let marker = u8::try_from(index).unwrap();
        let mut byte = [0];
        client.write_all(&[marker]).unwrap();
        server.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [marker]);
        server.write_all(&[!marker]).unwrap();
        client.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [!marker]);
        streams.push((client, server));
    }

    for address in [listener.bound_address(), registrations[0].remote_addr()] {
        let mut overflow = TcpStream::connect(address).unwrap();
        overflow
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        match overflow.read(&mut [0]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("forward above the shared budget was not closed: {result:?}"),
        }
    }

    let mut channel = opener.open_sftp(|| false).unwrap();
    let mut sftp = Sftp::new(
        channel.take_reader().unwrap(),
        channel.take_writer().unwrap(),
    )
    .unwrap();
    assert!(!sftp.canonicalize(".").unwrap().is_empty());
    verify_windows_relay_shell(worker, shell_command, exit_code);
    drop((sftp, channel, listener, streams, registrations));
}

#[cfg(windows)]
fn accept_forward_service(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("forward did not reach the test service: {error}"),
        }
    }
}

#[cfg(windows)]
fn verify_windows_relay_shell(mut worker: Session, command: &[u8], exit_code: u32) {
    use std::sync::mpsc;

    assert_eq!(worker.sandbox_backend(), Some("LPAC + Job"));
    let mut writer = worker.take_writer().unwrap();
    writer.write_all(command).unwrap();
    let mut reader = worker.take_reader().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let read = thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output).map(|_| output);
        let _ = send.send(result);
    });
    let output = receive
        .recv_timeout(Duration::from_secs(8))
        .unwrap()
        .unwrap();
    read.join().unwrap();
    assert!(String::from_utf8_lossy(&output).contains("relay-shell-ready"));
    assert_eq!(worker.exit_code(), Some(exit_code));
    worker.close().unwrap();
}
