use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::session_journal;

use super::super::{Viewer, ViewerSource, entry_editor, screenshot_export};
use super::*;

const ID: &str = "screenshot-access";
pub(in crate::workspace::journal_access) const REFERENCE: &str = "screenshots/marked.png";
const OTHER: &str = "screenshots/other.png";

pub(in crate::workspace::journal_access) struct Fixture(pub(super) PathBuf);

impl Fixture {
    pub(in crate::workspace::journal_access) fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-screenshot-access-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir_all(path.join("screenshots")).unwrap();
        let png = tiny_skia::Pixmap::new(3, 2).unwrap().encode_png().unwrap();
        fs::write(path.join(REFERENCE), &png).unwrap();
        fs::write(path.join(OTHER), png).unwrap();
        let fixture = Self(path);
        fixture.publish(REFERENCE);
        fixture
    }

    fn publish(&self, reference: &str) -> Arc<session_journal::Journal> {
        fs::write(
            self.0.join("journal.xml"),
            format!(
                r#"<session-journal formatVersion="1" id="{ID}"><meta><title>Images</title></meta>
                <entries><entry><id>screen</id><kind>SCREENSHOT</kind>
                <screenshotFile>{reference}</screenshotFile></entry></entries></session-journal>"#
            ),
        )
        .unwrap();
        Arc::new(session_journal::read(&self.0).unwrap())
    }

    pub(in crate::workspace::journal_access) fn open(
        &self,
        hosted: bool,
    ) -> (Workspace, window::Id, window::Id) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let source = ViewerSource::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source, "Images".into());
        let viewer = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery of the verified journal loaded from disk.
        send(
            &mut workspace,
            viewer,
            Message::DocumentLoaded(Ok(document)),
        );
        (workspace, owner, viewer)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viewer(workspace: &Workspace, id: window::Id) -> &Viewer {
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected journal viewer");
    };
    viewer
}

fn send(workspace: &mut Workspace, id: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(id, message));
}

fn action(workspace: &mut Workspace, id: window::Id, event: Event) {
    send(workspace, id, Message::ScreenshotAction(event));
}

#[test]
fn journal_thumbnail_menu_requires_current_image_and_an_open_context() {
    let fixture = Fixture::new();
    let (mut workspace, owner, id) = fixture.open(true);
    let document = Arc::clone(viewer(&workspace, id).document.as_ref().unwrap());
    let row = Row::new(0, &document.timeline[0]).unwrap();
    action(
        &mut workspace,
        id,
        Event::ThumbnailAction(row.clone(), Choice::Edit),
    );
    assert!(workspace.journal_edit_for_owner(owner).is_none());
    action(
        &mut workspace,
        id,
        Event::ThumbnailMenu(row.clone(), Point::ORIGIN),
    );
    assert!(viewer(&workspace, id).thumbnail_menu.is_some());

    let updated = fixture.publish(OTHER);
    let current = Row::new(0, &updated.timeline[0]).unwrap();
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
        panic!("expected journal viewer");
    };
    let _ = viewer.replace_timeline(id, updated);
    action(
        &mut workspace,
        id,
        Event::ThumbnailAction(row, Choice::Export),
    );
    assert!(workspace.journal_edit_for_owner(owner).is_none());

    action(
        &mut workspace,
        id,
        Event::ThumbnailMenu(current.clone(), Point::ORIGIN),
    );
    action(
        &mut workspace,
        id,
        Event::ThumbnailAction(current.clone(), Choice::Export),
    );
    let dialog = workspace.journal_edit_for_owner(owner).unwrap();
    assert_eq!(
        workspace.journal_title(dialog).as_deref(),
        Some(screenshot_export::TITLE)
    );
    assert!(workspace.close_journal_edit(dialog).is_some());
    action(
        &mut workspace,
        id,
        Event::ThumbnailMenu(current.clone(), Point::ORIGIN),
    );
    action(
        &mut workspace,
        id,
        Event::ThumbnailAction(current, Choice::Edit),
    );
    let dialog = workspace.journal_edit_for_owner(owner).unwrap();
    assert_eq!(
        workspace.journal_title(dialog).as_deref(),
        Some("Annotate screenshot")
    );
}

#[test]
fn journal_screenshot_access_binds_native_and_hosted_editor_menus_to_current_rows() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, owner, id) = fixture.open(hosted);
        send(
            &mut workspace,
            id,
            Message::EntryEditor(entry_editor::Event::Toggle),
        );
        let document = Arc::clone(viewer(&workspace, id).document.as_ref().unwrap());
        let row = Row::new(0, &document.timeline[0]).unwrap();
        action(
            &mut workspace,
            id,
            Event::EntryAction(row.clone(), Choice::Export),
        );
        assert!(workspace.journal_edit_for_owner(owner).is_none());
        action(
            &mut workspace,
            id,
            Event::EntryMenu(row.clone(), Point::ORIGIN),
        );
        assert_eq!(
            viewer(&workspace, id).entry_editor.screenshot_menu,
            Some((row.clone(), Point::ORIGIN))
        );

        let changed = fixture.publish(OTHER);
        // Test double for Iced delivery of the refreshed document from disk.
        let Mode::Viewer(current) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
            panic!("expected journal viewer");
        };
        let _ = current.replace_timeline(id, changed);
        action(
            &mut workspace,
            id,
            Event::EntryAction(row.clone(), Choice::Export),
        );
        assert!(workspace.journal_edit_for_owner(owner).is_none());

        let Mode::Viewer(current) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
            panic!("expected journal viewer");
        };
        let _ = current.replace_timeline(id, fixture.publish(REFERENCE));
        action(
            &mut workspace,
            id,
            Event::EntryMenu(row.clone(), Point::ORIGIN),
        );
        action(&mut workspace, id, Event::EntryAction(row, Choice::Export));
        let dialog = workspace.journal_edit_for_owner(owner).unwrap();
        assert_eq!(
            workspace.journal_title(dialog).as_deref(),
            Some(screenshot_export::TITLE)
        );
        assert!(workspace.journal_view(dialog).is_some());
        assert!(workspace.close_journal_edit(dialog).is_some());
        assert!(workspace.journal_edit_for_owner(owner).is_none());
    }
}

#[test]
fn journal_screenshot_access_requires_the_displayed_revision_and_current_policy() {
    let fixture = Fixture::new();
    let (mut workspace, owner, id) = fixture.open(false);
    send(&mut workspace, id, Message::SelectEntry(0, "screen".into()));
    send(
        &mut workspace,
        id,
        Message::ShowScreenshot(0, "screen".into()),
    );
    let current = viewer(&workspace, id);
    let revision = current.page_revision;
    let asset = Displayed::load(&current.source, "screen", REFERENCE).unwrap();
    // Test double for Iced delivery of the PNG actually read and decoded from disk.
    send(
        &mut workspace,
        id,
        Message::ScreenshotLoaded(revision, Ok(Arc::new(asset))),
    );
    action(
        &mut workspace,
        id,
        Event::ImageMenu(revision, Point::ORIGIN),
    );
    assert_eq!(viewer(&workspace, id).screenshot_menu, Some(Point::ORIGIN));
    action(
        &mut workspace,
        id,
        Event::ImageAction(revision.wrapping_sub(1), Choice::Export),
    );
    assert!(workspace.journal_edit_for_owner(owner).is_none());
    workspace.clipboard.policy.session_journal_allowed = false;
    action(
        &mut workspace,
        id,
        Event::ImageAction(revision, Choice::Export),
    );
    assert!(workspace.journal_edit_for_owner(owner).is_none());
    workspace.clipboard.policy.session_journal_allowed = true;
    action(
        &mut workspace,
        id,
        Event::ImageAction(revision, Choice::Export),
    );
    let dialog = workspace.journal_edit_for_owner(owner).unwrap();
    assert_eq!(
        workspace.journal_title(dialog).as_deref(),
        Some(screenshot_export::TITLE)
    );
    assert!(workspace.close_journal_edit(dialog).is_some());
    action(
        &mut workspace,
        id,
        Event::ImageMenu(revision, Point::ORIGIN),
    );
    action(
        &mut workspace,
        id,
        Event::ImageAction(revision, Choice::Edit),
    );
    let editor = workspace.journal_edit_for_owner(owner).unwrap();
    assert_eq!(
        workspace.journal_title(editor).as_deref(),
        Some(super::super::screenshot_editor::TITLE)
    );
}
