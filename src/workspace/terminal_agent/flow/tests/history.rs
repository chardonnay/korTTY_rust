use iced::futures::{StreamExt as _, executor::block_on};

use super::*;
use crate::workspace::terminal_agent::history::Message as History;

fn drain(fixture: &mut Fixture, task: Task<Desktop>) {
    let mut tasks = vec![task];
    while let Some(task) = tasks.pop() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
                    assert!(!matches!(
                        message,
                        Message::Decision(_, _) | Message::Command(_, _)
                    ));
                    tasks.push(fixture.send(id, message));
                }
            }
        }
    }
}

#[test]
fn terminal_agent_history_records_explicit_masked_start_and_reuses_prompt_without_running() {
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let first = fixture.open(Mode::Run);
    fixture.run_mut(first).manual_directory = Some(root.to_str().unwrap().into());
    no_task(fixture.send(
        first,
        Message::Goal("Inspect hosts password=fixture-secret".into()),
    ));
    let started = fixture.send(first, Message::Start);
    drain(&mut fixture, started);
    assert!(fixture.run(first).phase == Phase::ProbeApproval);
    assert!(fixture.run(first).activity.is_empty());
    let saved = crate::terminal_agent::history::list(&root, false).unwrap();
    assert_eq!(saved.entries.len(), 1);
    assert_eq!(saved.entries[0].goal, "Inspect hosts password=***");

    let reopened = fixture.open(Mode::Ask);
    let browse = fixture.send(reopened, Message::History(History::Open));
    drain(&mut fixture, browse);
    no_task(fixture.send(reopened, Message::History(History::Use(0, 0))));
    assert!(fixture.run(reopened).goal.is_empty());
    no_task(fixture.send(reopened, Message::History(History::Use(1, 0))));
    assert_eq!(fixture.run(reopened).goal, "Inspect hosts password=***");
    assert!(fixture.run(reopened).phase == Phase::Setup);
    assert!(fixture.run(reopened).activity.is_empty());
}
