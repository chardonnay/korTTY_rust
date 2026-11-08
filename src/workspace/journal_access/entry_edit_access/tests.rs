use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::advanced::{layout, widget::Tree};
use iced::widget::text_editor;
use iced::{Font, Pixels, Renderer, Size};

use crate::session_journal;

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let folder = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-entry-edit-route-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&folder).unwrap();
        fs::write(
            folder.join("journal.xml"),
            r#"<session-journal formatVersion="1" id="entry-route"><meta><title>Session</title></meta>
             <entries><entry><id>alpha</id><kind>USER_NOTE</kind><title>Original</title>
             <marker>NONE</marker><text>Keep &amp; reuse</text><logStartSeq>4</logStartSeq>
             <logEndSeq>6</logEndSeq><userNote>Original note</userNote></entry>
             <entry><id>beta</id><kind>USER_NOTE</kind><userNote>Another note</userNote></entry>
             </entries></session-journal>"#,
        ).unwrap();
        Self(folder)
    }

    fn viewer(&self, hosted: bool) -> (Workspace, window::Id) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let source = super::super::ViewerSource::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source, "Session".into());
        let id = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery, using the actual parsed journal.
        send(&mut workspace, id, Message::DocumentLoaded(Ok(document)));
        (workspace, id)
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

fn editor(workspace: &mut Workspace, id: window::Id, event: entry_editor::Event) {
    send(workspace, id, Message::EntryEditor(event));
}

fn dispatch(workspace: &mut Workspace, id: window::Id, event: Event) {
    send(workspace, id, Message::EntryEdit(event));
}

fn deliver_draft(workspace: &mut Workspace, id: window::Id) {
    let viewer = workspace.entry_viewer(id).unwrap();
    let load = viewer.entry_flow.loading.as_ref().unwrap();
    let draft = Draft::read(&viewer.source, &load.row.id).unwrap();
    // Test double for Iced delivery of the real bound draft.
    dispatch(
        workspace,
        id,
        Event::Loaded(load.revision, Ok(Arc::new(draft))),
    );
}

fn finish_save(workspace: &mut Workspace, id: window::Id) -> Arc<Journal> {
    let (revision, _) = workspace
        .entry_viewer(id)
        .unwrap()
        .entry_flow
        .saving
        .unwrap();
    let done = workspace.pending_shutdown.last().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done.is_closed() {
        assert!(
            Instant::now() < deadline,
            "journal entry writer did not finish"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let source = &workspace.entry_viewer(id).unwrap().source;
    let document = Arc::new(source.read(None).unwrap().unwrap());
    let draft = Arc::new(Draft::read(source, "alpha").unwrap());
    // Test double for Iced delivery after the production worker closed. The
    // fields are read from the document that worker actually published.
    dispatch(
        workspace,
        id,
        Event::Saved(
            revision,
            Ok(Arc::new(Saved {
                document: Some(Arc::clone(&document)),
                draft: Some(draft),
            })),
        ),
    );
    document
}

#[test]
fn journal_entry_edit_routes_save_redaction_layout_and_dirty_close_for_native_and_hosted() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, id) = fixture.viewer(hosted);
        let rules = r#"
            [[session-journal.replace]]
            pattern = "private-value"
            replacement = "[masked]"
        "#
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(rules.as_table())
            .unwrap();
        send(&mut workspace, id, Message::SelectEntry(0, "alpha".into()));
        editor(&mut workspace, id, entry_editor::Event::Toggle);
        deliver_draft(&mut workspace, id);

        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        for size in [
            Size::new(1000.0, 680.0),
            Size::new(470.0, 280.0),
            Size::new(620.0, 440.0),
        ] {
            let mut view = workspace.journal_view(id).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let bounds = view
                .as_widget_mut()
                .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size))
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }

        let generation = workspace
            .entry_viewer(id)
            .unwrap()
            .entry_editor
            .generation();
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Change(
                generation,
                entry_editor::Input::Title("  Prüfung private-value  ".into()),
            ),
        );
        editor(&mut workspace, id, entry_editor::Event::Save(generation));
        let document = finish_save(&mut workspace, id);
        assert_eq!(document.timeline[0].title, "Prüfung [masked]");
        assert_eq!(document.timeline[0].text, "Keep & reuse");
        assert_eq!(document.timeline[0].log_start, Some(4));
        assert!(!workspace.entry_viewer(id).unwrap().entry_editor.is_dirty());

        let generation = workspace
            .entry_viewer(id)
            .unwrap()
            .entry_editor
            .generation();
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Change(generation, entry_editor::Input::Title("Draft".into())),
        );
        send(&mut workspace, id, Message::Close);
        let token = workspace
            .entry_viewer(id)
            .unwrap()
            .entry_flow
            .close_token
            .unwrap();
        dispatch(&mut workspace, id, Event::CloseAnswer(token, false));
        assert!(workspace.entry_viewer(id).unwrap().entry_editor.is_dirty());
        assert!(
            workspace
                .entry_viewer(id)
                .unwrap()
                .entry_flow
                .close_token
                .is_none()
        );
        send(&mut workspace, id, Message::Close);
        let token = workspace
            .entry_viewer(id)
            .unwrap()
            .entry_flow
            .close_token
            .unwrap();
        dispatch(&mut workspace, id, Event::CloseAnswer(token, true));
        assert!(workspace.entry_viewer(id).is_none());
    }
}

#[test]
fn journal_entry_edit_context_changes_marker_without_replacing_prose_or_log_bounds() {
    let fixture = Fixture::new();
    let (mut workspace, id) = fixture.viewer(false);
    dispatch(&mut workspace, id, Event::Context(0, "alpha".into()));
    dispatch(&mut workspace, id, Event::ChooseMarker);
    deliver_draft(&mut workspace, id);
    let form = workspace
        .entry_viewer(id)
        .unwrap()
        .entry_flow
        .marker
        .as_ref()
        .unwrap();
    let error = form
        .draft
        .markers()
        .iter()
        .find(|marker| marker.id() == "error")
        .unwrap()
        .clone();
    dispatch(&mut workspace, id, Event::Marker(error));
    dispatch(&mut workspace, id, Event::ApplyMarker);
    let document = finish_save(&mut workspace, id);
    let row = &document.timeline[0];
    assert_eq!(row.marker_key.as_deref(), Some("error"));
    assert_eq!(row.text, "Keep & reuse");
    assert_eq!(row.log_end, Some(6));
    assert!(
        workspace
            .entry_viewer(id)
            .unwrap()
            .entry_flow
            .context
            .is_none()
    );
}

#[test]
fn journal_entry_note_translation_stays_in_draft_and_rejects_late_results() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, id) = fixture.viewer(hosted);
        workspace.window_settings.journal_ai.enabled = true;
        workspace.window_settings.journal_ai.endpoint = crate::local_ai::DEFAULT_ENDPOINT.into();
        workspace.window_settings.journal_ai.model = "fixture-note".into();
        send(&mut workspace, id, Message::SelectEntry(0, "alpha".into()));
        editor(&mut workspace, id, entry_editor::Event::Toggle);
        deliver_draft(&mut workspace, id);

        let profile = workspace
            .window_settings
            .journal_ai
            .profile()
            .unwrap()
            .unwrap();
        // Test double for an in-flight model request. No external or local model is contacted.
        let pending = |workspace: &mut Workspace| {
            let editor = &mut workspace.entry_viewer_mut(id).unwrap().entry_editor;
            (
                editor.generation(),
                editor.fixture_pending_translation(profile.clone()),
            )
        };
        // Test double for Iced delivery of a completed model response.
        let complete = |workspace: &mut Workspace, (generation, ticket), reply: &str| {
            editor(
                workspace,
                id,
                entry_editor::Event::Translate(
                    generation,
                    note_translation::Event::Finished(ticket, Ok(Arc::new(reply.into()))),
                ),
            );
        };
        let draft_fields = |workspace: &Workspace| {
            let edit = workspace
                .entry_viewer(id)
                .unwrap()
                .entry_editor
                .edit()
                .unwrap();
            let Changes::Fields { fields, .. } = edit.into_changes() else {
                panic!("expected an entry draft");
            };
            fields
        };
        let before = fs::read(fixture.0.join("journal.xml")).unwrap();
        let ticket = pending(&mut workspace);
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Change(
                ticket.0,
                entry_editor::Input::Note(text_editor::Action::Move(
                    text_editor::Motion::DocumentEnd,
                )),
            ),
        );
        complete(&mut workspace, ticket, "Translated note");
        assert_eq!(draft_fields(&workspace).note, "Translated note");
        assert_eq!(draft_fields(&workspace).text, "Keep & reuse");
        assert_eq!(fs::read(fixture.0.join("journal.xml")).unwrap(), before);
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Translate(ticket.0, note_translation::Event::Undo),
        );
        assert_eq!(draft_fields(&workspace).note, "Original note");

        let ticket = pending(&mut workspace);
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Change(
                ticket.0,
                entry_editor::Input::Note(text_editor::Action::Edit(text_editor::Edit::Paste(
                    Arc::new(" revised".into()),
                ))),
            ),
        );
        complete(&mut workspace, ticket, "Stale after editing");
        assert_eq!(draft_fields(&workspace).note, "Original note revised");

        let ticket = pending(&mut workspace);
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Translate(ticket.0, note_translation::Event::Cancel),
        );
        complete(&mut workspace, ticket, "Stale after canceling");
        assert_eq!(draft_fields(&workspace).note, "Original note revised");

        let ticket = pending(&mut workspace);
        editor(
            &mut workspace,
            id,
            entry_editor::Event::Select(1, "beta".into()),
        );
        dispatch(&mut workspace, id, Event::Discard(true));
        deliver_draft(&mut workspace, id);
        complete(&mut workspace, ticket, "Stale after selecting");
        assert_eq!(draft_fields(&workspace).note, "Another note");

        editor(
            &mut workspace,
            id,
            entry_editor::Event::Select(0, "alpha".into()),
        );
        deliver_draft(&mut workspace, id);
        let ticket = pending(&mut workspace);
        complete(&mut workspace, ticket, "Translated note");
        let next = pending(&mut workspace);
        editor(&mut workspace, id, entry_editor::Event::Save(next.0));
        complete(&mut workspace, next, "Stale after saving");
        let document = finish_save(&mut workspace, id);
        assert_eq!(document.timeline[0].note, "Translated note");
        assert_eq!(document.timeline[0].text, "Keep & reuse");
    }
}
