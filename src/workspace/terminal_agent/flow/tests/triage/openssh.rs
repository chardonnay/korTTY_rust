use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use kortty_ssh::{Authentication, Connect, Endpoint, SshSession, TrustStore};

use super::*;
use crate::session::StreamOptions;
use crate::ssh_connection::Connected;
use crate::ssh_dialog::{LoginTarget, Request};
use crate::ssh_worker::OpenSshFixture;
use crate::terminal_logging::Prepared;
use crate::workspace::ssh_access::SshTarget;
use crate::workspace::terminal_agent::planning;
use kortty_terminal::TerminalProfile;

#[cfg(target_os = "linux")]
mod pam;

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

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn reviewed_planning_probe_preserves_the_live_openssh_directory() {
    with_openssh_planning_review(|fixture, id, _, moved| {
        let epoch = fixture.run(id).epoch;
        move_to_other_directory(fixture, moved);
        let stale = fixture.send(
            id,
            Message::ReviewModel(request_review::Message::Send(epoch)),
        );
        finish_agent_task(fixture, stale);
        assert_planning_directory_blocked(fixture, id);
    });
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn plan_implementation_refuses_a_live_openssh_directory_change() {
    with_openssh_planning_review(|fixture, id, model, moved| {
        respond_to_plan_review(
            fixture,
            id,
            model,
            Stage::Options,
            json!({
                "status":"options","summary":"Approach","userMessage":"Review approach","options":[{
                    "title":"Review metadata","summary":"Inspect the directory metadata","feasibility":"Read-only",
                    "risks":[],"prerequisites":[],"steps":["Review metadata"],"alternatives":[]
                }]
            }),
        );
        let run = fixture.run(id);
        assert!(run.phase == Phase::PlanOptions, "{}", run.notice);
        let prepare = fixture.send(
            id,
            Message::Planning(planning::Message::PrepareImplementation(run.epoch, 0)),
        );
        finish_agent_task(fixture, prepare);
        respond_to_plan_review(
            fixture,
            id,
            model,
            Stage::Report,
            json!({
                "status":"final_plan","title":"Review metadata","summary":"Inspect directory metadata",
                "userMessage":"Review the implementation plan","prerequisites":[],"risks":[],
                "steps":["Inspect its metadata"],"successCriteria":["Record the directory metadata"]
            }),
        );
        let run = fixture.run(id);
        assert!(
            run.phase == Phase::PlanImplementationReview,
            "{}",
            run.notice
        );
        assert_eq!(fixture.workspace.terminal_agents.runs.len(), 1);
        let epoch = run.epoch;
        move_to_other_directory(fixture, moved);
        let stale = fixture.send(id, Message::Implement(epoch));
        finish_agent_task(fixture, stale);
        assert_planning_directory_blocked(fixture, id);
        assert_eq!(fixture.workspace.terminal_agents.runs.len(), 1);
    });
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn implementation_sudo_preflight_authenticates_before_the_first_model_review() {
    check_implementation_sudo_preflight(IsolationLevel::None);
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn isolated_ssh_implementation_sudo_preflight_uses_the_authenticated_terminal() {
    check_implementation_sudo_preflight(IsolationLevel::Sandbox);
}

fn check_implementation_sudo_preflight(isolation: IsolationLevel) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

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
    let receipt = root.join("reviewed-exec");
    let mut password_bytes = [0; 32];
    getrandom::fill(&mut password_bytes).unwrap();
    let sudo_password = URL_SAFE_NO_PAD.encode(password_bytes);
    fs::write(root.join("sudo-password"), &sudo_password).unwrap();
    let server = start_shell_with_exec(&ssh, &root, &root, &receipt, SUDO_PREFLIGHT_FIXTURE);
    attach_shell_with_isolation(&mut fixture, &ssh, &root, isolation);
    let login = ssh.request(root.join("client")).target.endpoint;
    set_sudo_vault(
        &mut fixture,
        root.join("fixture-vault"),
        &login,
        sudo_password,
    );

    let id = fixture.open(Mode::Run);
    no_task(fixture.send(id, Message::Goal("Check directory metadata".into())));
    fixture.run_mut(id).plan = Some("Review the directory metadata.".into());
    let start = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, start);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ProbeApproval, "{}", run.notice);
    assert!(
        run.reviewed_command()
            .unwrap()
            .1
            .contains("field rootEscalationMode")
    );
    assert!(!receipt.exists());
    let probe = fixture.send(id, Message::Approve(run.epoch));
    finish_agent_task(&mut fixture, probe);

    let run = fixture.run(id);
    assert!(run.phase == Phase::SudoPreflight, "{}", run.notice);
    assert_eq!(
        run.sudo_preflight.as_ref().unwrap().credential_label(),
        Some("Fixture sudo credential"),
    );
    let epoch = run.epoch;
    no_task(fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            epoch - 1,
            password::Source::Vault,
        )),
    ));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            epoch,
            password::Source::Ticket,
        )),
    ));
    assert!(
        !fs::read_to_string(&receipt)
            .unwrap()
            .contains(" kortty-sudo ")
    );
    assert_idle(&model);

    let (peer, probe) = hold_peer_probe(&mut fixture);
    let before_contention = fs::read_to_string(&receipt).unwrap();
    no_task(fixture.send(
        id,
        Message::Password(password::Message::AuthenticatePlan(
            epoch,
            password::Source::Vault,
        )),
    ));
    let run = fixture.run(id);
    assert!(run.phase == Phase::SudoPreflight);
    assert_ne!(run.epoch, epoch);
    assert!(run.sudo_preflight.is_some());
    assert_eq!(run.sudo_preflight_attempts, 0);
    assert_eq!(fs::read_to_string(&receipt).unwrap(), before_contention);
    finish_peer_probe(&mut fixture, peer, probe);

    for expected_phase in [Phase::SudoPreflight, Phase::ModelReview] {
        authenticate_plan_with_vault(&mut fixture, id, &model, |_| {});
        let run = fixture.run(id);
        assert!(run.phase == expected_phase, "{}", run.notice);
        if expected_phase == Phase::SudoPreflight {
            assert!(run.notice.contains("authentication was rejected"));
            assert!(run.sudo_terminal.is_none());
            assert_idle(&model);
        }
    }
    let run = fixture.run(id);
    assert!(
        run.phase == Phase::ModelReview && run.sudo_terminal.is_some(),
        "{}",
        run.notice
    );
    assert!(!root.join("approved-sudo-step").exists());
    respond_to_model_review(
        &mut fixture,
        id,
        &model,
        None,
        json!({
            "status":"run_commands","summary":"Inspect","userMessage":"Review one command",
            "commands":[{"command":"id -u","purpose":"Check the effective user",
                "risk":"requires_confirmation","elevation":"password_sudo"}]
        }),
    );
    let run = fixture.run(id);
    assert!(
        run.phase == Phase::Approval && run.reviewed_password_sudo().is_some(),
        "{}",
        run.notice
    );
    assert!(!root.join("approved-sudo-step").exists());
    let epoch = run.epoch;
    let (peer, probe) = hold_peer_probe(&mut fixture);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
    ));
    let run = fixture.run(id);
    assert!(run.phase == Phase::Approval);
    assert_ne!(run.epoch, epoch);
    assert!(run.reviewed_password_sudo().is_some());
    assert!(run.sudo_terminal.is_some());
    assert!(!root.join("approved-sudo-step").exists());
    finish_peer_probe(&mut fixture, peer, probe);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
    ));
    assert!(!root.join("approved-sudo-step").exists());
    let command = fixture.send(
        id,
        Message::Password(password::Message::Approve(
            fixture.run(id).epoch,
            password::Source::Ticket,
        )),
    );
    finish_agent_task(&mut fixture, command);
    assert_eq!(
        fs::read_to_string(root.join("approved-sudo-step")).unwrap(),
        "approved\n"
    );
    assert_eq!(
        fs::read_to_string(&receipt)
            .unwrap()
            .matches(" kortty-sudo ")
            .count(),
        2
    );
    assert_idle(&model);
    drop(fixture.workspace.close_terminal_agent(id));

    let skipped = fixture.open(Mode::Run);
    no_task(fixture.send(
        skipped,
        Message::Goal("Check the directory without preflight".into()),
    ));
    fixture.run_mut(skipped).plan = Some("Review directory metadata.".into());
    let start = fixture.send(skipped, Message::Start);
    finish_agent_task(&mut fixture, start);
    assert!(fixture.run(skipped).phase == Phase::ProbeApproval);
    let probe = fixture.send(skipped, Message::Approve(fixture.run(skipped).epoch));
    finish_agent_task(&mut fixture, probe);
    let run = fixture.run(skipped);
    assert!(run.phase == Phase::SudoPreflight);
    let skip = fixture.send(
        skipped,
        Message::Password(password::Message::SkipPlanAuthentication(run.epoch)),
    );
    finish_agent_task(&mut fixture, skip);
    let run = fixture.run(skipped);
    assert!(run.phase == Phase::ModelReview && run.sudo_terminal.is_none());
    assert_eq!(
        fs::read_to_string(&receipt)
            .unwrap()
            .matches(" kortty-sudo ")
            .count(),
        2
    );
    assert_idle(&model);
    drop(fixture.workspace.close_terminal_agent(skipped));
    let _ = fixture.workspace.sessions[&fixture.session]
        .session
        .stop()
        .recv_blocking();
    drop(fixture);
    let output = server.join().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("Accepted publickey for "));
}

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

const SUDO_PREFLIGHT_FIXTURE: &str = r#"
case "$SSH_ORIGINAL_COMMAND" in
    *"field rootEscalationMode"*)
        printf '%s\n' 'osRelease=Fixture' "currentDir=$FIXTURE_ROOT" \
            'alreadyRoot=false' 'sudoAvailable=true' 'passwordlessSudo=false'
        exit
        ;;
    *" kortty-sudo "*)
        stty raw -echo -echonl
        armed=$(printf '%s\n' "$SSH_ORIGINAL_COMMAND" | sed -n "s/^.*'\(KORTTY_SUDO_ARMED_[0-9a-f]\{32\}\)'$/\1/p")
        token=${armed#KORTTY_SUDO_ARMED_}
        [ "${#token}" -eq 32 ] || exit 1
        printf '%s\n[kortty-sudo-%s] ' "$armed" "$token"
        IFS= read -r supplied || exit 1
        [ "$supplied" = "$(cat "$FIXTURE_ROOT/sudo-password")" ] || exit 1
        if ! [ -e "$FIXTURE_ROOT/rejected-once" ]; then
            touch "$FIXTURE_ROOT/rejected-once"
            printf '\036%s:1\n' "$token"
            exit 1
        fi
        printf '\036%s:0\n' "$token"
        if IFS=' ' read -r request encoded; then
            [ -n "$request" ] && [ -n "$encoded" ] || exit 1
            printf 'approved\n' > "$FIXTURE_ROOT/approved-sudo-step"
            printf '\036%s:0\n' "$request"
        fi
        while IFS= read -r ignored; do :; done
        exit
        ;;
esac
exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"
"#;

fn with_openssh_planning_review(test: impl FnOnce(&mut Fixture, window::Id, &TcpListener, &Path)) {
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

    let id = fixture.open(Mode::Plan);
    no_task(fixture.send(id, Message::Goal("Plan a directory inventory".into())));
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
    assert_eq!(review.plan_stage(), Some(Stage::Questions));
    let input: serde_json::Value = serde_json::from_str(&review.input).unwrap();
    let snapshot = input["probe"].clone();
    assert_eq!(snapshot["currentDir"], root.to_str().unwrap());
    assert!(snapshot["availableDiskKb"].is_u64());
    assert_idle(&model);
    respond_to_plan_review(
        &mut fixture,
        id,
        &model,
        Stage::Questions,
        json!({"status":"questions","summary":"Choose scope","userMessage":"Review scope",
            "questions":[{"id":"q1","question":"Which scope?","options":["Current directory","Subdirectories"],"allowCustomAnswer":false}]}),
    );

    let run = fixture.run(id);
    assert!(run.phase == Phase::PlanQuestions, "{}", run.notice);
    let review = fixture.send(
        id,
        Message::Planning(planning::Message::SubmitAnswers(run.epoch)),
    );
    finish_agent_task(&mut fixture, review);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview, "{}", run.notice);
    let input: serde_json::Value =
        serde_json::from_str(&run.model_review.as_ref().unwrap().input).unwrap();
    assert_eq!(input["probe"], snapshot);
    assert_eq!(input["answers"], json!(["Current directory"]));

    test(&mut fixture, id, &model, &moved);
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

fn respond_to_plan_review(
    fixture: &mut Fixture,
    id: window::Id,
    model: &TcpListener,
    stage: Stage,
    reply: serde_json::Value,
) {
    respond_to_model_review(fixture, id, model, Some(stage), reply);
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

fn move_to_other_directory(fixture: &mut Fixture, moved: &Path) {
    fixture.workspace.sessions[&fixture.session]
        .session
        .send(b"move\r".to_vec())
        .unwrap();
    wait_for_directory(fixture, moved);
}

fn assert_planning_directory_blocked(fixture: &Fixture, id: window::Id) {
    let run = fixture.run(id);
    assert!(
        run.phase == Phase::Blocked && run.notice.contains("directory changed"),
        "{}",
        run.notice
    );
}

fn start_shell(
    ssh: &OpenSshFixture,
    root: &std::path::Path,
    moved: &std::path::Path,
    receipt: &std::path::Path,
) -> thread::JoinHandle<std::process::Output> {
    start_shell_with_exec(
        ssh,
        root,
        moved,
        receipt,
        r#"exec /bin/sh -c "$SSH_ORIGINAL_COMMAND""#,
    )
}

fn start_shell_with_exec(
    ssh: &OpenSshFixture,
    root: &Path,
    moved: &Path,
    receipt: &Path,
    execute: &str,
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
    FIXTURE_ROOT={}
    {execute}
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
    attach_shell_with_isolation(fixture, ssh, directory, IsolationLevel::None);
}

fn attach_shell_with_isolation(
    fixture: &mut Fixture,
    ssh: &OpenSshFixture,
    directory: &Path,
    isolation: IsolationLevel,
) {
    attach_reviewed_shell(
        fixture,
        ssh.request(ssh.directory().join("client")),
        &ssh.pinned_trust(),
        directory,
        isolation,
    );
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
