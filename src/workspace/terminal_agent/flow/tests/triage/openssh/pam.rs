use std::path::PathBuf;

use kortty_ssh::{HostKey, HostKeyStatus, TrustMode};

use super::*;
use crate::ssh_dialog::{Authentication as LoginAuthentication, LoginRequest};

#[test]
#[ignore = "requires the disposable loopback OpenSSH sudo/PAM fixture"]
fn real_pam_sudo_retries_and_reuses_the_direct_ssh_terminal() {
    check_real_pam(IsolationLevel::None, "direct");
}

#[test]
#[ignore = "requires the disposable loopback OpenSSH sudo/PAM fixture and built app"]
fn real_pam_sudo_retries_and_reuses_the_sandboxed_ssh_terminal() {
    check_real_pam(IsolationLevel::Sandbox, "sandbox");
}

fn check_real_pam(isolation: IsolationLevel, name: &str) {
    let _serial = exclusive_run();
    let root = PathBuf::from(std::env::var_os("KORTTY_SSH_PAM_ROOT").unwrap());
    assert_eq!(
        fs::read_to_string(root.join("kortty-ssh-pam-test-root")).unwrap(),
        "Disposable korTTY OpenSSH sudo/PAM fixture\n"
    );
    let port = fs::read_to_string(root.join("port"))
        .unwrap()
        .trim()
        .parse::<u16>()
        .unwrap();
    assert!(root.is_absolute() && port > 1024);
    let work = root.join("ssh-work");
    let receipt_name = format!("effective-uid-{name}");
    let owner_name = format!("owner-uid-{name}");
    let receipt = work.join(&receipt_name);
    let owner = work.join(&owner_name);
    let login = LoginTarget {
        host: "127.0.0.1".into(),
        port,
        username: "fixture".into(),
    };
    let endpoint = Endpoint::new(&login.host, port).unwrap();
    let trust = TrustStore::new(root.join(format!("pinned-{name}")), TrustMode::Strict);
    let public_key = fs::read_to_string(root.join("host.pub")).unwrap();
    let host_key = HostKey::from_openssh(public_key.trim()).unwrap();
    assert_eq!(
        trust.remember(&endpoint, &host_key).unwrap(),
        HostKeyStatus::Known
    );
    let request = Request {
        target: LoginRequest {
            endpoint: login.clone(),
            auth: LoginAuthentication::PrivateKey(root.join("client")),
            keyboard_interactive: false,
        },
        jump: None,
        profile: TerminalProfile::Xterm,
        isolation,
        options: Default::default(),
    };
    let (model, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &model.local_addr().unwrap().to_string());
    fixture
        .workspace
        .window_settings
        .terminal_notifications
        .ai_runs = false;
    attach_reviewed_shell(&mut fixture, request, &trust, &work, isolation);
    set_sudo_vault(
        &mut fixture,
        root.join(format!("wrong-sudo-{name}")),
        &login,
        "wrong-fixture-password".into(),
    );

    let id = fixture.open(Mode::Run);
    no_task(fixture.send(
        id,
        Message::Goal("Verify the fixture effective user".into()),
    ));
    fixture.run_mut(id).plan = Some("Verify sudo in the isolated directory.".into());
    let start = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, start);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ProbeApproval, "{}", run.notice);
    let probe = fixture.send(id, Message::Approve(run.epoch));
    finish_agent_task(&mut fixture, probe);
    let run = fixture.run(id);
    assert!(run.phase == Phase::SudoPreflight, "{}", run.notice);
    assert!(run.sudo_terminal.is_none());
    assert_idle(&model);

    let epoch = run.epoch;
    let (peer, probe) = hold_peer_probe(&mut fixture);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            epoch,
            password::Source::Vault,
        )),
    ));
    let run = fixture.run(id);
    assert!(run.phase == Phase::SudoPreflight, "{}", run.notice);
    assert!(run.notice.contains("Another Agent command"));
    assert_ne!(run.epoch, epoch);
    assert!(run.sudo_preflight.is_some() && run.sudo_terminal.is_none());
    assert_eq!(run.sudo_preflight_attempts, 0);
    assert_idle(&model);
    finish_peer_probe(&mut fixture, peer, probe);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            epoch,
            password::Source::Vault,
        )),
    ));
    assert_eq!(fixture.run(id).sudo_preflight_attempts, 0);

    authenticate_plan_with_vault(&mut fixture, id, &model, |fixture| {
        set_sudo_vault(
            fixture,
            root.join(format!("correct-sudo-{name}")),
            &login,
            "correct-fixture-password".into(),
        );
    });
    let run = fixture.run(id);
    assert!(run.phase == Phase::SudoPreflight, "{}", run.notice);
    assert!(run.notice.contains("authentication was rejected"));
    assert!(run.sudo_terminal.is_none());
    assert_idle(&model);

    authenticate_plan_with_vault(&mut fixture, id, &model, |_| {});
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview, "{}", run.notice);
    assert!(run.sudo_terminal.is_some());
    assert!(!receipt.exists() && !owner.exists());
    respond_to_model_review(
        &mut fixture,
        id,
        &model,
        None,
        json!({
            "status": "run_commands", "summary": "Write fixture receipts", "userMessage": "Review one command",
            "commands": [{
                "command": format!("/usr/bin/id -u > {receipt_name} && /usr/bin/stat -c %u -- {receipt_name} > {owner_name}"),
                "purpose": "Verify the effective user and receipt owner",
                "risk": "requires_confirmation",
                "elevation": "password_sudo"
            }]
        }),
    );
    let run = fixture.run(id);
    assert!(run.phase == Phase::Approval, "{}", run.notice);
    assert!(run.reviewed_password_sudo().is_some());
    assert!(!receipt.exists() && !owner.exists());
    let epoch = run.epoch;
    let (peer, probe) = hold_peer_probe(&mut fixture);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
    ));
    let run = fixture.run(id);
    assert!(run.phase == Phase::Approval, "{}", run.notice);
    assert!(run.notice.contains("Another Agent command"));
    assert_ne!(run.epoch, epoch);
    assert!(run.reviewed_password_sudo().is_some() && run.sudo_terminal.is_some());
    assert!(!receipt.exists() && !owner.exists());
    finish_peer_probe(&mut fixture, peer, probe);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
    ));
    assert!(!receipt.exists() && !owner.exists());
    assert_idle(&model);

    let command = fixture.send(
        id,
        Message::Password(password::Message::Approve(
            fixture.run(id).epoch,
            password::Source::Ticket,
        )),
    );
    finish_agent_task(&mut fixture, command);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview, "{}", run.notice);
    assert_eq!(fs::read_to_string(receipt).unwrap(), "0\n");
    assert_eq!(fs::read_to_string(owner).unwrap(), "0\n");
    assert_idle(&model);
    drop(fixture.workspace.close_terminal_agent(id));
    let _ = fixture.workspace.sessions[&fixture.session]
        .session
        .stop()
        .recv_blocking();
}
