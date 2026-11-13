use std::fs;
use std::io::{BufRead as _, BufReader};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kortty_control::mcp::Mode;
use kortty_control::{
    Client, ClientError, ErrorCode, Fault, Method, Request, Server, TabCreateParams,
    tab_create_result,
};
use kortty_pty::Pty;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: usize = 128 * 1024;
const CLI: &str = env!("CARGO_BIN_EXE_kortty-cli");

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        // The profile must leave room for the control socket in sockaddr_un.
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("ki-{:016x}", u64::from_ne_bytes(nonce)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        }
        #[cfg(windows)]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn next_request(server: &Server, output: Option<&Mutex<Vec<u8>>>) -> Request {
    let requests = server.requests();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match requests.try_recv() {
            Ok(request) => return request,
            Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                let terminal =
                    output.map_or_else(Vec::new, |output| output.lock().unwrap().clone());
                panic!(
                    "CLI request did not arrive: {error}; terminal: {}",
                    String::from_utf8_lossy(&terminal)
                );
            }
        }
    }
}

struct McpChild {
    process: Child,
    stdin: ChildStdin,
    replies: std::sync::mpsc::Receiver<Value>,
    reader: Option<thread::JoinHandle<()>>,
}

impl McpChild {
    fn start(root: &Path, timeout_ms: Option<u64>) -> Self {
        let mut command = Command::new(CLI);
        command.args(["mcp", "--config-dir"]).arg(root);
        if let Some(timeout_ms) = timeout_ms {
            command.arg("--timeout").arg(timeout_ms.to_string());
        }
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let (reply, replies) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let value = serde_json::from_str(&line).expect("stdio carries only MCP JSON");
                if reply.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            process,
            stdin,
            replies,
            reader: Some(reader),
        }
    }

    fn send(&mut self, request: Value) {
        serde_json::to_writer(&mut self.stdin, &request).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        self.replies.recv_timeout(TIMEOUT).unwrap()
    }

    fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{
                "protocolVersion":"2025-06-18",
                "capabilities":{},
                "clientInfo":{"name":"unverified\nfixture","version":"1"}
            }
        }));
        assert_eq!(self.recv()["result"]["protocolVersion"], "2025-06-18");
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    }

    fn tool(&mut self, id: u64, name: &str, arguments: Value) {
        self.send(json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        }));
    }

    fn recv_tool(&self, id: u64, error: bool) -> Value {
        let reply = self.recv();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], error, "{reply}");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}

impl Drop for McpChild {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = self.reader.take().unwrap().join();
    }
}

fn answer_panes(child: &McpChild, server: &Server, id: u64, local_shell_only: bool) {
    let request = next_request(server, None);
    assert_eq!(request.method, Method::Panes);
    assert_eq!(request.instance.as_ref(), server.instance());
    assert_eq!(
        request.params.flag("local_shell_only", false).unwrap(),
        local_shell_only
    );
    let panes = json!({"instance":server.instance(),"panes":[{"pane_id":"p1"}],"masked_count":0});
    request.reply.send(Ok(panes.clone()));
    assert_eq!(child.recv_tool(id, false), panes);
}

fn replace_listener(server: Server, root: &Path) -> Server {
    let old_instance = server.instance().to_owned();
    server.stop();
    let replacement = Server::start(root).unwrap();
    assert_ne!(replacement.instance(), old_instance);
    replacement.set_mcp_mode(Mode::Write);
    replacement
}

fn wait_for_inactive(request: &Request) {
    let deadline = Instant::now() + TIMEOUT;
    while request.reply.active() {
        assert!(Instant::now() < deadline, "retired request stayed active");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn mcp_stdio_negotiates_lists_tools_and_revokes_a_cancelled_write() {
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut child = McpChild::start(&profile.0, None);
    child.initialize();
    child.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(child.recv()["result"]["tools"].as_array().unwrap().len(), 5);
    child.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pane_list"}}));
    assert_eq!(child.recv()["result"]["isError"], true);
    assert!(server.requests().is_empty());

    server.set_mcp_mode(Mode::Write);
    child.send(json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}));
    assert_eq!(child.recv()["result"]["tools"].as_array().unwrap().len(), 8);
    child.send(json!({"jsonrpc":"2.0","id":"write","method":"tools/call","params":{"name":"pane_run","arguments":{"pane":"p1","command":"echo fixture"}}}));
    let pending = next_request(&server, None);
    let (peer, name) = pending.reply.mcp_client().unwrap();
    assert_eq!(name, "unverified_fixture");
    assert!(pending.reply.active());
    child.send(
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"write"}}),
    );
    wait_for_inactive(&pending);
    pending.reply.send(Ok(json!({"unexpected":true})));
    child.send(json!({"jsonrpc":"2.0","id":5,"method":"ping"}));
    assert_eq!(child.recv()["id"], 5);
    child.send(json!({"jsonrpc":"2.0","id":6,"method":"tools/list"}));
    assert_eq!(child.recv()["result"]["tools"].as_array().unwrap().len(), 8);
    child.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"pane_list"}}));
    let next = next_request(&server, None);
    assert_ne!(next.reply.mcp_client().unwrap().0, peer);
    next.reply.send(Ok(json!({"panes":[],"masked_count":0})));
    assert_eq!(child.recv()["result"]["isError"], false);
    drop(child);
    server.stop();
}

#[test]
fn mcp_stdio_recovers_the_first_unscoped_list_after_listener_replacement() {
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    server.set_mcp_mode(Mode::Write);
    let mut child = McpChild::start(&profile.0, None);
    child.initialize();

    child.tool(2, "pane_list", json!({}));
    answer_panes(&child, &server, 2, false);
    let server = replace_listener(server, &profile.0);
    child.tool(3, "pane_list", json!({"local_shell_only":true}));
    answer_panes(&child, &server, 3, true);

    let server = replace_listener(server, &profile.0);
    child.tool(4, "pane_read", json!({"pane":"p1"}));
    assert!(child.recv_tool(4, true)["error"].is_object());
    assert!(server.requests().is_empty());
    child.tool(5, "pane_list", json!({}));
    answer_panes(&child, &server, 5, false);

    let server = replace_listener(server, &profile.0);
    child.tool(6, "pane_run", json!({"pane":"p1","command":"echo fixture"}));
    assert!(child.recv_tool(6, true)["error"].is_object());
    assert!(server.requests().is_empty());
    child.tool(7, "pane_list", json!({}));
    answer_panes(&child, &server, 7, false);

    let mut server = server;
    for (id, name, arguments, method, result) in [
        (
            8,
            "tab_list",
            json!({}),
            Method::Tabs,
            json!({"tabs":[],"masked_count":0}),
        ),
        (
            9,
            "agent_list",
            json!({"kind":"codex","state":"working"}),
            Method::Agents,
            json!({"agents":[],"masked_count":0}),
        ),
    ] {
        server = replace_listener(server, &profile.0);
        child.tool(id, name, arguments);
        let request = next_request(&server, None);
        assert_eq!(request.method, method);
        assert_eq!(request.instance.as_ref(), server.instance());
        if method == Method::Agents {
            assert_eq!(request.params.required("kind").unwrap(), "codex");
            assert_eq!(request.params.required("state").unwrap(), "working");
        }
        request.reply.send(Ok(result.clone()));
        assert_eq!(child.recv_tool(id, false), result);
    }

    drop(child);
    server.stop();
}

#[test]
fn mcp_stdio_catalog_reconnect_requires_a_fresh_pane_list_for_reused_ids() {
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    server.set_mcp_mode(Mode::Read);
    let mut child = McpChild::start(&profile.0, None);
    child.initialize();
    child.tool(2, "pane_list", json!({}));
    answer_panes(&child, &server, 2, false);

    let server = replace_listener(server, &profile.0);
    child.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}));
    let catalog = child.recv();
    assert_eq!(catalog["id"], 3);
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    assert!(tools.iter().any(|tool| tool["name"] == "pane_run"));

    child.tool(4, "tab_list", json!({}));
    let tabs = next_request(&server, None);
    assert_eq!(tabs.method, Method::Tabs);
    tabs.reply.send(Ok(json!({"tabs":[],"masked_count":0})));
    assert_eq!(child.recv_tool(4, false)["tabs"], json!([]));
    child.tool(5, "agent_list", json!({}));
    let agents = next_request(&server, None);
    assert_eq!(agents.method, Method::Agents);
    agents.reply.send(Ok(json!({"agents":[],"masked_count":0})));
    assert_eq!(child.recv_tool(5, false)["agents"], json!([]));

    child.tool(6, "pane_list", json!({}));
    let failed = next_request(&server, None);
    assert_eq!(failed.method, Method::Panes);
    failed.reply.send(Err(Fault::new(
        ErrorCode::Unavailable,
        "Fixture unavailable",
    )));
    assert_eq!(
        child.recv_tool(6, true)["error"]["data"]["code"],
        "ui_unavailable"
    );

    child.tool(7, "pane_list", json!({}));
    let cancelled = next_request(&server, None);
    assert_eq!(cancelled.method, Method::Panes);
    child
        .send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}));
    wait_for_inactive(&cancelled);
    cancelled
        .reply
        .send(Ok(json!({"panes":[{"pane_id":"p1"}],"masked_count":0})));
    child.send(json!({"jsonrpc":"2.0","id":8,"method":"ping"}));
    assert_eq!(child.recv()["id"], 8);

    for (id, name, arguments) in [
        (9, "pane_read", json!({"pane":"p1"})),
        (
            10,
            "pane_wait_output",
            json!({"pane":"p1","contains":"fixture"}),
        ),
        (11, "pane_send_text", json!({"pane":"p1","text":"fixture"})),
        (
            12,
            "pane_run",
            json!({"pane":"p1","command":"echo fixture"}),
        ),
        (13, "pane_send_keys", json!({"pane":"p1","keys":["Enter"]})),
        (14, "pane_list", json!({"tab":"t1"})),
        (15, "tab_list", json!({"window":"w1"})),
    ] {
        child.tool(id, name, arguments);
        assert_eq!(
            child.recv_tool(id, true)["error"]["data"]["code"],
            "stale_instance",
            "{name} did not require a new pane list"
        );
        assert!(
            server.requests().is_empty(),
            "{name} reached the replacement"
        );
    }

    child.tool(16, "pane_list", json!({}));
    answer_panes(&child, &server, 16, false);
    child.tool(17, "pane_read", json!({"pane":"p1"}));
    let read = next_request(&server, None);
    assert_eq!(read.method, Method::Read);
    assert_eq!(read.instance.as_ref(), server.instance());
    assert_eq!(read.params.required("pane").unwrap(), "p1");
    read.reply.send(Ok(
        json!({"pane_id":"p1","text":"replacement","masked_count":0}),
    ));
    assert_eq!(child.recv_tool(17, false)["text"], "replacement");

    drop(child);
    server.stop();
}

#[test]
fn mcp_stdio_times_out_without_replaying_the_request() {
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    server.set_mcp_mode(Mode::Read);
    let mut child = McpChild::start(&profile.0, Some(1_500));
    child.initialize();
    child.tool(2, "pane_list", json!({}));
    let pending = next_request(&server, None);
    assert_eq!(pending.method, Method::Panes);
    assert!(pending.reply.active());
    assert!(
        child.recv_tool(2, true)["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("timed out"))
    );
    wait_for_inactive(&pending);
    pending.reply.send(Ok(json!({"unexpected":true})));
    assert!(server.requests().is_empty());

    child.tool(3, "pane_list", json!({}));
    answer_panes(&child, &server, 3, false);
    drop(child);
    server.stop();
}

#[cfg(unix)]
fn shell_command(cli: &str, profile: &Path) -> String {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    format!(
        "{} --config-dir {} pane get --current\n",
        quote(cli),
        quote(profile.to_str().unwrap())
    )
}

#[cfg(windows)]
fn shell_command(cli: &str, profile: &Path) -> String {
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if powershell.is_file() {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
        format!(
            "& {} --config-dir {} pane get --current\r",
            quote(cli),
            quote(profile.to_str().unwrap())
        )
    } else {
        format!(
            "\"{cli}\" --config-dir \"{}\" pane get --current\r",
            profile.display()
        )
    }
}

#[test]
fn cli_current_uses_real_pty_ancestry_and_fails_without_a_match() {
    // A native runner may transfer a cross-built executable to another path.
    let cli = std::env::var("KORTTY_CONTROL_TEST_CLI").unwrap_or_else(|_| CLI.into());
    assert!(Path::new(&cli).is_file(), "missing test CLI at {cli}");
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut pty = Pty::spawn(120, 24).unwrap();
    let shell = pty.shell_pid();
    let mut reader = pty.take_reader().unwrap();
    let collected = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&collected);
    let drain = thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            let mut output = output.lock().unwrap();
            let retained = count.min(MAX_OUTPUT.saturating_sub(output.len()));
            output.extend_from_slice(&bytes[..retained]);
        }
    });
    let mut writer = pty.take_writer().unwrap();
    writer
        .write_all(shell_command(&cli, &profile.0).as_bytes())
        .unwrap();

    // Test double for desktop dispatch. The CLI, OS ancestry, PTY and
    // authenticated request/reply transport are real.
    let resolve = next_request(&server, Some(&collected));
    assert_eq!(resolve.method, Method::Resolve);
    let pids = resolve.params.resolve_pids().unwrap();
    assert_ne!(pids.first().copied(), Some(shell));
    assert!(pids.contains(&shell), "{pids:?} lacks PTY shell {shell}");
    resolve.reply.send(Ok(json!({
        "pane": {"pane_id": "p11111111", "shell_pid": shell}, "matched_pid": shell
    })));

    let get = next_request(&server, Some(&collected));
    assert_eq!(get.method, Method::Pane);
    assert_eq!(get.params.string("pane").unwrap(), Some("p11111111"));
    get.reply.send(Ok(json!({"native_current_proof": true})));
    let deadline = Instant::now() + TIMEOUT;
    let marker = b"native_current_proof";
    loop {
        let output = collected.lock().unwrap();
        if output.windows(marker.len()).any(|part| part == marker) {
            break;
        }
        assert!(
            output.len() < MAX_OUTPUT && Instant::now() < deadline,
            "CLI response did not reach PTY: {}",
            String::from_utf8_lossy(&output)
        );
        drop(output);
        thread::sleep(Duration::from_millis(5));
    }
    thread::scope(|scope| {
        let external = scope.spawn(|| {
            Command::new(&cli)
                .arg("--config-dir")
                .arg(&profile.0)
                .args(["pane", "get", "--current"])
                .output()
                .unwrap()
        });
        let resolve = next_request(&server, None);
        assert_eq!(resolve.method, Method::Resolve);
        assert!(!resolve.params.resolve_pids().unwrap().contains(&shell));
        resolve
            .reply
            .send(Ok(json!({"pane": null, "matched_pid": null})));
        let outside = external.join().unwrap();
        assert_eq!(outside.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&outside.stderr).contains("--current needs"));
        assert!(server.requests().is_empty());
    });
    pty.shutdown().unwrap();
    drain.join().unwrap();
    server.stop();
}

#[test]
fn cli_tab_create_roundtrips_pending_and_ambiguous_error() {
    let cli = std::env::var("KORTTY_CONTROL_TEST_CLI").unwrap_or_else(|_| CLI.into());
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut client = Client::connect(&profile.0, TIMEOUT).unwrap();
    let instance = client.instance().to_owned();
    let call = |args: &[&str]| {
        Command::new(&cli)
            .arg("--config-dir")
            .arg(&profile.0)
            .args(args)
            .output()
            .unwrap()
    };
    for (params, code) in [
        (json!({"connection": null}), "invalid_params"),
        (
            json!({"connection": "db", "isolation": "Sandbox"}),
            "invalid_params",
        ),
        (
            json!({"connection": "db", "instance": "old"}),
            "stale_instance",
        ),
    ] {
        let Err(ClientError::Server { rpc, .. }) = client.call(Method::CreateTab.name(), &params)
        else {
            panic!("invalid tab.create reached the desktop");
        };
        assert_eq!(rpc["data"]["code"], code);
        if code == "stale_instance" {
            assert_eq!(rpc["data"]["instance"], instance);
            assert_eq!(rpc["data"]["requested"], "old");
        }
        assert!(server.requests().is_empty());
    }
    let invalid = call(&[
        "tab",
        "create",
        "--connection",
        "db",
        "--isolation",
        "container",
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--isolation must be one of"));
    assert!(server.requests().is_empty());

    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            call(&[
                "tab",
                "create",
                "--connection",
                " db ",
                "--isolation",
                " Sandbox ",
                "--incognito",
                "--window",
                "w2",
            ])
        });
        let request = next_request(&server, None);
        assert_eq!(request.method, Method::CreateTab);
        assert_eq!(
            request.params.string("instance").unwrap(),
            Some(instance.as_str())
        );
        assert_eq!(
            TabCreateParams::parse(&request.params).unwrap(),
            TabCreateParams {
                connection: "db",
                window: Some("w2"),
                isolation: Some("sandbox"),
                incognito: true,
            }
        );
        request
            .reply
            .send(Ok(tab_create_result(&request.instance, None)));
        let pending = pending.join().unwrap();
        assert!(pending.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&pending.stdout).unwrap(),
            json!({"instance": instance, "pending": true, "tab": null})
        );

        let ambiguous = scope.spawn(|| call(&["tab", "create", "--connection", "db", "--pretty"]));
        let request = next_request(&server, None);
        request.reply.send(Err(Fault::ambiguous_connection(
            "db",
            vec!["1".into(), "2".into()],
        )));
        let ambiguous = ambiguous.join().unwrap();
        assert_eq!(ambiguous.status.code(), Some(2));
        assert!(ambiguous.stdout.is_empty());
        let error: Value = serde_json::from_slice(&ambiguous.stderr).unwrap();
        assert_eq!(error["code"], -32026);
        assert_eq!(error["data"]["code"], "ambiguous_connection");
        assert_eq!(error["data"]["connection"], "db");
        assert_eq!(error["data"]["candidates"], json!(["1", "2"]));
    });
    server.stop();
}
