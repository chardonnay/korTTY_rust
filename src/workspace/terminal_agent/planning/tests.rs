use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use iced::futures::{StreamExt as _, executor::block_on};

use super::super::{assert_only_agent_notice_task, request_review};
use super::*;
use crate::ai_manager::Configuration;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::session::{GridSize, IbmKind, Session, Status};
use crate::session_security::LocalSettings;
use crate::snippets::tests::Profile;
use crate::terminal_view::selection_test_frame;
use crate::workspace::ai_quota;

const CONTEXT_LIMIT: u32 = 4096;
const OUTPUT_LIMIT: u32 = 768;
const LOCAL_GUIDANCE: &str = "Read the service inventory before planning.";
const CLASSIFIED_GUIDANCE: &str = "Use the reviewed recovery checklist.";

fn finish(workspace: &mut Workspace, task: Task<Desktop>, owner: window::Id) {
    let mut stream = iced_runtime::task::into_stream(task).expect("scheduled planning work");
    let mut completed = false;
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
            completed |= matches!(
                message,
                RunMessage::Planning(Message::Reply(..) | Message::Storage(..))
                    | RunMessage::Directory(..)
            );
            let before = workspace.terminal_agents.runs[&id].phase;
            let next = workspace.update_terminal_agent(id, message);
            let after = workspace.terminal_agents.runs[&id].phase;
            if before != after && matches!(after, Phase::PlanOptions | Phase::ProbeApproval) {
                assert_only_agent_notice_task(next, owner);
            } else {
                no_task(next);
            }
        }
    }
    assert!(
        completed,
        "missing planning or implementation preflight result"
    );
}

fn open_plan(
    workspace: &mut Workspace,
    owner: window::Id,
    source: crate::session::SessionId,
) -> window::Id {
    let previous: Vec<_> = workspace.terminal_agents.runs.keys().copied().collect();
    let _ = workspace.open_terminal_agent(owner, source, Mode::Plan);
    *workspace
        .terminal_agents
        .runs
        .keys()
        .find(|id| !previous.contains(id))
        .unwrap()
}

fn no_task(task: Task<Desktop>) {
    assert!(iced_runtime::task::into_stream(task).is_none());
}

fn finish_review(workspace: &mut Workspace, task: Task<Desktop>) {
    let mut tasks = vec![task];
    while let Some(task) = tasks.pop() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
                    tasks.push(workspace.update_terminal_agent(id, message));
                }
            }
        }
    }
}

fn no_model_request(listener: &TcpListener) {
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

fn reviewed_messages(workspace: &Workspace, id: window::Id) -> (u64, [String; 2]) {
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::ModelReview);
    let review = run.model_review.as_ref().unwrap();
    let messages = [review.system.as_str(), review.input.as_str()];
    assert!(
        messages
            .iter()
            .map(|message| message.chars().count())
            .sum::<usize>()
            <= CONTEXT_LIMIT as usize
    );
    let estimate = ai_quota::estimate_input(messages);
    assert!(review.quota.label().unwrap().contains(&format!(
        "about {estimate} input tokens and up to {OUTPUT_LIMIT} output tokens"
    )));
    (run.epoch, messages.map(str::to_owned))
}

#[cfg(unix)]
#[test]
fn planning_requires_fixed_probe_approval_and_reuses_the_masked_snapshot() {
    use std::time::Instant;

    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);
    let session = Session::start_local(
        GridSize::default(),
        kortty_terminal::TerminalProfile::Xterm,
        kortty_pty::LaunchOptions {
            working_directory: Some(root.clone()),
            ..Default::default()
        },
        true,
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while session.frame().status == Status::Starting {
        assert!(Instant::now() < deadline, "planning shell did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(session.local_shell_running());
    workspace.sessions.get_mut(&source).unwrap().session = session;

    let id = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Goal("Inspect the current working directory".into()),
    ));
    let prepare = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, prepare);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::ProbeApproval);
    let command = run.reviewed_command().unwrap().1;
    assert!(command.contains("availableDiskKb") && command.contains("sudo -n -l"));
    let probe_epoch = run.epoch;
    no_task(workspace.update_terminal_agent(id, RunMessage::Approve(probe_epoch - 1)));
    no_task(workspace.update_terminal_agent(id, RunMessage::ApproveRemaining(probe_epoch)));
    no_model_request(&listener);
    let probe = workspace.update_terminal_agent(id, RunMessage::Approve(probe_epoch));
    finish_review(&mut workspace, probe);

    let (epoch, messages) = reviewed_messages(&workspace, id);
    let root = root.to_str().unwrap();
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    let snapshot = payload["probe"].clone();
    assert_eq!(snapshot["currentDir"], root);
    assert!(!snapshot["osRelease"].as_str().unwrap().is_empty());
    assert!(snapshot["availableDiskKb"].is_u64());
    assert!(
        workspace.terminal_agents.runs[&id]
            .planning
            .as_ref()
            .unwrap()
            .probe_summary()
            .unwrap()
            .contains(root)
    );
    no_model_request(&listener);
    let questions = send_review(&mut workspace, id, epoch);
    let mut socket = accept(&listener);
    let wire = request(&mut socket);
    assert_eq!(wire["messages"][1]["content"], messages[1]);
    let answer = json!({"status":"questions","summary":"Clarify","userMessage":"Choose detail","questions":[
        {"id":"q1","question":"What level of detail?","options":["Brief","Detailed"],"allowCustomAnswer":false}
    ]});
    reply_with_plan_json(&mut socket, answer);
    drop(socket);
    finish_review(&mut workspace, questions);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanQuestions);
    let options = workspace
        .update_terminal_agent(id, RunMessage::Planning(Message::SubmitAnswers(run.epoch)));
    finish_review(&mut workspace, options);
    let (epoch, messages) = reviewed_messages(&workspace, id);
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(payload["probe"], snapshot);
    no_model_request(&listener);
    let options = send_review(&mut workspace, id, epoch);
    let mut socket = accept(&listener);
    assert_eq!(request(&mut socket)["messages"][1]["content"], messages[1]);
    reply_with_plan_json(
        &mut socket,
        json!({"status":"options","summary":"Choose","userMessage":"Available approach","options":[
            {"title":"Inspect metadata","summary":"Use read-only facts","feasibility":"The probe located the directory","risks":[],"prerequisites":[],"steps":["Review the directory metadata"],"alternatives":[]}
        ]}),
    );
    drop(socket);
    finish_review(&mut workspace, options);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions);
    let report =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::Choose(run.epoch, 0)));
    finish_review(&mut workspace, report);
    let (_, messages) = reviewed_messages(&workspace, id);
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(payload["probe"], snapshot);
    no_model_request(&listener);
    drop(workspace.close_terminal_agent(id));

    let skipped = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(
        skipped,
        RunMessage::Goal("Plan without reading the environment".into()),
    ));
    let bypass = workspace.update_terminal_agent(skipped, RunMessage::StartPlanWithoutProbe);
    finish_review(&mut workspace, bypass);
    let (_, messages) = reviewed_messages(&workspace, skipped);
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert!(payload.get("probe").is_none());
    assert!(workspace.terminal_agents.runs[&skipped].target.is_none());
    no_model_request(&listener);
    let _ = workspace.sessions[&source].session.stop().recv_blocking();
}

fn reply_with_plan_json(
    socket: &mut std::io::BufReader<std::net::TcpStream>,
    answer: serde_json::Value,
) {
    let body = json!({"choices":[{"message":{"content":answer.to_string()}}], "usage":{"total_tokens":17}}).to_string();
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
}

#[test]
fn failed_planning_answer_revision_keeps_the_previous_approaches() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);
    let id = open_plan(&mut workspace, owner, source);
    no_task(
        workspace.update_terminal_agent(id, RunMessage::Goal("Plan a service inventory".into())),
    );
    let start = workspace.update_terminal_agent(id, RunMessage::StartPlanWithoutProbe);
    finish_review(&mut workspace, start);

    let send = |workspace: &mut Workspace, answer, expected_answers| {
        let (epoch, messages) = reviewed_messages(workspace, id);
        let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
        assert_eq!(payload["answers"], expected_answers);
        no_model_request(&listener);
        let pending = send_review(workspace, id, epoch);
        let mut socket = accept(&listener);
        assert_eq!(request(&mut socket)["messages"][1]["content"], messages[1]);
        reply_with_plan_json(&mut socket, answer);
        drop(socket);
        finish_review(workspace, pending);
    };
    send(
        &mut workspace,
        json!({"status":"questions","summary":"Scope","userMessage":"Choose scope","questions":[
            {"id":"q1","question":"Which scope?","options":["Local","Test"],"allowCustomAnswer":false}
        ]}),
        json!([]),
    );
    let submit = |workspace: &mut Workspace| {
        let epoch = workspace.terminal_agents.runs[&id].epoch;
        let task = workspace
            .update_terminal_agent(id, RunMessage::Planning(Message::SubmitAnswers(epoch)));
        finish_review(workspace, task);
    };
    let approach = |title| {
        json!({"status":"options","summary":"Options","userMessage":"Choose approach","options":[
            {"title":title,"summary":"Inspect first","feasibility":"Records are available","risks":[],"prerequisites":[],"steps":["Inspect"],"alternatives":[]}
        ]})
    };
    submit(&mut workspace);
    send(
        &mut workspace,
        approach("Original approach"),
        json!(["Local"]),
    );

    let epoch = workspace.terminal_agents.runs[&id].epoch;
    no_task(workspace.update_terminal_agent(id, RunMessage::Planning(Message::EditAnswers(epoch))));
    let epoch = workspace.terminal_agents.runs[&id].epoch;
    no_task(
        workspace
            .update_terminal_agent(id, RunMessage::Planning(Message::SelectAnswer(epoch, 0, 1))),
    );
    submit(&mut workspace);
    send(
        &mut workspace,
        json!({"status":"blocked","summary":"Unavailable","userMessage":"Please retry."}),
        json!(["Test"]),
    );
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanQuestions);
    assert_eq!(run.notice, "Please retry.");
    let flow = run.planning.as_ref().unwrap();
    assert_eq!(flow.answers, ["Local"]);
    assert_eq!(flow.options[0].title, "Original approach");

    submit(&mut workspace);
    send(
        &mut workspace,
        approach("Revised approach"),
        json!(["Test"]),
    );
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions);
    let draft =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::Choose(run.epoch, 0)));
    finish_review(&mut workspace, draft);
    let (_, messages) = reviewed_messages(&workspace, id);
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(payload["answers"], json!(["Test"]));
    assert_eq!(payload["selectedApproach"]["title"], "Revised approach");
    no_model_request(&listener);
}

fn send_review(workspace: &mut Workspace, id: window::Id, epoch: u64) -> Task<Desktop> {
    workspace.update_terminal_agent(
        id,
        RunMessage::ReviewModel(request_review::Message::Send(epoch)),
    )
}

fn fixture(
    root: &Path,
    endpoint: SocketAddr,
    context_limit: u32,
) -> (Workspace, window::Id, crate::session::SessionId) {
    fixture_with_model(root, endpoint, context_limit, "test")
}

fn fixture_with_model(
    root: &Path,
    endpoint: SocketAddr,
    context_limit: u32,
    model: &str,
) -> (Workspace, window::Id, crate::session::SessionId) {
    let model_mode = if model.is_empty() {
        "server-default"
    } else {
        "manual"
    };
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {
            "profiles": [{"id": 1, "name": "Planning fixture", "endpoint": endpoint.to_string(),
                "model": model, "model_mode": model_mode, "selection_limit": context_limit,
                "max_output_tokens": OUTPUT_LIMIT,
                "usage_budget": {"token_limit": 12000}}],
            "default": 1, "last_id": 1
        },
        "skills": {"last_id": 52, "skills": {
            "51": {"name": "Service inventory", "tags": ["service"], "target": "AGENT", "enabled": true, "content": LOCAL_GUIDANCE},
            "52": {"name": "Recovery", "target": "BOTH", "enabled": true, "content": CLASSIFIED_GUIDANCE}
        }}
    })).unwrap();
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());
    let (owner, _) = workspace.open_window(false);
    // There is no runnable shell in this fixture. Advisory stages must work
    // without opening any command transport or probing the terminal.
    let source = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    workspace.add_tab(owner, source);
    let pane = workspace.sessions.get_mut(&source).unwrap();
    pane.local_security = Some(LocalSettings::default());
    pane.frame = selection_test_frame(pane.frame.screen.clone());
    Arc::get_mut(&mut pane.frame).unwrap().status = Status::Running;
    (workspace, owner, source)
}

#[test]
fn planning_reviews_loopback_server_selection_before_sending_without_a_model() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) = fixture_with_model(&root, endpoint, CONTEXT_LIMIT, "");
    let id = open_plan(&mut workspace, owner, source);
    let choice = workspace.terminal_agents.runs[&id].profiles[0].clone();
    no_task(workspace.update_terminal_agent(id, RunMessage::Profile(choice)));
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Goal("Outline a documentation update".into()),
    ));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, start);
    let (epoch, [system, input]) = reviewed_messages(&workspace, id);
    let destination = endpoint.to_string();
    let review = workspace.terminal_agents.runs[&id]
        .model_review
        .as_ref()
        .unwrap();
    assert_eq!(
        review.reviewed_destination(),
        Some((destination.as_str(), "Server selects the model"))
    );
    no_model_request(&listener);

    let worker = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        let payload = request(&mut socket);
        assert!(payload.get("model").is_none());
        assert_eq!(payload["messages"][0]["content"], system);
        assert_eq!(payload["messages"][1]["content"], input);
        let content = json!({"status":"questions","summary":"Clarify the audience",
            "userMessage":"Choose an audience","questions":[{"id":"q1","question":"Who will read it?",
            "options":["Users","Maintainers"],"allowCustomAnswer":false}]});
        let body = json!({"choices":[{"message":{"content":content.to_string()}}]}).to_string();
        socket
            .get_mut()
            .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
            .unwrap();
    });
    let task = send_review(&mut workspace, id, epoch);
    finish(&mut workspace, task, owner);
    worker.join().unwrap();
    let run = &workspace.terminal_agents.runs[&id];
    assert!(
        run.phase == Phase::PlanQuestions && run.target.is_none(),
        "{}",
        run.notice
    );
}

fn configure_planning_https(
    workspace: &mut Workspace,
    root: &Path,
    endpoint: &str,
    allow_planning: bool,
    credential_env: &str,
) {
    let previous = Configuration::load_fixture_from(root).unwrap();
    let mut draft = serde_json::to_value(&previous).unwrap();
    draft["profiles"]["profiles"][0] = json!({
        "id": 1, "name": "Planning HTTPS fixture", "connection_mode": "remote-open-ai-exact",
        "endpoint": endpoint, "model_mode": "server-default", "trusted_remote_endpoint": true,
        "reviewed_agent_https": true, "reviewed_planning_https": allow_planning,
        "credential_env": credential_env, "selection_limit": CONTEXT_LIMIT,
        "max_output_tokens": OUTPUT_LIMIT, "usage_budget": {"token_limit": 12000}
    });
    let config: Configuration = serde_json::from_value(draft).unwrap();
    config.save_fixture_at(root, &previous).unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());
}

fn review_planning_https(
    workspace: &mut Workspace,
    owner: window::Id,
    source: crate::session::SessionId,
    endpoint: &str,
    goal: String,
) -> (window::Id, u64, [String; 2]) {
    let id = open_plan(workspace, owner, source);
    let choice = workspace.terminal_agents.runs[&id].profiles[0].clone();
    no_task(workspace.update_terminal_agent(id, RunMessage::Profile(choice)));
    no_task(workspace.update_terminal_agent(id, RunMessage::Goal(goal)));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(workspace, start);
    let (epoch, messages) = reviewed_messages(workspace, id);
    let review = workspace.terminal_agents.runs[&id]
        .model_review
        .as_ref()
        .unwrap();
    assert_eq!(
        review.reviewed_destination(),
        Some((endpoint, "Server selects the model"))
    );
    assert_eq!(review.send_label(), "Send reviewed HTTPS request");
    (id, epoch, messages)
}

#[test]
fn planning_https_requires_its_own_saved_permission_and_rechecks_revocation() {
    let scratch = Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, "127.0.0.1:12345".parse().unwrap(), CONTEXT_LIMIT);
    let endpoint = "https://example.invalid/tenant/v1/planning/";
    configure_planning_https(&mut workspace, &root, endpoint, false, "");
    assert!(
        workspace
            .build_agent(source, Mode::Plan, None, None, false)
            .is_err()
    );
    assert!(
        workspace
            .build_agent(source, Mode::Ask, None, None, false)
            .is_ok()
    );

    configure_planning_https(&mut workspace, &root, endpoint, true, "");
    let (id, epoch, _) = review_planning_https(
        &mut workspace,
        owner,
        source,
        endpoint,
        "Outline a documentation update".into(),
    );
    configure_planning_https(&mut workspace, &root, endpoint, false, "");
    let task = send_review(&mut workspace, id, epoch);
    finish_review(&mut workspace, task);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(
        run.phase == Phase::Blocked && run.target.is_none(),
        "{}",
        run.notice
    );
    assert!(run.notice.contains("changed"), "{}", run.notice);
}

#[test]
#[ignore = "requires the private planning HTTPS fixture and its dedicated CA"]
fn planning_https_transmits_only_the_exact_masked_review_without_a_model() {
    use crate::local_ai::test_support::with_https_fixture_ca;

    let _exclusive = exclusive_run();
    let endpoint = std::env::var("KORTTY_TEST_HTTPS_APPLY_URL").unwrap();
    let ca = std::fs::read(std::env::var_os("KORTTY_TEST_HTTPS_APPLY_CA_DER").unwrap()).unwrap();
    let expected = std::env::var_os("KORTTY_TEST_HTTPS_APPLY_EXPECTED").unwrap();
    let token = std::env::var("KORTTY_TEST_HTTPS_APPLY_KEY").unwrap();
    let scratch = Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, "127.0.0.1:12345".parse().unwrap(), CONTEXT_LIMIT);
    configure_planning_https(
        &mut workspace,
        &root,
        &endpoint,
        true,
        "KORTTY_TEST_HTTPS_APPLY_KEY",
    );
    let (id, epoch, messages) = review_planning_https(
        &mut workspace,
        owner,
        source,
        &endpoint,
        format!("Outline a documentation update with reference {token}"),
    );
    assert!(!messages[1].contains(&token));
    assert!(messages[1].contains("reference ***"));
    std::fs::write(expected, serde_json::to_vec(&messages).unwrap()).unwrap();

    let task = with_https_fixture_ca(ca, || send_review(&mut workspace, id, epoch));
    finish(&mut workspace, task, owner);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(
        run.phase == Phase::PlanQuestions && run.target.is_none(),
        "{}",
        run.notice
    );
}

fn reject_json_format(listener: &TcpListener) -> serde_json::Value {
    let mut socket = accept(listener);
    let mut request = request(&mut socket);
    assert_eq!(
        request.as_object_mut().unwrap().remove("response_format"),
        Some(json!({"type":"json_object"}))
    );
    let response = fixture_http_response(
        "400 Bad Request",
        "",
        r#"{"error":{"code":"unsupported_value","param":"response_format"}}"#,
    );
    socket.get_mut().write_all(response.as_bytes()).unwrap();
    request
}

#[test]
fn terminal_agent_planning_walks_review_steps_and_reopens_without_execution() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);

    let (reviews, received) = mpsc::channel::<[String; 2]>();
    let server = listener.try_clone().unwrap();
    let planner = std::thread::spawn(move || {
        let listener = server;
        let mut reviewed = None;
        let report = json!({"status":"final_plan","title":"Inspect service","summary":"Record findings",
            "userMessage":"Review the final plan","prerequisites":[],"steps":["Read the configuration"],"risks":[],"successCriteria":["Find the configured port"]});
        for (index, result) in [
            json!({"status":"questions","summary":"Clarify","userMessage":"Choose environment","questions":[
                {"id":"q1","question":"Which environment? API_KEY=private-plan-question","options":["Local","Test"],"allowCustomAnswer":true},
                {"id":"q2","question":"What level of detail?","options":["Brief","Detailed"],"allowCustomAnswer":false}
            ]}),
            report.clone(), // Wrong status for Options, which can repair once.
            json!({"status":"options","summary":"Options","userMessage":"Choose approach","options":[{"title":"Review files","summary":"Inspect before changes","feasibility":"Files are available","risks":[],"prerequisites":[],"steps":["Inspect"],"alternatives":[]}]}),
            json!({"status":"blocked","summary":"Unavailable","userMessage":"The revision is unavailable."}),
            json!({"status":"options","summary":"Revised options","userMessage":"Choose a revised approach","options":[{"title":"Inspect unit metadata","summary":"Use read-only records","feasibility":"Metadata is available","risks":[],"prerequisites":[],"steps":["Inspect unit metadata"],"alternatives":[]}]}),
            json!({"status":"blocked","summary":"Unavailable","userMessage":"Only a final plan may settle this stage."}),
            report,
        ].into_iter().enumerate() {
            if matches!(index, 0 | 1 | 3 | 4 | 5) {
                reviewed = Some(received.recv_timeout(Duration::from_secs(5)).unwrap());
            }
            let [system, input] = reviewed.as_ref().unwrap();
            let rejected = matches!(index, 0 | 6).then(|| reject_json_format(&listener));
            let mut socket = accept(&listener);
            let request = request(&mut socket);
            if let Some(rejected) = rejected {
                assert_eq!(request, rejected);
            } else {
                assert_eq!(request["response_format"], json!({"type":"json_object"}));
            }
            assert_eq!(request["max_tokens"], OUTPUT_LIMIT);
            assert_eq!(request["messages"][0]["content"], *system);
            let sent = request["messages"][1]["content"].as_str().unwrap();
            if matches!(index, 2 | 6) {
                let repair: serde_json::Value = serde_json::from_str(sent).unwrap();
                assert_eq!(repair["reviewedContext"], *input);
            } else {
                assert_eq!(sent, input.as_str());
            }
            let request = request.to_string();
            assert!(request.contains("Execute nothing"));
            assert_eq!(request.contains("reviewedContext"), matches!(index, 2 | 6));
            if index >= 1 {
                assert!(request.contains("Local"));
                assert!(request.contains("Detailed"));
                assert!(request.contains("Second answer line"));
            }
            assert!(!request.contains("private-answer-note"));
            assert!(!request.contains("disallowed answer"));
            if matches!(index, 3 | 4) {
                assert!(request.contains("Keep service read-only"));
                assert!(request.contains("Use local metadata"));
            }
            if index >= 5 {
                assert!(request.contains("Inspect unit metadata"));
                assert!(request.contains("Keep the unit name unchanged"));
            }
            assert!(!request.contains("private-approach-note"));
            assert!(!request.contains("private-report-note"));
            let body = json!({"choices":[{"message":{"content":result.to_string()}}],
                "usage":{"prompt_tokens":index+1,"completion_tokens":0}}).to_string();
            let response = fixture_http_response("200 OK", "", &body);
            socket.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });

    let id = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(id, RunMessage::Goal("Inspect service".into())));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, start);
    let (epoch, [system, _]) = reviewed_messages(&workspace, id);
    assert!(system.contains(LOCAL_GUIDANCE));
    assert!(!system.contains(CLASSIFIED_GUIDANCE));
    no_model_request(&listener);
    let classifier = workspace.update_terminal_agent(
        id,
        RunMessage::ReviewModel(request_review::Message::ReviewSkills(epoch)),
    );
    finish_review(&mut workspace, classifier);
    let classifier_epoch = workspace.terminal_agents.runs[&id].epoch;
    assert_ne!(epoch, classifier_epoch);
    no_task(send_review(&mut workspace, id, classifier_epoch));
    no_model_request(&listener);
    let classification = workspace.update_terminal_agent(
        id,
        RunMessage::ReviewModel(request_review::Message::Classify(classifier_epoch)),
    );
    let mut socket = accept(&listener);
    let classifier_request = request(&mut socket);
    assert_eq!(classifier_request["max_tokens"], 128);
    let payload: serde_json::Value = serde_json::from_str(
        classifier_request["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(
        payload["requestContext"]["question"]
            .as_str()
            .unwrap()
            .trim(),
        "Inspect service"
    );
    assert!(payload["requestContext"].get("terminalSelection").is_none());
    let body = json!({"choices":[{"message":{"content":"{\"skillIds\":[\"52\"]}"}}],
        "usage":{"prompt_tokens":9,"completion_tokens":4}})
    .to_string();
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
    drop(socket);
    finish_review(&mut workspace, classification);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert_ne!(approval, classifier_epoch);
    assert!(messages[0].contains(CLASSIFIED_GUIDANCE));
    assert!(!messages[0].contains(LOCAL_GUIDANCE));
    assert!(
        workspace.terminal_agents.runs[&id]
            .model_review
            .as_ref()
            .unwrap()
            .quota
            .label()
            .unwrap()
            .contains("13 of 12000 tokens reported")
    );
    no_task(send_review(&mut workspace, id, epoch));
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let questions = send_review(&mut workspace, id, approval);
    finish(&mut workspace, questions, owner);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanQuestions && run.target.is_none());
    let epoch = run.epoch;
    let paste =
        |notes: &str| text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(notes.into())));
    let answer =
        |question, action| RunMessage::Planning(Message::EditCustomAnswer(epoch, question, action));
    no_task(
        workspace
            .update_terminal_agent(id, RunMessage::Planning(Message::SubmitAnswers(epoch - 1))),
    );
    no_task(
        workspace
            .update_terminal_agent(id, RunMessage::Planning(Message::SelectAnswer(epoch, 1, 1))),
    );
    no_task(workspace.update_terminal_agent(id, answer(1, paste("disallowed answer"))));
    no_task(workspace.update_terminal_agent(
        id,
        answer(0, paste(&"x".repeat(MAX_CUSTOM_ANSWER_BYTES + 1))),
    ));
    no_task(workspace.update_terminal_agent(
        id,
        answer(0, paste("API_KEY=private-answer-note\nSecond answer line")),
    ));
    assert!(workspace.terminal_agents.runs[&id].phase == Phase::PlanQuestions);
    no_model_request(&listener);
    let options =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::SubmitAnswers(epoch)));
    finish_review(&mut workspace, options);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert!(messages[0].contains(LOCAL_GUIDANCE));
    let answers = json!([
        "Local\nAdditional details\nAPI_KEY=***\nSecond answer line",
        "Detailed"
    ]);
    let payload: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(payload["answers"], answers);
    assert!(messages[1].contains("API_KEY=***"));
    assert!(!messages[1].contains("private-plan-question"));
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let options = send_review(&mut workspace, id, approval);
    finish(&mut workspace, options, owner);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions && run.target.is_none());
    let options_epoch = run.epoch;
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::EditAnswers(options_epoch)),
    ));
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanQuestions);
    let answer_epoch = run.epoch;
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::SelectAnswer(answer_epoch, 0, 1)),
    ));
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::ReturnToResults(answer_epoch)),
    ));
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions);
    assert_ne!(run.epoch, options_epoch);
    let initial_options = run.epoch;
    let edit = |epoch, action| RunMessage::Planning(Message::EditRefinement(epoch, action));
    no_task(workspace.update_terminal_agent(
        id,
        edit(
            initial_options,
            paste(&"x".repeat(MAX_REFINEMENT_BYTES + 1)),
        ),
    ));
    no_task(workspace.update_terminal_agent(
        id,
        edit(
            initial_options,
            paste("Keep service read-only.\nAPI_KEY=private-approach-note\nUse local metadata."),
        ),
    ));
    let revise =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::Revise(initial_options)));
    finish_review(&mut workspace, revise);
    let (approval, messages) = reviewed_messages(&workspace, id);
    let revised: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(
        revised["refinement"],
        "Keep service read-only.\nAPI_KEY=***\nUse local metadata."
    );
    assert_eq!(revised["answers"], answers);
    assert!(revised["selectedApproach"].is_null());
    no_task(workspace.update_terminal_agent(id, edit(initial_options, paste("Unreviewed notes"))));
    assert_eq!(reviewed_messages(&workspace, id).1, messages);
    no_task(send_review(&mut workspace, id, initial_options));
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let revise = send_review(&mut workspace, id, approval);
    finish(&mut workspace, revise, owner);

    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions);
    assert_eq!(run.notice, "The revision is unavailable.");
    assert_eq!(
        run.planning.as_ref().unwrap().options[0].title,
        "Review files"
    );
    let retry =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::Revise(run.epoch)));
    finish_review(&mut workspace, retry);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&messages[1]).unwrap(),
        revised
    );
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let retry = send_review(&mut workspace, id, approval);
    finish(&mut workspace, retry, owner);

    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions && run.target.is_none());
    let epoch = run.epoch;
    assert_eq!(
        run.planning.as_ref().unwrap().options[0].title,
        "Inspect unit metadata"
    );
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::Choose(initial_options, 0)),
    ));
    no_task(workspace.update_terminal_agent(id, edit(epoch, text_editor::Action::SelectAll)));
    no_task(workspace.update_terminal_agent(
        id,
        edit(
            epoch,
            paste("Keep the unit name unchanged.\nAPI_KEY=private-report-note"),
        ),
    ));
    let report = workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::PrepareImplementation(epoch, 0)),
    );
    finish_review(&mut workspace, report);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert!(messages[0].contains(LOCAL_GUIDANCE));
    let drafted: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
    assert_eq!(
        drafted["selectedApproach"]["title"],
        "Inspect unit metadata"
    );
    assert_eq!(
        drafted["refinement"],
        "Keep the unit name unchanged.\nAPI_KEY=***"
    );
    no_task(workspace.update_terminal_agent(id, RunMessage::Implement(approval)));
    assert_eq!(workspace.terminal_agents.runs.len(), 1);
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let report = send_review(&mut workspace, id, approval);
    finish(&mut workspace, report, owner);
    planner.join().unwrap();
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanImplementationReview && run.target.is_none());
    assert_eq!(run.presentation.model_usage.attempts().count(), 7);
    assert_eq!(run.presentation.model_usage.total(), Some(28));
    assert!(
        run.plan
            .as_deref()
            .unwrap()
            .contains("Find the configured port")
    );
    assert_eq!(workspace.terminal_agents.runs.len(), 1);
    let review_epoch = run.epoch;
    no_task(
        workspace
            .update_terminal_agent(id, RunMessage::Planning(Message::KeepReport(review_epoch))),
    );
    no_task(workspace.update_terminal_agent(id, RunMessage::Implement(review_epoch)));
    assert!(workspace.terminal_agents.runs[&id].phase == Phase::PlanReady);
    assert_eq!(workspace.terminal_agents.runs.len(), 1);

    let save = workspace.update_terminal_agent(id, RunMessage::Planning(Message::Save));
    finish(&mut workspace, save, owner);
    let record = store::list(&root).unwrap().pop().unwrap();
    let _ = workspace.close_terminal_agent(id);
    let reopened = open_plan(&mut workspace, owner, source);
    let browse = workspace.update_terminal_agent(reopened, RunMessage::Planning(Message::Browse));
    finish(&mut workspace, browse, owner);
    let epoch = workspace.terminal_agents.runs[&reopened].epoch;
    no_task(workspace.update_terminal_agent(
        reopened,
        RunMessage::Planning(Message::Open(epoch, record.id.clone())),
    ));
    let run = workspace.terminal_agents.runs.get_mut(&reopened).unwrap();
    assert!(run.phase == Phase::PlanReady && run.target.is_none());
    assert_eq!(run.goal, "Inspect service");
    // Supply an explicit reviewed directory as in the Windows flow. The
    // ordinary local-shell actor is deliberately absent in this fixture.
    run.manual_directory = Some(String::new());
    let epoch = run.epoch;
    no_task(workspace.update_terminal_agent(
        reopened,
        RunMessage::DirectoryChoice(root.to_string_lossy().into_owned()),
    ));
    let implement = workspace.update_terminal_agent(reopened, RunMessage::Implement(epoch));
    finish(&mut workspace, implement, owner);
    let plan = &workspace.terminal_agents.runs[&reopened];
    assert!(plan.phase == Phase::PlanHandedOff && plan.target.is_none());
    let (&id, run) = workspace
        .terminal_agents
        .runs
        .iter()
        .find(|(_, run)| run.mode == Mode::Run)
        .unwrap();
    assert!(run.phase == Phase::ProbeApproval);
    assert!(workspace.hosted_tool_owner(id).is_none());
    assert_eq!(run.plan, plan.plan);
    assert!(run.activity.iter().all(|line| !line.starts_with("$ ")));
    let _ = workspace.close_terminal_agent(reopened);
    let _ = workspace.close_terminal_agent(id);

    let changed = open_plan(&mut workspace, owner, source);
    let browse = workspace.update_terminal_agent(changed, RunMessage::Planning(Message::Browse));
    finish(&mut workspace, browse, owner);
    let epoch = workspace.terminal_agents.runs[&changed].epoch;
    no_task(workspace.update_terminal_agent(
        changed,
        RunMessage::Planning(Message::Open(epoch, record.id)),
    ));
    workspace
        .sessions
        .get_mut(&source)
        .unwrap()
        .local_security
        .as_mut()
        .unwrap()
        .working_directory = Some(root.join("changed"));
    let epoch = workspace.terminal_agents.runs[&changed].epoch;
    assert_only_agent_notice_task(
        workspace.update_terminal_agent(changed, RunMessage::Implement(epoch)),
        owner,
    );
    let run = &workspace.terminal_agents.runs[&changed];
    assert!(run.phase == Phase::Blocked && run.target.is_none());
    assert!(run.notice.contains("destination changed"));
}

#[test]
fn accepted_plan_uses_the_current_chat_tab_preference_and_keeps_the_exact_source() {
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);
    let planner = open_plan(&mut workspace, owner, source);
    assert_eq!(workspace.hosted_tool_owner(planner), Some(owner));
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut config = serde_json::to_value(&baseline).unwrap();
    config["general"]["agent_run_presentation"] = json!("chat_tab");
    let config: Configuration = serde_json::from_value(config).unwrap();
    config.save_fixture_at(&root, &baseline).unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    let destination = workspace
        .planning_destination(&workspace.terminal_agents.runs[&planner])
        .unwrap();
    let report = Report {
        title: "Inspect service".into(),
        summary: "Review its configuration".into(),
        steps: vec!["Read the configuration".into()],
        success_criteria: vec!["Identify the reported port".into()],
        prerequisites: Vec::new(),
        risks: Vec::new(),
    };
    let mut flow = Flow::new(destination);
    flow.report = Some(report.clone());
    let shell = super::super::flow::local_agent_shell(&workspace.sessions[&source]).unwrap();
    let directory = crate::terminal_agent::LocalDirectory::capture(root.clone()).unwrap();
    let plan = workspace.terminal_agents.runs.get_mut(&planner).unwrap();
    plan.goal = "Inspect service".into();
    plan.plan = Some(report.render());
    plan.planning = Some(flow);
    plan.manual_directory = Some(root.to_str().unwrap().into());
    plan.target = Some(RunTarget::Attached(crate::terminal_agent::Target::Local {
        shell,
        directory: directory.clone(),
    }));
    let epoch = plan.next(Phase::PlanImplementationReview, "Review the plan");
    no_task(workspace.update_terminal_agent(
        planner,
        RunMessage::DirectoryChoice(root.join("unreviewed").to_str().unwrap().into()),
    ));
    no_task(workspace.update_terminal_agent(planner, RunMessage::Implement(epoch - 1)));
    assert_eq!(workspace.terminal_agents.runs.len(), 1);
    let implement = workspace.update_terminal_agent(planner, RunMessage::Implement(epoch));
    let tool = workspace.active_hosted_tab(owner).unwrap();
    assert_ne!(tool.instance, planner);
    let run = tool.instance;
    assert_eq!(workspace.hosted_agent_title(run), "AI Agent");
    finish(&mut workspace, implement, owner);
    let state = &workspace.terminal_agents.runs[&run];
    assert!(state.phase == Phase::ProbeApproval);
    assert!(matches!(
        &state.target,
        Some(RunTarget::Attached(crate::terminal_agent::Target::Local { directory: current, .. }))
            if current == &directory
    ));
    assert_eq!(state.binding.live().unwrap().session, source);
    assert_eq!(state.plan.as_deref(), Some(report.render().as_str()));
    assert_eq!(state.profile().unwrap().id, 1);
    drop(workspace.update_terminal_agent(planner, RunMessage::Focus));
    assert_eq!(
        workspace.active_hosted_tab(owner).unwrap().instance,
        planner
    );
    no_task(workspace.update_terminal_agent(planner, RunMessage::Implement(epoch)));
    assert_eq!(workspace.terminal_agents.runs.len(), 2);
    no_model_request(&listener);
    drop(workspace.close_terminal_agent(planner));
    assert!(workspace.terminal_agents.runs[&run].phase == Phase::ProbeApproval);

    let changed = root.join("unreviewed");
    std::fs::create_dir(&changed).unwrap();
    let changed = crate::terminal_agent::LocalDirectory::capture(changed).unwrap();
    let epoch = workspace
        .terminal_agents
        .runs
        .get_mut(&run)
        .unwrap()
        .next(Phase::Directory, "Checking the local directory");
    drop(workspace.update_terminal_agent(run, RunMessage::Directory(epoch, Ok(changed))));
    let state = &workspace.terminal_agents.runs[&run];
    assert!(state.phase == Phase::Blocked && state.notice.contains("directory changed"));
    no_model_request(&listener);
}

#[test]
fn planning_format_fallback_rechecks_the_reviewed_route_before_resending() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);
    let id = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(id, RunMessage::Goal("Inspect service".into())));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, start);
    let (epoch, [system, input]) = reviewed_messages(&workspace, id);
    no_model_request(&listener);
    let server = std::thread::spawn(move || {
        let request = reject_json_format(&listener);
        assert_eq!(request["messages"][0]["content"], system);
        assert_eq!(request["messages"][1]["content"], input);
        listener
    });
    let mut stream =
        iced_runtime::task::into_stream(send_review(&mut workspace, id, epoch)).unwrap();
    let mut next_planning = |workspace: &mut Workspace| loop {
        let action = block_on(stream.next()).expect("planning task ended without an update");
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(window, message)) = action {
            assert_eq!(window, id);
            match message {
                RunMessage::Planning(_) => break message,
                RunMessage::History(_) => no_task(workspace.update_terminal_agent(id, message)),
                _ => panic!("unexpected agent task output: {message:?}"),
            }
        }
    };
    let message @ RunMessage::Planning(Message::Progress(
        _,
        usage::Progress::FormatFallback(usage::AttemptKind::Initial, _),
    )) = next_planning(&mut workspace)
    else {
        panic!("expected an authorization request for the Questions fallback");
    };
    let listener = server.join().unwrap();
    no_model_request(&listener);

    workspace
        .sessions
        .get_mut(&source)
        .unwrap()
        .local_security
        .as_mut()
        .unwrap()
        .working_directory = Some(root.join("changed"));
    no_task(workspace.update_terminal_agent(id, message));
    let message @ RunMessage::Planning(Message::Reply(..)) = next_planning(&mut workspace) else {
        panic!("expected the settled Questions result");
    };
    assert_only_agent_notice_task(workspace.update_terminal_agent(id, message), owner);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::Blocked);
    assert!(run.notice.contains("source route changed"));
    no_model_request(&listener);
    assert!(block_on(stream.next()).is_none());
}

#[test]
fn planning_blocks_before_review_when_the_system_and_input_exceed_the_context_limit() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) = fixture(&root, listener.local_addr().unwrap(), 64);
    let id = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(id, RunMessage::Goal("Inspect service".into())));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, start);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::Blocked);
    assert!(run.model_review.is_none() && run.target.is_none());
    assert!(run.notice.contains("limit"));
    no_model_request(&listener);
}

#[test]
fn planning_discards_review_when_the_retained_terminal_stops() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, owner, source) =
        fixture(&root, listener.local_addr().unwrap(), CONTEXT_LIMIT);
    let id = open_plan(&mut workspace, owner, source);
    no_task(workspace.update_terminal_agent(id, RunMessage::Goal("Inspect service".into())));
    let start = workspace.update_terminal_agent(id, RunMessage::Start);
    finish_review(&mut workspace, start);
    let (epoch, _) = reviewed_messages(&workspace, id);

    // The fixture's actor is disconnected while its pane remains in the workspace.
    let _ = workspace.update(Desktop::SessionUpdate(source));
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::Cancelled && run.model_review.is_none());
    assert!(workspace.sessions.contains_key(&source));
    no_task(send_review(&mut workspace, id, epoch));
    no_model_request(&listener);
}
