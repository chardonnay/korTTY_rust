use std::time::Duration;

use iced::futures::{StreamExt, executor::block_on};
use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::snippet_window::draft_store::hash_content;
use crate::snippet_window::recovery::Message as RecoveryAction;
use crate::snippet_window::{Message as SnippetMessage, SnippetWindow};
use crate::snippets::tests::Profile;

struct Fixture {
    profile: Profile,
    workspace: Workspace,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new();
        let directory = profile
            .path()
            .with_file_name(snippet_window::draft_store::DIRECTORY_NAME);
        let workspace = Workspace {
            snippet_recovery: State {
                worker: Some(Worker::start(directory).unwrap()),
                ..State::default()
            },
            ..Workspace::default()
        };
        Self { profile, workspace }
    }

    fn worker(&self) -> &Worker {
        self.workspace.snippet_recovery.worker.as_ref().unwrap()
    }

    fn catalog(&self) -> Vec<Summary> {
        self.worker()
            .list()
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap()
    }

    fn load(&self, key: SnippetId) -> Option<Snapshot> {
        self.worker()
            .load(key, None)
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap()
    }

    fn library(&self) -> snippets::Library {
        snippets::Library::open(self.profile.path()).unwrap()
    }

    fn publish_catalog(&mut self) {
        let list = self.workspace.reload_snippet_recovery();
        self.complete(list);
    }

    fn seed_saved_with_draft(&mut self) -> Snapshot {
        let key = self
            .library()
            .save(
                None,
                snippets::Input {
                    name: "Saved name".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "echo saved".into(),
                },
            )
            .unwrap();
        let draft = snapshot(key, Some(key));
        self.worker().save(Arc::new(draft.clone())).unwrap();
        self.publish_catalog();
        draft
    }

    fn open(&mut self, hosted: bool) -> window::Id {
        let (owner, _) = self.workspace.open_window(false);
        self.workspace
            .install_snippet_surface(
                owner,
                SnippetWindow::open(self.profile.path()).unwrap(),
                hosted,
            )
            .0
    }

    fn window(&self, id: window::Id) -> &SnippetWindow {
        &self.workspace.snippets[&id].window
    }

    fn send(&mut self, id: window::Id, message: SnippetMessage) -> Task<Message> {
        self.workspace.update(Message::Snippets(id, message))
    }

    fn complete(&mut self, task: Task<Message>) {
        let mut pending = VecDeque::from([task]);
        while let Some(task) = pending.pop_front() {
            let Some(stream) = iced_runtime::task::into_stream(task) else {
                continue;
            };
            for action in block_on(stream.collect::<Vec<_>>()) {
                if let iced_runtime::Action::Output(message) = action {
                    pending.push_back(self.workspace.update(message));
                }
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(done) = self.workspace.snippet_recovery.finish() {
            let _ = done.recv_blocking();
        }
    }
}

fn worker_message(task: Task<Message>) -> Message {
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(iced_runtime::Action::Output(message)) = block_on(stream.next()) else {
        panic!("expected the recovery worker reply");
    };
    message
}

fn snapshot(key: SnippetId, saved_id: Option<SnippetId>) -> Snapshot {
    Snapshot {
        key,
        saved_id,
        saved_at: 1_700_000_000_000,
        name: "Recovered name".into(),
        language: "bash".into(),
        category: "Operations".into(),
        metadata: snippets::Metadata {
            description: "Recovered description".into(),
            tags: vec!["recovered".into()],
            ..snippets::Metadata::default()
        },
        content: "echo recovered".into(),
        base_hash: saved_id.map(|_| hash_content("echo saved").unwrap()),
    }
}

#[test]
fn unanswered_recovery_is_not_overwritten_by_edit_save_or_close() {
    let mut fixture = Fixture::new();
    let draft = fixture.seed_saved_with_draft();
    let manager = fixture.open(false);
    let _ = fixture.send(manager, SnippetMessage::Open(draft.key));
    assert_eq!(
        fixture
            .window(manager)
            .recovery_choices(&RecoveryAction::Restore(draft.key))
            .len(),
        1
    );
    assert!(!fixture.window(manager).dirty());

    let _ = fixture.send(manager, SnippetMessage::Name("Manual edit".into()));
    let _ = fixture.send(
        manager,
        SnippetMessage::Edit(Action::Edit(Edit::Insert('!'))),
    );
    fixture
        .workspace
        .poll_snippet_recovery(Instant::now() + Duration::from_secs(3));
    assert_eq!(fixture.load(draft.key).as_ref(), Some(&draft));

    let _ = fixture.send(manager, SnippetMessage::Save);
    assert_eq!(
        fixture.library().get(draft.key).unwrap().name(),
        "Manual edit"
    );
    assert!(!fixture.window(manager).dirty());
    assert_eq!(fixture.load(draft.key).as_ref(), Some(&draft));
    let _ = fixture.send(manager, SnippetMessage::CloseActiveTab);
    assert!(!fixture.window(manager).owns_editor(draft.key));
    assert_eq!(fixture.load(draft.key).as_ref(), Some(&draft));

    let _ = fixture.send(manager, SnippetMessage::Open(draft.key));
    assert_eq!(
        fixture
            .window(manager)
            .recovery_choices(&RecoveryAction::Restore(draft.key))
            .len(),
        1
    );
    let _ = fixture.send(manager, SnippetMessage::Close);
    assert!(!fixture.workspace.is_snippet_surface(manager));
    assert_eq!(fixture.load(draft.key), Some(draft));
}

#[test]
fn restoring_an_existing_draft_is_unsaved_until_save_and_then_removes_its_file() {
    let mut fixture = Fixture::new();
    let draft = fixture.seed_saved_with_draft();
    let manager = fixture.open(true);
    let _ = fixture.send(manager, SnippetMessage::Open(draft.key));
    let load = fixture.send(
        manager,
        SnippetMessage::Recovery(RecoveryAction::Restore(draft.key)),
    );
    fixture.complete(load);
    assert!(fixture.window(manager).dirty());
    assert!(fixture.window(manager).owns_editor(draft.key));
    assert_eq!(
        fixture.library().get(draft.key).unwrap().content(),
        "echo saved"
    );
    assert_eq!(fixture.load(draft.key).as_ref(), Some(&draft));

    let _ = fixture.send(manager, SnippetMessage::Save);
    assert!(!fixture.window(manager).dirty());
    let library = fixture.library();
    let saved = library.get(draft.key).unwrap();
    assert_eq!(saved.name(), draft.name);
    assert_eq!(saved.content(), &*draft.content);
    assert!(saved.metadata() == &draft.metadata);
    assert!(fixture.load(draft.key).is_none());
}

#[test]
fn a_new_draft_waits_for_quiet_time_survives_close_cancel_and_keeps_its_first_save_id() {
    let mut fixture = Fixture::new();
    fixture.publish_catalog();
    let manager = fixture.open(false);
    let _ = fixture.send(manager, SnippetMessage::New);
    let _ = fixture.send(manager, SnippetMessage::Name("New snippet".into()));
    let before_edit = Instant::now();
    let _ = fixture.send(
        manager,
        SnippetMessage::Edit(Action::Edit(Edit::Paste(Arc::new("echo new".into())))),
    );
    fixture
        .workspace
        .poll_snippet_recovery(before_edit + Duration::from_millis(1999));
    assert!(fixture.catalog().is_empty());
    fixture
        .workspace
        .poll_snippet_recovery(Instant::now() + Duration::from_secs(2));
    let catalog = fixture.catalog();
    assert_eq!(catalog.len(), 1);
    let key = catalog[0].key;
    let draft = fixture.load(key).unwrap();
    assert_eq!(draft.saved_id, None);
    assert_eq!(&*draft.content, "echo new");
    assert!(fixture.library().entries().is_empty());

    let _ = fixture.send(manager, SnippetMessage::CloseActiveTab);
    let _ = fixture.send(manager, SnippetMessage::Cancel);
    assert!(fixture.window(manager).dirty());
    assert_eq!(fixture.load(key), Some(draft));

    let _ = fixture.send(manager, SnippetMessage::Save);
    let library = fixture.library();
    assert_eq!(library.entries().len(), 1);
    assert_eq!(library.get(key).unwrap().content(), "echo new");
    assert!(fixture.window(manager).owns_editor(key));
    assert!(fixture.load(key).is_none());
}

#[test]
fn the_same_orphan_can_be_adopted_only_once_across_two_workspaces() {
    let mut fixture = Fixture::new();
    let old_key = format!("{:032x}", 42).try_into().unwrap();
    let orphan = snapshot(old_key, None);
    fixture.worker().save(Arc::new(orphan.clone())).unwrap();
    fixture.publish_catalog();
    let first = fixture.open(false);
    let second = fixture.open(true);
    for manager in [first, second] {
        assert_eq!(
            fixture
                .window(manager)
                .recovery_choices(&RecoveryAction::RestoreOrphans)
                .len(),
            1
        );
    }

    let first_restore = fixture.send(
        first,
        SnippetMessage::Recovery(RecoveryAction::RestoreOrphans),
    );
    let second_restore = fixture.send(
        second,
        SnippetMessage::Recovery(RecoveryAction::RestoreOrphans),
    );
    assert_eq!(fixture.workspace.snippet_recovery.pending.len(), 1);
    assert!(fixture.workspace.snippet_recovery.claims.contains(&old_key));
    fixture.complete(second_restore);

    let adoption = fixture.workspace.update(worker_message(first_restore));
    // Observe the worker's new file before delivering the adoption reply to the editor.
    let catalog = fixture.catalog();
    assert_eq!(catalog.len(), 1);
    let new_key = catalog[0].key;
    assert_ne!(new_key, old_key);
    fixture.publish_catalog();
    assert!(
        fixture
            .workspace
            .snippet_recovery
            .catalog
            .iter()
            .any(|entry| entry.key == new_key)
    );
    assert!(fixture.workspace.snippet_recovery.claims.contains(&new_key));
    assert!(
        fixture
            .window(second)
            .recovery_choices(&RecoveryAction::RestoreOrphans)
            .is_empty()
    );
    let second_restore = fixture.send(
        second,
        SnippetMessage::Recovery(RecoveryAction::RestoreOrphans),
    );
    fixture.complete(second_restore);
    assert_eq!(fixture.workspace.snippet_recovery.pending.len(), 1);

    fixture.complete(adoption);
    assert!(fixture.workspace.snippet_recovery.claims.is_empty());
    assert!(fixture.window(first).dirty());
    assert!(!fixture.window(second).dirty());
    assert!(fixture.load(old_key).is_none());

    let adopted = fixture.load(new_key).unwrap();
    assert_eq!(adopted.saved_id, None);
    assert_eq!(adopted.content, orphan.content);
    assert!(adopted.metadata == orphan.metadata);
    assert_eq!(fixture.window(first).live_recovery_keys(), vec![new_key]);
    assert!(fixture.window(second).live_recovery_keys().is_empty());

    let _ = fixture.send(first, SnippetMessage::Save);
    let library = fixture.library();
    assert_eq!(library.entries().len(), 1);
    assert_eq!(library.get(new_key).unwrap().content(), &*orphan.content);
    assert!(fixture.catalog().is_empty());
}

#[test]
fn discard_waits_for_ack_and_reoffers_a_replaced_draft_after_failure() {
    let mut fixture = Fixture::new();
    let mut newer = fixture.seed_saved_with_draft();
    let key = newer.key;
    let manager = fixture.open(false);
    let _ = fixture.send(manager, SnippetMessage::Open(key));
    let _ = fixture.send(manager, SnippetMessage::Name("Current local edit".into()));
    let choice = RecoveryAction::Discard(key);
    let original_offer = fixture.window(manager).recovery_choices(&choice);
    assert_eq!(original_offer.len(), 1);

    // Replace the on-disk revision while the manager still shows its earlier offer.
    newer.content = "echo newer recovery".into();
    fixture.worker().save(Arc::new(newer.clone())).unwrap();
    let catalog = fixture.catalog();
    assert_ne!(catalog[0].revision, original_offer[0].revision);
    let stale_discard = fixture.send(manager, SnippetMessage::Recovery(choice.clone()));
    assert_eq!(
        fixture.window(manager).recovery_choices(&choice),
        original_offer
    );
    fixture.complete(stale_discard);
    assert_eq!(fixture.window(manager).recovery_choices(&choice), catalog);
    fixture
        .workspace
        .poll_snippet_recovery(Instant::now() + Duration::from_secs(3));
    assert_eq!(fixture.load(key), Some(newer));

    let discard = fixture.send(manager, SnippetMessage::Recovery(choice.clone()));
    assert!(fixture.load(key).is_none());
    // The file is gone, but a listing cannot clear the offer before the exact ACK.
    fixture.publish_catalog();
    assert_eq!(fixture.window(manager).recovery_choices(&choice), catalog);
    fixture.complete(discard);
    assert!(fixture.window(manager).recovery_choices(&choice).is_empty());
    fixture
        .workspace
        .poll_snippet_recovery(Instant::now() + Duration::from_secs(3));
    assert_eq!(fixture.load(key).unwrap().name, "Current local edit");
}

#[test]
fn discard_orphans_stops_on_a_stale_revision_and_retries_remaining_drafts() {
    let mut fixture = Fixture::new();
    let [first, mut middle, last] = [1, 2, 3].map(|number| {
        let key = format!("{number:032x}").try_into().unwrap();
        let mut draft = snapshot(key, None);
        draft.saved_at -= i64::from(number);
        fixture.worker().save(Arc::new(draft.clone())).unwrap();
        draft
    });
    fixture.publish_catalog();
    let manager = fixture.open(false);
    let choice = RecoveryAction::DiscardOrphans;
    assert_eq!(fixture.window(manager).recovery_choices(&choice).len(), 3);

    middle.content = "echo changed orphan".into();
    fixture.worker().save(Arc::new(middle.clone())).unwrap();
    let discard = fixture.send(manager, SnippetMessage::Recovery(choice.clone()));
    fixture.complete(discard);
    assert!(fixture.load(first.key).is_none());
    assert_eq!(fixture.load(middle.key).as_ref(), Some(&middle));
    assert_eq!(fixture.load(last.key).as_ref(), Some(&last));
    let remaining: Vec<_> = fixture
        .window(manager)
        .recovery_choices(&choice)
        .into_iter()
        .map(|summary| summary.key)
        .collect();
    assert_eq!(remaining, vec![middle.key, last.key]);

    let retry = fixture.send(manager, SnippetMessage::Recovery(choice));
    fixture.complete(retry);
    assert!(fixture.catalog().is_empty());
    assert!(fixture.workspace.snippet_recovery.claims.is_empty());
}

#[test]
fn delayed_lists_cannot_reoffer_discarded_or_adopted_orphans() {
    for action in [
        RecoveryAction::DiscardOrphans,
        RecoveryAction::RestoreOrphans,
    ] {
        let mut fixture = Fixture::new();
        let old_key = format!("{:032x}", 42).try_into().unwrap();
        let later_key = format!("{:032x}", 43).try_into().unwrap();
        fixture
            .worker()
            .save(Arc::new(snapshot(old_key, None)))
            .unwrap();
        fixture.publish_catalog();
        let first = fixture.open(false);
        let second = fixture.open(true);
        let choices = |fixture: &Fixture| {
            fixture
                .window(second)
                .recovery_choices(&RecoveryAction::RestoreOrphans)
                .into_iter()
                .map(|summary| summary.key)
                .collect::<Vec<_>>()
        };
        assert_eq!(choices(&fixture), vec![old_key]);
        // Hold the real list reply until after the later mutation is acknowledged.
        let stale = worker_message(fixture.workspace.reload_snippet_recovery());
        let mutation = fixture.send(first, SnippetMessage::Recovery(action));
        fixture.complete(mutation);
        assert!(fixture.load(old_key).is_none());
        assert!(choices(&fixture).is_empty());
        let _ = fixture.workspace.update(stale);
        assert!(choices(&fixture).is_empty());

        fixture
            .worker()
            .save(Arc::new(snapshot(later_key, None)))
            .unwrap();
        fixture.publish_catalog();
        assert_eq!(choices(&fixture), vec![later_key]);
    }
}

#[test]
fn a_late_catalog_does_not_remove_matching_but_still_unsaved_input() {
    let mut fixture = Fixture::new();
    let key = fixture
        .library()
        .save(
            None,
            snippets::Input {
                name: "Saved".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo saved".into(),
            },
        )
        .unwrap();
    let mut draft = snapshot(key, Some(key));
    draft.name = "Repeated unsaved name".into();
    draft.category.clear();
    draft.metadata = snippets::Metadata::default();
    draft.content = "echo saved".into();
    fixture.worker().save(Arc::new(draft.clone())).unwrap();
    let manager = fixture.open(false);
    let _ = fixture.send(manager, SnippetMessage::Open(key));
    let _ = fixture.send(manager, SnippetMessage::Name(draft.name.clone()));
    fixture.publish_catalog();
    assert!(fixture.window(manager).dirty());
    assert_eq!(fixture.load(key), Some(draft));
    assert_eq!(
        fixture
            .window(manager)
            .recovery_choices(&RecoveryAction::Restore(key))
            .len(),
        1
    );
}
