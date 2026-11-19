use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;

use iced::futures::FutureExt as _;
use serde_json::json;
use zeroize::Zeroizing;

use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};
use crate::session_journal::RecordKind;
use crate::snippets::tests::Profile as Scratch;

use super::*;

mod automation_preflight;
mod evidence_limits;
mod java_roles;
mod lm_studio_auto;
mod weighted_batch;

fn seeded(endpoint: SocketAddr) -> (Scratch, Workspace, Arc<Capture>, Source) {
    let scratch = Scratch::new();
    let profile = scratch.0.canonicalize().unwrap();
    let root = profile.join("journals");
    let capture = Arc::new(
        Capture::start(
            root.clone(),
            session_journal::Metadata {
                title: "Build fixture".into(),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    for (kind, text) in [
        (RecordKind::Input, "build.sh"),
        (
            RecordKind::Output,
            "build.sh exited 2; DB_PASSWORD=fixture-private",
        ),
        (
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 2)",
        ),
    ] {
        capture
            .sink()
            .try_write(kind, Zeroizing::new(text.into()), false)
            .unwrap();
    }
    let mut workspace = Workspace::default();
    workspace.journal_summaries.load(&profile);
    workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
    workspace.window_settings.journal_ai.enabled = true;
    workspace.window_settings.journal_ai.endpoint = endpoint.to_string();
    workspace.window_settings.journal_ai.model = "fixture:local".into();
    let source = Source::capture(&capture);
    (scratch, workspace, capture, source)
}

fn finish_capture(capture: &Capture) {
    let done = capture.finish();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "capture did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn journal_summary_row_badges_track_eligible_receipts_and_exact_identity() {
    let (server, _) = listener();
    let (_scratch, mut workspace, capture, source) = seeded(server.local_addr().unwrap());
    let state = &mut workspace.journal_summaries;
    let mut settings = state.queue.preferences().clone();
    settings.enabled = true;
    state.queue.set_preferences(settings).unwrap();
    let generation = "1".repeat(32);
    let receipt = Receipt {
        generation: generation.clone(),
        batch: generation.clone(),
        source: source.summary_receipt(),
        title: "Build fixture".into(),
        mode: PassMode::Closing,
        profile: None,
        source_approval: None,
        raw_only: false,
        automatic_title: false,
        title_approval: None,
        full_output: false,
        evidence_limits: None,
        screenshots: false,
        automation_vision: None,
    };
    state.queue.enqueue([receipt.clone()]).unwrap();
    state.live.push(Live {
        capture: Arc::clone(&capture),
        receipt,
        interval: Duration::from_secs(30),
        next: Instant::now(),
        force: false,
        running_preview: Arc::default(),
        redactor: Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            workspace.clipboard.policy.terminal_logging.clone(),
        )),
    });
    let root = source.directory().parent().unwrap();
    let mut catalog = session_journal::catalog(root).unwrap();
    assert_eq!(catalog.journals.len(), 1);
    assert!(state.row_statuses(&catalog).is_empty());
    finish_capture(&capture);
    catalog = session_journal::catalog(root).unwrap();
    assert_eq!(
        state.row_statuses(&catalog),
        [Some(RowStatus::FinishWaiting)]
    );
    catalog.journals[0].id.push_str("-replacement");
    assert_eq!(state.row_statuses(&catalog), [None]);
    state.paused.insert(generation);
    assert!(state.row_statuses(&catalog).is_empty());
}

#[test]
fn running_journal_cards_follow_cadence_and_record_observed_usage() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, seeded_capture, seeded_source) =
        seeded(endpoint.local_addr().unwrap());
    finish_capture(&seeded_capture);
    let capture = Arc::new(
        Capture::start(
            seeded_source.directory().parent().unwrap().to_owned(),
            Default::default(),
        )
        .unwrap(),
    );
    let source = Source::capture(&capture);
    let sink = capture.sink();
    for (kind, value) in [
        (RecordKind::Input, "build.sh"),
        (RecordKind::Output, "Still building"),
    ] {
        sink.try_write(kind, Zeroizing::new(value.into()), false)
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while capture.published().0 < 2 {
        assert!(
            Instant::now() < deadline,
            "capture did not publish the command"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let page = source.capture_page(None, None, None).unwrap();
    let first = page
        .records
        .iter()
        .find(|record| record.text == "build.sh")
        .unwrap();
    let age = |seconds| {
        first
            .timestamp
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .unwrap()
    };
    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            summary_interval_minutes: 1,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Running fixture".into(),
    );
    let run_pass = |workspace: &mut Workspace, now, age_now, probes: bool, expected: &[&str]| {
        let task = workspace.start_next_journal_summary_at(now, age_now);
        verify_cadence_pass(workspace, task, &endpoint, probes, expected);
    };
    let due = workspace.journal_summaries.live[0].next;
    run_pass(&mut workspace, due, age(899), false, &[]);
    assert!(source.read(None).unwrap().unwrap().timeline.is_empty());
    let due = workspace.journal_summaries.live[0].next;
    run_pass(&mut workspace, due, age(900), true, &["running"]);
    let (first_seq, published_at) = workspace.journal_summaries.live[0]
        .running_preview
        .lock()
        .unwrap()
        .unwrap();
    assert_eq!(first_seq, first.sequence);
    let before_refresh = published_at + Duration::from_secs(899);
    run_pass(&mut workspace, before_refresh, age(1799), false, &[]);
    // Make the scheduler due at the exact refresh boundary instead of its next regular tick.
    let refresh = published_at + Duration::from_secs(900);
    workspace.journal_summaries.live[0].next = refresh;
    run_pass(&mut workspace, refresh, age(1800), false, &["running"]);
    let journal = source.read(None).unwrap().unwrap();
    assert_eq!(
        journal
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AI_SUMMARY")
            .count(),
        1
    );
    assert_eq!(journal.summary.usage.call_count, 2);

    workspace.window_settings.open_tool_windows_as_tabs = true;
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_journal_viewer(owner, source.clone(), "Running fixture".into());
    let viewer = *workspace.journals.keys().next().unwrap();
    let task = summary(&mut workspace, viewer, Event::SummarizeNow);
    verify_cadence_pass(&mut workspace, task, &endpoint, false, &["running"]);
    let journal = source.read(None).unwrap().unwrap();
    assert_eq!(journal.summary.usage.call_count, 3);
    // Closing finalizes the command even though its most recent preview just published.
    finish_capture(&capture);
    let task = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    verify_cadence_pass(
        &mut workspace,
        task,
        &endpoint,
        false,
        &["session_ended", "closing"],
    );
    let journal = source.read(None).unwrap().unwrap();
    assert_eq!(
        journal
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AI_SUMMARY")
            .count(),
        1
    );
    assert!(
        journal
            .timeline
            .iter()
            .any(|entry| entry.kind == "SESSION_SUMMARY")
    );
    assert_eq!(journal.summary.usage.call_count, 5);
    assert_eq!(journal.summary.usage.prompt_tokens, 50);
    assert_eq!(journal.summary.usage.completion_tokens, 20);
    assert_eq!(journal.summary.usage.total_tokens, 70);
    assert!(!journal.summary.usage_partial);
}

fn verify_cadence_pass(
    workspace: &mut Workspace,
    task: Task<Desktop>,
    endpoint: &TcpListener,
    probe_expected: bool,
    expected: &[&str],
) {
    let mut actions = iced_runtime::task::into_stream(task).expect("due summary task");
    if probe_expected {
        let mut probe = accept(endpoint);
        let body = request(&mut probe);
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][1]["content"], "{}");
        probe
            .get_mut()
            .write_all(fixture_response("200 OK", "", r#"{"ready":true}"#).as_bytes())
            .unwrap();
        drop(probe);
    }
    for &stage in expected {
        let mut socket = accept(endpoint);
        let body = request(&mut socket);
        assert_eq!(body["stream"], true);
        let prompt: serde_json::Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        if stage == "closing" {
            assert!(prompt.as_str().unwrap().contains("Checked build"));
        } else {
            assert_eq!(prompt["end"], stage);
            assert_eq!(prompt["commands"][0]["text"], "build.sh");
        }
        let answer = json!({"choices":[{"index":0,"delta":{"content":r#"{"title":"Checked build","summary":"Summarized build output.","keywords":[]}"#},"finish_reason":"stop"}]});
        let usage = json!({"choices":[], "usage":{"prompt_tokens":10,"completion_tokens":4}});
        socket
            .get_mut()
            .write_all(
                crate::local_ai::test_support::fixture_http_response(
                    "200 OK",
                    "Content-Type: text/event-stream\r\n",
                    &format!("data: {answer}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
                )
                .as_bytes(),
            )
            .unwrap();
    }
    finish_pass(workspace, &mut actions);
    assert!(matches!(endpoint.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    assert!(workspace.journal_summaries.active.is_none());
}

fn show_manager(workspace: &mut Workspace) -> window::Id {
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.update(Desktop::SessionJournal(owner, JournalMessage::Open));
    *workspace.journals.keys().next().unwrap()
}

fn review(workspace: &mut Workspace, manager_id: window::Id, source: &Source) {
    let Mode::Manager(manager) = &workspace.journals[&manager_id].mode else {
        panic!("expected manager")
    };
    if let Some((revision, root)) = manager.reading.clone() {
        let catalog = session_journal::catalog(&root)
            .map(Arc::new)
            .map_err(|error| error.to_string());
        let _ = workspace.update(Desktop::SessionJournal(
            manager_id,
            JournalMessage::CatalogLoaded(revision, root, catalog),
        ));
    }
    let Mode::Manager(manager) = &workspace.journals[&manager_id].mode else {
        panic!("expected manager")
    };
    let select = super::super::manager_tree::Event::Press(
        manager.tree.generation(),
        session_journal::tree::NodeKey::Journal(source.directory().to_owned()),
    );
    let _ = workspace.update(Desktop::SessionJournal(
        manager_id,
        JournalMessage::Tree(select),
    ));
    let _ = summary(workspace, manager_id, Event::Review(false));
    assert!(
        workspace
            .journal_summaries
            .reviews
            .contains_key(&manager_id)
    );
}

fn summary(workspace: &mut Workspace, manager: window::Id, event: Event) -> Task<Desktop> {
    workspace.update(Desktop::SessionJournal(
        manager,
        JournalMessage::Summary(event),
    ))
}

fn next_worker(actions: &mut BoxStream<'static, iced_runtime::Action<Desktop>>) -> Desktop {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match actions.next().now_or_never() {
            Some(Some(iced_runtime::Action::Output(event @ Desktop::JournalSummary(_)))) => {
                return event;
            }
            Some(None) => panic!("summary worker task ended without an outcome"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "summary task did not progress");
        thread::sleep(Duration::from_millis(5));
    }
}

fn observe_response(
    workspace: &mut Workspace,
    actions: &mut BoxStream<'static, iced_runtime::Action<Desktop>>,
) -> Preview {
    loop {
        let event = next_worker(actions);
        assert!(matches!(
            &event,
            Desktop::JournalSummary(Worker::Progress(_))
        ));
        let _ = workspace.update(event);
        if let Some(preview) = workspace
            .journal_summaries
            .active
            .as_ref()
            .and_then(|active| active.visible.preview.as_ref())
            && preview.answer_bytes > 0
        {
            return preview.clone();
        }
    }
}

fn finish_pass(
    workspace: &mut Workspace,
    actions: &mut BoxStream<'static, iced_runtime::Action<Desktop>>,
) {
    loop {
        let event = next_worker(actions);
        let finished = matches!(&event, Desktop::JournalSummary(Worker::Finished(..)));
        let _ = workspace.update(event);
        if finished {
            break;
        }
    }
}

fn send_prefix(endpoint: &TcpListener) -> std::io::BufReader<std::net::TcpStream> {
    let mut check = accept(endpoint);
    let body = request(&mut check);
    assert_eq!(body["stream"], false);
    assert_eq!(body["messages"][1]["content"], "{}");
    check
        .get_mut()
        .write_all(fixture_response("200 OK", "", r#"{"ready":true}"#).as_bytes())
        .unwrap();
    drop(check);
    let mut stream = accept(endpoint);
    let body = request(&mut stream);
    assert_eq!(body["stream"], true);
    assert_eq!(body["response_format"]["type"], "json_object");
    let input = body["messages"][1]["content"].as_str().unwrap();
    assert!(input.contains("build.sh"));
    assert!(!input.contains("fixture-private"));
    stream
        .get_mut()
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    let content = r#"{"title":"Checked build","summary":"The build exited 2.","keywords":["invented","build.sh""#;
    writeln!(
        stream.get_mut(),
        "data: {}\n",
        json!({"choices":[{"index":0,"delta":{"content":content}}]})
    )
    .unwrap();
    stream
}

#[test]
fn journal_summary_manager_streams_verified_keywords_and_cancels_without_partial_entries() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, capture, source) = seeded(endpoint.local_addr().unwrap());
    finish_capture(&capture);
    let manager = show_manager(&mut workspace);
    review(&mut workspace, manager, &source);
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let (advance, wait) = mpsc::channel();
    let (finish_closing, wait_closing) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        wait.recv_timeout(Duration::from_secs(4)).unwrap();
        stream.get_mut().write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
        drop(stream);
        let mut closing = accept(&endpoint);
        let body = request(&mut closing);
        assert_eq!(body["stream"], true);
        wait_closing.recv_timeout(Duration::from_secs(4)).unwrap();
        closing.get_mut().write_all(fixture_response("200 OK", "", r#"{"title":"Closing fixture","summary":"The build was checked.","keywords":[]}"#).as_bytes()).unwrap();
        drop(closing);

        let mut canceled = send_prefix(&endpoint);
        expect_socket_closed(&mut canceled);
    });

    let _ = summary(&mut workspace, manager, Event::FullOutput(true));
    let task = summary(&mut workspace, manager, Event::Start);
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let preview = observe_response(&mut workspace, &mut actions);
    assert!(preview.stage == Stage::Window);
    assert_eq!(preview.keywords, ["build.sh"]);
    let label = summary_live_status(&preview);
    assert!(label.contains("build.sh"));
    assert!(!label.contains("Checked build") && !label.contains("invented"));
    let progress_id = workspace.journal_summaries.details.as_ref().unwrap().id;
    let active = workspace.journal_summaries.active.as_ref().unwrap();
    assert_eq!(active.visible.outcome.planned_windows, Some(1));
    let ticket = active.ticket;
    assert!(
        workspace
            .journal_summary_progress_view(progress_id)
            .is_some()
    );
    let _ = summary(&mut workspace, progress_id, Event::CloseProgress);
    assert!(workspace.journal_summaries.details.is_none());
    assert_eq!(
        workspace.journal_summaries.active.as_ref().unwrap().ticket,
        ticket
    );
    let _ = summary(&mut workspace, manager, Event::ShowProgress);
    let reopened = workspace.journal_summaries.details.as_ref().unwrap().id;
    assert_ne!(reopened, progress_id);
    assert!(workspace.journal_summary_progress_view(reopened).is_some());
    assert_eq!(workspace.journal_summaries.queue.pending().len(), 1);
    assert!(workspace.journal_summaries.queue.pending()[0].full_output);
    assert!(
        !source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "AI_SUMMARY")
    );
    advance.send(()).unwrap();
    loop {
        let event = next_worker(&mut actions);
        assert!(matches!(
            &event,
            Desktop::JournalSummary(Worker::Progress(_))
        ));
        let _ = workspace.update(event);
        let state = &workspace.journal_summaries;
        let active = state.active.as_ref().unwrap();
        if active
            .visible
            .preview
            .as_ref()
            .is_some_and(|preview| preview.stage == Stage::Closing)
            && let Some(estimate) = state.estimate(active, Instant::now())
        {
            assert_eq!(active.visible.outcome.windows, 1);
            assert!(estimate.active_percent() > 0 && estimate.active_percent() < 100);
            assert!(estimate.remaining.is_some());
            assert!(
                state
                    .progress_status(active, Instant::now())
                    .contains("remaining")
            );
            break;
        }
    }
    finish_closing.send(()).unwrap();
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.queue.pending().is_empty());
    let completed = source.read(None).unwrap().unwrap();
    assert!(
        completed
            .timeline
            .iter()
            .any(|entry| entry.kind == "AI_SUMMARY" && entry.title == "Checked build")
    );
    assert!(
        completed
            .timeline
            .iter()
            .any(|entry| entry.kind == "SESSION_SUMMARY" && entry.title == "Closing fixture")
    );

    review(&mut workspace, manager, &source);
    let task = summary(&mut workspace, manager, Event::Start);
    let mut canceled = iced_runtime::task::into_stream(task).unwrap();
    let _ = observe_response(&mut workspace, &mut canceled);
    let batch = workspace
        .journal_summaries
        .active
        .as_ref()
        .unwrap()
        .receipt
        .batch
        .clone();
    let _ = summary(
        &mut workspace,
        progress_id,
        Event::CancelProgress(batch.clone()),
    );
    assert!(
        !workspace
            .journal_summaries
            .active
            .as_ref()
            .unwrap()
            .stop
            .load(Ordering::Acquire)
    );
    let _ = summary(&mut workspace, reopened, Event::CancelProgress(batch));
    assert!(
        workspace
            .journal_summaries
            .active
            .as_ref()
            .unwrap()
            .visible
            .preview
            .is_none()
    );
    finish_pass(&mut workspace, &mut canceled);
    assert!(workspace.journal_summaries.active.is_none());
    assert!(workspace.journal_summaries.queue.pending().is_empty());
    let after = source.read(None).unwrap().unwrap();
    assert_eq!(after.timeline.len(), completed.timeline.len());
    for (after, before) in after.timeline.iter().zip(&completed.timeline) {
        assert_eq!(
            (&after.id, &after.kind, &after.title, &after.text),
            (&before.id, &before.kind, &before.title, &before.text)
        );
    }
    server.join().unwrap();
}

#[test]
fn automatic_journal_summary_streams_progress_only_and_pauses_when_policy_is_revoked() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, capture, source) = seeded(endpoint.local_addr().unwrap());
    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Build fixture".into(),
    );
    finish_capture(&capture);
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        expect_socket_closed(&mut stream);
    });
    let task = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let preview = observe_response(&mut workspace, &mut actions);
    assert!(preview.stage == Stage::Window);
    assert!(preview.keywords.is_empty());
    workspace.clipboard.policy.ai_allowed = false;
    let _ = workspace.update(Desktop::Noop);
    assert!(
        workspace
            .journal_summaries
            .active
            .as_ref()
            .unwrap()
            .visible
            .preview
            .is_none()
    );
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.active.is_none());
    assert_eq!(workspace.journal_summaries.queue.pending().len(), 1);
    assert!(
        !source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "AI_SUMMARY")
    );
    server.join().unwrap();
}

#[test]
fn automatic_journal_summary_persists_local_server_selection_and_omits_model() {
    use crate::ai_manager::Configuration;

    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, capture, source) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let configured = |address: SocketAddr| -> Configuration {
        serde_json::from_value(json!({
            "profiles": {
                "profiles": [{"id":1,"name":"Server-selected fixture","endpoint":address.to_string(),
                    "connection_mode":"external","model":"","model_mode":"server-default"}],
                "last_id":1
            },
            "local":{"roles":[null,null,null,1]}
        }))
        .unwrap()
    };
    configured(address)
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    assert!(workspace.manager_journal_profile_ready());
    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Build fixture".into(),
    );
    let reopened = Queue::load(&root).unwrap();
    let receipt = reopened.pending().front().unwrap();
    let choice = receipt.profile.as_ref().unwrap();
    assert!(choice.pins_server_default());
    assert_eq!(choice.destination, address.to_string());
    assert!(
        workspace
            .journal_summary_profile(Some(choice), false)
            .unwrap()
            .uses_server_default_model()
    );
    assert!(
        workspace
            .journal_summary_profile(Some(choice), true)
            .is_none()
    );

    finish_capture(&capture);
    let server = thread::spawn(move || {
        let mut check = accept(&endpoint);
        let body = request(&mut check);
        assert!(body.get("model").is_none());
        assert_eq!(body["stream"], false);
        check
            .get_mut()
            .write_all(fixture_response("200 OK", "", r#"{"ready":true}"#).as_bytes())
            .unwrap();
        drop(check);

        let mut stream = accept(&endpoint);
        let body = request(&mut stream);
        assert!(body.get("model").is_none());
        assert_eq!(body["stream"], true);
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("build.sh")
        );
        stream
            .get_mut()
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let content =
            r#"{"title":"Checked build","summary":"The build exited 2.","keywords":["build.sh"]}"#;
        writeln!(
            stream.get_mut(),
            "data: {}\n",
            json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}]})
        )
        .unwrap();
        stream.get_mut().write_all(b"data: [DONE]\n\n").unwrap();
        drop(stream);

        let mut closing = accept(&endpoint);
        let body = request(&mut closing);
        assert!(body.get("model").is_none());
        closing.get_mut().write_all(fixture_response("200 OK", "", r#"{"title":"Closing fixture","summary":"The build was checked.","keywords":[]}"#).as_bytes()).unwrap();
    });
    let task = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    finish_pass(&mut workspace, &mut actions);
    server.join().unwrap();
    assert!(workspace.journal_summaries.queue.pending().is_empty());
    assert!(
        source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "SESSION_SUMMARY")
    );

    let original = Configuration::load_fixture_from(&root).unwrap();
    let changed = if address.port() == 8000 { 8001 } else { 8000 };
    configured(SocketAddr::new(address.ip(), changed))
        .save_fixture_at(&root, &original)
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    assert!(
        workspace
            .journal_summary_profile(Some(choice), false)
            .is_none()
    );
    workspace
        .journal_summaries
        .queue
        .enqueue([receipt.clone()])
        .unwrap();
    let _ = workspace.start_next_journal_summary();
    assert!(workspace.journal_summaries.active.is_none());
    assert!(
        workspace
            .journal_summaries
            .paused
            .contains(&receipt.generation)
    );
    assert_eq!(Queue::load(&root).unwrap().pending().front(), Some(receipt));
}

#[test]
fn journal_summary_hands_a_closed_capture_to_the_saved_pass_after_a_live_write_failure() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, capture, source) = seeded(endpoint.local_addr().unwrap());
    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Build fixture".into(),
    );
    let receipt = workspace.journal_summaries.live[0].receipt.clone();
    let ticket = workspace.allocate();
    workspace.journal_summaries.active = Some(Active {
        ticket,
        periodic: true,
        started: Instant::now(),
        stop: Arc::new(AtomicBool::new(false)),
        transport: None,
        vision: None,
        vision_profile: None,
        progress: Arc::new(Mutex::new(PassProgress::default())),
        visible: PassProgress::default(),
        configuration: workspace.ai_configuration().unwrap(),
        profile: workspace.journal_summary_profile(receipt.profile.as_ref(), false),
        settings: workspace.window_settings.journal_ai.clone(),
        vault_revision: None,
        receipt,
    });
    finish_capture(&capture);
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        stream.get_mut().write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
        drop(stream);
        let mut closing = accept(&endpoint);
        let _ = request(&mut closing);
        closing.get_mut().write_all(fixture_response("200 OK", "", r#"{"title":"Closing fixture","summary":"The build was checked.","keywords":[]}"#).as_bytes()).unwrap();
    });

    let task = workspace.update(Desktop::JournalSummary(Worker::Finished(
        ticket,
        Err("The capture writer stopped accepting live publications.".into()),
    )));
    assert!(workspace.journal_summaries.status.is_none());
    assert!(workspace.journal_summaries.paused.is_empty());
    assert!(
        !workspace
            .journal_summaries
            .active
            .as_ref()
            .unwrap()
            .periodic
    );
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.queue.pending().is_empty());
    assert!(
        source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "SESSION_SUMMARY")
    );
    server.join().unwrap();
}

fn expect_socket_closed(socket: &mut impl Read) {
    match socket.read(&mut [0u8; 1]) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("expected cancellation to close the socket: {other:?}"),
    }
}

#[test]
fn journal_summary_cancel_stops_live_request_if_saving_cancel_fails() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (scratch, mut workspace, capture, source) = seeded(endpoint.local_addr().unwrap());
    finish_capture(&capture);
    let manager = show_manager(&mut workspace);
    review(&mut workspace, manager, &source);
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        expect_socket_closed(&mut stream);
    });
    let task = summary(&mut workspace, manager, Event::Start);
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let _ = observe_response(&mut workspace, &mut actions);
    let pending = scratch.0.join("journal-summary-jobs.json");
    let held = scratch.0.join("held-journal-summary-jobs.json");
    std::fs::rename(&pending, &held).unwrap();
    std::fs::create_dir(&pending).unwrap();

    let _ = summary(&mut workspace, manager, Event::Cancel);
    let state = &workspace.journal_summaries;
    let active = state.active.as_ref().unwrap();
    assert!(active.stop.load(Ordering::Acquire));
    assert!(active.visible.preview.is_none());
    assert!(state.paused.contains(&active.receipt.generation));
    assert!(
        state
            .status
            .as_deref()
            .unwrap()
            .contains("The saved pass remains")
    );
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.active.is_none());
    assert_eq!(workspace.journal_summaries.queue.pending().len(), 1);
    assert!(
        !source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "AI_SUMMARY")
    );
    server.join().unwrap();
    std::fs::remove_dir(&pending).unwrap();
    std::fs::rename(&held, &pending).unwrap();
    let _ = summary(&mut workspace, manager, Event::Cancel);
    assert!(workspace.journal_summaries.queue.pending().is_empty());
}

#[test]
fn journal_summary_configuration_change_pauses_other_receipts_in_the_active_batch() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, capture, source) = seeded(endpoint.local_addr().unwrap());
    finish_capture(&capture);
    let second = Capture::start(
        source.directory().parent().unwrap().to_owned(),
        Default::default(),
    )
    .unwrap();
    finish_capture(&second);
    let second = Source::capture(&second);
    let selected = workspace.journal_summary_choices().remove(0).profile;
    let batch = crate::private_files::nonce().unwrap();
    let jobs = [source, second].into_iter().map(|source| Receipt {
        generation: crate::private_files::nonce().unwrap(),
        batch: batch.clone(),
        source: source.summary_receipt(),
        title: "Reviewed fixture".into(),
        mode: PassMode::Reevaluate,
        profile: selected.clone(),
        source_approval: None,
        raw_only: false,
        automatic_title: false,
        title_approval: None,
        screenshots: false,
        automation_vision: None,
        full_output: false,
        evidence_limits: None,
    });
    workspace.journal_summaries.queue.enqueue(jobs).unwrap();
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        expect_socket_closed(&mut stream);
        endpoint
    });

    let task = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let _ = observe_response(&mut workspace, &mut actions);

    let unrelated_policy = crate::clipboard_policy::parse(
        r#"
            [meta]
            schema-version = 1

            [[ai-profile]]
            id = "policy-journal-unrelated-fixture"
            name = "Unrelated fixture"
            provider = "lm-studio"
            endpoint = "http://127.0.0.1:1"
        "#,
    )
    .unwrap();
    let previous = workspace.ai_configuration().unwrap();
    let projected = previous
        .as_ref()
        .clone()
        .with_policy_profiles(&unrelated_policy)
        .unwrap();
    workspace.set_test_ai_configuration(projected);
    workspace.preserve_journal_summary_for_policy_projection(&previous);
    let _ = workspace.update(Desktop::Noop);
    let state = &workspace.journal_summaries;
    assert!(!state.active.as_ref().unwrap().stop.load(Ordering::Acquire));
    assert!(state.paused.is_empty());

    workspace.set_test_ai_configuration(crate::ai_manager::Configuration::default());
    let _ = workspace.update(Desktop::Noop);
    let state = &workspace.journal_summaries;
    assert_eq!(state.queue.pending().len(), 2);
    assert!(
        state
            .queue
            .pending()
            .iter()
            .all(|receipt| state.paused.contains(&receipt.generation))
    );
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.active.is_none());
    assert_eq!(
        server.join().unwrap().accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
fn named_journal_summary_receipt_keeps_exact_profile_selection_on_restart() {
    use crate::ai_manager::Configuration;
    use crate::clipboard_policy;

    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, capture, source) = seeded(address);
    finish_capture(&capture);
    let declaration = |id, model| {
        format!(
            "\n[[ai-profile]]\nid = '{id}'\nname = 'Journal fixture'\nprovider = 'lm-studio'\nendpoint = 'http://{address}'\n{model}"
        )
    };
    let header = "[meta]\nschema-version = 1\n";
    let original = "policy-Reviewed-Journal-Fixture";
    let alternate = "policy-reviewed-Journal-Fixture";
    let initial = clipboard_policy::parse(&format!(
        "{header}{}",
        declaration(original, "model = 'fixture:local'\n")
    ))
    .unwrap();
    workspace.set_test_ai_configuration(
        Configuration::default()
            .with_policy_profiles(&initial)
            .unwrap(),
    );
    let selected = workspace
        .journal_summary_choices()
        .into_iter()
        .filter_map(|choice| choice.profile)
        .find(|choice| matches!(&choice.id, ProfileRef::Policy(id) if id == original))
        .expect("named policy profile choice");
    assert!(selected.policy_model_fingerprint.is_some());
    assert_eq!(serde_json::to_value(&selected).unwrap()["id"], original);
    let receipt = Receipt {
        generation: crate::private_files::nonce().unwrap(),
        batch: crate::private_files::nonce().unwrap(),
        source: source.summary_receipt(),
        title: "Named fixture".into(),
        mode: PassMode::Reevaluate,
        profile: Some(selected),
        source_approval: None,
        raw_only: false,
        automatic_title: false,
        title_approval: None,
        screenshots: false,
        automation_vision: None,
        full_output: false,
        evidence_limits: None,
    };
    let mut unbound = receipt.clone();
    unbound.profile.as_mut().unwrap().policy_model_fingerprint = None;
    assert!(
        workspace
            .journal_summaries
            .queue
            .enqueue([unbound])
            .is_err()
    );
    workspace
        .journal_summaries
        .queue
        .enqueue([receipt.clone()])
        .unwrap();
    let reopened = Queue::load(&scratch.0.canonicalize().unwrap()).unwrap();
    let saved = reopened.pending().front().unwrap();
    assert_eq!(saved, &receipt);

    let fallback: Configuration = serde_json::from_value(json!({
        "local": { "roles": [null, null, null, alternate] }
    }))
    .unwrap();
    let replacement = declaration(alternate, "");
    let installed = clipboard_policy::parse(&format!("{header}{replacement}")).unwrap();
    workspace.set_test_ai_configuration(fallback.clone().with_policy_profiles(&installed).unwrap());
    let current = workspace.ai_configuration().unwrap();
    assert!(workspace.policy_auto_model_pending(current.journal_summary_default_id().unwrap()));
    assert!(!workspace.journal_summary_model_pending(saved));
    assert!(
        workspace
            .journal_summary_profile(saved.profile.as_ref(), false)
            .is_none()
    );

    let restored = clipboard_policy::parse(&format!(
        "{header}{replacement}{}",
        declaration(original, "")
    ))
    .unwrap();
    workspace.set_test_ai_configuration(fallback.with_policy_profiles(&restored).unwrap());
    assert!(workspace.journal_summary_model_pending(saved));
}

#[test]
fn scheduled_journal_provider_approval_is_independent_and_excludes_earlier_runs() {
    use crate::ai_manager::Configuration;
    use crate::session_journal::{automation, tree::RunStatus};

    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, terminal, source) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let journals = source.directory().parent().unwrap().to_owned();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [
            {"id": 1, "name": "Terminal", "model": "terminal",
             "endpoint": format!("https://{address}/v1"),
             "connection_mode": "remote-open-ai", "trusted_remote_endpoint": true},
            {"id": 2, "name": "Scheduled", "model": "scheduled",
             "endpoint": format!("https://{address}/another"),
             "connection_mode": "anthropic", "trusted_remote_endpoint": true}
        ], "default": 1, "last_id": 2}
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let job = kortty_jobs::JobId::new("fixture").unwrap();
    let action: crate::scheduler_window::SshCommand = serde_json::from_value(json!({
        "host": "fixture.invalid", "port": 22, "username": "operator",
        "key_file": root.join("unused-key"), "command": "exit 0"
    }))
    .unwrap();
    let graph = kortty_jobs::JobGraph::new(vec![
        kortty_jobs::Job::new(job.clone(), "Scheduled fixture", action).unwrap(),
    ])
    .unwrap();
    let database = root.join("jobs.json");
    kortty_jobs::SchedulerStore::open(&database)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    workspace.scheduler = Some(crate::scheduler_window::SchedulerWindow::open(database).unwrap());
    workspace
        .window_settings
        .save(&root.join("window-settings.json"))
        .unwrap();
    let manager = show_manager(&mut workspace);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let Some(Review::Settings {
        automation_choices, ..
    }) = workspace.journal_summaries.reviews.get(&manager)
    else {
        panic!("expected summary settings");
    };
    let scheduled = automation_choices
        .iter()
        .find(|choice| {
            matches!(choice, AutomationChoice::Profile(choice)
            if choice.profile.as_ref().is_some_and(|profile| profile.id == ProfileRef::Numeric(2)))
        })
        .unwrap()
        .clone();
    let _ = summary(
        &mut workspace,
        manager,
        Event::PickAutomation(Box::new(scheduled)),
    );
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let queue = Queue::load(&root).unwrap();
    assert!(!queue.preferences().enabled);
    assert!(queue.preferences().reviewed_grant().is_none());
    let grant = queue.preferences().automation_grant().unwrap();
    assert_eq!(grant.profile.id, ProfileRef::Numeric(2));

    let record = |started, key: &str, profile, approval: Option<SourceApproval>, unresolved| {
        let run = automation::Run::new(
            automation::Kind::Job,
            "fixture".into(),
            "Scheduled fixture".into(),
            "COMMAND",
            started,
            automation::Options {
                enabled: true,
                closing: automation::Closing::Always,
                summary_profile: profile,
                unresolved_summary_profile: unresolved,
                ..Default::default()
            },
            automation::Caps::default(),
        )
        .unwrap()
        .with_summary_approval(approval);
        let recorder = automation::Recorder::start(
            &automation::CaptureSettings {
                root: journals.clone(),
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Default::default(),
            },
            run,
            automation::Destination {
                key: key.into(),
                label: key.into(),
                host: "fixture.invalid".into(),
                port: Some(22),
                username: "fixture".into(),
            },
            Arc::new(Redactor::for_journal(
                Secrets::default().seal().unwrap(),
                Default::default(),
            )),
        )
        .unwrap();
        recorder.output("completed", false);
        let artifact = recorder.finish(RunStatus::Success, Some(0));
        assert!(artifact.complete);
        artifact.source.id().to_owned()
    };
    let choices = workspace.automation_summary_providers();
    let choice = |id| {
        choices
            .iter()
            .find_map(|choice| {
                choice
                    .0
                    .as_ref()
                    .filter(|profile| profile.id == ProfileRef::Numeric(id))
                    .cloned()
            })
            .unwrap()
    };
    let different = choice(1);
    let local = choice(0);
    let source_key = AutomationSource::Job(job);
    let source_choice = workspace
        .automation_summary_sources()
        .into_iter()
        .find(|choice| choice.source == source_key)
        .unwrap();
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(
        &mut workspace,
        manager,
        Event::PickAutomationSource(source_choice.clone()),
    );
    let Some(Review::Settings { choices, .. }) = workspace.journal_summaries.reviews.get(&manager)
    else {
        panic!("expected summary settings");
    };
    let provider = choices
        .iter()
        .find(|choice| choice.profile.as_ref() == Some(&different))
        .unwrap()
        .clone();
    let _ = summary(
        &mut workspace,
        manager,
        Event::PickSourceProfile(Box::new(provider)),
    );
    let _ = summary(&mut workspace, manager, Event::SourceFullOutput(true));
    let _ = summary(&mut workspace, manager, Event::ApproveAutomationSource);
    let _ = summary(&mut workspace, manager, Event::Dismiss);
    let updated = Queue::load(&root).unwrap();
    let job_grant = updated.preferences().source_approval(&source_key).unwrap();
    assert_eq!(job_grant.grant.profile, different);
    assert!(job_grant.full_output && !updated.preferences().full_output);
    let old = record(
        Timestamp::from_second(grant.since.as_second() - 1).unwrap(),
        "old",
        Some(grant.profile.clone()),
        None,
        None,
    );
    let new = record(Timestamp::now(), "new", None, None, None);
    let blocked = record(
        Timestamp::now(),
        "blocked",
        Some(different.clone()),
        None,
        None,
    );
    let local_id = record(Timestamp::now(), "local", Some(local.clone()), None, None);
    let unresolved = record(
        Timestamp::now(),
        "imported",
        None,
        None,
        Some("java-fixture".into()),
    );
    let started = Timestamp::now();
    let approval = workspace
        .automation_summary_run_approval(
            &source_key,
            &automation::Options {
                enabled: true,
                closing: automation::Closing::Always,
                summary_profile: Some(different.clone()),
                ..Default::default()
            },
            started,
        )
        .unwrap();
    let approved = record(
        started,
        "approved",
        Some(different.clone()),
        Some(approval),
        None,
    );
    let report = automation::retention::Sweep {
        root: &journals,
        caps: automation::Caps::default(),
        user_delete: true,
        summarize: true,
        cancelled_closing: &[],
        protected: &[],
        completed: &[],
        recover: false,
        now: Timestamp::now(),
        cancelled: &AtomicBool::new(false),
    }
    .run()
    .unwrap();
    let jobs = [old, new, blocked, local_id, unresolved, approved].map(|id| {
        report
            .closing
            .iter()
            .find(|entry| entry.journal.id == id)
            .unwrap()
    });
    assert_eq!(jobs[2].profile.as_ref(), Some(&different));
    assert_eq!(jobs[3].profile.as_ref(), Some(&local));
    assert!(jobs[4].unresolved_profile);
    assert!(jobs[5].source_approval.is_some());
    assert!(
        workspace
            .automation_summary_estimate_profile(None, Some(&different))
            .is_none()
    );
    assert!(
        workspace
            .automation_summary_estimate_profile(None, Some(&local))
            .is_some()
    );
    workspace.journal_summaries.startup_recovery = Some((journals, Default::default()));
    let mut jobs = jobs.map(Clone::clone);
    jobs[2].source_approval = jobs[5].source_approval.clone();
    let _ = workspace.enqueue_automation_closing_summaries(&jobs);
    let queue = Queue::load(&root).unwrap();
    assert!(queue.pending()[0].raw_only);
    let scheduled = &queue.pending()[1];
    assert_eq!(scheduled.profile.as_ref(), Some(&grant.profile));
    assert!(queue.authorizes_reviewed(scheduled));
    assert!(queue.pending()[2].raw_only);
    assert_eq!(queue.pending()[3].profile.as_ref(), Some(&local));
    assert!(queue.pending()[4].raw_only);
    let scoped = queue.pending()[5].clone();
    assert!(scoped.full_output);
    assert_eq!(scoped.profile.as_ref(), Some(&different));
    assert!(queue.authorizes_reviewed(&scoped));
    let configuration = workspace.ai_configuration().unwrap();
    let profile = workspace
        .journal_summary_profile(scheduled.profile.as_ref(), false)
        .unwrap();
    let authority =
        https::Authority::new(Arc::clone(&configuration), scheduled.clone(), profile, None);
    authority.authorize().unwrap();
    let scoped_profile = workspace
        .journal_summary_profile(scoped.profile.as_ref(), false)
        .unwrap();
    let scoped_authority = https::Authority::new(configuration, scoped, scoped_profile, None);
    scoped_authority.authorize().unwrap();
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(
        &mut workspace,
        manager,
        Event::PickAutomation(Box::new(AutomationChoice::Local)),
    );
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert!(authority.authorize().is_err());
    scoped_authority.authorize().unwrap();
    let queue = Queue::load(&root).unwrap();
    assert_eq!(
        queue.preferences().automation_profile,
        Some(AutomationProfile::Local)
    );
    assert_eq!(queue.pending().len(), 5);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(
        &mut workspace,
        manager,
        Event::PickAutomationSource(source_choice),
    );
    let _ = summary(&mut workspace, manager, Event::RevokeAutomationSource);
    assert!(scoped_authority.authorize().is_err());
    assert_eq!(Queue::load(&root).unwrap().pending().len(), 4);
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    finish_capture(&terminal);
}

#[test]
fn journal_summary_https_approval_is_persisted_and_revoked_by_scope() {
    check_summary_https_approval(Some("fixture-model"), false);
}

#[test]
fn journal_summary_https_approval_binds_server_selected_model_mode() {
    check_summary_https_approval(None, false);
}

#[test]
fn journal_summary_loopback_tls_approval_requires_a_trusted_endpoint() {
    check_summary_https_approval(None, true);
}

fn check_summary_https_approval(model: Option<&str>, loopback: bool) {
    use crate::ai_manager::Configuration;

    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, old_capture, source) = seeded(address);
    let profile_root = scratch.0.canonicalize().unwrap();
    let journal_root = source.directory().parent().unwrap().to_owned();
    let configured = |model: Option<&str>, trusted: bool| -> Configuration {
        serde_json::from_value(json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "HTTPS fixture", "model": model.unwrap_or_default(),
                    "model_mode": if model.is_some() { "manual" } else { "server-default" },
                    "endpoint": format!("https://{address}/v1"),
                    "connection_mode": if loopback { "lm-studio-compatible" } else { "remote-open-ai" },
                    "trusted_remote_endpoint": trusted}],
                "default": model.map(|_| 1), "last_id": 1
            }
        }))
        .unwrap()
    };
    assert!(
        configured(model, false)
            .journal_summary_https_transport(1)
            .is_err()
    );
    configured(model, true)
        .save_fixture_at(&profile_root, &Configuration::default())
        .unwrap();
    let original = Configuration::load_fixture_from(&profile_root).unwrap();
    workspace.set_test_ai_configuration(original.clone());
    workspace
        .window_settings
        .save(&profile_root.join("window-settings.json"))
        .unwrap();
    let manager = show_manager(&mut workspace);
    let choices = workspace.journal_summary_choices();
    let remote = choices
        .into_iter()
        .find(|choice| {
            choice
                .reviewed()
                .is_some_and(|profile| profile.https.is_some())
        })
        .unwrap();
    assert!(
        remote
            .label
            .contains(&format!("https://{address}/v1/chat/completions"))
    );
    assert_eq!(
        remote.label.contains("Server selects the model"),
        model.is_none()
    );
    assert!(
        workspace
            .journal_summaries
            .queue
            .preferences()
            .https
            .is_none()
    );

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::Pick(Box::new(remote)));
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    let _ = summary(&mut workspace, manager, Event::FullOutput(true));
    let _ = summary(&mut workspace, manager, Event::AutomaticTitle(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);

    let new_capture = Arc::new(Capture::start(journal_root.clone(), Default::default()).unwrap());
    let register = |workspace: &mut Workspace, capture: &Arc<Capture>| {
        workspace.register_journal_summaries(
            Arc::clone(capture),
            ConnectionConfig {
                ai_summaries: true,
                ..Default::default()
            },
            Arc::new(Redactor::for_journal(
                Secrets::default().seal().unwrap(),
                Default::default(),
            )),
            "HTTPS approval fixture".into(),
        );
    };
    for capture in [&old_capture, &new_capture] {
        register(&mut workspace, capture);
    }
    let reopened = Queue::load(&profile_root).unwrap();
    assert!(!reopened.pending()[0].reviewed());
    let automatic = reopened.pending()[1].clone();
    assert!(automatic.full_output);
    assert!(reopened.authorizes_reviewed(&automatic));
    assert!(reopened.authorizes_title(&reopened.pending()[0]));
    assert!(!reopened.authorizes_title(&automatic));

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::RecordingTitleRequest(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let approved_capture = Arc::new(Capture::start(journal_root, Default::default()).unwrap());
    register(&mut workspace, &approved_capture);
    let reopened = Queue::load(&profile_root).unwrap();
    let approved = reopened.pending()[2].clone();
    assert!(reopened.authorizes_reviewed(&automatic));
    assert!(!reopened.authorizes_title(&automatic));
    assert!(reopened.authorizes_title(&approved));

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::RecordingTitleRequest(false));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let reopened = Queue::load(&profile_root).unwrap();
    assert!(reopened.authorizes_reviewed(&approved));
    assert!(!reopened.authorizes_title(&approved));

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::RecordingTitleRequest(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let reopened = Queue::load(&profile_root).unwrap();
    assert!(reopened.authorizes_reviewed(&approved));
    assert!(!reopened.authorizes_title(&approved));
    let configuration = workspace.ai_configuration().unwrap();
    let transport = workspace
        .journal_summary_profile(automatic.profile.as_ref(), false)
        .unwrap();
    assert_eq!(transport.uses_server_default_model(), model.is_none());
    let authority = https::Authority::new(
        Arc::clone(&configuration),
        automatic.clone(),
        transport.clone(),
        None,
    );
    authority.authorize().unwrap();

    configured(Some("changed-model"), true)
        .save_fixture_at(&profile_root, &original)
        .unwrap();
    assert!(authority.authorize().is_err());
    let changed = Configuration::load_fixture_from(&profile_root).unwrap();
    original.save_fixture_at(&profile_root, &changed).unwrap();
    authority.authorize().unwrap();

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::FullOutput(false));
    authority.authorize().unwrap();
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert!(authority.authorize().is_err());

    let _ = summary(&mut workspace, manager, Event::Settings);
    let Some(Review::Settings { choices, .. }) = workspace.journal_summaries.reviews.get(&manager)
    else {
        panic!("expected automatic summary settings");
    };
    let local = choices[0].clone();
    let _ = summary(&mut workspace, manager, Event::Pick(Box::new(local)));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert!(authority.authorize().is_err());
    assert_eq!(Queue::load(&profile_root).unwrap().pending().len(), 1);
    let credential = authority.credential();
    let (run, _cancel) =
        Run::begin_reviewed_https(transport, credential, move || authority.authorize()).unwrap();
    assert_eq!(
        run.preflight().unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    finish_capture(&old_capture);
    finish_capture(&new_capture);
    finish_capture(&approved_capture);
}

#[test]
fn journal_summary_cli_runs_reviewed_manual_and_automatic_passes() {
    use crate::ai_manager::Configuration;
    use crate::local_ai::cli::{Program, fixture_arguments, fixture_executable};
    use crate::session_security::IsolationLevel;

    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (scratch, mut workspace, manual, source) = seeded(endpoint.local_addr().unwrap());
    let root = scratch.0.canonicalize().unwrap();
    let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "local": {"roles": [null, null, null, 7]},
        "profiles": {
            "profiles": [{"id": 7, "name": "Journal CLI fixture", "connection_mode": "cli",
                "endpoint": "", "vision": "disabled", "cli": {
                    "path": executable, "arguments": fixture_arguments("fixture_journal_summary"),
                    "fingerprint": fingerprint
                }}],
            "default": 7, "last_id": 7
        }
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    workspace
        .window_settings
        .save(&root.join("window-settings.json"))
        .unwrap();

    finish_capture(&manual);
    let path = source.directory().join("journal.xml");
    let xml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        xml.replace(
            "<appLanguageCode>en</appLanguageCode>",
            "<appLanguageCode>de-private-token</appLanguageCode>",
        ),
    )
    .unwrap();
    let manager = show_manager(&mut workspace);
    review(&mut workspace, manager, &source);
    let cli = workspace.journal_summary_cli_choices().pop().unwrap();
    assert!(matches!(workspace.journal_summaries.reviews.get(&manager),
        Some(Review::Run { selected, .. }) if selected == &cli));
    assert!(!root.join("fixture-journal-probe").exists());
    workspace.clipboard.policy.minimum_isolation = IsolationLevel::Process;
    let refused = summary(&mut workspace, manager, Event::Start);
    assert!(iced_runtime::task::into_stream(refused).is_none());
    assert!(!root.join("fixture-journal-probe").exists());

    workspace.clipboard.policy.minimum_isolation = IsolationLevel::None;
    review(&mut workspace, manager, &source);
    let pass = summary(&mut workspace, manager, Event::Start);
    finish_pass(
        &mut workspace,
        &mut iced_runtime::task::into_stream(pass).unwrap(),
    );
    let reviewed = source.read(None).unwrap().unwrap();
    for kind in ["AI_SUMMARY", "SESSION_SUMMARY"] {
        assert!(
            reviewed
                .timeline
                .iter()
                .any(|entry| entry.kind == kind && entry.title == "Checked build")
        );
    }
    let probe = std::fs::read_to_string(root.join("fixture-journal-probe")).unwrap();
    let prompt = std::fs::read_to_string(root.join("fixture-journal-window")).unwrap();
    let closing = std::fs::read_to_string(root.join("fixture-journal-close")).unwrap();
    assert!(!probe.contains("build.sh"));
    assert!(!probe.contains("language code"));
    assert!(prompt.contains("build.sh"));
    assert!(!prompt.contains("fixture-private"));
    for request in [prompt, closing] {
        assert!(request.contains("Answer in language code de."));
        assert!(!request.contains("private-token"));
    }
    std::fs::remove_file(root.join("fixture-journal-close")).unwrap();
    assert!(!root.join("fixture-journal-title").exists());

    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let _ = summary(&mut workspace, manager, Event::Settings);
    assert!(matches!(workspace.journal_summaries.reviews.get(&manager),
        Some(Review::Settings { selected, .. }) if selected.profile.is_none()));
    assert!(workspace.journal_summary_profile(None, false).is_some());
    let _ = summary(&mut workspace, manager, Event::Dismiss);
    workspace
        .journal_summaries
        .queue
        .set_preferences(Default::default())
        .unwrap();
    let _ = summary(&mut workspace, manager, Event::Settings);
    assert!(matches!(workspace.journal_summaries.reviews.get(&manager),
        Some(Review::Settings { selected, .. }) if selected == &cli));
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .reviewed_grant()
            .is_none()
    );
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .reviewed_grant()
            .is_none()
    );
    let _ = summary(&mut workspace, manager, Event::AutomaticTitle(true));
    let _ = summary(&mut workspace, manager, Event::RecordingTitleRequest(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let automatic = Arc::new(
        Capture::start(
            source.directory().parent().unwrap().to_owned(),
            session_journal::Metadata {
                title: "Automatic CLI fixture".into(),
                connection_name: "Automatic CLI fixture".into(),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    for (kind, evidence) in [
        (RecordKind::Input, "build.sh"),
        (RecordKind::Output, "build.sh exited 2"),
    ] {
        automatic
            .sink()
            .try_write(kind, Zeroizing::new(evidence.into()), false)
            .unwrap();
    }
    workspace.register_journal_summaries(
        Arc::clone(&automatic),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Automatic CLI fixture".into(),
    );
    let queue = Queue::load(&root).unwrap();
    assert!(queue.pending()[0].cli());
    assert!(queue.authorizes_reviewed(&queue.pending()[0]));
    assert!(queue.authorizes_title(&queue.pending()[0]));
    finish_capture(&automatic);
    let pass = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    finish_pass(
        &mut workspace,
        &mut iced_runtime::task::into_stream(pass).unwrap(),
    );
    assert!(root.join("fixture-journal-close").is_file());
    let title = std::fs::read_to_string(root.join("fixture-journal-title")).unwrap();
    assert!(title.contains("Automatic CLI fixture"));
    assert!(title.contains("Checked build"));
    assert_eq!(
        Source::capture(&automatic)
            .read(None)
            .unwrap()
            .unwrap()
            .summary
            .title,
        "Build diagnostics"
    );
    assert!(Queue::load(&root).unwrap().pending().is_empty());
}
