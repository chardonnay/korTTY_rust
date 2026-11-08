use std::fs;
use std::path::PathBuf;

use iced::advanced::subscription::into_recipes;

use super::super::{ViewerSource, session_journal};
use super::*;
use crate::private_files::Directory;

struct Fixture {
    root: PathBuf,
    directory: PathBuf,
    workspace: Workspace,
    id: window::Id,
}

impl Fixture {
    fn new(hosted: bool) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-refresh-{}",
            crate::private_files::nonce().unwrap()
        ));
        let directory = root.join("journal");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("journal.xml"), xml("Original")).unwrap();
        let original = session_journal::read(&directory).unwrap();
        let source = ViewerSource::catalog(&original.summary).unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_journal_viewer(owner, source.clone(), "Original".into());
        let id = *workspace.journals.keys().next().unwrap();
        let mut fixture = Self {
            root,
            directory,
            workspace,
            id,
        };
        // Test double for the Iced reader task's delivery, using the real verified reader.
        let document = source.read(None).unwrap().unwrap();
        fixture.send(Message::DocumentLoaded(Ok(Arc::new(document))));
        fixture
    }

    fn viewer(&self) -> &Viewer {
        let Mode::Viewer(viewer) = &self.workspace.journals[&self.id].mode else {
            panic!("expected journal viewer")
        };
        viewer
    }

    fn send(&mut self, event: Message) {
        let _ = self
            .workspace
            .update(Desktop::SessionJournal(self.id, event));
    }

    fn refresh(&mut self, event: Event) {
        self.send(Message::ViewerRefresh(event));
    }

    fn publish(&self, title: &str) {
        Directory::open(&self.directory, false)
            .unwrap()
            .replace_file("journal.xml", xml(title).as_bytes())
            .unwrap();
    }

    fn revision(&self) -> u64 {
        self.viewer().refresh.active.as_ref().unwrap().revision
    }

    fn result(&self, manual: bool) -> Result<Option<Arc<Journal>>, String> {
        let viewer = self.viewer();
        let previous = (!manual)
            .then(|| {
                viewer
                    .document
                    .as_ref()
                    .unwrap()
                    .summary
                    .deletion_identity
                    .as_ref()
            })
            .flatten();
        viewer
            .source
            .read(previous)
            .map(|value| value.map(Arc::new))
            .map_err(|error| error.to_string())
    }

    fn title(&self) -> &str {
        &self.viewer().document.as_ref().unwrap().summary.title
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn xml(title: &str) -> String {
    format!(
        "<session-journal formatVersion=\"1\" id=\"refresh\"><meta><title>{title}</title></meta>\
         <entries><entry><id>row</id><kind>USER_NOTE</kind><title>Note</title>\
         <createdAt>2026-11-01T10:00:00Z</createdAt><logStartSeq>3</logStartSeq><logEndSeq>9</logEndSeq>\
         </entry></entries></session-journal>"
    )
}

#[test]
fn journal_viewer_refresh_coalesces_manual_work_and_keeps_unchanged_snapshot() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        assert_eq!(
            into_recipes(fixture.workspace.journal_viewer_subscription()).len(),
            1
        );
        let original = Arc::clone(fixture.viewer().document.as_ref().unwrap());
        let _ = fixture.workspace.update(Desktop::JournalViewerTick);
        let automatic = fixture.revision();
        let unchanged = fixture.result(false);
        assert!(matches!(&unchanged, Ok(None)));
        let _ = fixture.workspace.update(Desktop::JournalViewerTick);
        assert_eq!(fixture.revision(), automatic);
        fixture.refresh(Event::Request);
        assert!(fixture.viewer().refresh.manual_pending);
        // Test double for Iced delivery of a real, unchanged source observation.
        fixture.refresh(Event::Loaded(automatic, unchanged));
        assert!(Arc::ptr_eq(
            &original,
            fixture.viewer().document.as_ref().unwrap()
        ));
        let manual = fixture.revision();
        assert_ne!(automatic, manual);
        assert!(fixture.viewer().refresh.manual_running());

        fixture.publish("New edition");
        let changed = fixture.result(true);
        // Test doubles for a stale ticket followed by the verified manual response.
        fixture.refresh(Event::Loaded(automatic, changed.clone()));
        assert_eq!(fixture.title(), "Original");
        assert_eq!(fixture.revision(), manual);
        fixture.refresh(Event::Loaded(manual, changed));
        assert_eq!(fixture.title(), "New edition");
        assert!(!fixture.viewer().refresh.manual_running());
    }
}

#[test]
fn journal_viewer_refresh_rejects_a_replacement_and_late_policy_result() {
    let mut fixture = Fixture::new(false);
    fixture.publish("First update");
    let _ = fixture.workspace.update(Desktop::JournalViewerTick);
    let old = fixture.revision();
    let changed = fixture.result(false);
    fixture.workspace.clipboard.policy.session_journal_allowed = false;
    // Test double for a verified read completing after policy changed.
    fixture.refresh(Event::Loaded(old, changed));
    assert_eq!(fixture.title(), "Original");
    assert!(into_recipes(fixture.workspace.journal_viewer_subscription()).is_empty());
    fixture.workspace.clipboard.policy.session_journal_allowed = true;

    let moved = fixture.root.join("moved");
    fs::rename(&fixture.directory, &moved).unwrap();
    fs::create_dir(&fixture.directory).unwrap();
    fs::write(fixture.directory.join("journal.xml"), xml("Replacement")).unwrap();
    fixture.refresh(Event::Request);
    let revision = fixture.revision();
    let failed = fixture.result(true);
    assert!(failed.is_err());
    // Test double for delivery of the actual source verification failure.
    fixture.refresh(Event::Loaded(revision, failed));
    assert_eq!(fixture.title(), "Original");
    assert!(fixture.viewer().refresh.problem().is_some());

    fs::remove_dir_all(&fixture.directory).unwrap();
    fs::rename(&moved, &fixture.directory).unwrap();
    let _ = fixture.workspace.update(Desktop::JournalViewerTick);
    let restored = fixture.result(false);
    fixture.refresh(Event::Loaded(fixture.revision(), restored));
    assert_eq!(fixture.title(), "First update");
    assert!(fixture.viewer().refresh.problem().is_none());
}

#[test]
fn journal_viewer_refresh_preserves_capture_filter_and_restarts_its_page() {
    let mut fixture = Fixture::new(false);
    fixture.send(Message::SelectEntry(0, "row".into()));
    fixture.send(Message::CaptureForEntry);
    let page = fixture
        .viewer()
        .source
        .capture_page(None, Some(3..=9), None)
        .unwrap();
    // Test double for delivery of the real capture page.
    fixture.send(Message::CaptureLoaded(
        fixture.viewer().page_revision,
        Ok(Arc::new(page)),
    ));
    fixture.send(Message::CaptureQuery("needle".into()));
    fixture.send(Message::SearchCapture);
    let page = fixture
        .viewer()
        .source
        .capture_page(None, Some(3..=9), Some("needle"))
        .unwrap();
    fixture.send(Message::CaptureLoaded(
        fixture.viewer().page_revision,
        Ok(Arc::new(page)),
    ));

    fixture.publish("Updated");
    fixture.refresh(Event::Request);
    let changed = fixture.result(true);
    // Test double for Iced delivery of the real replacement document.
    fixture.refresh(Event::Loaded(fixture.revision(), changed));
    let viewer = fixture.viewer();
    assert_eq!(fixture.title(), "Updated");
    assert!(viewer.capture_shown && viewer.loading);
    assert!(viewer.capture.is_none());
    assert_eq!(viewer.range, Some(3..=9));
    assert_eq!(viewer.cursors, [None]);
    assert_eq!(viewer.search, "needle");
    assert_eq!(viewer.query, "needle");
}
