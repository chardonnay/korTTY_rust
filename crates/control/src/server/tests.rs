use std::fs;
use std::io::{Cursor, Read};

use crate::{Client, ClientError};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct TestProfile(PathBuf);

impl TestProfile {
    fn new() -> Self {
        // The profile must leave room for the control socket in sockaddr_un.
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("ks-{:016x}", u64::from_ne_bytes(nonce)));
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
fn mcp_allowlist_and_policy_are_enforced_by_the_listener() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let connect = || Client::connect_mcp(&profile.0, TEST_TIMEOUT, "fixture");
    assert!(
        matches!(connect(), Err(ClientError::Server { code, .. }) if code == ErrorCode::McpDisabled.wire())
    );
    server.set_mcp_mode(Mode::PolicyDenied);
    assert!(
        matches!(connect(), Err(ClientError::Server { code, .. }) if code == ErrorCode::Policy.wire())
    );
    server.set_mcp_mode(Mode::Read);
    let mut client = connect().unwrap();
    assert_eq!(
        client.call("mcp.capabilities", &json!({})).unwrap()["mcp_write_tools"],
        false
    );
    for method in [
        "tab.focus",
        "agent.prompt",
        "notification.show",
        "auth",
        "pane.send_text",
    ] {
        let error = client.call(method, &json!({})).unwrap_err();
        assert!(
            matches!(error, ClientError::Server { code, .. } if code == ErrorCode::McpMethod.wire())
        );
    }
    assert!(server.requests().is_empty());
    server.set_mcp_mode(Mode::Write);
    assert!(
        matches!(client.call("mcp.capabilities", &json!({})), Err(ClientError::Server { code, .. }) if code == ErrorCode::Stale.wire())
    );
    let mut client = connect().unwrap();
    assert!(
        matches!(client.call("pane.run", &json!({"pane":"@focused","command":"echo test"})), Err(ClientError::Server { code, .. }) if code == ErrorCode::InvalidParams.wire())
    );
    assert!(server.requests().is_empty());
    server.stop();
}

#[test]
fn mcp_listener_rejects_unmasked_results_and_matches_only_masked_output() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    server.set_mcp_mode(Mode::Read);
    let mut client = Client::connect_mcp(&profile.0, TEST_TIMEOUT, "fixture").unwrap();
    thread::scope(|scope| {
        let read = scope.spawn(|| client.call("pane.read", &json!({"pane":"p1"})));
        next_request(&server)
            .reply
            .send(Ok(json!({"lines":["unmasked test double"]})));
        assert!(
            matches!(read.join().unwrap(), Err(ClientError::Server { code, .. }) if code == ErrorCode::Internal.wire())
        );
    });
    thread::scope(|scope| {
        let wait = scope.spawn(|| {
            client.call(
                "pane.wait_output",
                &json!({"pane":"p1","contains":"***","lines":9999}),
            )
        });
        let request = next_request(&server);
        assert_eq!(request.method, Method::Read);
        assert_eq!(
            request.params.number("lines", 0).unwrap(),
            mcp::MAX_READ_LINES as u64
        );
        request.reply.send(Ok(json!({"pane_id":"p1","mode":"recent","lines":["known ***"],"columns":80,"rows":24,"alternate_screen":false,"osc_title":null,"truncated":false,"masked_count":1})));
        let result = wait.join().unwrap().unwrap();
        assert_eq!(result["match"], "***");
        assert_eq!(result["masked_count"], 1);
    });
    server.stop();
}

#[test]
fn subscription_events_share_the_authenticated_stream_with_request_replies() {
    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut client = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    let subscribed = client
        .call("events.subscribe", &json!({"kinds":["agent.added"]}))
        .unwrap();
    let second = client
        .call("events.subscribe", &json!({"kinds":["agent.removed"]}))
        .unwrap();
    let mut other = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    let foreign = other.call("events.subscribe", &json!({})).unwrap();
    let remaining = client
        .call(
            "events.unsubscribe",
            &json!({"subscription_id":foreign["subscription_id"]}),
        )
        .unwrap();
    assert_eq!(
        remaining["subscribed"],
        json!([subscribed["subscription_id"], second["subscription_id"]])
    );
    server
        .events()
        .publish("agent.added", json!({"pane_id":"p1"}));
    assert_eq!(client.call("ping", &json!({})).unwrap()["pong"], true);
    let event = client.next_event().unwrap();
    assert_eq!(event["method"], "event");
    assert_eq!(event["params"]["kind"], "agent.added");
    assert_eq!(event["params"]["pane_id"], "p1");
    assert!(event["params"]["at_millis"].as_u64().unwrap() > 0);
    let result = client
        .call(
            "events.unsubscribe",
            &json!({"subscription_id":subscribed["subscription_id"]}),
        )
        .unwrap();
    assert_eq!(result["subscribed"], json!([second["subscription_id"]]));
    assert_eq!(
        client.call("events.unsubscribe", &json!({})).unwrap()["subscribed"],
        json!([])
    );
    assert_eq!(client.call("ping", &json!({})).unwrap()["pong"], true);
    server.stop();
}

#[test]
fn interleaved_events_preserve_server_and_client_overflow_before_ping() {
    use crate::events::DEPTH;

    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut client = Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    let first = client
        .call("events.subscribe", &json!({"kinds":["agent.added"]}))
        .unwrap();
    let second = client
        .call("events.subscribe", &json!({"kinds":["agent.removed"]}))
        .unwrap();

    thread::scope(|scope| {
        let call = scope.spawn(|| {
            let response = client
                .call(Method::Read.name(), &json!({"pane":"p1"}))
                .unwrap();
            assert_eq!(response, json!({"lines":["test fixture"]}));
            let ping = client.call("ping", &json!({})).unwrap();
            assert_eq!(ping["pong"], true);
            assert_eq!(ping["instance"], client.instance());
        });
        // While the request is waiting for the desktop, this connection cannot
        // flush either server subscription. Both overflow before replying.
        let request = next_request(&server);
        assert_eq!(request.method, Method::Read);
        let events = server.events();
        for (kind, dropped) in [("agent.added", 5), ("agent.removed", 9)] {
            for index in 0..DEPTH + dropped {
                events.publish(kind, json!({"pane_id":"p1","index":index}));
            }
        }
        request.reply.send(Ok(json!({"lines":["test fixture"]})));
        call.join().unwrap();
    });

    let first_overflow = client.next_event().unwrap();
    assert_eq!(first_overflow["params"]["kind"], "events.overflow");
    assert_eq!(
        first_overflow["params"]["subscription_id"],
        first["subscription_id"]
    );
    assert_eq!(first_overflow["params"]["dropped"], 5);
    assert!(first_overflow["params"].get("source").is_none());

    let client_overflow = client.next_event().unwrap();
    assert_eq!(client_overflow["params"]["kind"], "events.overflow");
    assert_eq!(client_overflow["params"]["source"], "client");
    assert_eq!(client_overflow["params"]["dropped"], DEPTH);
    assert!(client_overflow["params"].get("subscription_id").is_none());
    assert!(
        client_overflow["params"]
            .get("server_markers_dropped")
            .is_none()
    );

    let second_overflow = client.next_event().unwrap();
    assert_eq!(second_overflow["params"]["kind"], "events.overflow");
    assert_eq!(
        second_overflow["params"]["subscription_id"],
        second["subscription_id"]
    );
    assert_eq!(second_overflow["params"]["dropped"], 9);
    assert!(second_overflow["params"].get("source").is_none());

    for index in 9..DEPTH + 9 {
        let event = client.next_event().unwrap();
        assert_eq!(event["params"]["kind"], "agent.removed");
        assert_eq!(event["params"]["index"], index);
    }
    assert_eq!(client.call("ping", &json!({})).unwrap()["pong"], true);
    server
        .events()
        .publish("agent.added", json!({"pane_id":"p1","index":DEPTH+5}));
    assert_eq!(client.next_event().unwrap()["params"]["index"], DEPTH + 5);
    server.stop();
}

#[test]
fn notification_normalizes_payload_before_per_connection_rate_limit() {
    fn record(
        server: &Server,
        client: &mut Client,
        input: Value,
        expected_title: &str,
        expected_body: &str,
        supported: bool,
    ) {
        thread::scope(|scope| {
            let call = scope.spawn(|| client.call(Method::Notify.name(), &input));
            let request = next_request(server);
            assert_eq!(request.method, Method::Notify);
            assert_eq!(request.params.required("title").unwrap(), expected_title);
            assert_eq!(request.params.required("body").unwrap(), expected_body);
            let result = json!({"shown":true,"supported":supported});
            request.reply.send(Ok(result.clone()));
            assert_eq!(call.join().unwrap().unwrap(), result);
        });
    }

    fn assert_busy(client: &mut Client) {
        let error = client
            .call(
                Method::Notify.name(),
                &json!({"title":"Build","body":"Ready"}),
            )
            .unwrap_err();
        let ClientError::Server { code, rpc, .. } = error else {
            panic!("Expected a server rate-limit error, got {error:?}");
        };
        assert_eq!(code, ErrorCode::Busy.wire());
        let retry = rpc["data"]["retry_after_millis"].as_u64().unwrap();
        assert!((1..=NOTIFY_INTERVAL.as_millis() as u64).contains(&retry));
    }

    let profile = TestProfile::new();
    let server = Server::start(&profile.0).unwrap();
    let connect = || Client::connect(&profile.0, TEST_TIMEOUT).unwrap();
    let mut first = connect();
    let title = "🙂".repeat(39);
    let body = "🧪".repeat(100);
    record(
        &server,
        &mut first,
        json!({"title":format!(" \t\u{202e}{title}\u{200b}x🙂too long \u{feff}"),
            "body":format!("\u{200e}\t{body}\0too long \n")}),
        &format!("korTTY · {title}x"),
        &body,
        true,
    );
    for (params, name) in [
        (json!({"title":" \t", "body":"\n"}), Some("title")),
        (
            json!({"title":"\u{202e}", "body":"\u{200b}\0"}),
            Some("title"),
        ),
        (json!({"title":12, "body":"ready"}), Some("title")),
        (json!({"title":"Build", "body":null}), Some("body")),
        (json!({"title":"Build"}), Some("body")),
        (
            json!({"title":"Build", "body":"ready", "instance":"extra"}),
            None,
        ),
    ] {
        let error = first.call(Method::Notify.name(), &params).unwrap_err();
        let ClientError::Server { code, rpc, .. } = error else {
            panic!("Expected a server parameter error, got {error:?}");
        };
        assert_eq!(code, ErrorCode::InvalidParams.wire());
        assert_eq!(rpc["data"]["param"].as_str(), name);
    }
    assert_busy(&mut first);

    let mut title_only = connect();
    record(
        &server,
        &mut title_only,
        json!({"title":"  Build finished  ","body":""}),
        "korTTY · Build finished",
        "",
        false,
    );
    assert_busy(&mut title_only);
    let mut body_only = connect();
    record(
        &server,
        &mut body_only,
        json!({"title":"","body":"  Ready  "}),
        "korTTY · ",
        "Ready",
        true,
    );
    assert!(server.requests().is_empty());
    server.stop();
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
        drop(request);
        let result = call.join().unwrap();
        assert!(
            matches!(&result, Err(ClientError::Server { code, .. }) if code == ErrorCode::Disabled.wire()),
            "Expected a disabled control reply after revocation, got {result:?}"
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
