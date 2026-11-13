use std::io::{ErrorKind, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use super::*;
use crate::local_ai::test_support::{accept, fixture_http_response, listener, request};
use crate::terminal_view::Selection;
use crate::workspace::ai_quota;
use request_review::Message as Ask;

const PROFILE_ROOT: &str = "KORTTY_INTERACTIVE_ASK_SKILLS_TEST_ROOT";
const TEST_NAME: &str = "workspace::terminal_agent::flow::tests::ask_hybrid::interactive_ask_classifies_reviewed_skills_and_sends_the_approved_messages";
const LOCAL_GUIDANCE: &str = "Read the disk inventory before forming advice.";
const CLASSIFIED_GUIDANCE: &str = "Use the reviewed recovery checklist.";
const PINNED_GUIDANCE: &str = "Keep this connection on the audited filesystem.";
const GOAL: &str = "Inspect disk usage. API_KEY=private-ask-goal";
const CONTEXT: &str = "APP_PASSWORD=private-selection";

fn with_skills(root: &std::path::Path, model: &TcpListener) -> Fixture {
    let endpoint = model.local_addr().unwrap().to_string();
    let mut fixture = Fixture::with_endpoint(false, false, &endpoint);
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [{
            "id": 1, "name": "Ask fixture", "model": "fixture:local", "endpoint": endpoint,
            "max_output_tokens": 8192, "usage_budget": {"token_limit": 3000}
        }], "default": 1, "last_id": 1},
        "skills": {"last_id": 54, "skills": {
            "51": {"name": "Disk", "tags": ["disk"], "target": "AGENT", "enabled": true, "content": LOCAL_GUIDANCE},
            "52": {"name": "Recovery", "target": "BOTH", "enabled": true, "content": CLASSIFIED_GUIDANCE},
            "53": {"name": "Chat only", "target": "CHAT", "enabled": true, "content": "Chat fixture body"},
            "54": {"name": "Audited connection", "target": "CONNECTION", "enabled": true, "content": PINNED_GUIDANCE}
        }}
    })).unwrap();
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());

    let pane = fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap();
    let mut saved = pane.saved_local.clone().unwrap();
    saved.id = 0;
    saved.terminal.ai_skills = vec![54];
    let mut store = Store::open(crate::profile::connections_path().unwrap()).unwrap();
    let local = store.save_local(saved).unwrap();
    pane.saved_local = store.local(local).cloned();
    let mut terminal = kortty_terminal::Terminal::new(80, 1);
    terminal.advance(CONTEXT.as_bytes());
    pane.frame = selection_test_frame(terminal.snapshot());
    Arc::get_mut(&mut pane.frame).unwrap().status = SessionStatus::Running;
    pane.selection = Some(Selection::from_cells((0, 0), (0, 79)));
    fixture.workspace.refresh_saved_terminal_preferences(&store);
    fixture
}

fn dispatch(fixture: &mut Fixture, id: window::Id, message: Message) {
    let task = fixture.send(id, message);
    finish_agent_task(fixture, task);
}

fn reply(socket: &mut std::io::BufReader<std::net::TcpStream>, content: &str) {
    let body = json!({
        "choices": [{"message": {"content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 9, "completion_tokens": 4}
    })
    .to_string();
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
}

fn no_request(model: &TcpListener) {
    assert!(matches!(model.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

#[test]
fn interactive_ask_classifies_reviewed_skills_and_sends_the_approved_messages() {
    let Some(root) = std::env::var_os(PROFILE_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let root = scratch.0.canonicalize().unwrap().join("ask-profile");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(PROFILE_ROOT, root)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    crate::profile::set_runtime_root(root.clone());
    assert_eq!(crate::profile::root().unwrap(), root);
    let (model, _) = listener();
    let mut fixture = with_skills(&root, &model);
    let skill_statistics = root.join("stats/skill-usage.json");
    let usage = local_ai::UsageAccount::new(
        root,
        1,
        local_ai::UsageBudget {
            token_limit: Some(3000),
            ..Default::default()
        },
    )
    .unwrap();
    let id = fixture.open(Mode::Ask);
    no_task(fixture.send(id, Message::Goal(GOAL.into())));
    dispatch(&mut fixture, id, Message::Start);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview);
    assert_eq!(run.context, "APP_PASSWORD=***");
    let local_epoch = run.epoch;
    let review = run.model_review.as_ref().unwrap();
    assert!(review.system.contains(LOCAL_GUIDANCE) && review.system.contains(PINNED_GUIDANCE));
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains("0 of 3000 tokens reported")
    );
    assert_eq!(usage.snapshot().unwrap().counts.requests, 0);
    no_request(&model);

    dispatch(
        &mut fixture,
        id,
        Message::ReviewModel(Ask::ReviewSkills(local_epoch)),
    );
    let classifier_epoch = fixture.run(id).epoch;
    assert_ne!(local_epoch, classifier_epoch);
    no_task(fixture.send(id, Message::ReviewModel(Ask::Send(local_epoch))));
    no_task(fixture.send(id, Message::ReviewModel(Ask::Send(classifier_epoch))));
    no_request(&model);
    let classification = fixture.send(id, Message::ReviewModel(Ask::Classify(classifier_epoch)));
    let mut socket = accept(&model);
    let wire = request(&mut socket);
    assert_eq!(wire["max_tokens"], 128);
    let payload: Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(
        payload["requestContext"],
        json!({
            "question":"Inspect disk usage. API_KEY=***", "terminalSelection":"APP_PASSWORD=***"
        })
    );
    let candidate_ids: Vec<_> = payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["id"].as_str().unwrap())
        .collect();
    assert_eq!(candidate_ids, ["51", "52"]);
    let encoded = wire.to_string();
    for private in [
        "private-ask-goal",
        "private-selection",
        LOCAL_GUIDANCE,
        CLASSIFIED_GUIDANCE,
        PINNED_GUIDANCE,
    ] {
        assert!(!encoded.contains(private));
    }
    reply(&mut socket, r#"{"skillIds":["52","54"]}"#);
    drop(socket);
    finish_agent_task(&mut fixture, classification);

    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview);
    let approval = run.epoch;
    assert_ne!(approval, classifier_epoch);
    let review = run.model_review.as_ref().unwrap();
    let system = review.system.to_string();
    let input = review.input.to_string();
    assert!(system.contains(CLASSIFIED_GUIDANCE) && system.contains(PINNED_GUIDANCE));
    assert!(!system.contains(LOCAL_GUIDANCE));
    let input_estimate = ai_quota::estimate_input([system.as_str(), input.as_str()]);
    let quota = review.quota.label().unwrap();
    assert!(quota.contains("13 of 3000 tokens reported"));
    assert!(quota.contains(&format!(
        "about {input_estimate} input tokens and up to 2048 output tokens"
    )));
    assert_eq!(usage.snapshot().unwrap().counts.requests, 1);
    crate::ai_manager::flush_skill_statistics_for_tests();
    assert!(!skill_statistics.exists());
    no_task(fixture.send(id, Message::ReviewModel(Ask::Send(classifier_epoch))));
    no_request(&model);

    let pending = fixture.send(id, Message::ReviewModel(Ask::Send(approval)));
    let mut socket = accept(&model);
    let wire = request(&mut socket);
    assert_eq!(wire["max_tokens"], 2048);
    assert_eq!(wire["messages"][0]["content"], system);
    assert_eq!(wire["messages"][1]["content"], input);
    let input: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(input["goal"], "Inspect disk usage. API_KEY=***");
    assert_eq!(input["context"], "APP_PASSWORD=***");
    reply(
        &mut socket,
        r#"{"status":"done","summary":"Done","userMessage":"The fixture answered."}"#,
    );
    drop(socket);
    finish_agent_task(&mut fixture, pending);
    assert!(fixture.run(id).phase == Phase::Finished);
    let snapshot = usage.snapshot().unwrap();
    assert_eq!(
        (
            snapshot.counts.requests,
            snapshot.in_flight,
            snapshot.reserved_output
        ),
        (2, 0, 0)
    );
    crate::ai_manager::flush_skill_statistics_for_tests();
    let recorded = std::fs::read_to_string(&skill_statistics).unwrap();
    let statistics: Value = serde_json::from_str(&recorded).unwrap();
    let skills = statistics["skills"].as_object().unwrap();
    assert_eq!(
        skills.keys().map(String::as_str).collect::<Vec<_>>(),
        ["52", "54"]
    );
    for (id, automatic, pinned) in [("52", 1, 0), ("54", 0, 1)] {
        let daily = skills[id]["daily"].as_object().unwrap();
        assert_eq!(daily.len(), 1);
        let bucket = daily.values().next().unwrap();
        assert_eq!(bucket["automatic"], automatic);
        assert_eq!(bucket["pinned"], pinned);
        assert_eq!(bucket["surfaces"]["agent"], 1);
    }
    assert!(!recorded.contains(CLASSIFIED_GUIDANCE));
    assert!(!recorded.contains(PINNED_GUIDANCE));
    no_request(&model);

    let stale = fixture.open(Mode::Ask);
    no_task(fixture.send(stale, Message::Goal(GOAL.into())));
    dispatch(&mut fixture, stale, Message::Start);
    assert!(fixture.run(stale).phase == Phase::ModelReview);
    let stale_epoch = fixture.run(stale).epoch;
    let local = fixture.workspace.sessions[&fixture.session]
        .saved_local
        .as_ref()
        .unwrap()
        .id;
    let mut store = Store::open(crate::profile::connections_path().unwrap()).unwrap();
    let mut saved = store.local(local).unwrap().clone();
    saved.terminal.ai_skills.clear();
    store.save_local(saved).unwrap();
    dispatch(
        &mut fixture,
        stale,
        Message::ReviewModel(Ask::Send(stale_epoch)),
    );
    assert!(fixture.run(stale).phase == Phase::Blocked);
    assert!(
        fixture
            .run(stale)
            .notice
            .contains("saved skill assignments changed")
    );
    assert_eq!(usage.snapshot().unwrap().counts.requests, 2);
    no_request(&model);

    fixture.workspace.refresh_saved_terminal_preferences(&store);
    let removed = fixture.open(Mode::Ask);
    no_task(fixture.send(removed, Message::Goal(GOAL.into())));
    dispatch(&mut fixture, removed, Message::Start);
    assert!(fixture.run(removed).phase == Phase::ModelReview);
    let removed_epoch = fixture.run(removed).epoch;
    store.delete(local).unwrap();
    dispatch(
        &mut fixture,
        removed,
        Message::ReviewModel(Ask::Send(removed_epoch)),
    );
    assert!(fixture.run(removed).phase == Phase::Blocked);
    assert_eq!(usage.snapshot().unwrap().counts.requests, 2);
    crate::ai_manager::flush_skill_statistics_for_tests();
    assert_eq!(std::fs::read_to_string(skill_statistics).unwrap(), recorded);
    no_request(&model);
}
