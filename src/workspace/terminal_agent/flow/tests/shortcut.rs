use std::io::Write as _;

use super::*;
use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};
use crate::terminal_agent::shortcut::Aliases;
use crate::terminal_view::Selection;

fn configure(fixture: &mut Fixture, endpoint: &str, show_setup: bool) {
    fixture.workspace.set_test_ai_configuration(
        serde_json::from_value(json!({
            "general": {"agent_shortcut": {"enabled": true}, "show_agent_setup": show_setup},
            "profiles": {
                "profiles": [{"id":1,"name":"Fixture","endpoint":endpoint,"model":"fixture"}],
                "default":1,"last_id":1
            }
        }))
        .unwrap(),
    );
}

fn submit(fixture: &mut Fixture, line: &str) -> window::Id {
    let existing = fixture
        .workspace
        .terminal_agents
        .runs
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let _window = fixture.workspace.open_agent_shortcut(
        fixture.owner,
        fixture.session,
        &Aliases::new("", false).unwrap(),
        line,
    );
    *fixture
        .workspace
        .terminal_agents
        .runs
        .keys()
        .find(|id| !existing.contains(id))
        .unwrap()
}

fn select(fixture: &mut Fixture, columns: u16, rows: u16, bytes: &[u8]) {
    let mut terminal = kortty_terminal::Terminal::new(columns, rows);
    terminal.advance(bytes);
    let pane = fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap();
    pane.frame = selection_test_frame(terminal.snapshot());
    Arc::get_mut(&mut pane.frame).unwrap().status = SessionStatus::Running;
    pane.selection = Some(Selection::from_cells((0, 0), (rows - 1, columns - 1)));
}

#[test]
fn agent_shortcut_default_and_plan_and_menu_keep_setup() {
    let mut fixture = Fixture::new(false, true);
    select(&mut fixture, 80, 1, b"Selected terminal text");
    let ask = submit(&mut fixture, "agent-ask inspect the context");
    assert!(fixture.run(ask).phase == Phase::Setup);
    assert_eq!(fixture.run(ask).context, "Selected terminal text");

    configure(&mut fixture, "127.0.0.1:12345", false);
    let menu = fixture.open(Mode::Ask);
    assert!(fixture.run(menu).phase == Phase::Setup);
    assert_eq!(fixture.run(menu).context, "Selected terminal text");
    for line in ["agent-plan plan this", "agent -plan plan that"] {
        let id = submit(&mut fixture, line);
        assert!(fixture.run(id).mode == Mode::Plan && fixture.run(id).phase == Phase::Setup);
        assert_eq!(fixture.run(id).epoch, 0);
    }
}

#[test]
fn agent_shortcut_direct_ask_sends_only_the_masked_question_even_with_large_selection() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let mut fixture = Fixture::new(false, true);
    configure(
        &mut fixture,
        &listener.local_addr().unwrap().to_string(),
        false,
    );
    select(&mut fixture, 320, 90, &vec![b'Z'; 320 * 90 - 1]);
    let pane = &fixture.workspace.sessions[&fixture.session];
    assert!(
        pane.selection
            .unwrap()
            .visible_joined_text(&pane.frame.screen)
            .len()
            > 24 * 1024
    );
    let ask = submit(&mut fixture, "agent-ask Inspect API_KEY=fixture-secret");
    let run = fixture.run(ask);
    assert!(run.mode == Mode::Ask && run.phase == Phase::ModelReview);
    assert!(run.started_order > 0 && run.context.is_empty() && run.commands.is_empty());
    let epoch = run.epoch;
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        let wire = request(&mut socket);
        let content = wire["messages"][1]["content"].as_str().unwrap();
        let input: serde_json::Value = serde_json::from_str(content).unwrap();
        assert_eq!(input["goal"], "Inspect API_KEY=***");
        assert_eq!(input["context"], "");
        assert!(!content.contains("fixture-secret"));
        socket
            .get_mut()
            .write_all(
                fixture_response(
                    "200 OK",
                    "",
                    r#"{"status":"done","summary":"Done","userMessage":"The fixture answered."}"#,
                )
                .as_bytes(),
            )
            .unwrap();
    });
    let _ = fixture.send(
        ask,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    assert!(fixture.run(ask).phase == Phase::Thinking);
    server.join().unwrap();
}

#[cfg(windows)]
#[test]
fn agent_shortcut_windows_local_run_requires_its_own_directory_confirmation() {
    use iced::futures::{StreamExt as _, executor::block_on};

    let (listener, _) = listener();
    let mut fixture = Fixture::new(false, true);
    configure(
        &mut fixture,
        &listener.local_addr().unwrap().to_string(),
        false,
    );
    let path = fixture.profile.0.canonicalize().unwrap();
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .local_security
        .as_mut()
        .unwrap()
        .working_directory = Some(path.clone());
    let id = submit(&mut fixture, "agent Inspect this directory");
    let run = fixture.run(id);
    assert!(run.phase == Phase::DirectoryConfirmation && run.target.is_none());
    assert_eq!(run.manual_directory.as_deref(), path.to_str());
    let epoch = run.epoch;
    no_task(fixture.send(id, Message::Approve(epoch)));
    no_task(fixture.send(id, Message::ConfirmDirectory(epoch - 1)));
    no_task(fixture.send(id, Message::DirectoryChoice(String::new())));
    no_task(fixture.send(id, Message::ConfirmDirectory(epoch)));
    assert!(fixture.run(id).phase == Phase::DirectoryConfirmation);
    no_task(fixture.send(id, Message::DirectoryChoice(path.to_str().unwrap().into())));
    let mut task =
        iced_runtime::task::into_stream(fixture.send(id, Message::ConfirmDirectory(epoch)))
            .unwrap();
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(
        actual,
        result @ Message::Directory(..),
    ))) = block_on(task.next())
    else {
        panic!("the confirmed directory must be captured by the existing preflight");
    };
    assert_eq!(actual, id);
    no_task(fixture.send(id, result));
    let run = fixture.run(id);
    assert!(run.phase == Phase::ProbeApproval && run.activity.is_empty());
    assert!(run.reviewed_command().is_some());
    let epoch = run.epoch;
    let mut task =
        iced_runtime::task::into_stream(fixture.send(id, Message::Approve(epoch))).unwrap();
    let Some(iced_runtime::Action::Output(Desktop::TerminalAgent(
        actual,
        directory @ Message::Directory(..),
    ))) = block_on(task.next())
    else {
        panic!("the approved probe must recheck its directory");
    };
    assert_eq!(actual, id);
    drop(fixture.send(id, directory));
    let run = fixture.run(id);
    assert!(run.phase == Phase::Probe);
    let epoch = run.epoch;
    let probe = Outcome {
        status: engine::Status::Exited(0),
        stdout: Default::default(),
        stderr: Default::default(),
    };
    no_task(fixture.send(id, Message::Command(epoch, Arc::new(Ok(probe)))));
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview && run.turns == 0);
    assert!(
        run.model_review
            .as_ref()
            .unwrap()
            .input
            .contains("Inspect this directory")
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
