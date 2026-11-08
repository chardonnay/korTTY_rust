use crate::terminal_clipboard::{self, ReadKind};

use super::super::Row;
use super::super::tests::{Fixture, REFERENCE};
use super::*;

#[test]
fn journal_screenshot_copy_path_requires_current_context_and_stays_relative() {
    let fixture = Fixture::new();
    let (mut workspace, _, viewer) = fixture.open(true);
    workspace.clipboard.policy.internal = true;
    workspace.clipboard.buffer.set("unchanged".into());
    let Mode::Viewer(current) = &workspace.journals[&viewer].mode else {
        panic!("expected journal viewer");
    };
    let row = Row::new(0, &current.document.as_ref().unwrap().timeline[0]).unwrap();
    let action = |event| Desktop::SessionJournal(viewer, Message::ScreenshotAction(event));
    let _ = workspace.update(action(Event::ThumbnailAction(
        row.clone(),
        Choice::CopyPath,
    )));
    assert_eq!(
        workspace.clipboard.buffer.get().as_deref(),
        Some("unchanged")
    );
    let _ = workspace.update(action(Event::ThumbnailMenu(
        row.clone(),
        iced::Point::ORIGIN,
    )));
    let _ = workspace.update(action(Event::StepThumbnailAction(row.clone(), true)));
    let _ = workspace.update(action(Event::ThumbnailAction(row, Choice::CopyPath)));
    assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some(REFERENCE));
    assert!(workspace.clipboard.buffer.png().is_none());
}

#[test]
fn journal_screenshot_copy_keeps_png_bytes_internal_and_rejects_stale_completion() {
    let fixture = Fixture::new();
    let (mut workspace, _, viewer) = fixture.open(false);
    workspace.clipboard.policy.internal = true;
    let Mode::Viewer(current) = &workspace.journals[&viewer].mode else {
        panic!("expected journal viewer");
    };
    let source = current.source.clone();
    let target = || Target::row(source.clone(), "screen".into(), REFERENCE.into());
    let bytes = std::fs::read(fixture.0.join(REFERENCE)).unwrap();
    let _ = workspace.copy_journal_screenshot(Origin::Viewer(viewer), target(), Choice::CopyImage);
    let ticket = workspace
        .journal_screenshot_copies
        .pending
        .as_ref()
        .unwrap()
        .ticket;
    // Test double for Iced delivery of the real, source-verified PNG preparation.
    let prepared = Arc::new(prepare(target(), true).unwrap());
    let _ = workspace.screenshot_copy_prepared(ticket, Ok(Arc::clone(&prepared)));
    assert!(workspace.clipboard.buffer.get().is_none());
    let png = workspace.clipboard.buffer.png().unwrap();
    assert_eq!(png.as_ref(), bytes);
    let result = terminal_clipboard::png_result(
        ReadKind::Data(vec!["text/plain".into(), "image/png".into()]),
        &png,
    );
    let kortty_terminal::ClipboardResult::Data(mimes) = result else {
        panic!("expected clipboard MIME data");
    };
    assert_eq!(mimes, [("image/png".into(), bytes)]);
    workspace.clipboard.buffer.set("new text".into());
    let _ = workspace.copy_journal_screenshot(Origin::Viewer(viewer), target(), Choice::CopyImage);
    let next = workspace
        .journal_screenshot_copies
        .pending
        .as_ref()
        .unwrap()
        .ticket;
    let _ = workspace.screenshot_copy_prepared(ticket, Ok(Arc::clone(&prepared)));
    assert!(workspace.clipboard.buffer.png().is_none());
    workspace.journals.remove(&viewer);
    let _ = workspace.screenshot_copy_prepared(next, Ok(prepared));
    assert!(workspace.journal_screenshot_copies.pending.is_none());
    assert_eq!(
        workspace.clipboard.buffer.get().as_deref(),
        Some("new text")
    );
}
