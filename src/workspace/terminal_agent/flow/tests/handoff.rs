use std::io::Write as _;
use std::net::TcpListener;

use super::*;
use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};

fn completed(private: bool) -> (Fixture, window::Id, TcpListener) {
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, private, &listener.local_addr().unwrap().to_string());
    let provider = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        let input = request(&mut socket).to_string();
        assert!(input.contains("Inspect service"));
        assert!(!input.contains("original-secret"));
        let body = r#"{"status":"done","summary":"Service checked","userMessage":"Check the worker. API_KEY=answer-secret"}"#;
        socket
            .get_mut()
            .write_all(fixture_response("200 OK", "", body).as_bytes())
            .unwrap();
        listener
    });
    let id = fixture.open(Mode::Ask);
    no_task(fixture.send(
        id,
        Message::Goal("Inspect service password=original-secret".into()),
    ));
    let task = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, task);
    assert!(fixture.run(id).phase == Phase::ModelReview);
    let epoch = fixture.run(id).epoch;
    let task = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, task);
    let listener = provider.join().unwrap();
    assert!(fixture.run(id).phase == Phase::Finished);
    assert!(fixture.run(id).can_chat_handoff());
    (fixture, id, listener)
}

fn no_provider_request(listener: &TcpListener) {
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn agent_handoff_creates_one_masked_chat_and_keeps_the_original_draft() {
    let _exclusive = exclusive_run();
    let (mut fixture, id, listener) = completed(false);
    let epoch = fixture.run(id).epoch;
    let source_tab = fixture.workspace.windows[&fixture.owner].tab().unwrap().id;
    no_task(fixture.send(
        id,
        Message::ChatFollowUp(epoch - 1, "Stale question".into()),
    ));
    assert!(fixture.run(id).chat_follow_up.is_empty());
    no_task(fixture.send(
        id,
        Message::ChatFollowUp(epoch, "What should I check next?".into()),
    ));
    let handoff = fixture.send(id, Message::ContinueInChat(epoch));
    assert!(fixture.run(id).chat_handoff_pending);
    no_task(fixture.send(id, Message::ContinueInChat(epoch)));
    finish_agent_task(&mut fixture, handoff);
    assert_eq!(fixture.workspace.terminal_chats.len(), 1);
    assert!(fixture.run(id).chat_handoff_dismissed);
    assert!(fixture.run(id).chat_follow_up.is_empty());
    no_provider_request(&listener);

    let paths = std::fs::read_dir(fixture.profile.0.join("ai-chats"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    let document = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(document.contains("Recorded Agent activity"));
    assert!(document.contains("Check the worker"));
    assert!(!document.contains("original-secret"));
    assert!(!document.contains("answer-secret"));
    assert!(!document.contains("What should I check next?"));

    no_task(fixture.send(
        id,
        Message::ChatFollowUp(epoch, "Keep this separate draft".into()),
    ));
    let task = fixture.send(id, Message::ContinueInChat(epoch));
    finish_agent_task(&mut fixture, task);
    assert_eq!(fixture.run(id).chat_follow_up, "Keep this separate draft");
    assert_eq!(fixture.workspace.terminal_chats.len(), 1);
    let _ = fixture.workspace.close_terminal_agent(id);
    fixture
        .workspace
        .select_tab(fixture.owner, source_tab, false);
    let task = fixture
        .workspace
        .continue_latest_agent_chat(fixture.owner, fixture.session);
    finish_agent_task(&mut fixture, task);
    assert_eq!(fixture.workspace.terminal_chats.len(), 1);
    no_provider_request(&listener);
}

#[test]
fn agent_handoff_revokes_queued_publication_when_the_source_stream_changes() {
    let _exclusive = exclusive_run();
    let (mut fixture, id, listener) = completed(false);
    let epoch = fixture.run(id).epoch;
    no_task(fixture.send(
        id,
        Message::ChatFollowUp(epoch, "Retain my question".into()),
    ));
    let task = fixture.send(id, Message::ContinueInChat(epoch));
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    fixture.workspace.revalidate_agent_chat_handoffs();
    finish_agent_task(&mut fixture, task);
    assert!(!fixture.run(id).chat_handoff_pending);
    assert_eq!(fixture.run(id).chat_follow_up, "Retain my question");
    assert!(fixture.workspace.terminal_chats.is_empty());
    assert!(
        !fixture
            .workspace
            .agent_chat_handoff_available(fixture.session)
    );
    assert!(!fixture.profile.0.join("ai-chats").exists());
    no_provider_request(&listener);
}

#[test]
fn private_agent_handoff_keeps_the_chat_in_memory_and_has_no_retained_menu_entry() {
    let _exclusive = exclusive_run();
    let (mut fixture, id, listener) = completed(true);
    let epoch = fixture.run(id).epoch;
    assert!(
        !fixture
            .workspace
            .agent_chat_handoff_available(fixture.session)
    );
    let task = fixture.send(id, Message::ContinueInChat(epoch));
    finish_agent_task(&mut fixture, task);
    assert_eq!(fixture.workspace.terminal_chats.len(), 1);
    assert!(fixture.run(id).chat_handoff_dismissed);
    assert!(!fixture.profile.0.join("ai-chats").exists());
    no_provider_request(&listener);
}
