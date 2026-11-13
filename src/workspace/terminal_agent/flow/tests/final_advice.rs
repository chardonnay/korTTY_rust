use std::io::{BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

use iced::futures::{StreamExt as _, executor::block_on};
use serde_json::Value;

use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use engine::usage::AttemptOutcome;

fn message(task: Task<Desktop>, id: window::Id) -> Message {
    let mut stream = iced_runtime::task::into_stream(task).expect("agent task");
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = action {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("missing agent update");
}

fn outcome(code: u32) -> Outcome {
    Outcome {
        status: engine::Status::Exited(code),
        stdout: Default::default(),
        stderr: Default::default(),
    }
}

fn complete_review(fixture: &mut Fixture, id: window::Id, probe: bool, code: u32) -> Task<Desktop> {
    let epoch = fixture.run(id).epoch;
    let approved = fixture.send(id, Message::Approve(epoch));
    let directory = message(approved, id);
    assert!(matches!(directory, Message::Directory(..)));
    let command_worker = fixture.send(id, directory);
    assert!(fixture.run(id).phase == if probe { Phase::Probe } else { Phase::Running });
    // Deliver a deterministic transport result to the reviewed, owned command epoch.
    drop(command_worker);
    fixture.send(
        id,
        Message::Command(fixture.run(id).epoch, Arc::new(Ok(outcome(code)))),
    )
}

fn prepare_local_run(fixture: &mut Fixture, id: window::Id, goal: &str) {
    fixture.run_mut(id).manual_directory = Some(
        fixture
            .profile
            .0
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
    );
    no_task(fixture.send(id, Message::Goal(goal.into())));
    let start = fixture.send(id, Message::Start);
    finish_agent_task(fixture, start);
    assert!(fixture.run(id).phase == Phase::ProbeApproval);
}

fn reply(socket: &mut impl Write, content: &str, finish: &str, tokens: u32) {
    let body = json!({
        "choices": [{"message": {"content": content}, "finish_reason": finish}],
        "usage": {"total_tokens": tokens},
    });
    socket
        .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
        .unwrap();
}

fn proposal(turn: usize) -> Value {
    json!({"command": format!("echo reviewed-{turn}"), "purpose":"Gather evidence", "risk":"read_only"})
}

fn user_message(body: &Value) -> Value {
    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap()
}

fn no_model_request(listener: &TcpListener) {
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

fn send_review(
    fixture: &mut Fixture,
    id: window::Id,
    listener: &TcpListener,
    expected: &mpsc::Sender<[String; 2]>,
) -> Task<Desktop> {
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview);
    let review = run.model_review.as_ref().unwrap();
    if run.binding.private() {
        assert!(review.quota.label().is_none());
    }
    let epoch = run.epoch;
    no_task(fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch - 1)),
    ));
    no_model_request(listener);
    let review = fixture.run(id).model_review.as_ref().unwrap();
    expected
        .send([review.system.clone(), review.input.clone()])
        .unwrap();
    fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    )
}

fn accept_reviewed(
    listener: &TcpListener,
    expected: &mpsc::Receiver<[String; 2]>,
) -> (BufReader<TcpStream>, Value) {
    let [system, input] = expected.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut socket = accept(listener);
    let body = request(&mut socket);
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["messages"][0]["content"], system);
    assert_eq!(body["messages"][1]["content"], input);
    (socket, body)
}

#[test]
fn eight_turn_agent_reviews_every_command_and_settles_one_final_advice_with_repair() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
    let (reviews, expected) = mpsc::channel();
    let server_listener = listener.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let listener = server_listener;
        for turn in 1..=engine::MAX_TURNS {
            if turn == engine::MAX_TURNS {
                let (mut socket, body) = accept_reviewed(&listener, &expected);
                assert_eq!(user_message(&body)["turn"], turn);
                reply(
                    socket.get_mut(),
                    r#"{"status":"needs_input","summary":"Clarify","userMessage":"A detail is missing.","question":"Which result?","answers":["First","Second"]}"#,
                    "stop",
                    3,
                );
            }
            let (mut socket, body) = accept_reviewed(&listener, &expected);
            let input = user_message(&body);
            assert_eq!(input["turn"], turn);
            assert_eq!(input["turnLimit"], engine::MAX_TURNS);
            assert!(
                body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("When turn equals turnLimit")
            );
            let mut commands = vec![proposal(turn)];
            if turn == engine::MAX_TURNS {
                assert!(input["steps"].to_string().contains("Your answer"));
                commands.push(proposal(turn + 1));
            }
            let response = json!({"status":"run_commands","summary":"Inspect","userMessage":"Gathering the requested evidence.","commands":commands});
            reply(socket.get_mut(), &response.to_string(), "stop", 2);
        }
        let (mut socket, body) = accept_reviewed(&listener, &expected);
        let input = user_message(&body);
        assert_eq!(input["goal"], "Collect evidence API_KEY=***");
        assert_eq!(input["turn"], engine::MAX_TURNS);
        assert_eq!(input["probe"]["exitStatus"], 0);
        let rows = input["commandResults"].as_array().unwrap();
        assert_eq!(rows.len(), engine::MAX_TURNS + 1);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row["command"], proposal(index + 1)["command"]);
        }
        assert_eq!(rows.last().unwrap()["exitStatus"], 7);
        assert!(input.get("steps").is_none());
        assert!(!body.to_string().contains("fixture-goal-secret"));
        let reviewed = body["messages"][1]["content"].as_str().unwrap();
        reply(
            socket.get_mut(),
            r#"{"status":"done","summary":"partial-fixture"#,
            "length",
            11,
        );

        let mut repair = accept(&listener);
        let repaired = request(&mut repair);
        assert_eq!(
            repaired["messages"][0]["content"],
            body["messages"][0]["content"]
        );
        let repaired = user_message(&repaired);
        assert_eq!(repaired["reviewedContext"], reviewed);
        assert!(!repaired.to_string().contains("partial-fixture"));
        reply(
            repair.get_mut(),
            r#"{"status":"done","summary":"Result","userMessage":"The reviewed evidence is complete.","commands":[],"needsReprobe":false}"#,
            "stop",
            5,
        );
    });

    let id = fixture.open(Mode::Run);
    fixture.run_mut(id).design.history = crate::terminal_agent::design::History::InlineBlocks;
    prepare_local_run(
        &mut fixture,
        id,
        "Collect evidence API_KEY=fixture-goal-secret",
    );
    no_task(complete_review(&mut fixture, id, true, 0));
    assert!(fixture.run(id).final_evidence.is_empty());
    for turn in 1..=engine::MAX_TURNS {
        assert_eq!(fixture.run(id).turns, turn - 1);
        let task = send_review(&mut fixture, id, &listener, &reviews);
        assert_eq!(fixture.run(id).turns, turn);
        no_task(fixture.send(id, message(task, id)));
        if turn == engine::MAX_TURNS {
            let run = fixture.run(id);
            assert!(run.phase == Phase::Question);
            let epoch = run.epoch;
            no_task(fixture.send(id, Message::Answer(epoch, "Second".into())));
            assert_eq!(fixture.run(id).turns, turn - 1);
            let answer = send_review(&mut fixture, id, &listener, &reviews);
            assert_eq!(fixture.run(id).turns, turn);
            no_task(fixture.send(id, message(answer, id)));
        }
        assert!(fixture.run(id).phase == Phase::Approval);
        no_task(complete_review(&mut fixture, id, false, 0));
        if turn == engine::MAX_TURNS {
            assert!(fixture.run(id).phase == Phase::Approval);
            no_task(complete_review(&mut fixture, id, false, 7));
        }
    }
    assert_eq!(fixture.run(id).turns, engine::MAX_TURNS);
    let task = send_review(&mut fixture, id, &listener, &reviews);
    assert!(fixture.run(id).phase == Phase::FinalAdvice);
    assert_eq!(fixture.run(id).turns, engine::MAX_TURNS);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(actual, progress))) =
        block_on(stream.next())
    else {
        panic!("final advice repair must report live progress");
    };
    assert_eq!(actual, id);
    assert!(matches!(
        progress,
        Message::Progress(_, engine::usage::Progress::Repairing(..))
    ));
    no_task(fixture.send(id, progress));
    let run = fixture.run(id);
    assert!(run.presentation.model_usage.repairing(run.epoch));
    assert_eq!(
        run.presentation
            .model_usage
            .attempts()
            .last()
            .unwrap()
            .outcome,
        AttemptOutcome::TokenLimit
    );
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(actual, settled))) =
        block_on(stream.next())
    else {
        panic!("final advice did not settle");
    };
    assert_eq!(actual, id);
    no_task(fixture.send(id, settled.clone()));
    no_task(fixture.send(id, settled));
    assert!(block_on(stream.next()).is_none());
    let run = fixture.run(id);
    assert!(run.phase == Phase::Finished);
    assert_eq!(run.turns, engine::MAX_TURNS);
    assert!(run.reviewed_command().is_none());
    assert!(
        run.activity
            .iter()
            .any(|line| line.contains("The reviewed evidence is complete."))
    );
    assert_eq!(run.presentation.model_usage.total(), Some(35));
    assert_eq!(run.presentation.model_usage.summary().unwrap().attempts, 11);
    server.join().unwrap();
    no_model_request(&listener);
}

#[test]
fn a_probe_alone_cannot_trigger_final_advice_and_a_final_result_cannot_propose_work() {
    let mut fixture = Fixture::new(false, true);
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.turns = engine::MAX_TURNS;
    let epoch = run.next(Phase::Probe, "Review complete");
    no_task(fixture.send(id, Message::Command(epoch, Arc::new(Ok(outcome(0))))));
    let run = fixture.run_mut(id);
    assert!(run.phase == Phase::Blocked);
    assert!(run.final_evidence.is_empty());
    assert!(run.notice.contains("continue manually"));

    let epoch = run.next(Phase::FinalAdvice, "Final advice pending");
    let proposal = Command {
        command: "echo unsafe".into(),
        purpose: "Test guard".into(),
        risk: engine::Risk::ReadOnly,
        elevation: Elevation::None,
    };
    let injected = Arc::new(
        Ok(Decision {
            summary: "Invalid".into(),
            message: "Must not dispatch".into(),
            action: Action::Commands(vec![proposal]),
        })
        .into(),
    );
    no_task(fixture.send(id, Message::Decision(epoch - 1, Arc::clone(&injected))));
    assert!(fixture.run(id).phase == Phase::FinalAdvice);
    no_task(fixture.send(id, Message::Decision(epoch, injected)));
    let run = fixture.run(id);
    assert!(run.phase == Phase::Blocked);
    assert!(run.commands.is_empty());
    assert!(run.notice.contains("continue manually"));
    assert!(
        run.activity
            .iter()
            .any(|line| line.contains("requires done or blocked"))
    );
}

#[test]
fn final_advice_repair_cannot_resend_evidence_after_the_terminal_is_replaced() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
    let (reviews, expected) = mpsc::channel();
    let server_listener = listener.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, body) = accept_reviewed(&server_listener, &expected);
        let body = user_message(&body);
        assert_eq!(body["commandResults"][0]["exitStatus"], 0);
        reply(socket.get_mut(), "invalid final reply", "stop", 5);
    });
    let id = fixture.open(Mode::Run);
    prepare_local_run(&mut fixture, id, "Inspect evidence");
    let run = fixture.run_mut(id);
    run.turns = engine::MAX_TURNS;
    let epoch = run.next(Phase::Running, "Approved command");
    run.final_evidence.start(
        epoch,
        &Command {
            command: "echo reviewed".into(),
            purpose: "Inspect fixture".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
    );
    no_task(fixture.send(id, Message::Command(epoch, Arc::new(Ok(outcome(0))))));
    let task = send_review(&mut fixture, id, &listener, &reviews);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(
        _,
        permit @ Message::Progress(_, engine::usage::Progress::Repairing(..)),
    ))) = block_on(stream.next())
    else {
        panic!("the worker must ask for authorization before sending a repair");
    };
    server.join().unwrap();
    no_model_request(&listener);
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    no_task(fixture.send(id, permit));
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(
        _,
        settled @ Message::Decision(..),
    ))) = block_on(stream.next())
    else {
        panic!("the denied repair must settle");
    };
    if let Message::Decision(_, report) = &settled {
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].total_tokens, Some(5));
        assert!(
            report
                .result
                .as_ref()
                .unwrap_err()
                .contains("was not authorized")
        );
    }
    no_task(fixture.send(id, settled));
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).notice.contains("source terminal changed"));
    assert!(fixture.run(id).presentation.model_usage.summary().is_none());
    no_model_request(&listener);
}

#[test]
fn run_review_keeps_the_learned_sudo_mask_when_sending_earlier_evidence() {
    use crate::terminal_logging::RedactionSecrets;

    const SECRET: &str = r#"fixture-"sudo\value"#;
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
    let id = fixture.open(Mode::Run);
    prepare_local_run(&mut fixture, id, "Inspect evidence");
    let run = fixture.run_mut(id);
    run.turns = 1;
    run.record(format!("Earlier evidence: {SECRET}"));
    let learned = RedactionSecrets::with_secret(None, SECRET).unwrap();
    run.redactor = run.redactor.with_overlay(&learned).unwrap();
    run.learned_sudo = Some(learned);
    let epoch = run.next(Phase::Running, "Waiting for a reviewed command");
    no_task(fixture.send(id, Message::Command(epoch, Arc::new(Ok(outcome(0))))));
    let (reviews, expected) = mpsc::channel();
    let task = send_review(&mut fixture, id, &listener, &reviews);
    let (mut socket, body) = accept_reviewed(&listener, &expected);
    let input = user_message(&body);
    assert_eq!(input["turn"], 2);
    assert_eq!(input["steps"][0], "Earlier evidence: ***");
    assert_eq!(input["target"]["passwordSudo"], false);
    assert_eq!(input["target"]["passwordlessSudo"], false);
    reply(
        socket.get_mut(),
        r#"{"status":"done","summary":"Done","userMessage":"Only masked evidence was used."}"#,
        "stop",
        2,
    );
    drop(socket);
    no_task(fixture.send(id, message(task, id)));
    assert!(fixture.run(id).phase == Phase::Finished);
    no_model_request(&listener);
}

#[test]
fn run_review_classifies_without_probe_output_and_refreshes_the_decision_quota() {
    use request_review::Message as Review;

    const LOCAL_GUIDANCE: &str = "Read the disk inventory before proposing a command.";
    const CLASSIFIED_GUIDANCE: &str = "Use the reviewed recovery checklist.";
    const PROBE_OUTPUT: &str = "probe-fixture-output-only";
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut fixture = Fixture::with_endpoint(false, false, &endpoint);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [{"id": 1, "name": "Fixture", "endpoint": endpoint,
            "model": "fixture", "max_output_tokens": 8192, "usage_budget": {"token_limit": 50000}}],
            "default": 1, "last_id": 1},
        "skills": {"last_id": 52, "skills": {
            "51": {"name": "Disk", "tags": ["disk"], "target": "AGENT", "enabled": true, "content": LOCAL_GUIDANCE},
            "52": {"name": "Recovery", "target": "BOTH", "enabled": true, "content": CLASSIFIED_GUIDANCE}
        }}
    })).unwrap();
    config.save_fixture_at(&root, &baseline).unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let usage = local_ai::UsageAccount::new(
        root,
        1,
        local_ai::UsageBudget {
            token_limit: Some(50000),
            ..Default::default()
        },
    )
    .unwrap();
    let id = fixture.open(Mode::Run);
    prepare_local_run(
        &mut fixture,
        id,
        "Inspect disk usage. API_KEY=private-run-goal",
    );
    let approval = fixture.run(id).epoch;
    let directory = message(fixture.send(id, Message::Approve(approval)), id);
    drop(fixture.send(id, directory));
    let run = fixture.run_mut(id);
    assert!(run.phase == Phase::Probe);
    run.record(PROBE_OUTPUT.into());
    let epoch = run.epoch;
    let prepared = fixture.send(id, Message::Command(epoch, Arc::new(Ok(outcome(0)))));
    finish_agent_task(&mut fixture, prepared);

    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    let initial_epoch = run.epoch;
    let review = run.model_review.as_ref().unwrap();
    assert!(review.system.contains(LOCAL_GUIDANCE));
    assert!(review.input.contains(PROBE_OUTPUT));
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains("0 of 50000 tokens reported")
    );
    no_model_request(&listener);
    let open = fixture.send(
        id,
        Message::ReviewModel(Review::ReviewSkills(initial_epoch)),
    );
    finish_agent_task(&mut fixture, open);
    let classifier_epoch = fixture.run(id).epoch;
    no_task(fixture.send(id, Message::ReviewModel(Review::Send(classifier_epoch))));
    no_model_request(&listener);

    let classified = fixture.send(id, Message::ReviewModel(Review::Classify(classifier_epoch)));
    let mut socket = accept(&listener);
    let body = request(&mut socket);
    assert_eq!(body["max_tokens"], 128);
    let payload = user_message(&body);
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(
        payload["requestContext"],
        json!({"question":"Inspect disk usage. API_KEY=***"})
    );
    let candidate_ids: Vec<_> = payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["id"].as_str().unwrap())
        .collect();
    assert_eq!(candidate_ids, ["51", "52"]);
    let encoded = body.to_string();
    for private in [
        PROBE_OUTPUT,
        "private-run-goal",
        LOCAL_GUIDANCE,
        CLASSIFIED_GUIDANCE,
    ] {
        assert!(!encoded.contains(private));
    }
    let response = json!({"choices":[{"message":{"content":"{\"skillIds\":[\"52\"]}"}}],
        "usage":{"prompt_tokens":9,"completion_tokens":4}})
    .to_string();
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &response).as_bytes())
        .unwrap();
    drop(socket);
    finish_agent_task(&mut fixture, classified);

    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    assert_ne!(run.epoch, classifier_epoch);
    let review = run.model_review.as_ref().unwrap();
    assert!(review.system.contains(CLASSIFIED_GUIDANCE));
    assert!(!review.system.contains(LOCAL_GUIDANCE));
    let estimate =
        crate::workspace::ai_quota::estimate_input([review.system.as_str(), review.input.as_str()]);
    let quota = review.quota.label().unwrap();
    assert!(quota.contains("13 of 50000 tokens reported"));
    assert!(quota.contains(&format!(
        "about {estimate} input tokens and up to 2048 output tokens"
    )));
    assert_eq!(usage.snapshot().unwrap().counts.requests, 1);
    no_task(fixture.send(id, Message::ReviewModel(Review::Send(initial_epoch))));
    let (reviews, expected) = mpsc::channel();
    let task = send_review(&mut fixture, id, &listener, &reviews);
    let (mut socket, body) = accept_reviewed(&listener, &expected);
    assert!(
        user_message(&body)["steps"]
            .to_string()
            .contains(PROBE_OUTPUT)
    );
    reply(
        socket.get_mut(),
        r#"{"status":"done","summary":"Done","userMessage":"The fixture answered."}"#,
        "stop",
        2,
    );
    drop(socket);
    finish_agent_task(&mut fixture, task);
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
    no_model_request(&listener);
}
