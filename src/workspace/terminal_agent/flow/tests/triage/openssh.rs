use std::fs;
use std::net::{SocketAddr, TcpListener};

use kortty_ssh::{Authentication, Connect, Endpoint, SshSession};

use super::*;
use crate::session::StreamOptions;
use crate::ssh_connection::Connected;
use crate::ssh_worker::OpenSshFixture;
use crate::terminal_logging::Prepared;
use crate::workspace::ssh_access::SshTarget;
use kortty_terminal::TerminalProfile;

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn reviewed_triage_preserves_the_live_openssh_target_and_rejects_a_directory_change() {
    let _serial = exclusive_run();
    let (model, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &model.local_addr().unwrap().to_string());
    fixture
        .workspace
        .window_settings
        .terminal_notifications
        .ai_runs = false;
    let ssh = OpenSshFixture::new();
    let root = ssh.directory().canonicalize().unwrap();
    let moved = root.join("moved");
    fs::create_dir(&moved).unwrap();
    let receipt = root.join("reviewed-exec");
    let server = start_shell(&ssh, &root, &moved, &receipt);
    attach_shell(&mut fixture, &ssh, &root);

    let id = fixture.open(Mode::Run);
    no_task(fixture.send(id, Message::Goal("Check the report directory".into())));
    let start = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, start);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ProbeApproval, "{}", run.notice);
    let wire = engine::wrap_ssh_command(root.to_str().unwrap(), run.reviewed_command().unwrap().1)
        .unwrap();
    let epoch = run.epoch;
    assert!(!receipt.exists());
    assert_idle(&model);
    let probe = fixture.send(id, Message::Approve(epoch));
    finish_agent_task(&mut fixture, probe);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), format!("{wire}\n"));

    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview, "{}", run.notice);
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.plan_stage(), Some(Stage::Triage));
    let request_input: serde_json::Value = serde_json::from_str(&review.input).unwrap();
    assert_eq!(request_input["target"]["directory"], root.to_str().unwrap());
    let reviewed_input = review.input.clone();
    assert_idle(&model);

    let epoch = run.epoch;
    let provider = thread::spawn(move || {
        let mut socket = accept(&model);
        let body = request(&mut socket);
        assert_eq!(body["messages"][1]["content"], reviewed_input);
        socket
            .get_mut()
            .write_all(proposal_response().as_bytes())
            .unwrap();
        model
    });
    let assessment = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, assessment);
    let model = provider.join().unwrap();
    let run = fixture.run(id);
    assert!(run.phase == Phase::TriageChoice, "{}", run.notice);
    let epoch = run.epoch;
    let choice = fixture.send(
        id,
        Message::Triage(Triage::Choose(epoch, Selection::Option(0))),
    );
    finish_agent_task(&mut fixture, choice);
    let run = fixture.run(id);
    assert!(
        run.phase == Phase::ModelReview && run.turns == 0,
        "{}",
        run.notice
    );
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.plan_stage(), None);
    let decision: serde_json::Value = serde_json::from_str(&review.input).unwrap();
    assert_eq!(decision["target"]["directory"], root.to_str().unwrap());
    assert!(
        decision["acceptedPlan"]
            .as_str()
            .unwrap()
            .contains("Review directory metadata")
    );
    assert_idle(&model);

    let epoch = run.epoch;
    fixture.workspace.sessions[&fixture.session]
        .session
        .send(b"move\r".to_vec())
        .unwrap();
    wait_for_directory(&mut fixture, &moved);
    let stale = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, stale);
    let run = fixture.run(id);
    assert!(
        run.phase == Phase::Blocked && run.notice.contains("directory changed"),
        "{}",
        run.notice
    );
    assert_eq!(fs::read_to_string(&receipt).unwrap(), format!("{wire}\n"));
    assert_idle(&model);
    let _ = fixture.workspace.sessions[&fixture.session]
        .session
        .stop()
        .recv_blocking();
    drop(fixture);
    let output = server.join().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("Accepted publickey for "));
}

fn start_shell(
    ssh: &OpenSshFixture,
    root: &std::path::Path,
    moved: &std::path::Path,
    receipt: &std::path::Path,
) -> thread::JoinHandle<std::process::Output> {
    let script = root.join("triage-shell.sh");
    let quote = |path: &std::path::Path| {
        let mut quoted = String::new();
        crate::scheduler_window::script::shell_quote(&mut quoted, path.to_str().unwrap());
        quoted
    };
    let script_body = format!(
        r#"if [ -n "$SSH_ORIGINAL_COMMAND" ]; then
    printf '%s\n' "$SSH_ORIGINAL_COMMAND" >> {}
    exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"
fi
report_directory() {{
    cd -- "$1" || exit 1
    printf '\033]7;file://127.0.0.1%s\007' "$PWD"
}}
stty -echo
report_directory {}
while IFS= read -r line; do
    if [ "$line" = move ]; then report_directory {}; fi
done
"#,
        quote(receipt),
        quote(root),
        quote(moved),
    );
    fs::write(&script, script_body).unwrap();
    ssh.serve_with_extra(
        "host",
        &format!("PermitUserRC no\nForceCommand /bin/sh {}\n", quote(&script)),
    )
}

fn attach_shell(fixture: &mut Fixture, ssh: &OpenSshFixture, directory: &std::path::Path) {
    let key = ssh.directory().join("client");
    let login = ssh.request(key.clone()).target.endpoint;
    let grid = GridSize::default();
    let connection = SshSession::connect(
        Connect {
            address: SocketAddr::from(([127, 0, 0, 1], login.port)),
            endpoint: Endpoint::new(&login.host, login.port).unwrap(),
            username: login.username.clone(),
            authentication: Authentication::private_key(fs::read(key).unwrap()),
            term: TerminalProfile::Xterm.term_name().into(),
            size: crate::session::ssh_size(grid),
        },
        ssh.pinned_trust(),
    )
    .unwrap();
    let capture = Prepared::new(
        Default::default(),
        Default::default(),
        "Private SSH triage fixture".into(),
        login.host.clone(),
        login.username.clone(),
    );
    let session = Session::ssh_with_logging(
        Some(Connected::Direct(Box::new(connection))),
        grid,
        TerminalProfile::Xterm,
        true,
        None,
        Some(Box::new(capture)),
        StreamOptions::default(),
    )
    .unwrap();
    let pane = fixture.workspace.store_session(session, grid, 14);
    fixture.workspace.sessions.get_mut(&pane).unwrap().ssh =
        Some(SshTarget::from_imported_route(login, None, IsolationLevel::None).unwrap());
    fixture.workspace.add_tab(fixture.owner, pane);
    fixture.session = pane;
    wait_for_directory(fixture, directory);
}

fn wait_for_directory(fixture: &mut Fixture, directory: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = directory.to_str().unwrap();
    loop {
        let pane = fixture
            .workspace
            .sessions
            .get_mut(&fixture.session)
            .unwrap();
        pane.frame = pane.session.frame();
        if pane.frame.status == SessionStatus::Running
            && fixture
                .workspace
                .agent_remote_path(fixture.owner, fixture.session)
                .as_deref()
                == Ok(path)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "missing SSH directory report for {path}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_idle(model: &TcpListener) {
    assert!(matches!(model.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}
