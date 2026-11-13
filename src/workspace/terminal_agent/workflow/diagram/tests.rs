use std::io::Write as _;

use iced::futures::{StreamExt as _, executor::block_on};
use iced::widget::text_editor;
use iced::window::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use iced_runtime::{Action, window::Action as WindowAction};
use serde_json::{Value, json};

use super::super::super::{
    Command, Elevation, Mode, Outcome, Phase as RunPhase, engine, flow::tests::Fixture,
};
use super::super::{Message as WorkflowMessage, ResultItem, Results};
use super::*;
use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};

const INITIAL: &str = "#!/usr/bin/env bash\necho original draft";
const EDITED: &str = "#!/usr/bin/env bash\necho exact edited draft";
const REVISED: &str = "#!/usr/bin/env bash\necho replacement source";
const FLOW: &str =
    "flowchart TD\nstart([\"Start\"]) --> work[\"Model work\"]\nwork --> endnode([\"Finish\"])";

struct HeadlessWindow;

impl HasWindowHandle for HeadlessWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl HasDisplayHandle for HeadlessWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

fn finished_workflow(endpoint: &str) -> (Fixture, window::Id) {
    let mut fixture = Fixture::with_endpoint(false, false, endpoint);
    let run_id = fixture.open(Mode::Run);
    let run = fixture.run_mut(run_id);
    run.goal = "Check a worker".into();
    run.epoch = 1;
    run.final_evidence.start(
        run.epoch,
        &Command {
            command: "printf observation".into(),
            purpose: "Read worker status".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
    );
    run.final_evidence.record_output(
        run.epoch,
        RunPhase::Running,
        &Outcome {
            status: engine::Status::Exited(0),
            stdout: Default::default(),
            stderr: Default::default(),
        },
        "Worker is ready".into(),
        String::new(),
    );
    run.phase = RunPhase::Finished;
    let epoch = run.epoch;
    drop(fixture.send(run_id, RunMessage::Workflow(WorkflowMessage::Open(epoch))));
    let (&id, dialog) = fixture
        .workspace
        .terminal_agents
        .workflows
        .iter_mut()
        .next()
        .unwrap();
    dialog.generation = 1;
    dialog.stage = Stage::Results(Results {
        items: vec![ResultItem {
            label: "Bash suggestion".into(),
            name: "worker.sh".into(),
            goal: "Check a worker".into(),
            language: Language::Bash,
            content: Some(text_editor::Content::with_text(INITIAL)),
            error: None,
            diagram: None,
        }],
        selected: 0,
    });
    (fixture, id)
}

fn send(fixture: &mut Fixture, id: window::Id, message: Message) -> Task<Desktop> {
    fixture.send(
        id,
        RunMessage::Workflow(WorkflowMessage::Flowchart(message)),
    )
}

fn edit(fixture: &mut Fixture, id: window::Id, code: &str) {
    for action in [
        text_editor::Action::SelectAll,
        text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(code.into()))),
    ] {
        drop(fixture.send(id, RunMessage::Workflow(WorkflowMessage::Edit(action))));
    }
}

fn review(fixture: &Fixture, id: window::Id) -> &Review {
    let dialog = &fixture.workspace.terminal_agents.workflows[&id];
    let Some(Phase::Review(review)) = &dialog.diagram.phase else {
        panic!("missing flowchart review: {}", dialog.notice);
    };
    review
}

fn reviewed_messages(fixture: &Fixture, id: window::Id) -> Value {
    let request = &review(fixture, id).prepared.as_ref().unwrap().request;
    json!([
        {"role": "system", "content": request.system()},
        {"role": "user", "content": request.input()},
    ])
}

fn saved(fixture: &Fixture, id: window::Id) -> &Saved {
    let dialog = &fixture.workspace.terminal_agents.workflows[&id];
    let Stage::Results(results) = &dialog.stage else {
        panic!("missing flowchart results: {}", dialog.notice);
    };
    results.items[0].diagram.as_ref().unwrap()
}

fn reply(task: Task<Desktop>, id: window::Id) -> Message {
    let mut stream = iced_runtime::task::into_stream(task).expect("flowchart reply task");
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(
            actual,
            RunMessage::Workflow(WorkflowMessage::Flowchart(message @ Message::Reply(..))),
        )) = action
        {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("missing flowchart reply");
}

fn assert_owned_preview(opened: Task<Desktop>, preview: window::Id, owner: window::Id) {
    let mut actions = iced_runtime::task::into_stream(opened).unwrap();
    while let Some(action) = block_on(actions.next()) {
        match action {
            Action::Window(WindowAction::GetScaleFactor(id, response)) => {
                assert_eq!(id, owner);
                response.send(1.0).unwrap();
            }
            Action::Window(WindowAction::GetMonitorSize(id, response)) => {
                assert_eq!(id, owner);
                response.send(None).unwrap();
            }
            Action::Window(WindowAction::Run(id, run)) => {
                assert_eq!(id, owner);
                run(&HeadlessWindow);
            }
            Action::Window(WindowAction::Open { id, on_open, .. }) => {
                assert_eq!(id, preview);
                on_open.send(preview).unwrap();
                assert!(matches!(
                    block_on(actions.next()),
                    Some(Action::Output(Desktop::OwnedDialogOpened(child, parent, None)))
                        if child == preview && parent == owner
                ));
                return;
            }
            _ => panic!("unexpected action when opening the workflow diagram"),
        }
    }
    panic!("the diagram did not request a native window");
}

#[test]
fn workflow_flowchart_reviews_the_edited_source_and_disregards_stale_events() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let (mut fixture, id) = finished_workflow(&listener.local_addr().unwrap().to_string());
    edit(&mut fixture, id, EDITED);
    drop(send(&mut fixture, id, Message::Open));
    let old_ticket = review(&fixture, id).scope.ticket;
    let edit_ticket = review(&fixture, id).edit_ticket;
    drop(send(
        &mut fixture,
        id,
        Message::Instructions(edit_ticket, "Show phases".into()),
    ));
    drop(send(
        &mut fixture,
        id,
        Message::Instructions(
            edit_ticket,
            "Show transitions API_KEY=fixture-instructions-secret".into(),
        ),
    ));
    let first_review = review(&fixture, id);
    let first_ticket = first_review.scope.ticket;
    assert_ne!(first_ticket, old_ticket);
    assert_eq!(first_review.scope.source.text.as_ref(), EDITED);
    let input: Value =
        serde_json::from_str(first_review.prepared.as_ref().unwrap().request.input()).unwrap();
    assert_eq!(input["instructions"], "Show transitions API_KEY=***");
    assert!(
        input["source"]
            .as_str()
            .unwrap()
            .contains("echo exact edited draft")
    );
    assert!(!input["source"].as_str().unwrap().contains("original draft"));
    assert!(
        iced_runtime::task::into_stream(send(&mut fixture, id, Message::Generate(old_ticket)))
            .is_none()
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let first_messages = reviewed_messages(&fixture, id);

    let server = std::thread::spawn(move || {
        let mut messages = Vec::new();
        let content = json!({"mermaid": FLOW, "codeReferences": []}).to_string();
        let http = fixture_response("200 OK", "", &content);
        for _ in 0..2 {
            let mut socket = accept(&listener);
            messages.push(request(&mut socket)["messages"].clone());
            socket.get_mut().write_all(http.as_bytes()).unwrap();
        }
        (listener, messages)
    });
    let first = reply(send(&mut fixture, id, Message::Generate(first_ticket)), id);
    assert!(matches!(&first, Message::Reply(_, Ok(chart)) if !chart.is_fallback()));
    let opened = send(&mut fixture, id, first);
    let first_saved = saved(&fixture, id).clone();
    assert!(first_saved.is_current(EDITED));
    assert!(first_saved.mermaid.contains("Model work"));
    assert_eq!(first_saved.instructions, "Show transitions API_KEY=***");
    let preview = fixture.workspace.terminal_agents.workflows[&id]
        .diagram
        .preview
        .unwrap();
    assert!(fixture.workspace.diagrams.contains_key(&preview));
    assert_owned_preview(opened, preview, id);
    drop(send(&mut fixture, id, Message::Open));
    assert_eq!(
        fixture.workspace.terminal_agents.workflows[&id]
            .diagram
            .preview,
        Some(preview)
    );

    drop(send(&mut fixture, id, Message::Regenerate));
    let second_ticket = review(&fixture, id).scope.ticket;
    assert_eq!(review(&fixture, id).instructions, first_saved.instructions);
    let second_messages = reviewed_messages(&fixture, id);
    let late = reply(send(&mut fixture, id, Message::Generate(second_ticket)), id);
    edit(&mut fixture, id, REVISED);
    drop(send(&mut fixture, id, late));
    assert_eq!(saved(&fixture, id), &first_saved);
    assert!(!fixture.workspace.diagrams.contains_key(&preview));

    drop(send(&mut fixture, id, Message::Open));
    let local_ticket = review(&fixture, id).scope.ticket;
    let local = reply(send(&mut fixture, id, Message::Local(local_ticket)), id);
    drop(send(&mut fixture, id, local));
    let local = saved(&fixture, id);
    assert_eq!(local.id, first_saved.id);
    assert!(local.is_current(REVISED));
    assert!(!local.mermaid.contains("Model work"));
    let (listener, sent) = server.join().unwrap();
    assert_eq!(sent, [first_messages, second_messages]);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
