use iced::futures::{StreamExt as _, executor::block_on};
use zeroize::Zeroizing;

use crate::workspace::terminal_ai::Turn;
use crate::{ai_manager::Configuration, session_journal::Metadata, snippets::tests::Profile};

use super::*;

struct Fixture {
    workspace: Workspace,
    capture: Arc<Capture>,
    record: store::Record,
    root: PathBuf,
    _profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let capture = Arc::new(Capture::start(root.join("journals"), Metadata::default()).unwrap());
        capture
            .ensure_agent_card(Card {
                id: "agent-entry".into(),
                title: "Task".into(),
                answer: "Answer".into(),
                model: "Fixture".into(),
                duration_ms: None,
                tokens: None,
            })
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap();
        let run = crate::private_files::nonce().unwrap();
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        let turn = Turn::from_agent("Task", "Answer", &[], &redactor).unwrap();
        let record = store::open_or_seed_agent_linked(
            &root,
            &run,
            "Task".into(),
            store::Draft {
                profile: crate::ai_manager::ProfileRef::Numeric(1),
                profile_name: "Fixture".into(),
                connection: String::new(),
                response_language: None,
                turns: vec![turn],
                attachment: None,
            },
            Some(store::AgentJournalRef {
                journal_id: capture.id().into(),
                entry_id: "agent-entry".into(),
            }),
            || Ok(()),
        )
        .unwrap();
        let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
            "profiles":[{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"test"}],"default":1,"last_id":1
        }})).unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        workspace
            .retired_journal_captures
            .push(Arc::clone(&capture));
        Self {
            workspace,
            capture,
            record,
            root,
            _profile: profile,
        }
    }

    fn begin(&mut self) -> (window::Id, Arc<AtomicBool>, Task<Desktop>) {
        let task = self
            .workspace
            .synchronize_agent_journal_chat(&self.record.summary.key);
        let (&ticket, work) = self
            .workspace
            .journal_chat_sync
            .pending
            .iter()
            .find(|(_, work)| work.targets[0].permit.load(Ordering::Acquire))
            .unwrap();
        (ticket, Arc::clone(&work.targets[0].permit), task)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.capture.finish().recv_blocking();
    }
}

#[test]
fn registered_journal_sync_revokes_queued_publication_for_quit_policy_config_and_source_changes() {
    for change in 0..6 {
        let mut fixture = Fixture::new();
        let (_, permit, _task) = fixture.begin();
        fixture
            .workspace
            .revalidate_agent_journal_synchronizations();
        assert!(permit.load(Ordering::Acquire));
        let input = fixture
            .capture
            .sink()
            .reserve_input(Zeroizing::new("FIFO gate".into()))
            .unwrap();
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        // Deliver the prepared worker result into the real writer, behind a
        // transport receipt, to exercise UI revocation before its commit.
        let pending = fixture
            .capture
            .link_agent_chat_checked(
                "agent-entry".into(),
                snapshot(&fixture.record, &redactor, Timestamp::now()).unwrap(),
                Arc::clone(&permit),
            )
            .unwrap();
        match change {
            0 => fixture.workspace.exiting = true,
            1 => fixture.workspace.scheduler_draining = true,
            2 => fixture.workspace.clipboard.policy.session_journal_allowed = false,
            3 => fixture
                .workspace
                .set_test_ai_configuration(Configuration::default()),
            4 => fixture.workspace.retired_journal_captures.clear(),
            _ => {
                let rule = "[[session-journal.replace]]\npattern = 'Answer'\nreplacement = '***'\n"
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                fixture
                    .workspace
                    .clipboard
                    .policy
                    .terminal_logging
                    .read_rule(rule.as_table())
                    .unwrap();
            }
        }
        fixture
            .workspace
            .revalidate_agent_journal_synchronizations();
        assert!(
            !permit.load(Ordering::Acquire),
            "change {change} retained authority"
        );
        input.send(()).unwrap();
        assert!(
            pending
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("revoked")
        );
        let journal = fixture.capture.directory().join("journal.xml");
        assert!(
            !std::fs::read_to_string(journal)
                .unwrap()
                .contains("<chatSnapshot>")
        );
    }
}

#[test]
fn newer_saved_chat_revision_revokes_old_sync_and_late_completion_cannot_clear_the_new_job() {
    let mut fixture = Fixture::new();
    let (old_ticket, old_permit, _old_task) = fixture.begin();
    let summary = store::save(
        &fixture.root,
        Some(&fixture.record.summary.key),
        "Renamed task".into(),
        store::Draft {
            profile: crate::ai_manager::ProfileRef::Numeric(1),
            profile_name: "Fixture".into(),
            connection: String::new(),
            response_language: None,
            turns: fixture.record.turns.clone(),
            attachment: None,
        },
    )
    .unwrap();
    fixture.record = store::load(&fixture.root, &summary.key).unwrap();
    let (new_ticket, new_permit, _new_task) = fixture.begin();
    assert_ne!(old_ticket, new_ticket);
    assert!(!old_permit.load(Ordering::Acquire));
    assert!(new_permit.load(Ordering::Acquire));
    let _ = fixture
        .workspace
        .update_journal_agent_chat(Message::Synchronized(old_ticket));
    let _ = fixture
        .workspace
        .update_journal_agent_chat(Message::Synchronized(old_ticket));
    assert!(
        fixture
            .workspace
            .journal_chat_sync
            .pending
            .contains_key(&new_ticket)
    );
    assert!(new_permit.load(Ordering::Acquire));
    let _ = fixture
        .workspace
        .update_journal_agent_chat(Message::Synchronized(new_ticket));
    assert!(!new_permit.load(Ordering::Acquire));
    assert!(fixture.workspace.journal_chat_sync.pending.is_empty());
}

#[test]
fn journal_sync_worker_publishes_and_releases_its_registered_permits_without_model_calls() {
    let mut fixture = Fixture::new();
    let (_, permit, task) = fixture.begin();
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    while let Some(action) = block_on(actions.next()) {
        if let iced_runtime::Action::Output(Desktop::JournalAgentChat(message)) = action {
            let _ = fixture.workspace.update_journal_agent_chat(message);
        }
    }
    assert!(!permit.load(Ordering::Acquire));
    assert!(fixture.workspace.journal_chat_sync.pending.is_empty());
    let journal = std::fs::read_to_string(fixture.capture.directory().join("journal.xml")).unwrap();
    assert!(journal.contains("<chatSnapshot>"));
    assert!(journal.contains("<frozen>false</frozen>"));
}
