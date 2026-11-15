use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

use iced::futures::StreamExt as _;

use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};

use super::*;

fn fixture(endpoint: SocketAddr) -> (tests::Scratch, Workspace, window::Id, SessionId, u64) {
    let scratch = tests::Scratch::new();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let grid = crate::session::GridSize::default();
    let shell =
        crate::session::Session::start(grid, kortty_terminal::TerminalProfile::Xterm).unwrap();
    let session = workspace.store_session(shell, grid, 14);
    workspace.add_tab(owner, session);
    let pane = workspace.sessions.get_mut(&session).unwrap();
    tests::wait(|| pane.session.journal_available());
    pane.frame = pane.session.frame();
    let tab = workspace.windows[&owner].active_tab_id().unwrap();
    workspace.window_settings.session_journal.directory =
        scratch.0.join("journals").display().to_string();
    workspace.window_settings.journal_ai.enabled = true;
    workspace.window_settings.journal_ai.endpoint = endpoint.to_string();
    workspace.window_settings.journal_ai.model = "fixture:local".into();
    workspace
        .window_settings
        .save(&scratch.0.join("window-settings.json"))
        .unwrap();
    let mut queue = crate::session_journal::summary_queue::Queue::load(&scratch.0).unwrap();
    let mut preferences = queue.preferences().clone();
    preferences.enabled = true;
    queue.set_preferences(preferences).unwrap();
    workspace.journal_summaries.load(&scratch.0);
    (scratch, workspace, owner, session, tab)
}

fn complete(workspace: &mut Workspace, task: Task<Desktop>) -> Task<Desktop> {
    let mut actions = iced_runtime::task::into_stream(task).expect("expected a journal task");
    let next = block_on(actions.next()).expect("journal operation stopped");
    let iced_runtime::Action::Output(Desktop::JournalCapture(message)) = next else {
        panic!("expected a journal operation completion");
    };
    workspace.update_journal_capture(message)
}

fn failed(workspace: &Workspace, tab: u64) -> u64 {
    let start = &workspace.journal_ai_starts[&tab];
    assert!(matches!(start.status, ProbeStatus::Failed(_)));
    start.ticket
}

#[test]
fn journal_preflight_manual_failure_retries_or_starts_a_raw_recording() {
    use std::io::Write as _;

    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (scratch, mut workspace, owner, session, tab) = fixture(server.local_addr().unwrap());
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept(&server);
            let payload = request(&mut stream);
            assert_eq!(payload["messages"][1]["content"], "{}");
            stream
                .get_mut()
                .write_all(fixture_response("200 OK", "", "Unavailable").as_bytes())
                .unwrap();
        }
    });
    let check = workspace.toggle_journal_for(owner, tab);
    assert!(!workspace.journal_captures.contains_key(&tab));
    assert!(!scratch.0.join("journals").is_dir());
    let _ = complete(&mut workspace, check);
    let initial = failed(&workspace, tab);
    let retry = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        initial,
        ProbeChoice::Retry,
    ));
    let _ = complete(&mut workspace, retry);
    let current = failed(&workspace, tab);
    assert_ne!(initial, current);
    let _ = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        initial,
        ProbeChoice::WithoutAi,
    ));
    assert!(!workspace.journal_captures.contains_key(&tab));
    let start = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        current,
        ProbeChoice::WithoutAi,
    ));
    let _ = complete(&mut workspace, start);
    let capture = workspace.journal_captures[&tab].metadata_capture().unwrap();
    assert!(!capture.summary_config().unwrap().ai_summaries);
    assert!(workspace.pending_journal_summary_sources().is_empty());
    workspace.stop_journal(tab);
    tests::wait(|| capture.done().is_closed());
    let stopped = workspace.sessions[&session].session.stop();
    tests::wait(|| stopped.is_closed());
    server.join().unwrap();
}

#[test]
fn journal_preflight_automatic_does_not_wait_for_the_provider_and_can_keep_the_capture() {
    use std::io::Write as _;

    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (_scratch, mut workspace, owner, session, tab) = fixture(server.local_addr().unwrap());
    let (release, pending) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&server);
        assert_eq!(request(&mut stream)["messages"][1]["content"], "{}");
        pending.recv_timeout(Duration::from_secs(10)).unwrap();
        stream
            .get_mut()
            .write_all(fixture_response("200 OK", "", "Unavailable").as_bytes())
            .unwrap();
    });
    workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .journal_config
        .auto_start = true;
    let start = workspace.reconcile_journal_sources();
    let mut events = iced_runtime::task::into_stream(start).unwrap();
    let Some(iced_runtime::Action::Output(Desktop::JournalCapture(message @ Message::Started(..)))) =
        block_on(events.next())
    else {
        panic!("the capture should start while the probe is unresolved");
    };
    let _ = workspace.update_journal_capture(message);
    let capture = workspace.journal_captures[&tab].metadata_capture().unwrap();
    assert_eq!(capture.status(), Status::Recording);
    assert!(workspace.pending_journal_summary_sources().is_empty());
    release.send(()).unwrap();
    let Some(iced_runtime::Action::Output(Desktop::JournalCapture(
        message @ Message::AiProbeFinished(..),
    ))) = block_on(events.next())
    else {
        panic!("expected the provider check result");
    };
    let _ = workspace.update_journal_capture(message);
    let ticket = failed(&workspace, tab);
    let _ = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        ticket,
        ProbeChoice::WithoutAi,
    ));
    assert!(!workspace.journal_ai_starts.contains_key(&tab));
    assert!(workspace.journal_captures[&tab].without_ai);
    assert_eq!(capture.status(), Status::Recording);
    assert!(workspace.pending_journal_summary_sources().is_empty());
    workspace.stop_journal(tab);
    tests::wait(|| capture.done().is_closed());
    let stopped = workspace.sessions[&session].session.stop();
    tests::wait(|| stopped.is_closed());
    server.join().unwrap();
}

#[test]
fn journal_preflight_manual_success_checks_once_and_ignores_a_stale_action() {
    use std::io::Write as _;

    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (scratch, mut workspace, owner, session, tab) = fixture(server.local_addr().unwrap());
    let (received, in_progress) = mpsc::channel();
    let (release, pending) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&server);
        assert_eq!(request(&mut stream)["messages"][1]["content"], "{}");
        received.send(()).unwrap();
        pending.recv_timeout(Duration::from_secs(10)).unwrap();
        stream
            .get_mut()
            .write_all(fixture_response("200 OK", "", r#"{"ready":true}"#).as_bytes())
            .unwrap();
    });
    let check = workspace.toggle_journal_for(owner, tab);
    let ticket = workspace.journal_ai_starts[&tab].ticket;
    in_progress.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!scratch.0.join("journals").exists());
    release.send(()).unwrap();
    let opening = complete(&mut workspace, check);
    assert!(matches!(
        workspace.journal_captures[&tab].phase,
        Phase::Starting(..)
    ));
    let ignored = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        ticket,
        ProbeChoice::Cancel,
    ));
    assert!(iced_runtime::task::into_stream(ignored).is_none());
    let _ = complete(&mut workspace, opening);
    assert!(workspace.journal_ai_starts.is_empty());
    let capture = workspace.journal_captures[&tab].metadata_capture().unwrap();
    assert!(capture.summary_config().unwrap().ai_summaries);
    assert_eq!(workspace.pending_journal_summary_sources().len(), 1);
    workspace.stop_journal(tab);
    tests::wait(|| capture.done().is_closed());
    let stopped = workspace.sessions[&session].session.stop();
    tests::wait(|| stopped.is_closed());
    server.join().unwrap();
}

#[test]
fn journal_preflight_manual_cancel_ignores_a_late_success() {
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (scratch, mut workspace, owner, session, tab) = fixture(server.local_addr().unwrap());
    let check = workspace.toggle_journal_for(owner, tab);
    let ticket = workspace.journal_ai_starts[&tab].ticket;
    let _ = workspace.update_journal_capture(Message::AiProbeChoice(
        owner,
        tab,
        ticket,
        ProbeChoice::Cancel,
    ));
    let late = workspace.update_journal_capture(Message::AiProbeFinished(tab, ticket, Ok(())));
    assert!(iced_runtime::task::into_stream(late).is_none());
    assert!(!workspace.journal_ai_starts.contains_key(&tab));
    assert!(!workspace.journal_captures.contains_key(&tab));
    assert!(!scratch.0.join("journals").exists());
    drop(server);
    let _ = complete(&mut workspace, check);
    let stopped = workspace.sessions[&session].session.stop();
    tests::wait(|| stopped.is_closed());
}
