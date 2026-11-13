use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;

use super::*;
use crate::ssh_worker::OpenSshFixture;
use crate::workspace::terminal_agent::{Phase, RunTarget};

const TEST_NAME: &str = "workspace::terminal_agent::swarm::conversation::tests::openssh::saved_swarm_reviews_each_command_over_pinned_openssh";

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn saved_swarm_reviews_each_command_over_pinned_openssh() {
    let Some(home) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
        let scratch = Profile::new();
        let home = scratch.0.canonicalize().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--ignored", "--nocapture"])
            .env(FIXTURE, &home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("TMPDIR", &home)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        return;
    };
    let root = home.join("private-profile");
    std::fs::create_dir(&root).unwrap();
    crate::profile::set_runtime_root(root.clone());
    let (model, _) = test_support::listener();
    let ssh = OpenSshFixture::new();
    let observer = TcpListener::bind("127.0.0.1:0").unwrap();
    observer.set_nonblocking(true).unwrap();
    let directory = ssh.directory().canonicalize().unwrap();
    let directory = directory.to_str().unwrap();
    let key = ssh.directory().join("client");
    let mut login = ssh.request(key.clone()).target.endpoint;
    let upstream = std::net::SocketAddr::from(([127, 0, 0, 1], login.port));
    let mut pins = ssh
        .pinned_trust()
        .selected_pins(&[kortty_ssh::Endpoint::new(&login.host, login.port).unwrap()])
        .unwrap();
    let (_, pin) = pins.pop().unwrap();
    login.port = observer.local_addr().unwrap().port();
    let endpoint = kortty_ssh::Endpoint::new(&login.host, login.port).unwrap();
    crate::ssh_connection::trust_store()
        .unwrap()
        .remember(&endpoint, &pin)
        .unwrap();
    let connection: Connection = serde_json::from_value(json!({
        "id":0,"name":"Pinned OpenSSH target","tag":"","jump":null,
        "emulation":"xterm-256color","tunnels":[],
        "login":{"endpoint":login,"auth":{"key-file":{"path":key,"passphrase":null}},"keyboard_interactive":false},
    })).unwrap();
    let target = Store::open(crate::profile::connections_path().unwrap())
        .unwrap()
        .save(connection)
        .unwrap();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles":[{"id":1,"name":"OpenSSH fixture","endpoint":model.local_addr().unwrap().to_string(),"model":"fixture:local"}],
        "default":1,"last_id":1,
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let origin = store::Origin::from_id(&crate::private_files::nonce().unwrap()).unwrap();
    let record = store::seed(
        &root,
        &origin,
        ProfileRef::Numeric(1),
        "OpenSSH fixture",
        SwarmReport {
            question: "previous-turn-marker".into(),
            targets: vec![SwarmTargetReport {
                target,
                label: "Original target".into(),
                outcome: RunOutcome::Success,
                detail: "previous-summary-marker".into(),
            }],
            comparison: None,
        },
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_saved_swarm(owner, root.clone(), record.clone());
    let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(&mut workspace, headless::catalog(group, Some(vec![target])));
    let act = |workspace: &mut Workspace, message| workspace.swarm_action(group, message);
    let idle = || {
        for listener in [&observer, &model] {
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    };
    idle();
    let _ = act(&mut workspace, Message::Mode(SwarmMode::Agent));
    let _ = act(
        &mut workspace,
        Message::Goal("Inspect the live fixture".into()),
    );
    let _ = act(
        &mut workspace,
        Message::Directory(Key::Saved(target), directory.into()),
    );
    assert_eq!(act(&mut workspace, Message::ReviewFollowUp).units(), 0);
    let epoch = workspace.terminal_agents.swarms[&group]
        .conversation
        .as_ref()
        .unwrap()
        .review
        .as_ref()
        .unwrap()
        .epoch;
    idle();
    assert_eq!(
        act(&mut workspace, Message::StartFollowUp(epoch)).units(),
        0
    );
    let member = *workspace.terminal_agents.swarms[&group]
        .members
        .keys()
        .next()
        .unwrap();
    let run = &workspace.terminal_agents.runs[&member];
    assert!(run.phase == Phase::ProbeApproval);
    let Some(RunTarget::Saved(route)) = &run.target else {
        panic!("expected the pinned saved route")
    };
    assert_eq!(route.directory(), directory);
    assert_eq!(
        route.fingerprints().collect::<Vec<_>>(),
        [(&endpoint, pin.fingerprint())]
    );
    let (_, probe) = run.reviewed_command().unwrap();
    let expected_probe = crate::terminal_agent::wrap_ssh_command(directory, probe).unwrap();
    let probe_epoch = run.epoch;
    idle();

    let receipt = ssh.directory().join("reviewed-wire");
    let script = ssh.directory().join("observe-wire.sh");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(&script, format!("printf '%s\\n' \"$SSH_ORIGINAL_COMMAND\" > {}\nexec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\"\n", quote(&receipt))).unwrap();
    let extra = format!("ForceCommand /bin/sh {}\n", quote(&script));
    let approve = |workspace: &mut Workspace, epoch| {
        act(
            workspace,
            Message::Member(member, super::super::super::MemberAction::Approve(epoch)),
        )
    };
    let run_ssh = |workspace: &mut Workspace, epoch, expected: &str| {
        let daemon = ssh.serve_with_extra("host", &extra);
        let relay = relay(&observer, upstream);
        let task = approve(workspace, epoch);
        let model_task = forward(workspace, task);
        let output = daemon.join().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stderr)
                .matches("Accepted publickey for ")
                .count(),
            1
        );
        relay.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(&receipt).unwrap(),
            format!("{expected}\n")
        );
        model_task
    };
    let staged = run_ssh(&mut workspace, probe_epoch, &expected_probe);
    settle(&mut workspace, staged);
    idle();
    let (model_task, reviewed) = send_model_review(&mut workspace, group, member);
    let command = "printf 'approved\\n' > continuation-command-marker";
    let response = json!({"status":"run_commands","summary":"Inspect","userMessage":"Review the fixture command.",
        "commands":[{"command":command,"purpose":"Create the isolated test marker","risk":"requires_confirmation"}]});
    let request = reply(&model, &reviewed, response);
    assert_fresh_question(&request, directory);
    settle(&mut workspace, model_task);
    let marker = ssh.directory().join("continuation-command-marker");
    let run = &workspace.terminal_agents.runs[&member];
    assert!(run.phase == Phase::Approval);
    assert_eq!(run.reviewed_command().unwrap().1, command);
    let command_epoch = run.epoch;
    assert_ne!(command_epoch, probe_epoch);
    assert_eq!(approve(&mut workspace, probe_epoch).units(), 0);
    assert!(!marker.exists());
    idle();

    let expected = crate::terminal_agent::wrap_ssh_command(directory, command).unwrap();
    let staged = run_ssh(&mut workspace, command_epoch, &expected);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "approved\n");
    settle(&mut workspace, staged);
    idle();
    let (final_task, reviewed) = send_model_review(&mut workspace, group, member);
    let request = reply(
        &model,
        &reviewed,
        json!({"status":"done","summary":"Done","userMessage":"The pinned target completed."}),
    );
    assert_fresh_question(&request, directory);
    settle(&mut workspace, final_task);
    let saved = store::load(&root, &record.id).unwrap();
    assert_eq!(saved.id, record.id);
    assert_eq!(saved.revision, record.revision + 1);
    assert_eq!(
        saved.turns[1].targets[0].detail,
        "The pinned target completed."
    );
    assert!(
        !saved.turns[1].targets[0]
            .detail
            .contains("continuation-command-marker")
    );
    idle();
}

fn forward(workspace: &mut Workspace, task: Task<Desktop>) -> Task<Desktop> {
    let mut events = iced_runtime::task::into_stream(task).expect("reviewed SSH command");
    let mut tasks = Vec::new();
    while let Some(action) = block_on(events.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
            tasks.push(workspace.update_terminal_agent(id, message));
        }
    }
    Task::batch(tasks)
}

fn reply(model: &TcpListener, reviewed: &[String; 2], response: Value) -> Value {
    let mut client = test_support::accept(model);
    let request = test_support::request(&mut client);
    assert_eq!(request["max_tokens"], 2048);
    assert_eq!(request["messages"][0]["content"], reviewed[0]);
    assert_eq!(request["messages"][1]["content"], reviewed[1]);
    client
        .get_mut()
        .write_all(test_support::fixture_response("200 OK", "", &response.to_string()).as_bytes())
        .unwrap();
    request
}

fn assert_fresh_question(request: &Value, directory: &str) {
    let input: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["goal"], "Inspect the live fixture");
    assert_eq!(input["target"]["directory"], directory);
    assert!(!request.to_string().contains("previous-"));
}

fn relay(observer: &TcpListener, upstream: std::net::SocketAddr) -> thread::JoinHandle<()> {
    let observer = observer.try_clone().unwrap();
    thread::spawn(move || {
        let client = test_support::accept(&observer).into_inner();
        let server = TcpStream::connect(upstream).unwrap();
        let copy = |mut reader: TcpStream, mut writer: TcpStream| {
            let _ = io::copy(&mut reader, &mut writer);
            let _ = writer.shutdown(Shutdown::Write);
        };
        thread::scope(|scope| {
            let reader = client.try_clone().unwrap();
            let writer = server.try_clone().unwrap();
            scope.spawn(move || copy(reader, writer));
            copy(server, client);
        });
    })
}
