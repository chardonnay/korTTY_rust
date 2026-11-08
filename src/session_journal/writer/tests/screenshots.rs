use crate::session_journal::annotation::{self, Annotation, Kind};
use crate::session_journal::screenshot_ai::Analysis;
use crate::session_journal::screenshot_edit::Draft as ScreenshotDraft;

use super::*;

fn png() -> Vec<u8> {
    let mut image = tiny_skia::Pixmap::new(32, 24).unwrap();
    image.fill(tiny_skia::Color::WHITE);
    image.encode_png().unwrap()
}

fn line(y: f32) -> Annotation {
    Annotation {
        kind: Kind::Pen,
        color: [0xe1, 0x1d, 0x48],
        stroke_width: 2.0,
        font_size: 28.0,
        text: String::new(),
        points: vec![[3.0, y], [25.0, y]],
    }
}

fn stored_marks(capture: &Capture, id: &str) -> Vec<Annotation> {
    let xml =
        fs::read_to_string(capture.directory().join(crate::session_journal::DOCUMENT)).unwrap();
    let document = roxmltree::Document::parse(&xml).unwrap();
    annotation::read(crate::session_journal::entry_edit::find_entry(&document, id).unwrap())
        .unwrap()
}

fn append_note(sink: &Sink, text: &str) {
    sink.try_write(RecordKind::Note, Zeroizing::new(text.into()), false)
        .unwrap();
}

#[test]
fn live_screenshot_analysis_uses_committed_receipt_and_preserves_latest_note_through_finish() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let original = png();
    let receipt = wait_edit(sink.try_screenshot_with_receipt(original.clone()).unwrap()).unwrap();
    assert_eq!(capture.published().1, 1);
    let source = viewer::Source::capture(&capture);
    let asset =
        Arc::new(viewer::Screenshot::review(&source, &receipt.id, &receipt.reference).unwrap());
    assert_eq!(asset.bytes(), original);
    let draft = Arc::new(ScreenshotDraft::read(&source, &receipt.id, &receipt.reference).unwrap());
    let edit = draft.edit(Vec::new(), "Most recent note".into()).unwrap();
    wait_edit(capture.edit_screenshot(edit).unwrap()).unwrap();

    let analysis = Arc::new(Analysis {
        description: "Panel <ready>".into(),
        tags: vec!["build".into()],
        model: "local vision".into(),
    });
    let stored = wait_edit(
        capture
            .store_screenshot_analysis(asset.clone(), analysis.clone())
            .unwrap(),
    )
    .unwrap();
    assert!(stored.changed && !stored.durability_warning);
    wait_timeline(&capture, 3);
    let repeated = wait_edit(
        capture
            .store_screenshot_analysis(asset.clone(), analysis)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(repeated, screenshot_analysis_store::Outcome::default());
    assert_eq!(capture.published().1, 3);

    let visible = capture.directory().join(&receipt.reference);
    fs::rename(&visible, visible.with_extension("previous")).unwrap();
    fs::write(&visible, &original).unwrap();
    let stale = Arc::new(Analysis {
        description: "Result for the old file identity".into(),
        tags: Vec::new(),
        model: "local vision".into(),
    });
    assert!(wait_edit(capture.store_screenshot_analysis(asset, stale).unwrap()).is_err());
    assert_eq!(capture.status(), Status::Recording);
    append_note(&sink, "Still capturing after a stale analysis");
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);

    let xml =
        fs::read_to_string(capture.directory().join(crate::session_journal::DOCUMENT)).unwrap();
    let document = roxmltree::Document::parse(&xml).unwrap();
    let entry = crate::session_journal::entry_edit::find_entry(&document, &receipt.id).unwrap();
    let entry_xml = &xml[entry.range()];
    for fragment in [
        "<aiDescription>Panel &lt;ready&gt;</aiDescription>",
        "<aiTags><tag>build</tag></aiTags>",
        "<aiAnalysisModel>local vision</aiAnalysisModel>",
        "<userNote>Most recent note</userNote>",
        "<editedAt>",
        "<logStartSeq>1</logStartSeq>",
    ] {
        assert!(entry_xml.contains(fragment), "missing XML field {fragment}");
    }
    assert!(!xml.contains("Result for the old file identity"));
    assert!(xml.contains("Still capturing after a stale analysis"));
}

#[test]
fn live_screenshot_edit_preserves_marks_across_other_updates_and_restores_original() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let original = png();
    sink.try_screenshot(original.clone()).unwrap();
    wait_timeline(&capture, 1);
    let source = viewer::Source::capture(&capture);
    let document = read(capture.directory()).unwrap();
    let row = &document.timeline[0];
    let reference = row.screenshot.as_deref().unwrap();
    let visible = capture.directory().join(reference);
    let backup = visible.with_extension("orig.png");
    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let xml_path = capture.directory().join(crate::session_journal::DOCUMENT);
    let original_xml = fs::read(&xml_path).unwrap();
    let unchanged = draft
        .edit(draft.annotations().to_vec(), draft.note().to_owned())
        .unwrap();
    assert_eq!(
        wait_edit(capture.edit_screenshot(unchanged).unwrap()).unwrap(),
        crate::session_journal::screenshot_edit::Outcome::default()
    );
    assert_eq!(fs::read(xml_path).unwrap(), original_xml);
    assert!(!backup.exists());

    let first = draft.edit(vec![line(6.0)], "First note".into()).unwrap();
    let stale = draft.edit(vec![line(18.0)], "Stale note".into()).unwrap();

    append_note(&sink, "Queued before the screenshot edit");
    let outcome = wait_edit(capture.edit_screenshot(first).unwrap()).unwrap();
    assert!(!outcome.durability_warning && !outcome.cleanup_warning);
    wait_timeline(&capture, 3);
    assert_eq!(stored_marks(&capture, &row.id), [line(6.0)]);
    assert_eq!(fs::read(&backup).unwrap(), original);
    let first_png = fs::read(&visible).unwrap();
    assert_ne!(first_png, original);
    assert!(wait_edit(capture.edit_screenshot(stale).unwrap()).is_err());
    assert_eq!(fs::read(&visible).unwrap(), first_png);

    // A normal note edit must preserve the annotation model and its original image.
    let fields = Draft::read(&source, &row.id).unwrap();
    let update = fields
        .edit(Changes::Fields {
            fields: Fields {
                note: "Edited via the normal entry editor".into(),
                ..fields.fields().clone()
            },
            marker: fields.marker().clone(),
        })
        .unwrap();
    wait_edit(capture.edit_entry(update).unwrap()).unwrap();
    wait_timeline(&capture, 4);
    assert_eq!(stored_marks(&capture, &row.id), [line(6.0)]);
    assert_eq!(fs::read(&visible).unwrap(), first_png);

    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let stale_image = draft.edit(vec![line(18.0)], "Stale image".into()).unwrap();
    let held = visible.with_extension("saved.png");
    fs::rename(&visible, &held).unwrap();
    fs::write(&visible, &first_png).unwrap();
    assert!(wait_edit(capture.edit_screenshot(stale_image).unwrap()).is_err());
    assert_eq!(fs::read(&visible).unwrap(), first_png);
    assert_eq!(stored_marks(&capture, &row.id), [line(6.0)]);
    fs::remove_file(&visible).unwrap();
    fs::rename(held, &visible).unwrap();

    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let edit = draft
        .edit(vec![line(18.0)], "Replaced marks".into())
        .unwrap();
    wait_edit(capture.edit_screenshot(edit).unwrap()).unwrap();
    wait_timeline(&capture, 5);
    let new = tiny_skia::Pixmap::decode_png(&fs::read(&visible).unwrap()).unwrap();
    let base = tiny_skia::Pixmap::decode_png(&original).unwrap();
    assert_eq!(new.pixel(10, 6), base.pixel(10, 6));
    assert_ne!(new.pixel(10, 18), base.pixel(10, 18));
    assert_eq!(fs::read(&backup).unwrap(), original);

    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let clear = draft.edit(Vec::new(), "Cleared marks".into()).unwrap();
    let result = capture.edit_screenshot(clear).unwrap();
    // Finish drains accepted mutations before writing the final journal document.
    wait(capture.finish());
    let outcome = wait_edit(result).unwrap();
    assert!(!outcome.durability_warning && !outcome.cleanup_warning);
    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(fs::read(&visible).unwrap(), original);
    assert!(!backup.exists());
    assert!(stored_marks(&capture, &row.id).is_empty());
    let final_document = read(capture.directory()).unwrap();
    let final_row = final_document
        .timeline
        .iter()
        .find(|entry| entry.id == row.id)
        .unwrap();
    assert_eq!(final_row.note, "Cleared marks");
    assert_eq!(final_row.screenshot, row.screenshot);
    assert_eq!(final_row.log_start, row.log_start);
    assert_eq!(final_row.log_end, row.log_end);
}

#[test]
fn live_screenshot_edit_distinguishes_rollback_from_published_durability_warning() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let original = png();
    sink.try_screenshot(original.clone()).unwrap();
    wait_timeline(&capture, 1);
    let source = viewer::Source::capture(&capture);
    let document = read(capture.directory()).unwrap();
    let row = &document.timeline[0];
    let reference = row.screenshot.as_deref().unwrap();
    let visible = capture.directory().join(reference);
    let backup = visible.with_extension("orig.png");
    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let edit = draft.edit(vec![line(6.0)], "Not committed".into()).unwrap();

    let pending = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&pending).unwrap();
    assert!(wait_edit(capture.edit_screenshot(edit).unwrap()).is_err());
    fs::remove_dir(pending).unwrap();
    assert_eq!(fs::read(&visible).unwrap(), original);
    assert!(!backup.exists());
    assert!(stored_marks(&capture, &row.id).is_empty());
    assert_eq!(capture.status(), Status::Recording);
    assert_eq!(capture.published().1, 1);

    // Test double injects the sync failure after the real XML and image are installed.
    assert!(
        sink.shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let draft = Arc::new(ScreenshotDraft::read(&source, &row.id, reference).unwrap());
    let edit = draft.edit(vec![line(18.0)], "Committed".into()).unwrap();
    let outcome = wait_edit(capture.edit_screenshot(edit).unwrap()).unwrap();
    assert!(outcome.durability_warning);
    append_note(&sink, "Recording continues after the warning");
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_ne!(fs::read(&visible).unwrap(), original);
    assert_eq!(stored_marks(&capture, &row.id), [line(18.0)]);
    let final_document = read(capture.directory()).unwrap();
    let final_row = final_document
        .timeline
        .iter()
        .find(|entry| entry.id == row.id)
        .unwrap();
    assert_eq!(final_row.note, "Committed");
}
