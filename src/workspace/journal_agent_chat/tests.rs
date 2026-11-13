use iced::futures::{StreamExt as _, executor::block_on};

use super::*;
use crate::workspace::terminal_ai::{Turn, agent_follow_up::Step};
use crate::{ai_manager::Configuration, session_journal::Metadata, snippets::tests::Profile};

#[test]
fn agent_journal_card_links_exact_saved_chat_and_keeps_a_frozen_copy_after_rename() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let journal_root = root.join("journal-fixture");
    let capture = Arc::new(Capture::start(journal_root, Metadata::default()).unwrap());
    let run = crate::private_files::nonce().unwrap();
    let target = AgentJournal {
        capture: Arc::downgrade(&capture),
        source: Source::capture(&capture),
        card: Card {
            id: format!("agent-{run}"),
            title: "Inspect worker".into(),
            answer: "Check its environment.".into(),
            model: "Fixture".into(),
            duration_ms: Some(1250),
            tokens: Some(0),
        },
    };
    let permit = Arc::new(AtomicBool::new(true));
    target.ensure_card(&permit).unwrap();
    let _ = capture.finish().recv_blocking();
    assert_eq!(capture.status(), Status::Complete);

    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let turn = Turn::from_agent(
        "Inspect worker",
        "Check its environment.",
        &[Step {
            kind: StepKind::Command,
            title: "Probe".into(),
            detail: "Read configuration".into(),
            command: Some("printf fixture".into()),
            output: vec!["API_KEY=fixture-secret".into()],
            exit_code: Some(7),
            status: Some(StepStatus::Completed),
            elapsed_seconds: Some(1),
            truncated: None,
        }],
        &redactor,
    )
    .unwrap();
    let record = store::open_or_seed_agent_linked(
        &root,
        &run,
        "Inspect worker".into(),
        store::Draft {
            profile: crate::ai_manager::ProfileRef::Numeric(1),
            profile_name: "Fixture".into(),
            connection: String::new(),
            response_language: None,
            turns: vec![turn.clone()],
            attachment: None,
        },
        Some(target.reference()),
        || Ok(()),
    )
    .unwrap();

    permit.store(false, Ordering::Release);
    assert!(target.link(&record, &redactor, &permit).is_err());
    let card_before = target
        .source
        .read(None)
        .unwrap()
        .unwrap()
        .timeline
        .into_iter()
        .find(|card| card.id == target.card.id)
        .unwrap();
    assert!(!card_before.has_agent_chat());
    permit.store(true, Ordering::Release);
    target.link(&record, &redactor, &permit).unwrap();
    let card = target
        .source
        .read(None)
        .unwrap()
        .unwrap()
        .timeline
        .into_iter()
        .find(|card| card.id == target.card.id)
        .unwrap();
    let linked = agent_chat::read_linked_chat(&target.source, &card.id).unwrap();
    assert_eq!(
        linked.chat_id.as_deref(),
        Some(record.summary.key.id.as_str())
    );
    assert!(card.has_agent_chat());
    let transcript = card.chat_details.join("\n");
    for expected in ["(final)", "Exit 7", "$ printf fixture", "API_KEY=***"] {
        assert!(transcript.contains(expected), "missing {expected}");
    }
    assert!(!transcript.contains("fixture-secret"));

    let updated = store::save(
        &root,
        Some(&record.summary.key),
        "Renamed worker".into(),
        store::Draft {
            profile: crate::ai_manager::ProfileRef::Numeric(1),
            profile_name: "Fixture".into(),
            connection: String::new(),
            response_language: None,
            turns: vec![
                turn,
                Turn::from_parts(None, "Later question".into(), "Later answer".into()),
            ],
            attachment: None,
        },
    )
    .unwrap();
    let updated = store::load(&root, &updated.key).unwrap();
    target.link(&updated, &redactor, &permit).unwrap();
    let frozen = target
        .source
        .read(None)
        .unwrap()
        .unwrap()
        .timeline
        .into_iter()
        .find(|entry| entry.id == card.id)
        .unwrap();
    assert_eq!(frozen.chat_details, card.chat_details);

    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"test"}], "default":1,"last_id":1
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let task =
        workspace.start_journal_agent_chat(owner, target.source.clone(), card.id.clone(), None);
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    while let Some(action) = block_on(actions.next()) {
        if let iced_runtime::Action::Output(message) = action {
            let _ = workspace.update(message);
        }
    }
    assert_eq!(workspace.terminal_chats.len(), 1);
    assert!(workspace.journal_chat_transcripts.is_empty());
}
