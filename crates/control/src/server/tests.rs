use std::fs;
use std::io::{Cursor, Read};

use crate::{Client, ClientError};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct TestProfile(PathBuf);

impl TestProfile {
    fn new() -> Self {
        // Keep Unix socket names well below sockaddr_un's path limit.
        #[cfg(unix)]
        let temporary = PathBuf::from("/tmp");
        #[cfg(windows)]
        let temporary = std::env::temp_dir();
        let path = temporary.join(format!("kt-{}", uuid().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn raw_connection(endpoint: &Endpoint) -> BufReader<Stream> {
    #[cfg(unix)]
    let stream = Stream::connect(endpoint.path.as_ref().unwrap()).unwrap();
    #[cfg(windows)]
    let stream = Stream::connect((std::net::Ipv4Addr::LOCALHOST, endpoint.port)).unwrap();
    stream.set_read_timeout(Some(TEST_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TEST_TIMEOUT)).unwrap();
    BufReader::new(stream)
}

fn exchange(stream: &mut BufReader<Stream>, request: Value) -> Value {
    serde_json::to_writer(stream.get_mut(), &request).unwrap();
    stream.get_mut().write_all(b"\n").unwrap();
    let bytes = read_frame(
        stream,
        &AtomicBool::new(true),
        Some(Instant::now() + TEST_TIMEOUT),
    )
    .unwrap()
    .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn next_request(server: &Server) -> Request {
    let requests = server.requests();
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match requests.try_recv() {
            Ok(request) => return request,
            Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("could not receive the dispatched request: {error}"),
        }
    }
}

#[test]
fn real_client_authenticates_and_dispatches_without_losing_large_response_id() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let directory = endpoint::directory(&profile.0, false).unwrap();
    let descriptor = Endpoint::read(&directory).unwrap();
    let mut peer = raw_connection(&descriptor);
    let response = exchange(
        &mut peer,
        json!({
            "jsonrpc": "2.0", "id": "wrong-token", "method": "auth",
            "params": {"token": "invalid", "client_kind": "cli"}
        }),
    );
    assert_eq!(response["id"], "wrong-token");
    assert_eq!(
        response["error"]["data"]["code"],
        ErrorCode::Unauthorized.wire()
    );
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);

    let mut client = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    assert_eq!(client.instance(), descriptor.instance_id);
    thread::scope(|scope| {
        let call = scope.spawn(move || {
            let result = client.call(Method::Read.name(), &json!({"pane": "current"}));
            assert!(matches!(result, Err(ClientError::Server { code, .. }) if code == ErrorCode::TooLarge.wire()));
            let result = client.call(Method::Ping.name(), &json!({})).unwrap();
            assert_eq!(result["pong"], true);
            assert_eq!(result["instance"], client.instance());
        });
        // Minimal desktop test double. The connection, authentication, request queue and reply are real.
        let request = next_request(&server);
        assert_eq!(request.method, Method::Read);
        assert_eq!(request.instance.as_ref(), descriptor.instance_id);
        assert_eq!(request.params.string("pane").unwrap(), Some("current"));
        assert!(request.reply.active());
        request
            .reply
            .send(Ok(Value::String("a".repeat(MAX_RESULT))));
        call.join().unwrap();
    });
    server.stop();
}

#[test]
fn revoke_cancels_inflight_reply_and_stop_releases_descriptor_and_profile_lock() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let directory = endpoint::directory(&profile.0, false).unwrap();
    let descriptor = Endpoint::read(&directory).unwrap();
    let mut client = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    assert!(Server::start(&profile.0).is_err());
    assert_eq!(
        Endpoint::read(&directory).unwrap().instance_id,
        descriptor.instance_id
    );
    thread::scope(|scope| {
        let call =
            scope.spawn(move || client.call(Method::Read.name(), &json!({"pane": "current"})));
        let request = next_request(&server);
        assert!(request.reply.active());
        server.revoke();
        assert!(!server.is_active());
        assert!(!request.reply.active());
        assert!(server.requests().is_closed());
        request.reply.send(Ok(json!({"too_late": true})));
        assert!(
            matches!(call.join().unwrap(), Err(ClientError::Server { code, .. }) if code == ErrorCode::Disabled.wire())
        );
    });
    server.stop();
    assert!(!directory.join("endpoint.json").exists());
    #[cfg(unix)]
    assert!(!directory.join(endpoint::SOCKET).exists());

    let server = Server::start(&profile.0).unwrap();
    let mut client = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    assert_ne!(client.instance(), descriptor.instance_id);
    let result = client.call(
        Method::FocusPane.name(),
        &json!({"pane": "current", "instance": &descriptor.instance_id}),
    );
    assert!(
        matches!(result, Err(ClientError::Server { code, .. }) if code == ErrorCode::Stale.wire())
    );
    assert!(server.requests().is_empty());
    server.stop();
}

#[cfg(unix)]
#[test]
fn rejects_public_directory_and_descriptor_without_clobbering_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let profile = TestProfile::new();
    let control = profile.0.join("control");
    fs::create_dir(&control).unwrap();
    fs::set_permissions(&control, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(Server::start(&profile.0), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
    );

    fs::set_permissions(&control, fs::Permissions::from_mode(0o700)).unwrap();
    let descriptor_path = control.join("endpoint.json");
    fs::write(&descriptor_path, b"untrusted descriptor").unwrap();
    fs::set_permissions(&descriptor_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(Server::start(&profile.0), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert_eq!(fs::read(&descriptor_path).unwrap(), b"untrusted descriptor");
    assert!(!control.join(endpoint::SOCKET).exists());
    assert!(
        matches!(Client::connect(&profile.0, TEST_TIMEOUT), Err(ClientError::Connect(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn refuses_extended_acl_on_an_otherwise_private_directory_or_descriptor() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    fn change_acl(path: &Path, arguments: &[&str]) {
        let output = Command::new("/bin/chmod")
            .args(arguments)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let profile = TestProfile::new();
    let control = endpoint::directory(&profile.0, true).unwrap();
    change_acl(&control, &["+a", "everyone allow list,search"]);
    assert_eq!(
        fs::metadata(&control).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(
        matches!(Server::start(&profile.0), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    change_acl(&control, &["-N"]);

    let descriptor = control.join("endpoint.json");
    fs::write(&descriptor, b"disposable test fixture").unwrap();
    fs::set_permissions(&descriptor, fs::Permissions::from_mode(0o600)).unwrap();
    change_acl(&descriptor, &["+a", "everyone allow read"]);
    assert_eq!(
        fs::metadata(&descriptor).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        matches!(Server::start(&profile.0), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert_eq!(fs::read(&descriptor).unwrap(), b"disposable test fixture");
    assert!(
        matches!(Client::connect(&profile.0, TEST_TIMEOUT), Err(ClientError::Connect(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
}

#[cfg(unix)]
#[test]
fn refuses_to_connect_through_a_symlink_in_place_of_the_socket() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let directory = endpoint::directory(&profile.0, false).unwrap();
    let socket = directory.join(endpoint::SOCKET);
    let moved = directory.join("moved.sock");
    fs::rename(&socket, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &socket).unwrap();
    assert!(
        matches!(Client::connect(&profile.0, TEST_TIMEOUT), Err(ClientError::Connect(error)) if error.kind() == io::ErrorKind::InvalidData)
    );
    server.stop();
}

#[test]
fn response_remains_bounded_when_only_the_request_id_is_huge() {
    let mut stream = Vec::new();
    respond(
        &mut stream,
        Value::String("a".repeat(MAX_RESULT)),
        Ok(json!(true)),
    )
    .unwrap();
    assert!(stream.len() <= MAX_RESULT + 1);
    let response: Value = serde_json::from_slice(&stream).unwrap();
    assert_eq!(response["id"], Value::Null);
    assert_eq!(
        response["error"]["data"]["code"],
        ErrorCode::TooLarge.wire()
    );
}

// Test double for the interval inside a blocking fill_buf call.
struct ReadBoundary<F> {
    frame: Cursor<&'static [u8]>,
    before_data: F,
}

impl<F: FnMut()> Read for ReadBoundary<F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.frame.read(buffer)
    }
}

impl<F: FnMut()> BufRead for ReadBoundary<F> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        (self.before_data)();
        self.frame.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.frame.consume(amount);
    }
}

#[test]
fn bytes_arriving_after_read_deadline_are_left_unconsumed() {
    let deadline = Instant::now() + Duration::from_millis(30);
    let mut reader = ReadBoundary {
        frame: Cursor::new(b"late\n".as_slice()),
        before_data: || thread::sleep(deadline.saturating_duration_since(Instant::now())),
    };
    let error = read_frame(&mut reader, &AtomicBool::new(true), Some(deadline)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(reader.frame.position(), 0);
}

#[test]
fn bytes_arriving_after_revocation_are_left_unconsumed() {
    let active = AtomicBool::new(true);
    let mut reader = ReadBoundary {
        frame: Cursor::new(b"late\n".as_slice()),
        before_data: || active.store(false, Ordering::Release),
    };
    assert!(read_frame(&mut reader, &active, None).unwrap().is_none());
    assert_eq!(reader.frame.position(), 0);
}
