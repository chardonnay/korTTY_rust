use std::fs;
use std::path::PathBuf;

use crate::session_journal::{self, Capture, Metadata};
use crate::workspace::journal_capture_access::DockSource;
use crate::workspace::journal_dock::{Publication, Revision};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-live-journal-screenshot-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn capture(&self, run: u64, width: u32) -> (DockSource, Vec<TimelineEntry>, Key) {
        let capture =
            Arc::new(Capture::start(self.0.join("journals"), Metadata::default()).unwrap());
        let png = tiny_skia::Pixmap::new(width, 3)
            .unwrap()
            .encode_png()
            .unwrap();
        capture.sink().try_screenshot(png).unwrap();
        let _ = capture.finish().recv_blocking();
        assert_eq!(capture.status(), session_journal::Status::Complete);
        let entries = session_journal::read(capture.directory()).unwrap().timeline;
        let key = entries.iter().find_map(Key::for_entry).unwrap();
        (
            DockSource {
                tab: 7,
                run,
                title: "Captured journal".into(),
                capture,
            },
            entries,
            key,
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn journal_dock_observes_publications_without_repeating_unchanged_updates() {
    let scratch = Scratch::new();
    let capture =
        Arc::new(Capture::start(scratch.0.join("journals"), Metadata::default()).unwrap());
    let mut publication = Publication {
        run: 1,
        seen: Revision::of(&capture),
        capture: Arc::clone(&capture),
    };
    assert!(!publication.changed());

    let png = tiny_skia::Pixmap::new(4, 3).unwrap().encode_png().unwrap();
    capture
        .sink()
        .try_screenshot_with_receipt(png)
        .unwrap()
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert!(publication.changed());
    assert!(!publication.changed());

    let _ = capture.finish().recv_blocking();
    assert!(publication.changed());
    assert!(publication.seen.closed);
    assert!(!publication.changed());
}

#[test]
fn journal_dock_screenshot_keeps_the_lightbox_on_scroll_and_ignores_a_late_run() {
    let scratch = Scratch::new();
    let (first, first_entries, first_key) = scratch.capture(41, 2);
    let (next, next_entries, next_key) = scratch.capture(42, 4);
    let first_source = Source::capture(&first.capture);
    let next_source = Source::capture(&next.capture);
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let dock = &mut workspace.windows.get_mut(&owner).unwrap().journal_dock;
    assert!(dock.place(Placement::Right));
    dock.bind(first);
    dock.bound.as_mut().unwrap().timeline.entries = Arc::new(first_entries);
    let _ = workspace.update_journal_dock(
        owner,
        Message::Bound(7, 41, Action::ShowScreenshot(first_key.clone())),
    );
    let old_ticket = workspace.windows[&owner]
        .journal_dock
        .images
        .pending
        .unwrap();

    let dock = &mut workspace.windows.get_mut(&owner).unwrap().journal_dock;
    assert!(dock.place(Placement::Hidden));
    assert!(dock.place(Placement::Left));
    dock.bind(next);
    dock.bound.as_mut().unwrap().timeline.entries = Arc::new(next_entries);
    let _ = workspace.update_journal_dock(
        owner,
        Message::Bound(7, 42, Action::ShowScreenshot(next_key.clone())),
    );
    assert_eq!(
        workspace.windows[&owner].journal_dock.images.pending,
        Some(old_ticket)
    );

    for reader in &workspace.pending_shutdown {
        let _ = reader.recv_blocking();
    }
    // Test double for delivery of Iced's worker result. The reviewed image is from real disk.
    let old =
        Arc::new(Displayed::load(&first_source, first_key.id(), first_key.reference()).unwrap());
    let _ = workspace.update_journal_dock(owner, Message::ScreenshotLoaded(old_ticket, Ok(old)));
    let dock = &workspace.windows[&owner].journal_dock;
    let next_ticket = dock.images.pending.unwrap();
    assert_ne!(next_ticket, old_ticket);
    assert!(
        dock.bound
            .as_ref()
            .unwrap()
            .images
            .full
            .as_ref()
            .unwrap()
            .result
            .is_none()
    );

    for reader in &workspace.pending_shutdown {
        let _ = reader.recv_blocking();
    }
    // Test double for delivery of the second real, independently reviewed image.
    let shown =
        Arc::new(Displayed::load(&next_source, next_key.id(), next_key.reference()).unwrap());
    let _ = workspace.update_journal_dock(
        owner,
        Message::ScreenshotLoaded(next_ticket, Ok(Arc::clone(&shown))),
    );
    let _ = workspace.update_journal_dock(
        owner,
        Message::ThumbnailVisible(7, 42, next_key.clone(), false),
    );
    let dock = &workspace.windows[&owner].journal_dock;
    let state = &dock.bound.as_ref().unwrap().images;
    assert!(Arc::ptr_eq(reviewed(state, &next_key).unwrap(), &shown));

    let (other, _) = workspace.open_window(false);
    for (choice, title) in [
        (Choice::Export, "Export screenshot"),
        (Choice::Edit, "Annotate screenshot"),
    ] {
        let _ = workspace.update_journal_dock(
            owner,
            Message::Bound(
                7,
                42,
                Action::ScreenshotMenu {
                    key: next_key.clone(),
                    full: true,
                    point: Point::new(12.0, 18.0),
                },
            ),
        );
        let _ = workspace.update_journal_dock(
            owner,
            Message::Bound(
                7,
                42,
                Action::ChooseScreenshot {
                    key: next_key.clone(),
                    full: true,
                    choice,
                },
            ),
        );
        let dialog = workspace.journal_edit_for_owner(owner).unwrap();
        assert_eq!(workspace.journal_title(dialog).as_deref(), Some(title));
        assert!(workspace.journal_edit_for_owner(other).is_none());
        assert!(workspace.dock_screenshot_source_current(owner, 7, 42, &next_source));
        assert!(!workspace.dock_screenshot_source_current(owner, 7, 41, &first_source));
        assert!(workspace.close_journal_edit(dialog).is_some());
    }

    assert!(workspace.dismiss_journal_dock_screenshot(owner));
    assert!(!workspace.dismiss_journal_dock_screenshot(owner));

    let dock = &mut workspace.windows.get_mut(&owner).unwrap().journal_dock;
    dock.bound
        .as_mut()
        .unwrap()
        .images
        .visible(next_key.clone(), true);
    let _ = workspace.update_journal_dock(owner, Message::Bound(7, 42, Action::Refresh));
    let bound = workspace.windows[&owner]
        .journal_dock
        .bound
        .as_ref()
        .unwrap();
    assert!(bound.refresh);
    assert_eq!(bound.images.visible, [next_key]);
}
