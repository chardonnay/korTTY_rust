use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::advanced::{layout, widget::Tree};
use iced::{Font, Pixels, Renderer};

use crate::session_journal;

use super::super::{Draft, entry_editor};
use super::*;

const XML: &str = r#"<session-journal formatVersion="1" id="delete-route">
<meta><title>Session</title><errorCount>1</errorCount><logParts>1</logParts></meta>
<entries><entry><id>shot</id><kind>SCREENSHOT</kind><title>Screenshot entry</title>
<marker>ERROR</marker><screenshotFile>screenshots/shot-1.png</screenshotFile>
<logStartSeq>4</logStartSeq><logEndSeq>4</logEndSeq></entry>
<entry><id>keep</id><kind>USER_NOTE</kind><text>Keep this entry</text></entry></entries>
</session-journal>"#;
const LOG: &[u8] = b"{\"seq\":4,\"k\":\"screenshot\",\"file\":\"screenshots/shot-1.png\"}\n";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-entry-delete-route-{}",
            crate::private_files::nonce().unwrap(),
        ));
        fs::create_dir_all(root.join("screenshots")).unwrap();
        fs::write(root.join("journal.xml"), XML).unwrap();
        fs::write(root.join("session-log-1.json"), LOG).unwrap();
        let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
        fs::write(root.join("screenshots/shot-1.png"), &png).unwrap();
        fs::write(root.join("screenshots/shot-1.orig.png"), png).unwrap();
        Self(root)
    }

    fn viewer(&self, hosted: bool) -> (Workspace, window::Id, window::Id) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let source = ViewerSource::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source.clone(), "Session".into());
        let id = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery of the actual on-disk snapshot.
        send(&mut workspace, id, Message::DocumentLoaded(Ok(document)));
        let draft = Arc::new(Draft::read(&source, "shot").unwrap());
        let viewer = workspace.entry_viewer_mut(id).unwrap();
        viewer.entry_editor.toggle();
        viewer.entry_editor.select(0, draft);
        (workspace, owner, id)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(workspace: &mut Workspace, id: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(id, message));
}

fn dialog(workspace: &mut Workspace, source: window::Id) -> window::Id {
    let editor = &workspace.entry_viewer(source).unwrap().entry_editor;
    let generation = editor.generation();
    let plan = editor.draft().unwrap().prepare_delete().unwrap();
    send(
        workspace,
        source,
        Message::EntryEditor(entry_editor::Event::Delete(generation)),
    );
    let id = workspace.journal_edit_for_owner(source).unwrap();
    // Test double for Iced delivery of the verified deletion plan.
    send(
        workspace,
        id,
        Message::EntryDelete(Event::Prepared(Ok(Arc::new(plan)))),
    );
    id
}

#[test]
fn journal_entry_delete_confirmation_and_real_worker_match_native_and_hosted_viewers() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, owner, source) = fixture.viewer(hosted);
        let viewer = workspace.entry_viewer_mut(source).unwrap();
        let generation = viewer.entry_editor.generation();
        viewer
            .entry_editor
            .change(generation, entry_editor::Input::Title("Unsaved".into()));
        let dialog_id = dialog(&mut workspace, source);
        assert_eq!(workspace.journal_edit_for_owner(owner), Some(dialog_id));
        assert_eq!(workspace.journal_title(dialog_id).as_deref(), Some(TITLE));
        assert!(workspace.journal_entry_deletes.dialogs[&dialog_id].has_draft);
        {
            let mut view = workspace.journal_view(dialog_id).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
            let bounds = view
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(460.0, 224.0)),
                )
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }
        send(
            &mut workspace,
            dialog_id,
            Message::EntryDelete(Event::Dismiss),
        );
        send(
            &mut workspace,
            dialog_id,
            Message::EntryDelete(Event::Confirm),
        );
        assert_eq!(
            fs::read(fixture.0.join("journal.xml")).unwrap(),
            XML.as_bytes()
        );
        assert!(fixture.0.join("screenshots/shot-1.png").exists());
        assert!(
            workspace
                .entry_viewer(source)
                .unwrap()
                .entry_editor
                .is_dirty()
        );

        let dialog_id = dialog(&mut workspace, source);
        send(
            &mut workspace,
            dialog_id,
            Message::EntryDelete(Event::Confirm),
        );
        assert!(workspace.entry_viewer(source).unwrap().entry_flow.deleting);
        send(
            &mut workspace,
            dialog_id,
            Message::EntryDelete(Event::Dismiss),
        );
        assert!(
            workspace
                .journal_entry_deletes
                .dialogs
                .contains_key(&dialog_id)
        );
        let done = workspace.pending_shutdown.last().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !done.is_closed() {
            assert!(
                Instant::now() < deadline,
                "journal entry deletion worker did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let updated = session_journal::read(&fixture.0).unwrap();
        assert_eq!(updated.timeline.len(), 1);
        assert_eq!(updated.timeline[0].id, "keep");
        assert!(!fixture.0.join("screenshots/shot-1.png").exists());
        assert!(fixture.0.join("screenshots/shot-1.orig.png").exists());
        assert_eq!(fs::read(fixture.0.join("session-log-1.json")).unwrap(), LOG);
        assert!(
            fs::read_to_string(fixture.0.join("journal.xml"))
                .unwrap()
                .contains("<errorCount>0</errorCount>")
        );
        // Test double for Iced delivery after the real, successful disk operation.
        let finished = Finished {
            outcome: Outcome::default(),
            document: Some(Arc::new(updated)),
        };
        send(
            &mut workspace,
            dialog_id,
            Message::EntryDelete(Event::Finished(Ok(Arc::new(finished)))),
        );
        assert!(workspace.journal_edit_for_owner(source).is_none());
        let viewer = workspace.entry_viewer(source).unwrap();
        assert!(viewer.entry_editor.is_open());
        assert!(!viewer.entry_editor.is_dirty());
        assert!(viewer.entry_editor.selection().is_none());
        assert_eq!(viewer.document.as_ref().unwrap().timeline[0].id, "keep");
    }
}

#[test]
fn journal_entry_delete_checks_current_policy_and_selected_generation_on_confirm() {
    let fixture = Fixture::new();
    let (mut workspace, _, source) = fixture.viewer(true);
    let first = dialog(&mut workspace, source);
    let rules = "[session-journal]\nallow-delete = false"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    workspace
        .clipboard
        .policy
        .terminal_logging
        .read_rule(rules.as_table())
        .unwrap();
    send(&mut workspace, first, Message::EntryDelete(Event::Confirm));
    assert!(matches!(
        workspace.journal_entry_deletes.dialogs[&first].phase,
        Phase::Report(_)
    ));
    send(&mut workspace, first, Message::EntryDelete(Event::Dismiss));
    let generation = workspace
        .entry_viewer(source)
        .unwrap()
        .entry_editor
        .generation();
    send(
        &mut workspace,
        source,
        Message::EntryEditor(entry_editor::Event::Delete(generation)),
    );
    assert!(workspace.journal_entry_deletes.dialogs.is_empty());

    workspace.clipboard.policy.terminal_logging = Default::default();
    let second = dialog(&mut workspace, source);
    workspace
        .entry_viewer_mut(source)
        .unwrap()
        .entry_editor
        .revert();
    send(&mut workspace, second, Message::EntryDelete(Event::Confirm));
    assert!(matches!(
        workspace.journal_entry_deletes.dialogs[&second].phase,
        Phase::Report(_)
    ));
    assert!(!workspace.entry_viewer(source).unwrap().entry_flow.deleting);
    assert_eq!(
        fs::read(fixture.0.join("journal.xml")).unwrap(),
        XML.as_bytes()
    );
    assert!(fixture.0.join("screenshots/shot-1.png").exists());
    assert_eq!(fs::read(fixture.0.join("session-log-1.json")).unwrap(), LOG);
}
