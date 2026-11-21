use std::io::{ErrorKind, Write as _};
use std::thread;

use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::workspace::terminal_agent::triage::{Message as Triage, Selection};
use engine::planning::{Stage, TriageMode};

#[cfg(unix)]
mod openssh;

fn proposal() -> serde_json::Value {
    json!({
        "status":"triage", "summary":"Two approaches", "userMessage":"Choose one",
        "complex":true, "reasons":["The directory may be shared"],
        "recommended":0, "options":[{
            "title":"Inspect metadata", "summary":"Review file counts",
            "feasibility":"Works in the reviewed directory", "risks":[], "prerequisites":[],
            "steps":["Review directory metadata"], "alternatives":[]
        }]
    })
}

fn proposal_response() -> String {
    fixture_http_response(
        "200 OK",
        "",
        &json!({"choices":[{"message":{"content":proposal().to_string()}}]}).to_string(),
    )
}

pub(in crate::workspace::terminal_agent) fn complete_probe(fixture: &mut Fixture) -> window::Id {
    let id = fixture.open(Mode::Run);
    let directory = LocalDirectory::capture(fixture.profile.0.canonicalize().unwrap()).unwrap();
    let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
    let run = fixture.run_mut(id);
    run.goal = "Check the report directory".into();
    run.target = Some(RunTarget::Attached(Target::Local { shell, directory }));
    let epoch = run.next(Phase::Probe, "Checking the directory");
    let task = fixture.send(
        id,
        Message::Command(
            epoch,
            Arc::new(Ok(Outcome {
                status: engine::Status::Exited(0),
                stdout: Default::default(),
                stderr: Default::default(),
            })),
        ),
    );
    finish_agent_task(fixture, task);
    id
}

#[test]
fn triage_review_binds_the_selected_approach_to_a_separate_agent_review() {
    let _serial = exclusive_run();
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &listener.local_addr().unwrap().to_string());
    let id = complete_probe(&mut fixture);
    let run = fixture.run(id);
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.plan_stage(), Some(Stage::Triage));
    assert_eq!(review.triage_mode(), Some(TriageMode::Auto));
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        let body = request(&mut socket);
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("advisory planning")
        );
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("Check the report directory")
        );
        socket
            .get_mut()
            .write_all(proposal_response().as_bytes())
            .unwrap();
        listener
    });
    let epoch = fixture.run(id).epoch;
    let send = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, send);
    let listener = server.join().unwrap();
    let run = fixture.run(id);
    assert!(run.phase == Phase::TriageChoice && run.turns == 0 && run.plan.is_none());
    let epoch = run.epoch;
    no_task(fixture.send(
        id,
        Message::Triage(Triage::Choose(epoch - 1, Selection::AsRequested)),
    ));
    no_task(fixture.send(
        id,
        Message::Triage(Triage::Instructions(
            epoch,
            "Use only the report directory.".into(),
        )),
    ));
    let choice = fixture.send(
        id,
        Message::Triage(Triage::Choose(epoch, Selection::Option(0))),
    );
    finish_agent_task(&mut fixture, choice);
    let run = fixture.run(id);
    let review = run.model_review.as_ref().unwrap();
    let input: serde_json::Value = serde_json::from_str(&review.input).unwrap();
    let plan = input["acceptedPlan"].as_str().unwrap();
    assert!(plan.contains("Review directory metadata"));
    assert!(plan.contains("Use only the report directory."));
    assert!(run.phase == Phase::ModelReview && run.turns == 0 && run.commands.is_empty());
    assert_eq!(review.plan_stage(), None);
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

#[test]
fn triage_can_be_skipped_before_sending_and_is_omitted_when_planning_is_denied() {
    let mut fixture = Fixture::new(false, false);
    let id = complete_probe(&mut fixture);
    let epoch = fixture.run(id).epoch;
    let skip = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::SkipTriage(epoch)),
    );
    finish_agent_task(&mut fixture, skip);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.turns == 0 && run.plan.is_none());
    assert_eq!(run.model_review.as_ref().unwrap().plan_stage(), None);

    let mut fixture = Fixture::new(false, false);
    fixture
        .workspace
        .clipboard
        .policy
        .ai_planning_feature_allowed = false;
    let id = complete_probe(&mut fixture);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    assert_eq!(run.model_review.as_ref().unwrap().plan_stage(), None);
}

#[test]
fn triage_unusable_reply_preserves_the_original_reviewed_request() {
    let mut fixture = Fixture::new(false, false);
    let id = complete_probe(&mut fixture);
    let epoch = fixture
        .run_mut(id)
        .next(Phase::Thinking, "Waiting for the assessment");
    let reply = fixture.send(
        id,
        Message::Triage(Triage::Reply(
            epoch,
            Arc::new(Err("Invalid response".into()).into()),
        )),
    );
    finish_agent_task(&mut fixture, reply);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.plan.is_none() && run.turns == 0);
    assert!(
        run.activity
            .iter()
            .any(|entry| entry.contains("no usable result"))
    );
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.plan_stage(), None);
    assert!(review.input.contains("Check the report directory"));
}
