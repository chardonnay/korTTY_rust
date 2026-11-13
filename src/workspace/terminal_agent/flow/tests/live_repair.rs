use std::io::{ErrorKind, Read as _, Write as _};
use std::sync::mpsc;
use std::thread;

use iced::futures::{Stream, StreamExt as _, executor::block_on};
use serde_json::Value;

use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::terminal_agent::design::{History, inline};
use engine::usage::{Attempt, AttemptKind, AttemptOutcome, Progress};

const WAIT: Duration = Duration::from_secs(5);
const GOAL: &str = "Inspect the worker password=fixture-private-goal";
const INVALID: &str = "This is not JSON. fixture-invalid-provider-text";
const ANSWER: &str = "The fixture recovered.";

enum Settlement {
    Respond(mpsc::Receiver<()>),
    ExpectCancellation,
}

fn http(content: &str, usage: Value) -> String {
    fixture_http_response(
        "200 OK",
        "",
        &json!({"choices": [{"message": {"content": content}}], "usage": usage}).to_string(),
    )
}

fn start_ask(fixture: &mut Fixture) -> (window::Id, Task<Desktop>) {
    let id = fixture.open(Mode::Ask);
    fixture.run_mut(id).design.history = History::InlineBlocks;
    no_task(fixture.send(id, Message::Goal(GOAL.into())));
    no_task(fixture.send(id, Message::Start));
    assert!(fixture.run(id).phase == Phase::ModelReview);
    assert!(
        fixture
            .run(id)
            .model_review
            .as_ref()
            .unwrap()
            .quota
            .label()
            .is_none()
    );
    let epoch = fixture.run(id).epoch;
    let task = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    assert!(fixture.run(id).phase == Phase::Thinking);
    (id, task)
}

fn start(
    settlement: Settlement,
) -> (
    Fixture,
    window::Id,
    Task<Desktop>,
    mpsc::Receiver<()>,
    thread::JoinHandle<()>,
) {
    let (listener, _) = listener();
    let (seen, repair_seen) = mpsc::sync_channel(1);
    let mut fixture =
        Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
    let server = thread::spawn(move || {
        let reviewed = {
            let mut initial = accept(&listener);
            let body = request(&mut initial);
            let reviewed = body["messages"][1]["content"].as_str().unwrap().to_owned();
            assert!(reviewed.contains("Inspect the worker"));
            assert!(!reviewed.contains("fixture-private-goal"));
            initial
                .get_mut()
                .write_all(
                    http(INVALID, json!({"prompt_tokens": 0, "total_tokens": 12})).as_bytes(),
                )
                .unwrap();
            reviewed
        };
        let mut repair = accept(&listener);
        let body = request(&mut repair);
        let input = body["messages"][1]["content"].as_str().unwrap();
        let repair_input: Value = serde_json::from_str(input).unwrap();
        assert_eq!(repair_input["reviewedContext"], reviewed);
        assert_eq!(
            repair_input["instruction"],
            "Return a corrected, complete decision object."
        );
        assert!(!input.contains("fixture-invalid-provider-text"));
        seen.send(()).unwrap();
        match settlement {
            Settlement::Respond(release) => {
                release
                    .recv_timeout(WAIT)
                    .expect("the test released the repair response");
                let answer = json!({"status":"done", "summary":"Repaired", "userMessage":ANSWER})
                    .to_string();
                repair
                    .get_mut()
                    .write_all(
                        http(&answer, json!({"prompt_tokens":17, "completion_tokens":0}))
                            .as_bytes(),
                    )
                    .unwrap();
            }
            Settlement::ExpectCancellation => match repair.read(&mut [0_u8; 1]) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                    ) => {}
                result => panic!("the cancelled repair should close its socket: {result:?}"),
            },
        }
    });
    let (id, task) = start_ask(&mut fixture);
    (fixture, id, task, repair_seen, server)
}

fn next_update(
    stream: &mut (impl Stream<Item = iced_runtime::Action<Desktop>> + Unpin),
    id: window::Id,
) -> Message {
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = action {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("the Agent worker did not return the expected update");
}

fn held_repair(fixture: &Fixture, id: window::Id) -> [u64; 2] {
    let run = fixture.run(id);
    assert!(run.phase == Phase::Thinking);
    assert!(run.notice.contains("Repairing the AI response format"));
    let summary = run.presentation.model_usage.summary().unwrap();
    assert_eq!(summary.attempts, 1);
    assert_eq!((summary.input.sum, summary.input.known), (0, 1));
    assert_eq!((summary.output.sum, summary.output.known), (0, 0));
    assert_eq!((summary.total.sum, summary.total.known), (12, 1));
    assert!(run.activity.is_empty());
    let update = run.inline_update(false).unwrap();
    let [initial, repair] = update.transcript.steps.as_slice() else {
        panic!("the held repair should present exactly two model steps");
    };
    assert_eq!(
        (initial.kind, initial.state, initial.tokens),
        (
            inline::Kind::Thinking,
            inline::StepState::Completed,
            Some(12)
        )
    );
    assert_eq!(
        (repair.kind, repair.state, repair.tokens),
        (inline::Kind::Thinking, inline::StepState::Running, None)
    );
    assert!(
        inline::render(update.style, &update.transcript, 100, "…")
            .iter()
            .any(|line| line.plain().contains("AI response repair"))
    );
    [initial.id, repair.id]
}

fn final_steps(fixture: &Fixture, id: window::Id, expected: [u64; 2], state: inline::StepState) {
    let update = fixture.run(id).inline_update(false).unwrap();
    let steps: Vec<_> = update
        .transcript
        .steps
        .iter()
        .filter(|step| step.kind == inline::Kind::Thinking)
        .collect();
    let [initial, repair] = steps.as_slice() else {
        panic!("settlement should replace the repair row instead of adding another");
    };
    assert_eq!([initial.id, repair.id], expected);
    assert_eq!(initial.state, inline::StepState::Completed);
    assert_eq!(repair.state, state);
}

fn assert_progress_ignored(fixture: &mut Fixture, id: window::Id, epoch: u64, initial: &Attempt) {
    let (permit, received) = async_channel::bounded(1);
    let _ = fixture.send(
        id,
        Message::Progress(epoch, Progress::Repairing(initial.clone(), permit)),
    );
    assert_eq!(
        received.try_recv(),
        Err(async_channel::TryRecvError::Closed)
    );
    let run = fixture.run(id);
    assert!(run.phase == Phase::Thinking);
    assert!(run.presentation.model_usage.summary().is_none());
    assert!(!run.notice.contains("Repairing the AI response format"));
}

#[test]
fn terminal_agent_live_repair_projects_pending_and_accounts_each_attempt_once() {
    let _exclusive = exclusive_run();
    let (release, held) = mpsc::sync_channel(1);
    let (mut fixture, id, task, repair_seen, server) = start(Settlement::Respond(held));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let started = next_update(&mut stream, id);
    let Message::Progress(epoch, Progress::Repairing(initial, _)) = &started else {
        panic!("repair start must reach the workspace before the final decision");
    };
    assert_eq!(initial.kind, AttemptKind::Initial);
    assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
    assert!(matches!(
        repair_seen.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_progress_ignored(&mut fixture, id, epoch - 1, initial);

    let generation = fixture.workspace.sessions[&fixture.session].stream_generation;
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation = generation + 1;
    assert_progress_ignored(&mut fixture, id, *epoch, initial);
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation = generation;

    let ai_allowed = fixture.workspace.clipboard.policy.ai_allowed;
    fixture.workspace.clipboard.policy.ai_allowed = false;
    assert_progress_ignored(&mut fixture, id, *epoch, initial);
    fixture.workspace.clipboard.policy.ai_allowed = ai_allowed;

    let configuration = fixture.workspace.ai_configuration().unwrap();
    let mut changed = serde_json::to_value(configuration.as_ref()).unwrap();
    changed["profiles"]["profiles"][0]["model"] = json!("changed-fixture-model");
    let changed: Configuration = serde_json::from_value(changed).unwrap();
    assert!(changed.agent_transport(1).is_ok());
    fixture.workspace.set_test_ai_configuration(changed);
    assert_progress_ignored(&mut fixture, id, *epoch, initial);
    fixture
        .workspace
        .set_test_ai_configuration(configuration.as_ref().clone());

    let _ = fixture.send(id, started.clone());
    repair_seen
        .recv_timeout(WAIT)
        .expect("authorized repair reached the held loopback response");
    let rows = held_repair(&fixture, id);
    let _ = fixture.send(id, started);
    assert_eq!(held_repair(&fixture, id), rows);

    release.send(()).unwrap();
    let final_update = next_update(&mut stream, id);
    assert!(matches!(final_update, Message::Decision(..)));
    let _ = fixture.send(id, final_update.clone());
    let _ = fixture.send(id, final_update);
    server.join().unwrap();
    let run = fixture.run(id);
    assert!(run.phase == Phase::Finished);
    let summary = run.presentation.model_usage.summary().unwrap();
    assert_eq!(summary.attempts, 2);
    assert_eq!((summary.input.sum, summary.input.known), (17, 2));
    assert_eq!((summary.output.sum, summary.output.known), (0, 1));
    assert_eq!((summary.total.sum, summary.total.known), (29, 2));
    assert_eq!(run.presentation.model_usage.total(), Some(29));
    assert!(run.activity.iter().any(|row| row.contains(ANSWER)));
    assert!(
        run.activity
            .iter()
            .all(|row| !row.contains("fixture-invalid-provider-text"))
    );
    final_steps(&fixture, id, rows, inline::StepState::Completed);
}

#[test]
fn terminal_agent_live_repair_cancel_closes_owned_socket_and_settles_the_same_step() {
    let _exclusive = exclusive_run();
    let (mut fixture, id, task, repair_seen, server) = start(Settlement::ExpectCancellation);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let started = next_update(&mut stream, id);
    assert!(matches!(
        started,
        Message::Progress(_, Progress::Repairing(..))
    ));
    let _ = fixture.send(id, started.clone());
    repair_seen
        .recv_timeout(WAIT)
        .expect("repair request reached the loopback server");
    let rows = held_repair(&fixture, id);
    let _ = fixture.send(id, Message::Cancel);
    let _ = fixture.send(id, started);
    let final_update = next_update(&mut stream, id);
    assert!(matches!(final_update, Message::Decision(..)));
    let _ = fixture.send(id, final_update);
    server.join().unwrap();
    let run = fixture.run(id);
    assert!(run.phase == Phase::Cancelled);
    let summary = run.presentation.model_usage.summary().unwrap();
    assert_eq!(summary.attempts, 2);
    assert_eq!((summary.total.sum, summary.total.known), (12, 1));
    assert!(run.activity.is_empty());
    final_steps(&fixture, id, rows, inline::StepState::Cancelled);
}

#[test]
fn terminal_agent_format_fallback_reuses_reviewed_input_only_with_current_redaction_rules() {
    let _exclusive = exclusive_run();
    for revoke in [false, true] {
        let (listener, _) = listener();
        let mut fixture =
            Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            let mut structured = request(&mut first);
            assert_eq!(
                structured
                    .as_object_mut()
                    .unwrap()
                    .remove("response_format")
                    .unwrap()["type"],
                "json_object"
            );
            assert!(!structured.to_string().contains("fixture-private-goal"));
            first.get_mut().write_all(fixture_http_response(
                "400 Bad Request", "", r#"{"error":{"code":"unsupported_response_format","message":"server-private-error"}}"#,
            ).as_bytes()).unwrap();
            (listener, structured)
        });
        let (id, task) = start_ask(&mut fixture);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let progress = next_update(&mut stream, id);
        assert!(matches!(
            progress,
            Message::Progress(_, Progress::FormatFallback(AttemptKind::Initial, _))
        ));
        let (listener, expected) = server.join().unwrap();
        assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
        if revoke {
            let rule = "[[session-journal.replace]]\npattern = 'worker'\n"
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            fixture
                .workspace
                .clipboard
                .policy
                .terminal_logging
                .read_rule(rule.as_table())
                .unwrap();
            no_task(fixture.send(id, progress));
            let settled = next_update(&mut stream, id);
            let Message::Decision(_, report) = &settled else {
                panic!("expected denied decision");
            };
            assert_eq!(report.attempts.len(), 1);
            assert!(report.result.is_err());
            no_task(fixture.send(id, settled));
            assert!(fixture.run(id).phase == Phase::Blocked);
            assert!(fixture.run(id).notice.contains("redaction rules changed"));
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock)
            );
        } else {
            let server = thread::spawn(move || {
                let mut fallback = accept(&listener);
                assert_eq!(request(&mut fallback), expected);
                let answer = json!({"status":"done", "summary":"Compatible", "userMessage":ANSWER});
                fallback
                    .get_mut()
                    .write_all(
                        http(
                            &answer.to_string(),
                            json!({"prompt_tokens":7,"completion_tokens":5}),
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            });
            no_task(fixture.send(id, progress));
            assert!(fixture.run(id).notice.contains("Retrying without"));
            assert!(fixture.run(id).presentation.model_usage.summary().is_none());
            let settled = next_update(&mut stream, id);
            no_task(fixture.send(id, settled));
            let run = fixture.run(id);
            assert!(run.phase == Phase::Finished);
            assert_eq!(run.presentation.model_usage.summary().unwrap().attempts, 1);
            assert_eq!(run.presentation.model_usage.total(), Some(12));
            server.join().unwrap();
        }
    }
}

fn limited_initial_response(content: &'static str) -> (Fixture, window::Id) {
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, true, &listener.local_addr().unwrap().to_string());
    let (request_seen, seen) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        let body = request(&mut socket);
        let input = body["messages"][1]["content"].as_str().unwrap();
        assert!(input.contains("Inspect the worker"));
        assert!(!input.contains("fixture-private-goal"));
        request_seen.send(()).unwrap();
        held.recv_timeout(WAIT)
            .expect("the test released the length-limited initial reply");
        let body = json!({
            "choices": [{"message": {"content": content}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 2048, "total_tokens": 2057}
        });
        socket
            .get_mut()
            .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
            .unwrap();
        listener
    });

    let (id, task) = start_ask(&mut fixture);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    seen.recv_timeout(WAIT)
        .expect("the initial request reached the loopback fixture");
    assert!(fixture.run(id).presentation.model_usage.summary().is_none());
    release.send(()).unwrap();
    let listener = server.join().unwrap();
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    // Close the listener so an unexpected retry cannot wait on an unserved socket.
    drop(listener);
    let update = next_update(&mut stream, id);
    let Message::Decision(_, report) = &update else {
        panic!("a length-limited reply must settle without starting a repair");
    };
    let [initial] = report.attempts.as_slice() else {
        panic!("the Agent must make exactly one attempt for a length-limited initial reply");
    };
    assert_eq!(initial.kind, AttemptKind::Initial);
    assert_eq!(
        (
            initial.input_tokens,
            initial.output_tokens,
            initial.total_tokens
        ),
        (Some(9), Some(2048), Some(2057))
    );
    let _ = fixture.send(id, update);
    assert!(block_on(stream.next()).is_none());
    let run = fixture.run(id);
    let usage = run.presentation.model_usage.summary().unwrap();
    assert_eq!(usage.attempts, 1);
    assert_eq!(run.presentation.model_usage.total(), Some(2057));
    let transcript = run.inline_update(false).unwrap().transcript;
    assert_eq!(
        transcript
            .steps
            .iter()
            .filter(|step| step.kind == inline::Kind::Thinking)
            .count(),
        1
    );
    assert!(
        !transcript
            .steps
            .iter()
            .any(|step| step.title == "AI response repair")
    );
    (fixture, id)
}

#[test]
fn terminal_agent_live_repair_does_not_retry_invalid_length_limited_output() {
    let _exclusive = exclusive_run();
    let (fixture, id) =
        limited_initial_response(r#"{"status":"done","summary":"fixture-invalid-provider-text"#);
    let run = fixture.run(id);
    assert!(run.phase == Phase::Blocked);
    let attempt = run.presentation.model_usage.attempts().next().unwrap();
    assert_eq!(attempt.outcome, AttemptOutcome::TokenLimit);
    assert!(
        run.notice.to_ascii_lowercase().contains("output limit"),
        "{}",
        run.notice
    );
    assert!(run.notice.contains("2048 tokens"));
    assert!(
        run.notice
            .contains("Shorten the task or reduce the terminal context")
    );
    assert!(!run.notice.contains("fixture-invalid-provider-text"));
    assert!(run.activity.is_empty());
}

#[test]
fn terminal_agent_live_repair_accepts_valid_decision_at_output_limit() {
    let _exclusive = exclusive_run();
    let (fixture, id) = limited_initial_response(
        r#"{"status":"done","summary":"Complete","userMessage":"The fixture recovered."}"#,
    );
    let run = fixture.run(id);
    assert!(run.phase == Phase::Finished);
    let attempt = run.presentation.model_usage.attempts().next().unwrap();
    assert_eq!(attempt.outcome, AttemptOutcome::Completed);
    assert!(run.activity.iter().any(|row| row.contains(ANSWER)));
}
