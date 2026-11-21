use std::path::PathBuf;
use std::process::Command;

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
    let work = std::env::var_os("KORTTY_SSH_PAM_WORK")
        .map_or_else(|| root.join("ssh-work"), PathBuf::from);
    assert!(
        work.to_str().unwrap().starts_with('/'),
        "set an absolute remote path"
    );
    let receipt_name = format!("effective-uid-{name}");
    let owner_name = format!("owner-uid-{name}");
    let login = LoginTarget {
        host: "127.0.0.1".into(),
        port,
        username: "fixture".into(),
    };
    let endpoint = Endpoint::new(&login.host, port).unwrap();
    let pins = root.join(format!("pinned-{name}"));
    let client = root.join("client");
    let trust = TrustStore::new(pins.clone(), TrustMode::Strict);
    let public_key = fs::read_to_string(root.join("host.pub")).unwrap();
    let host_key = HostKey::from_openssh(public_key.trim()).unwrap();
    assert_eq!(
        trust.remember(&endpoint, &host_key).unwrap(),
        HostKeyStatus::Known
    );
    let mut directory = String::new();
    crate::scheduler_window::script::shell_quote(&mut directory, work.to_str().unwrap());
    let receipt_query = format!(
        "cd {directory} || exit; for receipt in {receipt_name} {owner_name}; do \
         if test -e \"$receipt\"; then /bin/cat -- \"$receipt\" || exit; \
         else printf 'absent\\n'; fi; done"
    );
    // Read the receipts over an independent pinned login so the client and
    // the disposable sudo/PAM server can run on different systems.
    #[cfg(windows)]
    let (ssh, null) = (
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/OpenSSH/ssh.exe"),
        "NUL",
    );
    #[cfg(not(windows))]
    let (ssh, null) = (PathBuf::from("/usr/bin/ssh"), "/dev/null");
    let check_receipts = |expected: &[u8]| {
        let known_hosts = if cfg!(windows) {
            pins.to_str().unwrap().replace('\\', "/")
        } else {
            pins.to_str().unwrap().to_owned()
        };
        let output = Command::new(&ssh)
            .args(["-T", "-F", null])
            .args([
                "-oBatchMode=yes",
                "-oIdentitiesOnly=yes",
                "-oIdentityAgent=none",
                "-oStrictHostKeyChecking=yes",
                "-oConnectTimeout=5",
                "-oServerAliveInterval=2",
                "-oServerAliveCountMax=2",
            ])
            .arg(format!("-oGlobalKnownHostsFile={null}"))
            .arg(format!("-oUserKnownHostsFile={known_hosts}"))
            .args(["-p", &port.to_string(), "-i"])
            .arg(&client)
            .args(["fixture@127.0.0.1", &receipt_query])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, expected);
    };
    let absent = b"absent\nabsent\n";
    let request = Request {
        target: LoginRequest {
            endpoint: login.clone(),
            auth: LoginAuthentication::PrivateKey(client.clone()),
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
    let ask = fixture.open(Mode::Ask);
    no_task(fixture.send(ask, Message::Goal("Review the fixture question".into())));
    let start = fixture.send(ask, Message::Start);
    finish_agent_task(&mut fixture, start);
    assert!(fixture.run(ask).phase == Phase::ModelReview);

    let plan = fixture.open(Mode::Plan);
    no_task(fixture.send(plan, Message::Goal("Review the fixture plan".into())));
    let start = fixture.send(plan, Message::StartPlanWithoutProbe);
    finish_agent_task(&mut fixture, start);
    assert!(fixture.run(plan).phase == Phase::ModelReview);
    assert_idle(&model);

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
    check_receipts(absent);
    respond_to_model_review(
        &mut fixture,
        id,
        &model,
        None,
        json!({
            "status": "run_commands", "summary": "Write fixture receipts", "userMessage": "Review one command",
            "commands": [{
                "command": format!(
                    "/usr/bin/id -u > {receipt_name} && \
                     /usr/bin/stat -c %u -- {receipt_name} > {owner_name} && \
                     /usr/bin/chgrp -- fixture {receipt_name} {owner_name} && \
                     /usr/bin/chmod 0640 -- {receipt_name} {owner_name}"
                ),
                "purpose": "Verify the effective user and receipt owner",
                "risk": "requires_confirmation",
                "elevation": "password_sudo"
            }]
        }),
    );
    let run = fixture.run(id);
    assert!(run.phase == Phase::Approval, "{}", run.notice);
    assert!(run.reviewed_password_sudo().is_some());
    check_receipts(absent);
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
    check_receipts(absent);
    finish_peer_probe(&mut fixture, peer, probe);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
    ));
    check_receipts(absent);
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
    check_receipts(b"0\n0\n");
    assert!(fixture.run(ask).phase == Phase::ModelReview);
    assert!(fixture.run(plan).phase == Phase::ModelReview);
    respond_to_model_review(
        &mut fixture,
        ask,
        &model,
        None,
        json!({
            "status": "done",
            "summary": "Reviewed fixture answer",
            "userMessage": "The independent Ask completed."
        }),
    );
    assert!(fixture.run(ask).phase == Phase::Finished);
    assert!(fixture.run(plan).phase == Phase::ModelReview);
    assert!(fixture.run(id).phase == Phase::ModelReview);
    assert_idle(&model);
    drop(fixture.workspace.close_terminal_agent(id));
    drop(fixture.workspace.close_terminal_agent(ask));
    drop(fixture.workspace.close_terminal_agent(plan));
    let _ = fixture.workspace.sessions[&fixture.session]
        .session
        .stop()
        .recv_blocking();
}
