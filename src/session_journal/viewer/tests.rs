use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use super::super::{Metadata, MetadataEdit, catalog, edit_metadata};
use super::*;

const ID: &str = "viewer-source-id";
const REFERENCE: &str = "screenshots/shot-000001.png";

struct Fixture {
    scratch: PathBuf,
    root: PathBuf,
    journal: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let scratch = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-viewer-source-{}",
            crate::private_files::nonce().unwrap(),
        ));
        let root = scratch.join("journals");
        let journal = root.join("journal");
        fs::create_dir_all(&journal).unwrap();
        write_xml(&journal, ID, "Original");
        Self {
            scratch,
            root,
            journal,
        }
    }

    fn source(&self) -> Source {
        Source::catalog(&catalog(&self.root).unwrap().journals[0]).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn write_xml(directory: &Path, id: &str, title: &str) {
    fs::write(directory.join("journal.xml"), format!(
        "<session-journal formatVersion=\"1\" id=\"{id}\"><meta><title>{title}</title><logParts>1</logParts></meta>\
        <entries><entry><id>shot</id><kind>SCREENSHOT</kind><screenshotFile>{REFERENCE}</screenshotFile></entry></entries></session-journal>"
    )).unwrap();
}

fn write_assets(directory: &Path, text: &str) -> Vec<u8> {
    fs::write(
        directory.join("session-log.json"),
        format!(
            "{{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"{ID}\",\"part\":1}}\n\
         {{\"seq\":1,\"t\":\"2026-10-01T08:00:00Z\",\"k\":\"out\",\"x\":\"{text}\"}}\n\
         {{\"type\":\"end\"}}\n"
        ),
    )
    .unwrap();
    fs::create_dir_all(directory.join("screenshots")).unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    fs::write(directory.join(REFERENCE), &png).unwrap();
    png
}

#[test]
fn journal_viewer_source_skips_unchanged_reads_and_accepts_atomic_metadata_publication() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert_eq!(source.directory(), fixture.journal);
    assert_eq!(source.id(), ID);
    let old = source.read(None).unwrap().unwrap();
    let previous = old.summary.deletion_identity.as_ref().unwrap();
    assert!(source.matches_location(previous));
    assert!(source.read(Some(previous)).unwrap().is_none());
    assert!(source.read(None).unwrap().is_some());

    edit_metadata(
        &fixture.journal,
        ID,
        &MetadataEdit::Title("Changed title".into()),
    )
    .unwrap();
    let current = source.read(Some(previous)).unwrap().unwrap();
    assert_eq!(current.summary.title, "Changed title");
    let current_stamp = current.summary.deletion_identity.as_ref().unwrap();
    assert_ne!(current_stamp, previous);
    assert!(source.matches_location(current_stamp));
    assert!(source.read(Some(current_stamp)).unwrap().is_none());
}

#[test]
fn journal_viewer_source_rejects_a_same_id_directory_replacement_and_its_assets() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let png = write_assets(&fixture.journal, "original output");
    let page = source.capture_page(None, Some(1..=1), None).unwrap();
    assert_eq!(page.records[0].text, "original output");
    assert_eq!(
        Screenshot::review(&source, "shot", REFERENCE)
            .unwrap()
            .bytes(),
        png
    );

    fs::rename(&fixture.journal, fixture.root.join("moved")).unwrap();
    fs::create_dir(&fixture.journal).unwrap();
    write_xml(&fixture.journal, ID, "Replacement");
    write_assets(&fixture.journal, "replacement output");
    let opened = Directory::open(&fixture.journal, false).unwrap();
    let replacement = Source::from_open_directory(&fixture.journal, ID, &opened).unwrap();
    assert!(!source.same_journal(&replacement));
    assert!(source.require_location(&fixture.journal, &opened).is_err());
    assert!(source.read(None).is_err());
    assert!(source.capture_page(None, None, None).is_err());
    assert!(Screenshot::review(&source, "shot", REFERENCE).is_err());
    assert_eq!(
        replacement.read(None).unwrap().unwrap().summary.title,
        "Replacement"
    );
}

#[test]
fn journal_viewer_source_rejects_a_replacement_root_even_with_the_original_directory() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let old = source.read(None).unwrap().unwrap();
    let previous = old.summary.deletion_identity.as_ref();
    let detached = fixture.scratch.join("detached-root");
    fs::rename(&fixture.root, &detached).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    fs::rename(detached.join("journal"), &fixture.journal).unwrap();
    let current = fixture.source();
    assert!(!source.same_journal(&current));
    assert!(source.read(previous).is_err());
}

#[test]
fn journal_viewer_source_rejects_foreign_xml_and_hardlink_documents() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let old = source.read(None).unwrap().unwrap();
    let alias = fixture.root.join("hardlink.xml");
    fs::hard_link(fixture.journal.join("journal.xml"), &alias).unwrap();
    assert!(source.read(old.summary.deletion_identity.as_ref()).is_err());
    fs::remove_file(alias).unwrap();
    write_assets(&fixture.journal, "sensitive output");
    write_xml(
        &fixture.journal,
        "a-different-journal-id",
        "Foreign content",
    );
    assert!(source.read(old.summary.deletion_identity.as_ref()).is_err());
    assert!(source.capture_page(None, None, None).is_err());
    assert!(Screenshot::review(&source, "shot", REFERENCE).is_err());
}

#[test]
fn journal_screenshot_source_rejects_a_linked_image_and_private_reference() {
    let fixture = Fixture::new();
    let source = fixture.source();
    write_assets(&fixture.journal, "original output");
    let alias = fixture.root.join("linked.png");
    fs::hard_link(fixture.journal.join(REFERENCE), alias).unwrap();
    assert!(Screenshot::review(&source, "shot", REFERENCE).is_err());
    assert!(!Screenshot::is_visible_reference(
        "screenshots/shot.orig.png"
    ));
    assert!(!Screenshot::is_visible_reference(
        "screenshots/shot.ORIG.png"
    ));
}

#[test]
fn journal_viewer_source_capture_matches_catalog_and_does_not_keep_the_writer_alive() {
    let fixture = Fixture::new();
    let root = fixture.scratch.join("capture-root");
    let capture = Capture::start(root.clone(), Metadata::default()).unwrap();
    let source = Source::capture(&capture);
    let catalog = catalog(&root).unwrap();
    assert!(source.same_journal(&Source::catalog(&catalog.journals[0]).unwrap()));
    let original = source.read(None).unwrap().unwrap();
    let done = capture.done();
    drop(capture);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done.is_closed() {
        assert!(
            Instant::now() < deadline,
            "viewer retained the journal writer"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let completed = source
        .read(original.summary.deletion_identity.as_ref())
        .unwrap()
        .unwrap();
    assert!(completed.summary.ended_at.is_some());
}
