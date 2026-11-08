use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::advanced::{layout, widget::Tree};
use iced::{Font, Pixels, Renderer};

use crate::session_journal::{self, entry_edit::LegacyMarker};

use super::super::entry_editor;
use super::form::{Edit, RuleChange};
use super::*;

const XML: &str = r#"<session-journal formatVersion="1" id="marker-dialog">
<meta><title>Markers</title></meta>
<entries>
<entry><id>one</id><kind>USER_NOTE</kind><title>Needs review</title><marker>NONE</marker></entry>
<entry><id>two</id><kind>USER_NOTE</kind><title>Needs review</title><marker>NONE</marker><markerSource>USER</markerSource></entry>
</entries></session-journal>"#;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-marker-dialog-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("journal.xml"), XML).unwrap();
        Self(root)
    }

    fn open(&self, hosted: bool) -> (Workspace, window::Id, window::Id) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let source = ViewerSource::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source, "Markers".into());
        let id = *workspace.journals.keys().next().unwrap();
        // Test double for delivery of the verified on-disk journal.
        let _ = workspace.update(Desktop::SessionJournal(
            id,
            Message::DocumentLoaded(Ok(document)),
        ));
        let _ = workspace.update(Desktop::SessionJournal(
            id,
            Message::EntryEditor(entry_editor::Event::Toggle),
        ));
        (workspace, owner, id)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(workspace: &mut Workspace, id: window::Id, event: Event) {
    let _ = workspace.update(Desktop::SessionJournal(id, Message::Markers(event)));
}

fn wait_for_worker(workspace: &Workspace) {
    let done = workspace.pending_shutdown.last().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done.is_closed() {
        assert!(
            Instant::now() < deadline,
            "journal marker worker did not finish"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn journal_marker_dialog_reorders_rules_and_removes_only_deleted_definition_references() {
    let mut form = form::Form::new(Settings::default());
    form.update(Edit::Add);
    let first = form.settings().definitions[0].id.clone();
    form.update(Edit::Add);
    let second = form.settings().definitions[1].id.clone();
    form.update(Edit::Duplicate);
    assert_eq!(form.settings().definitions.len(), 3);
    form.update(Edit::AddRule);
    form.update(Edit::AddRule);
    form.update(Edit::ChangeRule(2, 1, RuleChange::Marker(second.clone())));
    form.update(Edit::ChangeRule(
        2,
        1,
        RuleChange::Pattern("priority".into()),
    ));
    form.update(Edit::MoveRule(false));
    assert_eq!(form.settings().rules[0].marker_id, second);
    form.update(Edit::ChangeRule(
        2,
        0,
        RuleChange::Pattern("stale edit".into()),
    ));
    assert_eq!(form.settings().rules[0].pattern, "priority");
    form.update(Edit::SelectDefinition(first));
    form.update(Edit::Delete);
    assert_eq!(form.settings().rules.len(), 1);
    assert_eq!(form.settings().rules[0].marker_id, second);
    assert_eq!(form.settings().definitions.len(), 2);
}

#[test]
fn journal_marker_dialog_native_and_hosted_apply_preserves_manual_markers_and_opt_in() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, owner, source) = fixture.open(hosted);
        send(&mut workspace, source, Event::Open);
        let id = workspace.journal_edit_for_owner(owner).unwrap();
        assert_eq!(workspace.journal_title(id).as_deref(), Some(TITLE));
        {
            let mut view = workspace.journal_view(id).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let bounds = view
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &Renderer::new(Font::DEFAULT, Pixels(13.0)),
                    &layout::Limits::new(Size::ZERO, DEFAULT_SIZE),
                )
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }
        send(&mut workspace, id, Event::Edit(Edit::Add));
        send(&mut workspace, id, Event::Cancel);
        assert!(
            workspace
                .journal_markers
                .current
                .settings
                .definitions
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("journal.xml")).unwrap(),
            XML
        );

        send(&mut workspace, source, Event::Open);
        let id = workspace.journal_edit_for_owner(owner).unwrap();
        send(&mut workspace, id, Event::Edit(Edit::Add));
        send(&mut workspace, id, Event::Edit(Edit::AddRule));
        send(
            &mut workspace,
            id,
            Event::Edit(Edit::ChangeRule(
                1,
                0,
                RuleChange::Pattern("needs review".into()),
            )),
        );
        send(&mut workspace, id, Event::Edit(Edit::Enabled(true)));
        let mut settings = workspace
            .journal_markers
            .dialog
            .as_ref()
            .unwrap()
            .form
            .settings()
            .clone();
        settings.enabled = false;
        send(&mut workspace, id, Event::Apply);
        wait_for_worker(&workspace);
        assert_eq!(
            fs::read_to_string(fixture.0.join("journal.xml")).unwrap(),
            XML
        );
        let prepared = Arc::new(Prepared::new(settings).unwrap());
        // Test double for Iced delivery of the compiled rule profile. The apply worker uses the real XML.
        send(&mut workspace, id, Event::Prepared(Ok(prepared)));
        assert!(!workspace.journal_marker_rules().enabled());
        let Mode::Viewer(viewer) = &workspace.journals[&source].mode else {
            panic!("expected viewer");
        };
        assert!(!viewer.can_refresh());
        wait_for_worker(&workspace);
        let current = Arc::new(session_journal::read(&fixture.0).unwrap());
        assert_eq!(current.timeline[0].marker_name, "New marker");
        assert!(current.timeline[1].marker_name.is_empty());
        // Test double for the apply worker's real refreshed journal delivery.
        send(
            &mut workspace,
            id,
            Event::Applied(Ok(Arc::new(Applied {
                outcome: MarkerOutcome {
                    changed: 1,
                    durability_warning: false,
                },
                document: Some(current),
            }))),
        );
        send(&mut workspace, id, Event::Cancel);
        assert!(workspace.journal_edit_for_owner(owner).is_none());
        assert_eq!(
            workspace
                .journal_marker_definitions()
                .iter()
                .filter(|definition| !definition.is_builtin())
                .count(),
            1
        );
        assert!(!workspace.journal_marker_rules().enabled());
    }
}

#[test]
fn journal_marker_dialog_refreshes_picker_without_discarding_dirty_fields() {
    let fixture = Fixture::new();
    let (mut workspace, owner, source) = fixture.open(false);
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&source).unwrap().mode else {
        panic!("expected viewer");
    };
    let draft = entry_edit::Draft::read(&viewer.source, "one").unwrap();
    viewer.entry_editor.select(0, Arc::new(draft));
    let generation = viewer.entry_editor.generation();
    viewer.entry_editor.change(
        generation,
        entry_editor::Input::Title("Unsaved title".into()),
    );

    send(&mut workspace, source, Event::Open);
    let id = workspace.journal_edit_for_owner(owner).unwrap();
    send(&mut workspace, id, Event::Apply);
    assert!(workspace.pending_shutdown.is_empty());
    let mut settings = Settings::default();
    settings
        .add_definition("Needs attention", [50, 100, 150], LegacyMarker::Info)
        .unwrap();
    settings.definitions[0].name = " Needs attention ".into();
    workspace.install_journal_markers(Arc::new(Prepared::new(settings).unwrap()));
    let Mode::Viewer(viewer) = &workspace.journals[&source].mode else {
        panic!("expected viewer");
    };
    assert!(viewer.entry_editor.is_dirty());
    assert!(
        viewer
            .entry_editor
            .draft()
            .unwrap()
            .markers()
            .iter()
            .any(|marker| marker.to_string() == "Needs attention")
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("journal.xml")).unwrap(),
        XML
    );

    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&source).unwrap().mode else {
        panic!("expected viewer");
    };
    viewer.entry_editor.revert();
    let custom = viewer
        .entry_editor
        .draft()
        .unwrap()
        .markers()
        .iter()
        .find(|marker| marker.to_string() == "Needs attention")
        .unwrap()
        .clone();
    viewer.entry_editor.change(
        viewer.entry_editor.generation(),
        entry_editor::Input::Marker(custom),
    );
    workspace.install_journal_markers(Arc::new(Prepared::default()));
    let Mode::Viewer(viewer) = &workspace.journals[&source].mode else {
        panic!("expected viewer");
    };
    assert!(viewer.entry_editor.is_dirty());
    assert!(viewer.entry_editor.edit().is_err());
}
