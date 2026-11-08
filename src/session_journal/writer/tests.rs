use std::fs;
use std::io::Read as _;
use std::time::{Duration, Instant};

use crate::session_journal::entry_edit::{Changes, Draft, Fields, LegacyMarker};
use crate::session_journal::{CaptureKind, capture_page, read, read_screenshot};

use super::*;

mod markers;
mod screenshots;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().canonicalize().unwrap();
        let path = base.join(format!("kortty-journal-writer-{}", uuid().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn journals(&self) -> PathBuf {
        self.0.join("journals")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait(done: Receiver<()>) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "journal writer did not finish");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        done.try_recv(),
        Err(async_channel::TryRecvError::Closed)
    ));
}

fn wait_edit<T>(result: Receiver<Result<T, String>>) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match result.try_recv() {
            Ok(result) => return result,
            Err(async_channel::TryRecvError::Closed) => {
                panic!("journal mutation was not acknowledged");
            }
            Err(async_channel::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "journal mutation timed out");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn wait_timeline(capture: &Capture, revision: usize) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while capture.published().1 < revision {
        assert!(
            Instant::now() < deadline,
            "journal timeline was not published"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn archived_part(directory: &Path, name: &str) -> String {
    let file = fs::File::open(directory.join(format!("{name}.zst"))).unwrap();
    assert!(!directory.join(name).exists());
    let mut text = String::new();
    crate::logfile_window::zstandard::Decoder::new(file, || false)
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[test]
fn live_entry_deletion_rechecks_row_and_preserves_historical_capture_bytes() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    sink.try_write(
        RecordKind::Note,
        Zeroizing::new("Retained note".into()),
        false,
    )
    .unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    sink.try_screenshot(png.clone()).unwrap();
    wait_timeline(&capture, 2);

    let document = read(capture.directory()).unwrap();
    let retained = document
        .timeline
        .iter()
        .find(|entry| entry.kind == "USER_NOTE")
        .unwrap();
    let screenshot = document
        .timeline
        .iter()
        .find(|entry| entry.kind == "SCREENSHOT")
        .unwrap();
    let image = capture
        .directory()
        .join(screenshot.screenshot.as_deref().unwrap());
    let original = image.with_extension("orig.png");
    fs::copy(&image, &original).unwrap();
    let source = viewer::Source::capture(&capture);
    let draft = Draft::read(&source, &screenshot.id).unwrap();
    let stale = Arc::new(draft.prepare_delete().unwrap());
    let marker = draft
        .markers()
        .iter()
        .find(|marker| marker.legacy() == LegacyMarker::Error)
        .unwrap()
        .clone();
    wait_edit(
        capture
            .edit_entry(draft.edit(Changes::Marker(marker)).unwrap())
            .unwrap(),
    )
    .unwrap();
    wait_timeline(&capture, 3);

    let xml_path = capture.directory().join("journal.xml");
    assert!(
        fs::read_to_string(&xml_path)
            .unwrap()
            .contains("<errorCount>1</errorCount>")
    );
    let log = capture.directory().join("session-log.json");
    let log_before = fs::read(&log).unwrap();
    assert!(wait_edit(capture.delete_entry(stale).unwrap()).is_err());
    assert_eq!(fs::read(&image).unwrap(), png);
    let confirmed = Draft::read(&source, &screenshot.id)
        .unwrap()
        .prepare_delete()
        .unwrap();
    let result = wait_edit(capture.delete_entry(Arc::new(confirmed)).unwrap()).unwrap();
    assert!(!result.cleanup_warning && !result.durability_warning);
    wait_timeline(&capture, 4);
    assert_eq!(capture.published(), (2, 4, false));
    assert!(!image.exists());
    assert_eq!(fs::read(&original).unwrap(), png);
    assert_eq!(fs::read(&log).unwrap(), log_before);
    let current = read(capture.directory()).unwrap();
    assert_eq!(current.timeline.len(), 1);
    assert_eq!(current.timeline[0].id, retained.id);
    assert_eq!(current.timeline[0].log_start, retained.log_start);
    let xml = fs::read_to_string(&xml_path).unwrap();
    assert!(xml.contains("<errorCount>0</errorCount>"));
    assert!(xml.contains("<screenshotCount>1</screenshotCount>"));
    assert!(xml.contains("<logEntryCount>2</logEntryCount>"));

    sink.try_write(
        RecordKind::Note,
        Zeroizing::new("Queued after deletion".into()),
        false,
    )
    .unwrap();
    wait(capture.finish());
    assert_eq!(capture.published(), (3, 5, true));
    let finalized = read(capture.directory()).unwrap();
    assert!(
        finalized
            .timeline
            .iter()
            .any(|entry| entry.text == "Queued after deletion")
    );
    assert!(
        finalized
            .timeline
            .iter()
            .all(|entry| entry.id != screenshot.id)
    );
    assert!(
        archived_part(capture.directory(), "session-log.json")
            .as_bytes()
            .starts_with(&log_before)
    );
}

#[test]
fn live_entry_deletion_distinguishes_rollback_durability_and_asset_warnings() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    sink.try_screenshot(png.clone()).unwrap();
    wait_timeline(&capture, 1);
    sink.try_screenshot(png.clone()).unwrap();
    wait_timeline(&capture, 2);
    let document = read(capture.directory()).unwrap();
    let source = viewer::Source::capture(&capture);
    let [first, second] = document.timeline.as_slice() else {
        panic!("expected two screenshots")
    };
    let image = |entry: &crate::session_journal::TimelineEntry| {
        capture
            .directory()
            .join(entry.screenshot.as_deref().unwrap())
    };
    let first_image = image(first);
    let second_image = image(second);
    let deletion = Arc::new(
        Draft::read(&source, &first.id)
            .unwrap()
            .prepare_delete()
            .unwrap(),
    );

    let pending = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&pending).unwrap();
    assert!(wait_edit(capture.delete_entry(Arc::clone(&deletion)).unwrap()).is_err());
    assert_eq!(read(capture.directory()).unwrap().timeline.len(), 2);
    assert_eq!(fs::read(&first_image).unwrap(), png);
    assert_eq!(capture.published(), (2, 2, false));
    fs::remove_dir(pending).unwrap();

    // Test double injects the durability failure after the real atomic XML replacement.
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let outcome = wait_edit(capture.delete_entry(deletion).unwrap()).unwrap();
    assert!(outcome.durability_warning && !outcome.cleanup_warning);
    wait_timeline(&capture, 3);
    assert_eq!(read(capture.directory()).unwrap().timeline[0].id, second.id);
    assert_eq!(fs::read(&first_image).unwrap(), png);

    let deletion = Draft::read(&source, &second.id)
        .unwrap()
        .prepare_delete()
        .unwrap();
    let moved = second_image.with_extension("saved.png");
    fs::rename(&second_image, &moved).unwrap();
    fs::write(&second_image, &png).unwrap();
    let outcome = wait_edit(capture.delete_entry(Arc::new(deletion)).unwrap()).unwrap();
    assert!(outcome.cleanup_warning && !outcome.durability_warning);
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    assert!(read(capture.directory()).unwrap().timeline.is_empty());
    assert_eq!(fs::read(&second_image).unwrap(), png);
    assert_eq!(fs::read(&moved).unwrap(), png);
}

#[test]
fn live_entry_deletion_releases_all_three_text_fields_from_the_timeline_quota() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    for text in ["First", "Second"] {
        capture
            .sink()
            .try_write(RecordKind::Note, Zeroizing::new(text.into()), false)
            .unwrap();
    }
    wait_timeline(&capture, 2);
    let document = read(capture.directory()).unwrap();
    let source = viewer::Source::capture(&capture);
    let first = Draft::read(&source, &document.timeline[0].id).unwrap();
    let second = Draft::read(&source, &document.timeline[1].id).unwrap();
    let edit = |draft: &Draft| {
        draft
            .edit(Changes::Fields {
                fields: Fields {
                    title: "T".repeat(200 * 1024),
                    text: "t".repeat(200 * 1024),
                    note: "N".repeat(200 * 1024),
                },
                marker: draft.marker().clone(),
            })
            .unwrap()
    };
    wait_edit(capture.edit_entry(edit(&first)).unwrap()).unwrap();
    let blocked = wait_edit(capture.edit_entry(edit(&second)).unwrap()).unwrap_err();
    assert!(blocked.contains("timeline text exceeds its storage limit"));
    wait_timeline(&capture, 3);
    let deletion = Draft::read(&source, first.id())
        .unwrap()
        .prepare_delete()
        .unwrap();
    let outcome = wait_edit(capture.delete_entry(Arc::new(deletion)).unwrap()).unwrap();
    assert!(!outcome.cleanup_warning && !outcome.durability_warning);
    wait_edit(capture.edit_entry(edit(&second)).unwrap()).unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(capture.published(), (2, 5, true));
    let document = read(capture.directory()).unwrap();
    assert_eq!(document.timeline.len(), 1);
    assert_eq!(document.timeline[0].id, second.id());
    assert_eq!(document.timeline[0].title.len(), 200 * 1024);
    assert_eq!(document.timeline[0].text.len(), 200 * 1024);
    assert_eq!(document.timeline[0].note.len(), 200 * 1024);
}

#[test]
fn live_entry_edit_preserves_capture_anchors_and_rejects_a_stale_draft() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    sink.try_write(
        RecordKind::Note,
        Zeroizing::new("Original note".into()),
        false,
    )
    .unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    sink.try_screenshot(png.clone()).unwrap();
    wait_timeline(&capture, 2);

    let original = read(capture.directory()).unwrap();
    let source = viewer::Source::capture(&capture);
    let note = original
        .timeline
        .iter()
        .find(|entry| entry.kind == "USER_NOTE")
        .unwrap();
    let screenshot = original
        .timeline
        .iter()
        .find(|entry| entry.kind == "SCREENSHOT")
        .unwrap();
    let draft = Draft::read(&source, &note.id).unwrap();
    let error = draft
        .markers()
        .iter()
        .find(|marker| marker.legacy() == LegacyMarker::Error)
        .unwrap()
        .clone();
    let update = draft
        .edit(Changes::Fields {
            fields: Fields {
                title: "  Edited & <title>  ".into(),
                text: "Updated text".into(),
                note: "Private note".into(),
            },
            marker: error,
        })
        .unwrap();
    let saved = capture.edit_entry(update).unwrap();
    sink.try_write(RecordKind::Note, Zeroizing::new("Later note".into()), false)
        .unwrap();
    wait_edit(saved).unwrap();
    let stale = draft.edit(Changes::Marker(draft.marker().clone())).unwrap();
    assert!(
        wait_edit(capture.edit_entry(stale).unwrap())
            .unwrap_err()
            .contains("entry changed")
    );

    // Read the screenshot draft only after the previously queued note was published.
    wait_timeline(&capture, 4);
    let draft = Draft::read(&source, &screenshot.id).unwrap();
    let marker = draft
        .markers()
        .iter()
        .find(|marker| marker.legacy() == LegacyMarker::Important)
        .unwrap()
        .clone();
    let edited = capture
        .edit_entry(draft.edit(Changes::Marker(marker)).unwrap())
        .unwrap();
    wait(capture.finish());
    wait_edit(edited).unwrap();
    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(capture.published(), (3, 5, true));

    let saved = read(capture.directory()).unwrap();
    let saved_note = saved
        .timeline
        .iter()
        .find(|entry| entry.id == note.id)
        .unwrap();
    assert_eq!(saved_note.title, "Edited & <title>");
    assert_eq!(saved_note.text, "Updated text");
    assert_eq!(saved_note.note, "Private note");
    assert_eq!(saved_note.marker_key.as_deref(), Some("error"));
    assert_eq!(
        (
            saved_note.log_part,
            saved_note.log_start,
            saved_note.log_end
        ),
        (note.log_part, note.log_start, note.log_end)
    );
    let saved_shot = saved
        .timeline
        .iter()
        .find(|entry| entry.id == screenshot.id)
        .unwrap();
    assert_eq!(saved_shot.marker_key.as_deref(), Some("important"));
    assert_eq!(saved_shot.screenshot, screenshot.screenshot);
    assert_eq!(
        (
            saved_shot.log_part,
            saved_shot.log_start,
            saved_shot.log_end
        ),
        (
            screenshot.log_part,
            screenshot.log_start,
            screenshot.log_end
        )
    );
    assert_eq!(
        read_screenshot(
            capture.directory(),
            saved_shot.screenshot.as_deref().unwrap()
        )
        .unwrap(),
        png
    );
    assert!(
        saved
            .timeline
            .iter()
            .any(|entry| entry.text == "Later note")
    );
    let xml = fs::read_to_string(capture.directory().join("journal.xml")).unwrap();
    assert!(xml.contains("<errorCount>1</errorCount>"));
    assert_eq!(xml.matches("<editedAt>").count(), 2);
    assert_eq!(xml.matches("<markerSource>USER</markerSource>").count(), 2);
    let log = capture_page(capture.directory(), None, None, None).unwrap();
    assert_eq!(log.records.len(), 3);
    assert_eq!(log.records[0].text, "Original note");
    assert_eq!(log.records[2].text, "Later note");
}

#[test]
fn live_entry_edit_restores_unpublished_fields_and_keeps_published_fields_after_sync_error() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Before editing".into()),
            false,
        )
        .unwrap();
    wait_timeline(&capture, 1);
    let original = read(capture.directory()).unwrap();
    let source = viewer::Source::capture(&capture);
    let draft = Draft::read(&source, &original.timeline[0].id).unwrap();
    let edit = draft
        .edit(Changes::Fields {
            fields: Fields {
                text: "Published text".into(),
                ..draft.fields().clone()
            },
            marker: draft.marker().clone(),
        })
        .unwrap();
    let pending = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&pending).unwrap();
    let rejected = wait_edit(capture.edit_entry(edit.clone()).unwrap()).unwrap_err();
    assert!(rejected.starts_with("Could not save the session journal entry"));
    assert_eq!(
        read(capture.directory()).unwrap().timeline[0].text,
        "Before editing"
    );
    assert_eq!(capture.status(), Status::Recording);
    fs::remove_dir(pending).unwrap();

    // Test double injects the sync error after real atomic XML replacement.
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let published = wait_edit(capture.edit_entry(edit).unwrap()).unwrap_err();
    assert!(published.contains("durability could not be confirmed"));
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Still recording".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    let document = read(capture.directory()).unwrap();
    assert_eq!(document.timeline[0].text, "Published text");
    assert_eq!(document.timeline[1].text, "Still recording");
}

#[test]
fn live_metadata_edits_survive_later_notes_screenshots_and_finalization() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let title = capture
        .edit_metadata(MetadataEdit::Title("  Renamed & <literal>  ".into()))
        .unwrap();
    let description = capture
        .edit_metadata(MetadataEdit::Description(
            "First line\nSecond & <line>".into(),
        ))
        .unwrap();
    let pinned = capture.edit_metadata(MetadataEdit::Pinned(true)).unwrap();
    capture
        .sink()
        .try_write(RecordKind::Note, Zeroizing::new("Later note".into()), false)
        .unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    capture.sink().try_screenshot(png.clone()).unwrap();
    let finished = capture.finish();
    wait_edit(title).unwrap();
    wait_edit(description).unwrap();
    wait_edit(pinned).unwrap();
    wait(finished);
    assert_eq!(capture.status(), Status::Complete);
    let document = read(capture.directory()).unwrap();
    assert_eq!(document.summary.title, "Renamed & <literal>");
    assert_eq!(document.summary.description, "First line\nSecond & <line>");
    assert!(document.summary.pinned);
    assert!(document.summary.ended_at.is_some());
    assert_eq!(document.timeline[0].text, "Later note");
    assert_eq!(document.timeline.len(), 2);
    assert_eq!(
        read_screenshot(
            capture.directory(),
            document.timeline[1].screenshot.as_deref().unwrap()
        )
        .unwrap(),
        png
    );
    assert_eq!(
        capture
            .edit_metadata(MetadataEdit::Title("Too late".into()))
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn live_journal_pin_can_be_released_without_rewriting_an_unchanged_flag() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let path = capture.directory().join("journal.xml");
    wait_edit(capture.edit_metadata(MetadataEdit::Pinned(true)).unwrap()).unwrap();
    assert!(read(capture.directory()).unwrap().summary.pinned);
    let stamp = crate::private_files::FileStamp::read(&fs::File::open(&path).unwrap()).unwrap();
    wait_edit(capture.edit_metadata(MetadataEdit::Pinned(true)).unwrap()).unwrap();
    stamp
        .require_unchanged(&fs::File::open(&path).unwrap())
        .unwrap();
    wait_edit(capture.edit_metadata(MetadataEdit::Pinned(false)).unwrap()).unwrap();
    assert!(!read(capture.directory()).unwrap().summary.pinned);
    assert_eq!(capture.status(), Status::Recording);
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("After releasing pin".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    let document = read(capture.directory()).unwrap();
    assert_eq!(capture.status(), Status::Complete);
    assert!(!document.summary.pinned);
    assert_eq!(document.timeline[0].text, "After releasing pin");
}

#[test]
fn failed_metadata_save_preserves_fields_and_capture_remains_recording() {
    let fixture = Fixture::new();
    let capture = Capture::start(
        fixture.journals(),
        Metadata {
            title: "Keep this title".into(),
            description: "Keep this description".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let pending = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&pending).unwrap();
    let rejected = capture
        .edit_metadata(MetadataEdit::Title("Not committed".into()))
        .unwrap();
    assert!(wait_edit(rejected).is_err());
    assert!(wait_edit(capture.edit_metadata(MetadataEdit::Pinned(true)).unwrap()).is_err());
    assert!(pending.is_dir());
    assert_eq!(capture.status(), Status::Recording);
    assert_eq!(
        read(capture.directory()).unwrap().summary.title,
        "Keep this title"
    );
    fs::remove_dir(&pending).unwrap();
    wait_edit(
        capture
            .edit_metadata(MetadataEdit::Description("Saved later".into()))
            .unwrap(),
    )
    .unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Still recording".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    let document = read(capture.directory()).unwrap();
    assert_eq!(document.summary.title, "Keep this title");
    assert_eq!(document.summary.description, "Saved later");
    assert!(!document.summary.pinned);
    assert_eq!(document.timeline[0].text, "Still recording");
}

fn assert_metadata_version_conflict(capture: &Capture, expected: &str) {
    let result = capture
        .edit_metadata(MetadataEdit::Title("Writer must not replace this".into()))
        .unwrap();
    assert!(wait_edit(result).is_err());
    assert_eq!(capture.status(), Status::Recording);
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("A later note".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    assert!(matches!(capture.status(), Status::Failed(_)));
    assert_eq!(
        fs::read_to_string(capture.directory().join("journal.xml")).unwrap(),
        expected
    );
}

#[test]
fn replaced_journal_xml_survives_metadata_error_and_a_later_note() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let replacement = capture.directory().join("replacement.xml");
    let foreign = r#"<session-journal formatVersion="1" id="foreign-journal"><meta><title>External replacement</title></meta><entries/></session-journal>"#;
    fs::write(&replacement, foreign).unwrap();
    fs::rename(replacement, capture.directory().join("journal.xml")).unwrap();
    assert_metadata_version_conflict(&capture, foreign);
}

#[test]
fn external_same_id_xml_edit_survives_metadata_error_and_a_later_note() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let external = format!(
        r#"<session-journal formatVersion="1" id="{}"><meta><title>External title</title><description>External description preserved</description></meta><entries/></session-journal>"#,
        capture.id()
    );
    fs::write(capture.directory().join("journal.xml"), &external).unwrap();
    assert_metadata_version_conflict(&capture, &external);
}

#[test]
fn initial_metadata_publication_preserves_a_document_that_appeared() {
    let fixture = Fixture::new();
    let root = storage::Storage::root(&fixture.journals()).unwrap();
    let mut directory = root.child("unpublished").unwrap();
    directory.lock_metadata().unwrap();
    let path = fixture.journals().join("unpublished/journal.xml");
    fs::write(&path, b"External initial document").unwrap();
    let error = directory
        .metadata_checked(b"Must not replace this", || Ok(()))
        .err()
        .unwrap();
    assert!(!error.published);
    directory.discard(&root, "unpublished", "session-log.json");
    assert_eq!(fs::read(path).unwrap(), b"External initial document");
}

#[test]
fn capture_directory_identity_rejects_a_replaced_original_path() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    assert!(capture.matches_directory(capture.directory()).unwrap());
    let alias = fixture.journals().join(
        capture
            .directory()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_uppercase(),
    );
    if alias.exists() {
        assert!(capture.matches_directory(&alias).unwrap());
    }
    wait(capture.finish());
    let moved = fixture.journals().join("moved-original");
    fs::rename(capture.directory(), &moved).unwrap();
    fs::create_dir(capture.directory()).unwrap();
    assert!(!capture.matches_directory(capture.directory()).unwrap());
    assert!(capture.matches_directory(&moved).unwrap());
}

#[test]
fn published_metadata_survives_a_sync_error_and_later_recording() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    for edit in [
        MetadataEdit::Title("Already published title".into()),
        MetadataEdit::Description("Already published description".into()),
        MetadataEdit::Pinned(true),
    ] {
        // Test double injects only the sync error after the actual atomic XML replacement.
        assert!(
            capture
                .sink
                .shared
                .input
                .try_send(Item::FailNextMetadataSync)
                .is_ok()
        );
        let result = capture.edit_metadata(edit).unwrap();
        assert!(wait_edit(result).is_err());
        assert_eq!(capture.status(), Status::Recording);
        capture
            .sink()
            .try_write(
                RecordKind::Note,
                Zeroizing::new("After publication".into()),
                false,
            )
            .unwrap();
    }
    wait(capture.finish());
    assert_eq!(capture.status(), Status::Complete);
    let document = read(capture.directory()).unwrap();
    assert_eq!(document.summary.title, "Already published title");
    assert_eq!(
        document.summary.description,
        "Already published description"
    );
    assert!(document.summary.pinned);
    assert_eq!(document.timeline.len(), 3);
    assert!(document.summary.ended_at.is_some());
}

#[test]
fn failed_startup_discards_its_unpublished_lock_and_directory() {
    let fixture = Fixture::new();
    let root = storage::Storage::root(&fixture.journals()).unwrap();
    let mut directory = root.child("unpublished").unwrap();
    directory.lock_metadata().unwrap();
    directory
        .save_new("session-log.yaml", b"Unpublished log header")
        .unwrap();
    directory.discard(&root, "unpublished", "session-log.yaml");
    assert!(!fixture.journals().join("unpublished").exists());
}

#[test]
fn live_writer_excludes_offline_metadata_edits_until_finalization_completes() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let error = crate::session_journal::edit_metadata(
        capture.directory(),
        capture.id(),
        &MetadataEdit::Title("Offline edit".into()),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    wait(capture.finish());
    let document = crate::session_journal::edit_metadata(
        capture.directory(),
        capture.id(),
        &MetadataEdit::Title("Offline after finalization".into()),
    )
    .unwrap();
    assert_eq!(document.summary.title, "Offline after finalization");
}

#[test]
fn busy_metadata_queue_does_not_truncate_capture_or_discard_accepted_records() {
    // Test double for a disk worker stalled before its first receive.
    let (input, receiver) = async_channel::bounded(1);
    let (_completion, done) = async_channel::bounded(1);
    let capture = Capture {
        id: "queue-test".into(),
        directory: PathBuf::new(),
        root_identity: Default::default(),
        directory_identity: Default::default(),
        log_format: LogFormat::default(),
        archives: Default::default(),
        sink: Sink {
            shared: Arc::new(Shared {
                input,
                marker_rules: Mutex::new(Arc::new(super::markers::Compiled::default())),
                sequence: AtomicU64::default(),
                timeline: AtomicUsize::default(),
                progress: Mutex::new(Progress {
                    status: Status::Recording,
                    truncation: None,
                    received_text: false,
                    seed_sent: false,
                    screenshot_pending: false,
                    entry_mutation_pending: false,
                    screenshot_bytes: 0,
                }),
            }),
        },
        done,
    };
    capture
        .sink()
        .try_write(
            RecordKind::Output,
            Zeroizing::new("Accepted first".into()),
            false,
        )
        .unwrap();
    let error = capture
        .edit_metadata(MetadataEdit::Title("Try later".into()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(capture.status(), Status::Recording);
    assert_eq!(capture.sink.shared.lock().truncation, None);
    assert!(
        matches!(receiver.try_recv(), Ok(Item::Text { text, .. }) if *text == "Accepted first")
    );
    assert!(!receiver.is_closed());
    let _ = capture
        .edit_metadata(MetadataEdit::Title("Retry accepted".into()))
        .unwrap();
    assert!(matches!(receiver.try_recv(), Ok(Item::Metadata { .. })));
}

#[test]
fn writes_readable_java_jsonl_timeline_notes_and_private_screenshot() {
    let fixture = Fixture::new();
    let capture = Capture::start(
        fixture.journals(),
        Metadata {
            title: "Saved & literal".into(),
            description: "Capture details".into(),
            connection_name: "SSH".into(),
            host: "example.test".into(),
            port: Some(22),
            username: "operator".into(),
            seeded: true,
            log_format: LogFormat::Json,
        },
    )
    .unwrap();
    assert_eq!(capture.status(), Status::Recording);
    assert!(
        read(capture.directory())
            .unwrap()
            .summary
            .ended_at
            .is_none()
    );

    let sink = capture.sink();
    sink.try_seed_batch(vec![Zeroizing::new("prior line".into())], 3)
        .unwrap();
    sink.try_write(
        RecordKind::Output,
        Zeroizing::new("visible output".into()),
        true,
    )
    .unwrap();
    sink.try_write(
        RecordKind::Note,
        Zeroizing::new("review <later> & resume".into()),
        false,
    )
    .unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    sink.try_screenshot(png.clone()).unwrap();
    let first = capture.finish();
    let second = capture.finish();
    wait(first);
    wait(second);
    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(
        sink.try_write(RecordKind::Output, Zeroizing::new("later".into()), false),
        Err(SendError::Closed)
    );

    let directory = capture.directory();
    let document = read(directory).unwrap();
    assert_eq!(document.summary.title, "Saved & literal");
    assert_eq!(document.summary.description, "Capture details");
    assert_eq!(document.summary.host, "example.test");
    assert_eq!(document.summary.port, Some(22));
    assert!(document.summary.ended_at.is_some());
    assert_eq!(document.timeline.len(), 2);
    let note = &document.timeline[0];
    assert_eq!(note.kind, "USER_NOTE");
    assert_eq!(note.text, "review <later> & resume");
    assert_eq!(
        (note.log_part, note.log_start, note.log_end),
        (Some(1), Some(4), Some(4))
    );
    let screenshot = &document.timeline[1];
    assert_eq!(screenshot.kind, "SCREENSHOT");
    assert_eq!(screenshot.log_start, Some(5));
    assert_eq!(
        read_screenshot(directory, screenshot.screenshot.as_deref().unwrap()).unwrap(),
        png
    );

    let log = capture_page(directory, None, None, None).unwrap();
    assert!(log.status.notice().is_none());
    assert!(log.next.is_none());
    assert_eq!(
        log.records
            .iter()
            .map(|record| (record.sequence, record.kind))
            .collect::<Vec<_>>(),
        [
            (1, CaptureKind::Seed),
            (2, CaptureKind::Note),
            (3, CaptureKind::Output),
            (4, CaptureKind::Note),
            (5, CaptureKind::Screenshot)
        ]
    );
    assert!(log.records[1].text.contains("3 older lines omitted"));
    assert!(log.records[2].partial);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for path in [
            fixture.journals(),
            directory.to_owned(),
            directory.join("journal.xml"),
            directory.join("session-log.json.zst"),
            directory.join(screenshot.screenshot.as_deref().unwrap()),
        ] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        }
    }
}

#[test]
fn rotates_within_hard_bound_and_records_the_truncation() {
    let fixture = Fixture::new();
    let capture = Capture::start_with_limits(
        fixture.journals(),
        Metadata::default(),
        Limits {
            part_bytes: 720,
            parts: 2,
        },
    )
    .unwrap();
    let sink = capture.sink();
    for marker in ["a", "b", "c"] {
        sink.try_write(
            RecordKind::Output,
            Zeroizing::new(marker.repeat(300)),
            false,
        )
        .unwrap();
    }
    wait(capture.finish());
    assert!(matches!(capture.status(), Status::Truncated(_)));
    let directory = capture.directory();
    for part in ["session-log.json", "session-log-2.json"] {
        assert!(archived_part(directory, part).len() <= 720);
    }
    assert!(!directory.join("session-log-3.json").exists());
    assert!(!directory.join("session-log-3.json.zst").exists());
    let doc = read(directory).unwrap();
    assert_eq!(doc.summary.log_parts, 2);
    assert_eq!(doc.timeline.len(), 1);
    assert_eq!(doc.timeline[0].kind, "SYSTEM");
    assert!(doc.timeline[0].text.contains("storage limit"));
    let log = capture_page(directory, None, None, None).unwrap();
    assert_eq!(log.records.len(), 2);
    assert_eq!(log.records[0].sequence, 1);
    assert_eq!(log.records[1].sequence, 2);
}

#[test]
fn journal_capture_formats_round_trip_java_framing_and_live_rotation() {
    let fixture = Fixture::new();
    for format in LogFormat::ALL {
        let capture = Capture::start_with_limits(
            fixture.journals(),
            Metadata {
                connection_name: "München & \"test\"\nnext".into(),
                log_format: format,
                ..Metadata::default()
            },
            Limits {
                part_bytes: 1250,
                parts: 3,
            },
        )
        .unwrap();
        let sink = capture.sink();
        let note = format!(
            "Grüße & <tag> \"quote\"\nzweite\tZeile\n{}",
            "a".repeat(610)
        );
        sink.try_write(RecordKind::Note, Zeroizing::new(note.clone()), false)
            .unwrap();
        wait_timeline(&capture, 1);

        let directory = capture.directory();
        let live = capture.live_tail(None).unwrap();
        assert!(live.reset);
        assert_eq!(live.skipped_lines, 0);
        assert_eq!(live.records.len(), 1);
        assert_eq!(live.records[0].text, note);
        assert_eq!(live.records[0].kind, CaptureKind::Note);

        let output = "b".repeat(620);
        sink.try_write(RecordKind::Output, Zeroizing::new(output.clone()), true)
            .unwrap();
        let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
        sink.try_screenshot(png).unwrap();
        wait_timeline(&capture, 2);
        let live = capture.live_tail(Some(&live.cursor)).unwrap();
        assert!(!live.reset);
        assert_eq!(live.skipped_lines, 0);
        assert_eq!(live.records.len(), 1);
        assert_eq!(live.records[0].sequence, 2);
        assert_eq!(live.records[0].text, output);
        assert!(live.records[0].partial);

        let first_name = format.part_name(1);
        let first_before_stop = archived_part(directory, &first_name);
        let backfill = capture.live_tail(None).unwrap();
        assert!(backfill.reset);
        assert_eq!(
            backfill
                .records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(backfill.skipped_lines, 0);
        wait(capture.finish());
        assert_eq!(capture.status(), Status::Complete);
        let document = read(directory).unwrap();
        assert!(document.summary.log_parts >= 2);
        let metadata = fs::read_to_string(directory.join("journal.xml")).unwrap();
        assert!(metadata.contains(&format!("<logFormat>{format}</logFormat>")));
        for part in 1..=document.summary.log_parts {
            let bytes = archived_part(directory, &format.part_name(part));
            if part == 1 {
                assert_eq!(bytes, first_before_stop);
            }
            assert!(bytes.len() <= 1250);
            match format {
                LogFormat::Xml => {
                    let xml = roxmltree::Document::parse(&bytes).unwrap();
                    let root = xml.root_element();
                    assert!(root.has_tag_name("session-log"));
                    assert_eq!(root.attribute("journalId"), Some(capture.id()));
                    assert_eq!(
                        root.attribute("part").unwrap().parse::<u32>().unwrap(),
                        part
                    );
                    let meta = root.first_element_child().unwrap();
                    assert_eq!(
                        meta.attribute("connectionName"),
                        Some("München & \"test\"\nnext")
                    );
                    assert_eq!(meta.attribute("tabSessionId"), Some(capture.id()));
                    assert!(bytes.ends_with("</session-log>\n"));
                }
                LogFormat::Json => {
                    assert!(bytes.starts_with('{'));
                    assert!(bytes.ends_with("{\"type\":\"end\"}\n"));
                }
                LogFormat::Yaml => {
                    assert!(bytes.starts_with("formatVersion: 1\njournalId: \""));
                    assert!(bytes.contains("\nentries:\n- "));
                    assert!(bytes.ends_with("end: true\n"));
                }
            }
        }
        let page = viewer::Source::capture(&capture)
            .capture_page(None, None, None)
            .unwrap();
        assert!(page.status.notice().is_none());
        assert_eq!(page.records.len(), 3);
        assert_eq!(page.records[0].text, note);
        assert_eq!(page.records[1].text, output);
        assert!(page.records[1].partial);
        assert_eq!(page.records[2].kind, CaptureKind::Screenshot);
        assert_eq!(
            Some(page.records[2].text.as_str()),
            document.timeline[1].screenshot.as_deref()
        );
    }
}

#[test]
fn journal_live_cursor_pages_a_rotated_archive_backlog_without_replaying_or_skipping_records() {
    let fixture = Fixture::new();
    let capture = Capture::start_with_limits(
        fixture.journals(),
        Metadata::default(),
        Limits {
            part_bytes: 1_425_000,
            parts: 2,
        },
    )
    .unwrap();
    let sink = capture.sink();
    let text = "x".repeat(12_000);
    for _ in 0..24 {
        sink.try_write(RecordKind::Output, Zeroizing::new(text.clone()), false)
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while capture.published().0 < 24 {
        assert!(
            Instant::now() < deadline,
            "journal did not flush its first records"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let initial = capture.live_tail(None).unwrap();
    assert_eq!(initial.records.last().unwrap().sequence, 24);
    let mut cursor = initial.cursor.clone();

    for _ in 24..130 {
        sink.try_write(RecordKind::Output, Zeroizing::new(text.clone()), false)
            .unwrap();
    }
    sink.try_write(
        RecordKind::Note,
        Zeroizing::new("Rotation flushed".into()),
        false,
    )
    .unwrap();
    wait_timeline(&capture, 1);
    let directory = capture.directory();
    let archive = directory.join("session-log.json.zst");
    assert_eq!(capture.status(), Status::Recording);
    assert!(archive.is_file());
    assert!(!directory.join("session-log.json").exists());
    assert!(directory.join("session-log-2.json").is_file());

    let mut actual = Vec::new();
    for _ in 0..16 {
        let page = capture.live_tail(Some(&cursor)).unwrap();
        assert!(!page.reset);
        assert_eq!(page.skipped_lines, 0);
        actual.extend(page.records.iter().map(|record| record.sequence));
        cursor = page.cursor;
        if !page.more {
            break;
        }
    }
    assert_eq!(actual, (25..=131).collect::<Vec<_>>());

    // A byte-identical archive at the same name still needs the writer's file identity.
    let moved = archive.with_extension("retained");
    fs::rename(&archive, &moved).unwrap();
    fs::copy(moved, &archive).unwrap();
    assert!(capture.live_tail(Some(&initial.cursor)).is_err());
    wait(capture.finish());
}

#[test]
fn journal_compression_preserves_plain_log_if_archive_name_is_occupied() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Keep this log".into()),
            false,
        )
        .unwrap();
    wait_timeline(&capture, 1);
    let directory = capture.directory();
    let archive = directory.join("session-log.json.zst");
    fs::write(&archive, b"Existing file owned by another operation").unwrap();
    wait(capture.finish());

    assert_eq!(capture.status(), Status::Complete);
    assert_eq!(
        fs::read(archive).unwrap(),
        b"Existing file owned by another operation"
    );
    let plain = fs::read_to_string(directory.join("session-log.json")).unwrap();
    assert!(plain.ends_with("{\"type\":\"end\"}\n"));
    assert_eq!(
        capture_page(directory, None, None, None).unwrap().records[0].text,
        "Keep this log"
    );
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".stage-")
    }));
}

#[test]
fn full_queue_stops_acceptance_without_dropping_its_fifo_prefix() {
    // Test double for a disk worker stalled before its first receive.
    let (input, receiver) = async_channel::bounded(1);
    let sink = Sink {
        shared: Arc::new(Shared {
            input,
            marker_rules: Mutex::new(Arc::new(super::markers::Compiled::default())),
            sequence: AtomicU64::default(),
            timeline: AtomicUsize::default(),
            progress: Mutex::new(Progress {
                status: Status::Recording,
                truncation: None,
                received_text: false,
                seed_sent: false,
                screenshot_pending: false,
                entry_mutation_pending: false,
                screenshot_bytes: 0,
            }),
        }),
    };
    sink.try_write(RecordKind::Output, Zeroizing::new("first".into()), false)
        .unwrap();
    assert_eq!(
        sink.try_write(RecordKind::Output, Zeroizing::new("dropped".into()), false),
        Err(SendError::Full)
    );
    assert_eq!(
        sink.try_write(RecordKind::Output, Zeroizing::new("late".into()), false),
        Err(SendError::Closed)
    );
    assert!(matches!(receiver.try_recv(), Ok(Item::Text { text, .. }) if *text == "first"));
    assert!(matches!(
        receiver.try_recv(),
        Err(async_channel::TryRecvError::Closed)
    ));
    let progress = sink.shared.lock();
    assert_eq!(progress.status, Status::Finishing);
    assert_eq!(progress.truncation, Some("the disk writer queue filled"));
}

#[test]
fn dropping_capture_finishes_even_with_an_existing_sink() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let directory = capture.directory().to_owned();
    let done = capture.done();
    let sink = capture.sink();
    sink.try_write(RecordKind::Seed, Zeroizing::new("keep".into()), false)
        .unwrap();
    drop(capture);
    wait(done);
    assert!(read(&directory).unwrap().summary.ended_at.is_some());
    assert_eq!(
        capture_page(&directory, None, None, None)
            .unwrap()
            .records
            .len(),
        1
    );
    assert_eq!(
        sink.try_write(RecordKind::Output, Zeroizing::new("late".into()), false),
        Err(SendError::Closed)
    );
}
