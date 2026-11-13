use crate::session_journal::agent_chat::{Card, Message as ChatMessage, Role, Snapshot, Step};

use super::*;

fn card(id: &str) -> Card {
    Card {
        id: id.into(),
        title: "Reviewed task".into(),
        answer: "Task complete".into(),
        model: "Fixture model".into(),
        duration_ms: Some(1200),
        tokens: Some(0),
    }
}

fn snapshot(second: i64) -> Snapshot {
    Snapshot {
        chat_id: "chat-1".into(),
        title: "Task follow-up".into(),
        captured_at: Timestamp::from_second(1_800_000_000 + second).unwrap(),
        frozen: false,
        messages: vec![
            ChatMessage {
                role: Role::User,
                content: "Check the service".into(),
                profile: String::new(),
                steps: Vec::new(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "It is running".into(),
                profile: "Fixture model".into(),
                steps: vec![Step {
                    kind: "ACTION",
                    status: Some("COMPLETED"),
                    title: "Read status".into(),
                    summary: "Inspected status".into(),
                    command: Some("systemctl status example".into()),
                    exit_code: Some(0),
                    output: "active".into(),
                    elapsed_seconds: Some(1),
                    truncated: Some(false),
                }],
            },
        ],
    }
}

fn xml(capture: &Capture) -> String {
    fs::read_to_string(capture.directory().join("journal.xml")).unwrap()
}

#[test]
fn live_agent_card_requires_the_current_handoff_permit() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let permit = Arc::new(AtomicBool::new(false));
    let revision = capture.published().1;
    let rejected = capture
        .ensure_agent_card_checked(card("revoked"), Some(Arc::clone(&permit)))
        .unwrap();
    assert!(wait_edit(rejected).is_err());
    assert_eq!(capture.published().1, revision);
    assert!(!xml(&capture).contains("revoked"));
    permit.store(true, Ordering::Release);
    let accepted = capture
        .ensure_agent_card_checked(card("current"), Some(permit))
        .unwrap();
    wait_edit(accepted).unwrap();
    wait(capture.finish());
    assert_eq!(read(capture.directory()).unwrap().timeline[0].id, "current");
}

#[test]
fn live_agent_card_is_idempotent_and_chat_updates_preserve_edits_markers_and_freeze_on_close() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    let revision = capture.published().1;
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    assert_eq!(capture.published().1, revision);
    let source = viewer::Source::capture(&capture);
    let draft = Draft::read(&source, "run-1").unwrap();
    let marker = draft
        .markers()
        .iter()
        .find(|marker| marker.legacy() == LegacyMarker::Error)
        .unwrap()
        .clone();
    let fields = Fields {
        title: "User title".into(),
        text: "User correction".into(),
        note: "Retain this note".into(),
    };
    wait_edit(
        capture
            .edit_entry(draft.edit(Changes::Fields { fields, marker }).unwrap())
            .unwrap(),
    )
    .unwrap();
    let edited = read(capture.directory()).unwrap().timeline[0].edited_at;
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(1))
            .unwrap(),
    )
    .unwrap();
    let original = xml(&capture);
    assert!(original.contains("<agentTokens>0</agentTokens>"));
    let linked = format!("<chatLinkedAt>{}</chatLinkedAt>", snapshot(1).captured_at);
    assert!(original.contains(&linked));
    assert!(original.contains("<frozen>false</frozen>"));
    let revision = capture.published().1;
    wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(1))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(capture.published().1, revision);
    let mut newer = snapshot(2);
    newer.messages.push(ChatMessage {
        role: Role::User,
        content: "What next?".into(),
        profile: String::new(),
        steps: Vec::new(),
    });
    wait_edit(capture.link_agent_chat("run-1".into(), newer).unwrap()).unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Subsequent capture".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    let document = read(capture.directory()).unwrap();
    let entry = document
        .timeline
        .iter()
        .find(|entry| entry.id == "run-1")
        .unwrap();
    assert_eq!(entry.title, "User title");
    assert_eq!(entry.text, "User correction");
    assert_eq!(entry.note, "Retain this note");
    assert_eq!(entry.marker, "ERROR");
    assert_eq!(entry.edited_at, edited);
    assert!(
        entry
            .chat_details
            .iter()
            .any(|line| line.contains("(final)"))
    );
    assert!(
        entry
            .chat_details
            .iter()
            .any(|line| line.contains("What next?"))
    );
    let finished = xml(&capture);
    assert!(finished.contains(&linked));
    assert!(finished.contains("<frozen>true</frozen>"));
    assert_eq!(
        document
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AGENT")
            .count(),
        1
    );
    assert!(
        capture
            .link_agent_chat("run-1".into(), snapshot(3))
            .is_err()
    );
}

#[test]
fn live_agent_chat_rejects_wrong_entries_chats_and_stale_or_frozen_copies() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Manual entry".into()),
            false,
        )
        .unwrap();
    wait_timeline(&capture, 1);
    let note = read(capture.directory()).unwrap().timeline[0].id.clone();
    assert!(wait_edit(capture.ensure_agent_card(card(&note)).unwrap()).is_err());
    assert!(wait_edit(capture.link_agent_chat(note, snapshot(1)).unwrap()).is_err());
    assert!(
        wait_edit(
            capture
                .link_agent_chat("missing".into(), snapshot(1))
                .unwrap()
        )
        .is_err()
    );
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(2))
            .unwrap(),
    )
    .unwrap();
    let retained = xml(&capture);
    let mut foreign = snapshot(3);
    foreign.chat_id = "another-chat".into();
    for rejected in [foreign, snapshot(1)] {
        assert!(wait_edit(capture.link_agent_chat("run-1".into(), rejected).unwrap()).is_err());
        assert_eq!(xml(&capture), retained);
    }
    let mut frozen = snapshot(3);
    frozen.frozen = true;
    wait_edit(capture.link_agent_chat("run-1".into(), frozen).unwrap()).unwrap();
    let retained = xml(&capture);
    assert!(
        wait_edit(
            capture
                .link_agent_chat("run-1".into(), snapshot(4))
                .unwrap()
        )
        .is_err()
    );
    assert_eq!(xml(&capture), retained);
    assert_eq!(capture.status(), Status::Recording);
    wait(capture.finish());
}

#[test]
fn live_agent_publication_rolls_back_before_replace_and_retains_uncertain_commits() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let staged = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&staged).unwrap();
    assert!(wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).is_err());
    assert!(read(capture.directory()).unwrap().timeline.is_empty());
    fs::remove_dir(&staged).unwrap();
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let warning = wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap_err();
    assert!(warning.contains("was saved, but its durability could not be confirmed"));
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    let unlinked = xml(&capture);
    fs::create_dir(&staged).unwrap();
    assert!(
        wait_edit(
            capture
                .link_agent_chat("run-1".into(), snapshot(1))
                .unwrap()
        )
        .is_err()
    );
    assert_eq!(xml(&capture), unlinked);
    fs::remove_dir(&staged).unwrap();
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let warning = wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(1))
            .unwrap(),
    )
    .unwrap_err();
    assert!(warning.contains("was saved, but its durability could not be confirmed"));
    wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(1))
            .unwrap(),
    )
    .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    let result = xml(&capture);
    assert!(result.contains("<chatId>chat-1</chatId>"));
    assert!(result.contains("<frozen>true</frozen>"));
    assert_eq!(read(capture.directory()).unwrap().timeline.len(), 1);
}

#[test]
fn live_agent_chat_aggregate_cap_counts_xml_escaping_without_stopping_capture() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let mut large = snapshot(1);
    large.messages = (0..5)
        .map(|_| ChatMessage {
            role: Role::Assistant,
            content: "&".repeat(24 * 1024),
            profile: String::new(),
            steps: Vec::new(),
        })
        .collect();
    large.validate().unwrap();
    for index in 0..4 {
        let id = format!("run-{index}");
        wait_edit(capture.ensure_agent_card(card(&id)).unwrap()).unwrap();
        let result = wait_edit(capture.link_agent_chat(id, large.clone()).unwrap());
        assert_eq!(result.is_ok(), index < 3);
    }
    wait_edit(
        capture
            .link_agent_chat("run-3".into(), snapshot(2))
            .unwrap(),
    )
    .unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Still recording".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    assert!(xml(&capture).contains("Still recording"));
}

#[test]
fn live_agent_chat_revocation_while_queued_prevents_publication_and_releases_the_mutation_slot() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    wait_edit(capture.ensure_agent_card(card("run-1")).unwrap()).unwrap();
    let input = capture
        .sink()
        .reserve_input(Zeroizing::new("FIFO test".into()))
        .unwrap();
    let permit = Arc::new(AtomicBool::new(true));
    let pending = capture
        .link_agent_chat_checked("run-1".into(), snapshot(1), Arc::clone(&permit))
        .unwrap();
    let blocked = capture
        .link_agent_chat("run-1".into(), snapshot(2))
        .unwrap_err();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    let appended = capture.ensure_agent_card(card("run-2")).unwrap();
    permit.store(false, Ordering::Release);
    input.send(()).unwrap();
    assert!(
        wait_edit(pending)
            .unwrap_err()
            .contains("handoff was revoked")
    );
    wait_edit(appended).unwrap();
    assert!(!xml(&capture).contains("<chatSnapshot>"));
    assert!(xml(&capture).contains("run-2"));
    assert_eq!(capture.status(), Status::Recording);
    wait_edit(
        capture
            .link_agent_chat("run-1".into(), snapshot(2))
            .unwrap(),
    )
    .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    assert!(xml(&capture).contains("<frozen>true</frozen>"));
}
