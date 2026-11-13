use std::io::ErrorKind;
use std::net::TcpListener;

use serde_json::{Value, json};

use super::*;
use crate::connection_store::{LocalProfile, Store, TerminalOverrides};
use crate::local_ai::{self, test_support};
use request_review::Message as Review;

const FIRST: &str = "Keep the first disk inventory private to this target.";
const SECOND: &str = "Use the second connection's audited inventory.";
const LOCAL: &str = "Read the disk inventory.";
const CLASSIFIED: &str = "Use the reviewed recovery checklist.";
const MASKED_GOAL: &str = "Inspect disk usage. API_KEY=***";

fn idle(model: &TcpListener) {
    assert!(matches!(model.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

fn response(socket: &mut impl std::io::Write, content: &str) {
    let body = json!({"choices":[{"message":{"content":content}}],
        "usage":{"prompt_tokens":9,"completion_tokens":4}})
    .to_string();
    socket
        .write_all(test_support::fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
}

#[test]
fn swarm_requires_explicit_https_selection_and_checks_opt_in_for_each_target() {
    let (model, _) = test_support::listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let destination = format!("https://{}/v1", model.local_addr().unwrap());
    let mut draft = json!({"profiles": {
        "profiles":[{"id":1, "name":"HTTPS Swarm fixture", "model":"fixture",
            "connection_mode":"remote-open-ai", "endpoint":destination,
            "trusted_remote_endpoint":true, "reviewed_agent_https":true}],
        "default":1, "last_id":1
    }});
    let config: Configuration = serde_json::from_value(draft.clone()).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    let saved = Configuration::load_fixture_from(&root).unwrap();
    workspace.set_test_ai_configuration(saved.clone());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, true);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let swarm = &workspace.terminal_agents.swarms[&id];
    assert!(swarm.profile.is_none());
    assert!(swarm.modes.contains(&SwarmMode::Advisory));
    let profile = swarm.profiles[0].clone();
    let _ = workspace.swarm_action(id, Message::Mode(SwarmMode::Advisory));
    let _ = workspace.swarm_action(id, Message::Goal("Inspect password=fixture-secret".into()));
    let keys = review_source(&mut workspace, id, &root);
    assert_eq!(keys.len(), 2);
    for key in keys {
        let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    }
    let _ = workspace.swarm_action(id, Message::Start);
    assert!(workspace.terminal_agents.swarms[&id].queue.is_none());
    let _ = workspace.swarm_action(id, Message::Profile(profile));
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    let members: Vec<_> = workspace.terminal_agents.swarms[&id]
        .members
        .keys()
        .copied()
        .collect();
    assert_eq!(members.len(), 2);
    for member in &members {
        let run = &workspace.terminal_agents.runs[member];
        assert!(run.phase == Phase::ModelReview);
        let review = run.model_review.as_ref().unwrap();
        assert!(review.transport().0.is_remote());
        assert_eq!(review.send_label(), "Send reviewed HTTPS request");
        assert!(!review.input.contains("fixture-secret"));
    }
    idle(&model);
    draft["profiles"]["profiles"][0]["reviewed_agent_https"] = json!(false);
    let revoked: Configuration = serde_json::from_value(draft).unwrap();
    revoked.save_fixture_at(&root, &saved).unwrap();
    for member in members {
        let epoch = workspace.terminal_agents.runs[&member].epoch;
        let task = workspace.swarm_action(
            id,
            Message::Member(member, MemberAction::ReviewModel(Review::Send(epoch))),
        );
        drain(&mut workspace, task);
    }
    let swarm = &workspace.terminal_agents.swarms[&id];
    assert!(
        swarm
            .queue
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .all(|row| row.status == Status::Blocked && row.detail.contains("changed"))
    );
    idle(&model);
}

#[test]
fn swarm_cli_sends_only_independently_reviewed_masked_payloads() {
    use super::super::comparison::Message as Compare;
    use crate::local_ai::cli::{Program, fixture_executable};

    let _exclusive = test_support::exclusive_run();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
    let decision = json!({"status":"done","summary":"Checked","userMessage":"Target passed."});
    let draft: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [{"id": 7, "name": "Reviewed Swarm CLI",
            "connection_mode": "cli", "endpoint": "", "vision": "disabled", "cli": {
                "path": executable, "arguments": ["fixture_scheduled_proposal", decision.to_string()],
                "fingerprint": fingerprint
            }}], "default": 7, "last_id": 7}
    })).unwrap();
    draft
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let config = Configuration::load_fixture_from(&root).unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.set_test_ai_configuration(config.clone());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, true);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let swarm = &workspace.terminal_agents.swarms[&id];
    let choice = swarm.profile.as_ref().unwrap();
    assert_eq!(choice.id, 7);
    assert!(choice.reviewed_cli.is_some());
    let _ = workspace.swarm_action(id, Message::Mode(SwarmMode::Advisory));
    let _ = workspace.swarm_action(id, Message::Goal("Inspect password=fixture-secret".into()));
    let keys = review_source(&mut workspace, id, &root);
    assert_eq!(keys.len(), 2);
    for key in keys {
        let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    }
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    let members: Vec<_> = workspace.terminal_agents.swarms[&id]
        .members
        .keys()
        .copied()
        .collect();
    assert_eq!(members.len(), 2);
    let marker = root.join("fixture-scheduled-prompt");
    for member in members {
        assert!(!marker.exists());
        let run = &workspace.terminal_agents.runs[&member];
        assert!(run.phase == Phase::ModelReview);
        let review = run.model_review.as_ref().unwrap();
        assert_eq!(review.send_label(), "Run reviewed CLI request");
        assert!(review.classifier.is_none());
        let reviewed = review.cli.as_ref().unwrap().primary().to_owned();
        assert!(reviewed.contains("password=***"));
        assert!(!reviewed.contains("fixture-secret"));
        let task = workspace.swarm_action(
            id,
            Message::Member(member, MemberAction::ReviewModel(Review::Send(run.epoch))),
        );
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        while let Some(action) = block_on(stream.next()) {
            if let iced_runtime::Action::Output(Desktop::TerminalAgent(target, message)) = action {
                assert_eq!(target, member);
                let next = workspace.update_terminal_agent(target, message);
                drain(&mut workspace, next);
            }
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), reviewed);
        std::fs::remove_file(&marker).unwrap();
    }
    let swarm = &workspace.terminal_agents.swarms[&id];
    assert!(
        swarm
            .queue
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .all(|row| row.status == Status::Succeeded)
    );
    let task = workspace.swarm_action(id, Message::Comparison(Compare::Review));
    drain(&mut workspace, task);
    let comparison = &workspace.terminal_agents.swarms[&id].comparison;
    let reviewed = comparison.cli_review().unwrap().primary().to_owned();
    let (_, epoch, _) = comparison.review().unwrap();
    assert!(reviewed.contains("Target passed."));
    assert!(!reviewed.contains("fixture-secret"));
    assert!(!marker.exists());
    let task = workspace.swarm_action(id, Message::Comparison(Compare::Send(epoch)));
    drain(&mut workspace, task);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), reviewed);
    std::fs::remove_file(&marker).unwrap();
    assert!(
        workspace.terminal_agents.swarms[&id]
            .notice
            .contains("local comparison is available")
    );

    let task = workspace.swarm_action(id, Message::Comparison(Compare::Review));
    drain(&mut workspace, task);
    let (_, epoch, _) = workspace.terminal_agents.swarms[&id]
        .comparison
        .review()
        .unwrap();
    Configuration::default()
        .save_fixture_at(&root, &config)
        .unwrap();
    let task = workspace.swarm_action(id, Message::Comparison(Compare::Send(epoch)));
    drain(&mut workspace, task);
    assert!(!marker.exists());
    assert!(
        workspace.terminal_agents.swarms[&id]
            .notice
            .contains("changed")
    );
}

#[test]
fn live_swarm_reviews_exact_masked_requests_and_scopes_pins_to_each_member() {
    let _exclusive = test_support::exclusive_run();
    let (model, _) = test_support::listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles":[{"id":1,"name":"Swarm fixture","endpoint":model.local_addr().unwrap().to_string(),
            "model":"test","max_output_tokens":8192,"usage_budget":{"token_limit":7000}}],"default":1,"last_id":1},
        "skills": {"last_id":55,"skills":{
            "51":{"name":"Disk","tags":["disk"],"target":"AGENT","enabled":true,"content":LOCAL},
            "52":{"name":"Recovery","target":"BOTH","enabled":true,"content":CLASSIFIED},
            "53":{"name":"Chat only","target":"CHAT","enabled":true,"content":"Excluded chat guidance"},
            "54":{"name":"First connection","target":"CONNECTION","enabled":true,"content":FIRST},
            "55":{"name":"Second connection","target":"CONNECTION","enabled":true,"content":SECOND}
        }}
    })).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let mut store = Store::open(crate::profile::connections_path_at(&root)).unwrap();
    let sessions = [54, 55].map(|skill| {
        let session = local(&mut workspace, owner, false);
        let id = store
            .save_local(LocalProfile {
                id: 0,
                name: format!("Swarm connection {skill}"),
                shell: Default::default(),
                command: None,
                working_directory: None,
                shell_integration: false,
                emulation: "xterm-256color".into(),
                terminal: TerminalOverrides {
                    ai_skills: vec![skill],
                    ..Default::default()
                },
            })
            .unwrap();
        workspace.sessions.get_mut(&session).unwrap().saved_local = store.local(id).cloned();
        session
    });
    workspace.refresh_saved_terminal_preferences(&store);

    let _ = workspace.open_agent_swarm(owner);
    let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let _ = workspace.swarm_action(group, Message::Mode(SwarmMode::Advisory));
    let _ = workspace.swarm_action(
        group,
        Message::Goal("Inspect disk usage. API_KEY=private-swarm-goal".into()),
    );
    for key in review_source(&mut workspace, group, &root) {
        let _ = workspace.swarm_action(group, Message::Toggle(key, true));
    }
    let start = workspace.swarm_action(group, Message::Start);
    drain(&mut workspace, start);
    let swarm = &workspace.terminal_agents.swarms[&group];
    assert!(
        swarm
            .queue
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .all(|row| row.status == Status::Review)
    );
    let [first, second] = sessions.map(|session| {
        swarm
            .members
            .iter()
            .find_map(|(&id, ticket)| (ticket.key().pane().unwrap().0 == session).then_some(id))
            .unwrap()
    });
    for (id, pin, other) in [(first, FIRST, SECOND), (second, SECOND, FIRST)] {
        let run = &workspace.terminal_agents.runs[&id];
        assert!(run.phase == Phase::ModelReview);
        let review = run.model_review.as_ref().unwrap();
        assert!(review.system.contains(pin) && review.system.contains(LOCAL));
        assert!(
            !review.system.contains(other) && !review.system.contains("Excluded chat guidance")
        );
        assert!(
            review
                .quota
                .label()
                .unwrap()
                .contains("up to 2048 output tokens")
        );
    }
    idle(&model);

    let action = |workspace: &mut Workspace, id, message| {
        workspace.swarm_action(
            group,
            Message::Member(id, MemberAction::ReviewModel(message)),
        )
    };
    let first_epoch = workspace.terminal_agents.runs[&first].epoch;
    let second_epoch = workspace.terminal_agents.runs[&second].epoch;
    let opened = action(&mut workspace, first, Review::ReviewSkills(first_epoch));
    drain(&mut workspace, opened);
    let classifier_epoch = workspace.terminal_agents.runs[&first].epoch;
    assert_ne!(first_epoch, classifier_epoch);
    assert_eq!(
        action(&mut workspace, first, Review::Send(classifier_epoch)).units(),
        0
    );
    idle(&model);
    let transport = workspace
        .ai_configuration()
        .unwrap()
        .agent_transport(1)
        .unwrap()
        .0;
    let hold_workers = || {
        (0..local_ai::MAX_WORKERS)
            .map(|_| local_ai::Session::begin(transport.clone()).unwrap())
            .collect::<Vec<_>>()
    };
    let run = &workspace.terminal_agents.runs[&first];
    let request_review::Classification::Preview { text, .. } =
        &run.model_review.as_ref().unwrap().classification
    else {
        panic!("the classifier must have a review before dispatch");
    };
    let classifier_text = text.clone();
    let workers = hold_workers();
    let busy = action(&mut workspace, first, Review::Classify(classifier_epoch));
    drain(&mut workspace, busy);
    let run = &workspace.terminal_agents.runs[&first];
    assert_ne!(run.epoch, classifier_epoch);
    assert!(run.phase == Phase::ModelReview && run.notice.contains("workers are busy"));
    let request_review::Classification::Preview { text, quota } =
        &run.model_review.as_ref().unwrap().classification
    else {
        panic!("a busy classifier must retain its review");
    };
    assert_eq!(text, &classifier_text);
    assert!(quota.label().unwrap().contains("up to 128 output tokens"));
    let classifier_epoch = run.epoch;
    idle(&model);
    drop(workers);
    let classified = action(&mut workspace, first, Review::Classify(classifier_epoch));
    let mut socket = test_support::accept(&model);
    let wire = test_support::request(&mut socket);
    assert_eq!(wire["max_tokens"], 128);
    let payload: Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(payload["requestContext"], json!({"question":MASKED_GOAL}));
    let candidates: Vec<_> = payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["id"].as_str().unwrap())
        .collect();
    assert_eq!(candidates, ["51", "52"]);
    let encoded = wire.to_string();
    for private in ["private-swarm-goal", FIRST, SECOND, LOCAL, CLASSIFIED] {
        assert!(!encoded.contains(private));
    }
    response(socket.get_mut(), r#"{"skillIds":["52","54"]}"#);
    drop(socket);
    drain(&mut workspace, classified);
    let first_run = &workspace.terminal_agents.runs[&first];
    assert_ne!(first_run.epoch, classifier_epoch);
    let review = first_run.model_review.as_ref().unwrap();
    assert!(review.system.contains(FIRST) && review.system.contains(CLASSIFIED));
    assert!(!review.system.contains(SECOND) && !review.system.contains(LOCAL));
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains("13 of 7000 tokens reported")
    );
    let first_approval = first_run.epoch;
    let (system, input) = (review.system.clone(), review.input.clone());
    let workers = hold_workers();
    let busy = action(&mut workspace, first, Review::Send(first_approval));
    drain(&mut workspace, busy);
    let run = &workspace.terminal_agents.runs[&first];
    assert_ne!(run.epoch, first_approval);
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    assert!(run.notice.contains("workers are busy"));
    let review = run.model_review.as_ref().unwrap();
    assert_eq!((&review.system, &review.input), (&system, &input));
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains("up to 2048 output tokens")
    );
    idle(&model);
    drop(workers);
    assert_eq!(workspace.terminal_agents.runs[&second].epoch, second_epoch);
    assert_eq!(
        action(&mut workspace, first, Review::Send(first_approval)).units(),
        0
    );
    idle(&model);

    for id in [first, second] {
        let run = &workspace.terminal_agents.runs[&id];
        assert!(run.phase == Phase::ModelReview);
        let epoch = run.epoch;
        let review = run.model_review.as_ref().unwrap();
        let system = review.system.clone();
        let input = review.input.clone();
        let pending = action(&mut workspace, id, Review::Send(epoch));
        let mut socket = test_support::accept(&model);
        let wire = test_support::request(&mut socket);
        assert_eq!(wire["max_tokens"], 2048);
        assert_eq!(wire["messages"][0]["content"], system);
        assert_eq!(wire["messages"][1]["content"], input);
        assert_eq!(
            serde_json::from_str::<Value>(&input).unwrap()["goal"],
            MASKED_GOAL
        );
        response(
            socket.get_mut(),
            r#"{"status":"done","summary":"Done","userMessage":"This target finished."}"#,
        );
        drop(socket);
        let mut events = iced_runtime::task::into_stream(pending).unwrap();
        while let Some(event) = block_on(events.next()) {
            if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = event {
                assert_eq!(actual, id);
                let follow = workspace.update_terminal_agent(actual, message);
                drain(&mut workspace, follow);
            }
        }
        assert!(!workspace.terminal_agents.runs.contains_key(&id));
        idle(&model);
    }
    let swarm = &workspace.terminal_agents.swarms[&group];
    assert!(
        swarm
            .queue
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .all(|row| row.status == Status::Succeeded)
    );
    let usage = local_ai::UsageAccount::new(
        root,
        1,
        local_ai::UsageBudget {
            token_limit: Some(7000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(usage.snapshot().unwrap().counts.requests, 3);
}
