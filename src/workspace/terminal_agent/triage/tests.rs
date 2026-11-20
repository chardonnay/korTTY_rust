use super::*;
use crate::workspace::terminal_agent::flow::tests::{
    Fixture, finish_agent_task, triage::complete_probe,
};
use crate::workspace::terminal_agent::request_review;

fn begin_choice(fixture: &mut Fixture) -> window::Id {
    let id = complete_probe(fixture);
    let epoch = fixture.run_mut(id).next(Phase::Thinking, "Checking");
    let reply = Reply::Triage {
        complex: true,
        reasons: vec!["The report has more than one source.".into()],
        recommended: 0,
        options: vec![Approach {
            title: "Review counts".into(),
            summary: "Compare the summaries.".into(),
            feasibility: "Uses the reviewed report directory.".into(),
            risks: vec![],
            prerequisites: vec![],
            steps: vec!["Read the report index.".into()],
            alternatives: vec![],
        }],
    };
    let task = fixture.send(
        id,
        RunMessage::Triage(Message::Reply(epoch, Arc::new(Ok(reply).into()))),
    );
    finish_agent_task(fixture, task);
    assert!(fixture.run_mut(id).phase == Phase::TriageChoice);
    id
}

#[test]
fn triage_clock_expires_the_owned_choice_and_preserves_the_original_review() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = begin_choice(&mut fixture);
    let epoch = fixture.run_mut(id).epoch;
    let edit = fixture.send(
        id,
        RunMessage::Triage(Message::Instructions(epoch, "Unaccepted draft".into())),
    );
    finish_agent_task(&mut fixture, edit);
    let early = fixture.workspace.update(Desktop::AgentStatusClock);
    finish_agent_task(&mut fixture, early);
    assert!(fixture.run_mut(id).phase == Phase::TriageChoice);

    fixture.run_mut(id).triage.choice.as_mut().unwrap().deadline = Instant::now();
    let tick = fixture.workspace.update(Desktop::AgentStatusClock);
    finish_agent_task(&mut fixture, tick);
    let late = fixture.send(
        id,
        RunMessage::Triage(Message::Choose(epoch, Selection::Option(0))),
    );
    finish_agent_task(&mut fixture, late);
    let run = fixture.run_mut(id);
    let review = run.model_review.as_ref().unwrap();
    assert!(run.phase == Phase::ModelReview && run.plan.is_none() && run.triage.choice.is_none());
    assert_eq!(review.plan_stage(), None);
    assert!(review.input.contains("Check the report directory"));
    assert!(!review.input.contains("Unaccepted draft"));
    assert_eq!(run.turns, 0);
}

#[test]
fn triage_cancellation_ignores_late_choice_and_send_messages() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = begin_choice(&mut fixture);
    let epoch = fixture.run_mut(id).epoch;
    for message in [
        RunMessage::Cancel,
        RunMessage::Triage(Message::Choose(epoch, Selection::Option(0))),
        RunMessage::ReviewModel(request_review::Message::Send(epoch)),
    ] {
        let task = fixture.send(id, message);
        finish_agent_task(&mut fixture, task);
    }
    let run = fixture.run_mut(id);
    assert!(run.phase == Phase::Cancelled);
    assert!(run.plan.is_none() && run.triage.choice.is_none() && run.model_review.is_none());
    assert_eq!(run.turns, 0);
}
