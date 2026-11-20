use super::*;
use crate::terminal_agent::{Action, Decision};
use crate::workspace::terminal_agent::flow::tests::{
    Fixture, finish_agent_task, triage::complete_probe,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn begin_question(fixture: &mut Fixture) -> window::Id {
    let id = accept_question(fixture, 0);
    assert!(fixture.run_mut(id).phase == Phase::Question);
    id
}

fn accept_question(fixture: &mut Fixture, previous_questions: usize) -> window::Id {
    let id = complete_probe(fixture);
    let epoch = fixture.run_mut(id).epoch;
    let skipped = fixture.send(
        id,
        RunMessage::ReviewModel(request_review::Message::SkipTriage(epoch)),
    );
    finish_agent_task(fixture, skipped);
    let run = fixture.run_mut(id);
    run.turns = 1;
    run.questions = previous_questions;
    let epoch = run.next(Phase::Thinking, "Waiting for the reviewed model");
    let reply = Decision {
        summary: "Choose the first report".into(),
        message: "There are two reports.".into(),
        action: Action::Input {
            question: "Which report should be read first?".into(),
            answers: vec!["Monday".into(), "Tuesday".into()],
        },
    };
    let decision = fixture.send(id, RunMessage::Decision(epoch, Arc::new(Ok(reply).into())));
    finish_agent_task(fixture, decision);
    id
}

#[test]
fn clarification_limit_opens_a_new_review_without_refunding_the_model_turn() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = accept_question(&mut fixture, crate::terminal_agent::MAX_QUESTIONS);
    let run = fixture.run_mut(id);
    assert!(run.phase == Phase::ModelReview && run.question.is_none());
    assert_eq!(run.turns, 1);
    let review = run.model_review.as_ref().unwrap();
    let input: Value = serde_json::from_str(&review.input).unwrap();
    assert_eq!(input["turn"], 2);
    assert_eq!(
        input["clarifications"],
        json!([{
            "question":"Which report should be read first?",
            "answer":null,
            "instructions":null
        }])
    );
}

fn send(fixture: &mut Fixture, id: window::Id, message: Message) {
    let task = fixture.send(id, RunMessage::Clarification(message));
    finish_agent_task(fixture, task);
}

fn assert_review(fixture: &mut Fixture, id: window::Id) -> Value {
    let run = fixture.run_mut(id);
    assert!(run.phase == Phase::ModelReview && run.question.is_none());
    assert_eq!(run.turns, 0);
    let review = run.model_review.as_ref().unwrap();
    serde_json::from_str(&review.input).unwrap()
}

#[test]
fn own_clarification_is_masked_tied_to_its_question_and_retained_in_later_reviews() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = begin_question(&mut fixture);
    let epoch = fixture.run_mut(id).epoch;
    send(
        &mut fixture,
        id,
        Message::Draft(epoch - 1, "Stale draft".into()),
    );
    send(&mut fixture, id, Message::Submit(epoch));
    assert!(fixture.run_mut(id).phase == Phase::Question);
    send(
        &mut fixture,
        id,
        Message::Draft(epoch, "Wednesday API_KEY=private-answer".into()),
    );
    send(
        &mut fixture,
        id,
        Message::Instructions(epoch, "Use the table of contents.".into()),
    );
    send(&mut fixture, id, Message::Submit(epoch));
    let expected = json!([{
        "question":"Which report should be read first?",
        "answer":"Wednesday API_KEY=***",
        "instructions":"Use the table of contents."
    }]);
    assert_eq!(assert_review(&mut fixture, id)["clarifications"], expected);
    let run = fixture.run_mut(id);
    for index in 0..10 {
        run.record(format!("Later probe {index}"));
    }
    let later: Value =
        serde_json::from_str(&run.decision_input(false, 8192, None).unwrap()).unwrap();
    assert_eq!(later["clarifications"], expected);
    assert!(!later.to_string().contains("private-answer"));
}

#[test]
fn deferred_clarification_keeps_approved_instructions_and_expiry_discards_its_draft() {
    for expired in [false, true] {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let id = begin_question(&mut fixture);
        let epoch = fixture.run_mut(id).epoch;
        send(
            &mut fixture,
            id,
            Message::Draft(epoch, "Unsubmitted answer".into()),
        );
        send(
            &mut fixture,
            id,
            Message::Instructions(epoch, "Start with the smaller report.".into()),
        );
        if expired {
            fixture.run_mut(id).question.as_mut().unwrap().deadline = Instant::now();
            let tick = fixture.workspace.update(Desktop::AgentStatusClock);
            finish_agent_task(&mut fixture, tick);
        } else {
            send(&mut fixture, id, Message::Defer(epoch));
        }
        send(&mut fixture, id, Message::Suggested(epoch, 0));
        let next = assert_review(&mut fixture, id);
        assert_eq!(
            next["clarifications"],
            json!([{
                "question":"Which report should be read first?",
                "answer":null,
                "instructions":(!expired).then_some("Start with the smaller report.")
            }])
        );
        assert!(!next.to_string().contains("Unsubmitted answer"));
    }
}
