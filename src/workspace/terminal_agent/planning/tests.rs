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
    assert!(run.phase == Phase::ModelReview && run.target.is_none());
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
            json!({"status":"questions","summary":"Clarify","userMessage":"Choose environment","questions":[{"id":"q1","question":"Which environment? API_KEY=private-plan-question","options":["Local","Test"],"allowCustomAnswer":false}]}),
            report.clone(), // Wrong status for Options, which can repair once.
            json!({"status":"options","summary":"Options","userMessage":"Choose approach","options":[{"title":"Review files","summary":"Inspect before changes","feasibility":"Files are available","risks":[],"prerequisites":[],"steps":["Inspect"],"alternatives":[]}]}),
            json!({"status":"blocked","summary":"Unavailable","userMessage":"Only a final plan may settle this stage."}),
            report,
        ].into_iter().enumerate() {
            if matches!(index, 0 | 1 | 3) {
                reviewed = Some(received.recv_timeout(Duration::from_secs(5)).unwrap());
            }
            let [system, input] = reviewed.as_ref().unwrap();
            let rejected = matches!(index, 0 | 4).then(|| reject_json_format(&listener));
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
            if matches!(index, 2 | 4) {
                let repair: serde_json::Value = serde_json::from_str(sent).unwrap();
                assert_eq!(repair["reviewedContext"], *input);
            } else {
                assert_eq!(sent, input.as_str());
            }
            let request = request.to_string();
            assert!(request.contains("Execute nothing"));
            assert_eq!(request.contains("reviewedContext"), matches!(index, 2 | 4));
            if index >= 1 {
                assert!(request.contains("Local"));
            }
            if index >= 3 {
                assert!(request.contains("Review files"));
            }
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
    no_task(workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::Answer(epoch - 1, "Local".into())),
    ));
    assert!(workspace.terminal_agents.runs[&id].phase == Phase::PlanQuestions);
    let options = workspace.update_terminal_agent(
        id,
        RunMessage::Planning(Message::Answer(epoch, "Local".into())),
    );
    finish_review(&mut workspace, options);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert!(messages[0].contains(LOCAL_GUIDANCE));
    assert!(messages[1].contains("Local"));
    assert!(messages[1].contains("API_KEY=***"));
    assert!(!messages[1].contains("private-plan-question"));
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let options = send_review(&mut workspace, id, approval);
    finish(&mut workspace, options, owner);
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanOptions && run.target.is_none());
    let epoch = run.epoch;
    let report =
        workspace.update_terminal_agent(id, RunMessage::Planning(Message::Choose(epoch, 0)));
    finish_review(&mut workspace, report);
    let (approval, messages) = reviewed_messages(&workspace, id);
    assert!(messages[0].contains(LOCAL_GUIDANCE));
    assert!(messages[1].contains("Review files"));
    no_model_request(&listener);
    reviews.send(messages).unwrap();
    let report = send_review(&mut workspace, id, approval);
    finish(&mut workspace, report, owner);
    planner.join().unwrap();
    let run = &workspace.terminal_agents.runs[&id];
    assert!(run.phase == Phase::PlanReady && run.target.is_none());
    assert_eq!(run.presentation.model_usage.attempts().count(), 5);
    assert_eq!(run.presentation.model_usage.total(), Some(15));
    assert!(
        run.plan
            .as_deref()
            .unwrap()
            .contains("Find the configured port")
    );

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
    let run = &workspace.terminal_agents.runs[&reopened];
    assert!(run.phase == Phase::ProbeApproval);
    assert!(run.activity.iter().all(|line| !line.starts_with("$ ")));
    let _ = workspace.close_terminal_agent(reopened);

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
