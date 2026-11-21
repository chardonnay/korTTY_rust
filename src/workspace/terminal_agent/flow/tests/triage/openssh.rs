use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use kortty_ssh::{Authentication, Connect, Endpoint, SshSession, TrustStore};

use super::*;
use crate::session::StreamOptions;
use crate::ssh_connection::Connected;
use crate::ssh_dialog::{LoginTarget, Request};
use crate::terminal_logging::Prepared;
use crate::workspace::ssh_access::SshTarget;
use kortty_terminal::TerminalProfile;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod pam;
#[cfg(unix)]
mod unix;

fn hold_peer_probe(fixture: &mut Fixture) -> (window::Id, Task<Desktop>) {
    let peer = fixture.open(Mode::Run);
    no_task(fixture.send(peer, Message::Goal("Review another fixture probe".into())));
    let start = fixture.send(peer, Message::Start);
    finish_agent_task(fixture, start);
    assert!(fixture.run(peer).phase == Phase::ProbeApproval);
    let probe = fixture.send(peer, Message::Approve(fixture.run(peer).epoch));
    assert!(fixture.run(peer).phase == Phase::Probe);
    (peer, probe)
}

fn finish_peer_probe(fixture: &mut Fixture, peer: window::Id, probe: Task<Desktop>) {
    finish_agent_task(fixture, probe);
    assert!(
        fixture.run(peer).probe_reviewed,
        "{}",
        fixture.run(peer).notice
    );
    drop(fixture.workspace.close_terminal_agent(peer));
}

fn set_sudo_vault(
    fixture: &mut Fixture,
    vault_path: std::path::PathBuf,
    login: &LoginTarget,
    secret: String,
) {
    use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

    let mut master = [0; 32];
    getrandom::fill(&mut master).unwrap();
    let mut vault = Vault::create(&vault_path, &master).unwrap();
    vault
        .save(
            CredentialInput {
                label: "Fixture sudo credential".into(),
                username: login.username.clone(),
                server_pattern: format!("{}:{}", login.host, login.port),
                kind: CredentialKind::SudoPassword,
                ..CredentialInput::default()
            },
            Secret::new(secret),
        )
        .unwrap();
    fixture.workspace.vault = Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
        vault_path, vault,
    ));
}

fn authenticate_plan_with_vault(
    fixture: &mut Fixture,
    id: window::Id,
    model: &TcpListener,
    before_result: impl FnOnce(&mut Fixture),
) {
    use iced::futures::{StreamExt as _, executor::block_on};

    let authenticate = fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            fixture.run(id).epoch,
            password::Source::Vault,
        )),
    );
    let mut stream = iced_runtime::task::into_stream(authenticate).unwrap();
    let mut challenged = false;
    let mut before_result = Some(before_result);
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(owner, message)) = action {
            assert_eq!(owner, id);
            let challenge = matches!(&message, Message::Password(password::Message::Challenge(_)));
            if matches!(
                &message,
                Message::Password(password::Message::Finished(_, _))
            ) && let Some(before_result) = before_result.take()
            {
                before_result(fixture);
            }
            let next = fixture.send(id, message);
            finish_agent_task(fixture, next);
            if challenge {
                challenged = true;
                assert!(fixture.run(id).phase == Phase::SudoPassword);
                assert_idle(model);
                let epoch = fixture.run(id).epoch;
                no_task(fixture.send(id, Message::Password(password::Message::Use(epoch))));
            }
        }
    }
    assert!(challenged);
    assert!(before_result.is_none());
}

fn respond_to_model_review(
    fixture: &mut Fixture,
    id: window::Id,
    model: &TcpListener,
    stage: Option<Stage>,
    reply: serde_json::Value,
) {
    assert_idle(model);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview, "{}", run.notice);
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.plan_stage(), stage);
    let reviewed_input = review.input.clone();
    let send = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(run.epoch)),
    );
    let mut socket = accept(model);
    assert_eq!(
        request(&mut socket)["messages"][1]["content"],
        reviewed_input
    );
    let body = json!({"choices":[{"message":{"content":reply.to_string()}}]});
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
        .unwrap();
    drop(socket);
    finish_agent_task(fixture, send);
}

fn attach_reviewed_shell(
    fixture: &mut Fixture,
    mut request: Request,
    trust: &TrustStore,
    directory: &Path,
    isolation: IsolationLevel,
) {
    let login = request.target.endpoint.clone();
    let grid = GridSize::default();
    let connection = if isolation == IsolationLevel::None {
        let crate::ssh_dialog::Authentication::PrivateKey(key) = request.target.auth else {
            panic!("The OpenSSH fixture requires an unencrypted private key");
        };
        let direct = SshSession::connect(
            Connect {
                address: SocketAddr::from(([127, 0, 0, 1], login.port)),
                endpoint: Endpoint::new(&login.host, login.port).unwrap(),
                username: login.username.clone(),
                authentication: Authentication::private_key(fs::read(key).unwrap()),
                term: TerminalProfile::Xterm.term_name().into(),
                size: crate::session::ssh_size(grid),
            },
            trust.clone(),
        )
        .unwrap();
        Connected::Direct(Box::new(direct))
    } else {
        request.isolation = isolation;
        let worker = crate::ssh_worker::dial_test_executable(
            request,
            false,
            &crate::ssh_worker::test_executable(),
            trust,
        )
        .unwrap();
        assert!(worker.sandbox_backend().is_some());
        Connected::Worker(Box::new(worker))
    };
    let capture = Prepared::new(
        Default::default(),
        Default::default(),
        "Private SSH triage fixture".into(),
        login.host.clone(),
        login.username.clone(),
    );
    let session = Session::ssh_with_logging(
        Some(connection),
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
        Some(SshTarget::from_imported_route(login, None, isolation).unwrap());
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
