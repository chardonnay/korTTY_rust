use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iced::{widget::text_editor, window};
use zeroize::Zeroizing;

use super::super::manager_tree::Event;
use super::super::metadata;
use super::super::{Desktop, Manager, Message as JournalMessage, Mode, Workspace, tree};
use super::{Message, Request};
use crate::session_journal::{self, Capture, Metadata, RecordKind, Status};

struct Fixture {
    workspace: Workspace,
    scratch: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(hosted: bool) -> Self {
        let scratch = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-content-ui-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&scratch).unwrap();
        let root = create_root(&scratch, "journals");
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        Self {
            workspace,
            scratch,
            root,
        }
    }

    fn record(&self, title: &str, output: &str) {
        let capture = Capture::start(
            self.root.clone(),
            Metadata {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap();
        let write =
            capture
                .sink()
                .try_write(RecordKind::Output, Zeroizing::new(output.into()), false);
        let finished = capture.finish();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !finished.is_closed() {
            assert!(Instant::now() < deadline, "journal writer did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        write.unwrap();
        assert_eq!(capture.status(), Status::Complete);
    }

    fn open(&mut self) -> window::Id {
        let (owner, _) = self.workspace.open_window(false);
        self.send(owner, JournalMessage::Open);
        let source = self
            .workspace
            .journals
            .iter()
            .find_map(|(&id, surface)| {
                (self.workspace.journal_owner(id, surface) == Some(owner)
                    && matches!(surface.mode, Mode::Manager(_)))
                .then_some(id)
            })
            .unwrap();
        self.complete_catalog(source);
        source
    }

    fn send(&mut self, source: window::Id, message: JournalMessage) {
        let _ = self
            .workspace
            .update(Desktop::SessionJournal(source, message));
    }

    fn manager(&self, source: window::Id) -> &Manager {
        let Mode::Manager(manager) = &self.workspace.journals[&source].mode else {
            panic!("expected journal manager");
        };
        manager
    }

    fn complete_catalog(&mut self, source: window::Id) {
        let (revision, root) = self.manager(source).reading.clone().unwrap();
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced delivery, with an actual catalog from disk.
        self.send(
            source,
            JournalMessage::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
    }

    fn active(&self, source: window::Id) -> &Request {
        self.manager(source)
            .content_search
            .active
            .as_ref()
            .expect("one active scan")
    }

    fn completion(&self, source: window::Id) -> Message {
        let request = self.active(source);
        // Test double for Iced scheduling/delivery. The actual scanner reads the
        // request's catalog, XML and capture records.
        Message::Finished(request.ticket, Ok(Arc::new(request.scan().unwrap())))
    }

    fn deliver(&mut self, source: window::Id, message: Message) {
        self.send(source, JournalMessage::ContentSearch(message));
    }

    fn complete(&mut self, source: window::Id) {
        self.deliver(source, self.completion(source));
    }

    fn search(&mut self, source: window::Id, query: &str) {
        self.send(source, JournalMessage::Search(query.into()));
    }

    fn assert_titles(&self, source: window::Id, expected: &[&str]) {
        let actual = self
            .manager(source)
            .results()
            .map(|row| row.title.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected.iter().copied().collect());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn create_root(scratch: &Path, name: &str) -> PathBuf {
    crate::private_files::Directory::open(scratch, false)
        .unwrap()
        .private_child(name, true)
        .unwrap();
    scratch.join(name)
}

fn seed_import(root: &Path, id: &str, title: &str, note: &str) {
    let directory = root.join(format!("journal-{id}"));
    fs::create_dir_all(&directory).unwrap();
    // The import follows the pinned Java JAXB shape. All values are private test literals.
    fs::write(directory.join("journal.xml"), format!(
        r#"<session-journal formatVersion="1" id="{id}"><meta><title>{title}</title><sourceKind>JOB</sourceKind><sourceId>test-job</sourceId><sourceName>Test job</sourceName><startedAt>2026-11-06T10:00:00Z</startedAt></meta><entries><entry><id>note-1</id><kind>USER_NOTE</kind><userNote>{note}</userNote></entry></entries></session-journal>"#
    )).unwrap();
}

#[test]
fn journal_content_search_unions_metadata_and_real_content_before_source_filter() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        seed_import(&fixture.root, "metadata", "Magnet title", "Unrelated note");
        seed_import(
            &fixture.root,
            "xml",
            "XML note",
            "A MagNet from the imported note",
        );
        fixture.record("Terminal capture", "A MAGNET from terminal output");
        let source = fixture.open();

        fixture.search(source, "  mAgNeT  ");
        assert!(!fixture.manager(source).content_search.enabled);
        assert!(fixture.manager(source).content_search.active.is_none());
        fixture.assert_titles(source, &["Magnet title"]);
        fixture.send(source, JournalMessage::SearchContents(true));
        fixture.assert_titles(source, &["Magnet title"]);
        fixture.complete(source);
        fixture.assert_titles(source, &["Magnet title", "XML note", "Terminal capture"]);

        fixture.send(source, JournalMessage::Filter(tree::Filter::Interactive));
        fixture.complete(source);
        fixture.assert_titles(source, &["Terminal capture"]);
        fixture.send(source, JournalMessage::Filter(tree::Filter::Jobs));
        fixture.complete(source);
        fixture.assert_titles(source, &["Magnet title", "XML note"]);
        let generation = fixture.manager(source).tree.generation();
        // Test double for the current tree's select-all event.
        fixture.send(source, JournalMessage::Tree(Event::SelectAll(generation)));
        let manager = fixture.manager(source);
        let catalog = manager.catalog.as_ref().unwrap();
        let selected = manager
            .tree
            .selected_indices(catalog)
            .into_iter()
            .map(|index| catalog.journals[index].title.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(selected, BTreeSet::from(["Magnet title", "XML note"]));
    }
}

#[test]
fn journal_content_search_coalesces_latest_query_and_rejects_toggle_or_clear_results() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        seed_import(&fixture.root, "old", "Original", "first-token");
        seed_import(&fixture.root, "middle", "Intermediate", "unused-token");
        seed_import(&fixture.root, "latest", "Latest", "last-token");
        let source = fixture.open();
        fixture.send(source, JournalMessage::SearchContents(true));
        assert!(fixture.manager(source).content_search.active.is_none());

        fixture.search(source, "first-token");
        let original_ticket = fixture.active(source).ticket;
        let original = fixture.completion(source);
        fixture.search(source, "unused-token");
        fixture.search(source, "last-token");
        assert_eq!(fixture.active(source).ticket, original_ticket);
        assert!(fixture.active(source).cancelled.load(Ordering::Relaxed));
        fixture.deliver(source, original.clone());
        fixture.assert_titles(source, &[]);
        let latest_ticket = fixture.active(source).ticket;
        assert_ne!(latest_ticket, original_ticket);
        fixture.deliver(source, original);
        assert_eq!(fixture.active(source).ticket, latest_ticket);
        fixture.complete(source);
        fixture.assert_titles(source, &["Latest"]);
        assert!(fixture.manager(source).content_search.active.is_none());

        fixture.search(source, "first-token");
        let toggled = fixture.completion(source);
        fixture.send(source, JournalMessage::SearchContents(false));
        fixture.assert_titles(source, &[]);
        fixture.send(source, JournalMessage::SearchContents(true));
        fixture.deliver(source, toggled);
        fixture.assert_titles(source, &[]);
        fixture.complete(source);
        fixture.assert_titles(source, &["Original"]);

        fixture.search(source, "last-token");
        let cleared = fixture.completion(source);
        fixture.search(source, "  ");
        fixture.deliver(source, cleared);
        fixture.assert_titles(source, &["Original", "Intermediate", "Latest"]);
        assert!(fixture.manager(source).content_search.active.is_none());
    }
}

#[test]
fn journal_content_search_rejects_old_results_after_catalog_refresh_or_folder_change() {
    for hosted in [false, true] {
        for replace_root in [false, true] {
            let mut fixture = Fixture::new(hosted);
            seed_import(&fixture.root, "same-id", "Original", "old-signal");
            let source = fixture.open();
            fixture.send(source, JournalMessage::SearchContents(true));
            fixture.search(source, "old-signal");
            let original = fixture.completion(source);
            let root = if replace_root {
                create_root(&fixture.scratch, "alternative")
            } else {
                fixture.root.clone()
            };
            seed_import(&root, "same-id", "Updated", "new-signal");
            fixture.workspace.window_settings.session_journal.directory =
                root.to_str().unwrap().into();
            fixture.send(source, JournalMessage::Refresh);
            fixture.complete_catalog(source);
            assert_eq!(fixture.manager(source).root.as_ref(), Some(&root));
            fixture.deliver(source, original);
            fixture.assert_titles(source, &[]);
            fixture.complete(source);
            fixture.assert_titles(source, &[]);
            fixture.search(source, "new-signal");
            fixture.complete(source);
            fixture.assert_titles(source, &["Updated"]);
        }
    }
}

#[test]
fn journal_content_search_restores_selected_hit_and_description_draft_after_refresh() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        seed_import(&fixture.root, "note", "Journal note", "unique-text");
        let directory = fixture.root.join("journal-note");
        let target = (directory.clone(), "note".to_owned());
        let source = fixture.open();
        fixture.search(source, "unique-text");
        fixture.assert_titles(source, &[]);
        fixture.send(source, JournalMessage::SearchContents(true));
        fixture.complete(source);
        fixture.assert_titles(source, &["Journal note"]);

        let revision = fixture.manager(source).revision;
        // Test double for selecting the matching leaf in the current journal tree.
        fixture.send(source, JournalMessage::Select(revision, directory.clone()));
        assert_eq!(
            fixture.manager(source).description_target.as_ref(),
            Some(&target)
        );
        fixture.send(
            source,
            JournalMessage::Metadata(metadata::Message::Description(
                target.clone(),
                text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
                    "Unsaved description draft".into(),
                ))),
            )),
        );
        assert_eq!(
            fixture.manager(source).description.text(),
            "Unsaved description draft"
        );
        assert!(fixture.workspace.journal_delete_available(source));

        fixture.send(source, JournalMessage::Refresh);
        fixture.complete_catalog(source);
        let _ = fixture.active(source);
        fixture.assert_titles(source, &[]);
        assert!(fixture.manager(source).selected().is_none());
        assert!(!fixture.workspace.journal_delete_available(source));
        fixture.send(source, JournalMessage::OpenSelected);
        fixture.send(
            source,
            JournalMessage::Metadata(metadata::Message::SaveDescription(target.clone())),
        );
        assert_eq!(fixture.workspace.journals.len(), 1);
        assert!(fixture.workspace.pending_shutdown.is_empty());

        fixture.complete(source);
        let manager = fixture.manager(source);
        assert_eq!(manager.selected().unwrap().directory, directory);
        assert_eq!(manager.description_target.as_ref(), Some(&target));
        assert_eq!(manager.description.text(), "Unsaved description draft");
        assert!(
            session_journal::read(&directory)
                .unwrap()
                .summary
                .description
                .is_empty()
        );
    }
}
