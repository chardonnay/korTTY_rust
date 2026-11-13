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
        automatic_title: false,
        full_output: false,
        screenshots: false,
    };
    state.queue.enqueue([receipt.clone()]).unwrap();
    state.live.push(Live {
        capture: Arc::clone(&capture),
        receipt,
        interval: Duration::from_secs(30),
        next: Instant::now(),
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
    let server = thread::spawn(move || {
        let mut stream = send_prefix(&endpoint);
        wait.recv_timeout(Duration::from_secs(4)).unwrap();
        stream.get_mut().write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
        drop(stream);
        let mut closing = accept(&endpoint);
        let body = request(&mut closing);
        assert_eq!(body["stream"], true);
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
        automatic_title: false,
        screenshots: false,
        full_output: false,
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
        automatic_title: false,
        screenshots: false,
        full_output: false,
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
fn journal_summary_https_approval_is_persisted_and_revoked_by_scope() {
    use crate::ai_manager::Configuration;

    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, old_capture, source) = seeded(address);
    let profile_root = scratch.0.canonicalize().unwrap();
    let journal_root = source.directory().parent().unwrap().to_owned();
    let configured = |model| -> Configuration {
        serde_json::from_value(json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "HTTPS fixture", "model": model,
                    "endpoint": format!("https://{address}/v1"),
                    "connection_mode": "remote-open-ai", "trusted_remote_endpoint": true}],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap()
    };
    configured("fixture-model")
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
    let _ = summary(&mut workspace, manager, Event::SaveSettings);

    let new_capture = Arc::new(Capture::start(journal_root.clone(), Default::default()).unwrap());
    for capture in [&old_capture, &new_capture] {
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
    }
    let reopened = Queue::load(&profile_root).unwrap();
    assert!(!reopened.pending()[0].reviewed());
    let automatic = reopened.pending()[1].clone();
    assert!(automatic.full_output);
    assert!(reopened.authorizes_reviewed(&automatic));
    let configuration = workspace.ai_configuration().unwrap();
    let transport = workspace
        .journal_summary_profile(automatic.profile.as_ref(), false)
        .unwrap();
    let authority = https::Authority::new(
        Arc::clone(&configuration),
        automatic.clone(),
        transport.clone(),
        None,
    );
    authority.authorize().unwrap();

    configured("changed-model")
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
    let (run, _cancel) =
        Run::begin_reviewed_https(transport, move || authority.authorize()).unwrap();
    assert_eq!(
        run.preflight().unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    finish_capture(&old_capture);
    finish_capture(&new_capture);
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
    assert!(!probe.contains("build.sh"));
    assert!(prompt.contains("build.sh"));
    assert!(!prompt.contains("fixture-private"));
    std::fs::remove_file(root.join("fixture-journal-close")).unwrap();

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
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let automatic = Arc::new(
        Capture::start(
            source.directory().parent().unwrap().to_owned(),
            Default::default(),
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
    finish_capture(&automatic);
    let pass = workspace.update(Desktop::JournalSummary(Worker::Pulse));
    finish_pass(
        &mut workspace,
        &mut iced_runtime::task::into_stream(pass).unwrap(),
    );
    assert!(root.join("fixture-journal-close").is_file());
    assert!(Queue::load(&root).unwrap().pending().is_empty());
}
