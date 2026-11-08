use std::fs;
use std::path::PathBuf;

use super::*;
use crate::session_journal::{DOCUMENT, metadata::lock_metadata};

const ID: &str = "entry-delete-fixture";
const IMAGE: &str = "screenshots/shot-000001.png";
const ORIGINAL: &str = "screenshots/shot-000001.orig.png";
const LOG: &str = "session-log.json";
const TARGET: &str = concat!(
    "<entry><id>target</id><kind>SCREENSHOT</kind><marker>ERROR</marker>",
    "<markerId>removed</markerId><title>Delete this</title>",
    "<screenshotFile>screenshots/shot-000001.png</screenshotFile>",
    "<logStartSeq>1</logStartSeq><logEndSeq>3</logEndSeq><future>&#x41;</future></entry>"
);
const REMOVED_MARKER: &str =
    "<marker><id>removed</id><name>To remove</name><legacyMarker>ERROR</legacyMarker></marker>";
const USED_MARKER: &str =
    "<marker><id>shared</id><name>Keep this</name><legacyMarker>ERROR</legacyMarker></marker>";

fn document() -> String {
    format!(
        r#"<session-journal id="{ID}" formatVersion="1">
<meta><title>Private fixture</title><errorCount>3</errorCount><screenshotCount>1</screenshotCount>
  <unknown attr="keep">&#x41;</unknown>
</meta>
<entries>
{TARGET}
<entry><id>keep</id><kind>AGENT</kind><marker>ERROR</marker><markerId> Shared </markerId>
  <chatSnapshot><chatTitle>Before</chatTitle></chatSnapshot>
</entry>
</entries>
<markerDefinitions>{REMOVED_MARKER}<!-- keep marker metadata -->{USED_MARKER}</markerDefinitions>
</session-journal>"#
    )
}

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-entry-delete-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = root.join("journal");
        fs::create_dir_all(journal.join("screenshots")).unwrap();
        let fixture = Self { root, journal };
        fixture.write_xml(&document());
        let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
        fs::write(fixture.journal.join(IMAGE), &png).unwrap();
        fs::write(fixture.journal.join(ORIGINAL), &png).unwrap();
        fs::write(fixture.journal.join(LOG), b"{\"type\":\"end\"}\n").unwrap();
        fixture
    }

    fn write_xml(&self, xml: &str) {
        fs::write(self.journal.join(DOCUMENT), xml).unwrap();
    }

    fn read_xml(&self) -> String {
        fs::read_to_string(self.journal.join(DOCUMENT)).unwrap()
    }

    fn draft(&self) -> Draft {
        let directory = Directory::open(&self.journal, false).unwrap();
        let source = Source::from_open_directory(&self.journal, ID, &directory).unwrap();
        Draft::read(&source, "target").unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn journal_entry_delete_preserves_current_xml_recounts_and_keeps_capture_and_original_image() {
    let fixture = Fixture::new();
    let deletion = fixture.draft().prepare_delete().unwrap();
    let current = document().replace(
        "<chatTitle>Before</chatTitle>",
        "<chatTitle>Current &amp; exact</chatTitle>",
    );
    fixture.write_xml(&current);

    assert_eq!(commit(&deletion).unwrap(), Outcome::default());
    let expected = current
        .replace(TARGET, "")
        .replace(REMOVED_MARKER, "")
        .replace("<errorCount>3</errorCount>", "<errorCount>1</errorCount>");
    assert_eq!(fixture.read_xml(), expected);
    assert!(!fixture.journal.join(IMAGE).exists());
    assert!(fixture.journal.join(ORIGINAL).is_file());
    assert_eq!(
        fs::read(fixture.journal.join(LOG)).unwrap(),
        b"{\"type\":\"end\"}\n"
    );
    assert!(!format!("{deletion:?}").contains("Delete this"));
}

#[test]
fn journal_entry_delete_rejects_full_row_changes_before_and_after_confirmation() {
    let fixture = Fixture::new();
    let draft = fixture.draft();
    let changed = document().replace("<future>&#x41;</future>", "<future>B</future>");
    fixture.write_xml(&changed);
    assert!(draft.prepare_delete().is_err());
    fixture.write_xml(&document());
    let deletion = draft.prepare_delete().unwrap();
    fixture.write_xml(&changed);
    assert!(commit(&deletion).is_err());
    assert_eq!(fixture.read_xml(), changed);
    assert!(fixture.journal.join(IMAGE).is_file());
}

#[test]
fn journal_entry_delete_protects_new_image_references_and_replacement_image_identity() {
    let fixture = Fixture::new();
    let deletion = fixture.draft().prepare_delete().unwrap();
    let shared = document().replace(
        "<chatSnapshot>",
        &format!("<screenshotFile>{IMAGE}</screenshotFile><chatSnapshot>"),
    );
    fixture.write_xml(&shared);
    assert_eq!(commit(&deletion).unwrap(), Outcome::default());
    assert!(fixture.journal.join(IMAGE).is_file());
    assert!(fixture.read_xml().contains("<id>keep</id>"));

    fixture.write_xml(&document());
    let deletion = fixture.draft().prepare_delete().unwrap();
    fs::rename(
        fixture.journal.join(IMAGE),
        fixture.journal.join("previous.png"),
    )
    .unwrap();
    let replacement = tiny_skia::Pixmap::new(3, 3).unwrap().encode_png().unwrap();
    fs::write(fixture.journal.join(IMAGE), &replacement).unwrap();
    assert_eq!(
        commit(&deletion).unwrap(),
        Outcome {
            cleanup_warning: true,
            durability_warning: false
        },
    );
    assert!(!fixture.read_xml().contains("<id>target</id>"));
    assert_eq!(fs::read(fixture.journal.join(IMAGE)).unwrap(), replacement);
}

#[test]
fn journal_entry_delete_will_not_mutate_under_live_lock_or_duplicate_identity() {
    let fixture = Fixture::new();
    let deletion = fixture.draft().prepare_delete().unwrap();
    let lock = lock_metadata(&fixture.journal).unwrap();
    assert!(commit(&deletion).is_err());
    assert_eq!(fixture.read_xml(), document());
    drop(lock);

    let ambiguous = document().replace("<id>keep</id>", "<id>target</id>");
    fixture.write_xml(&ambiguous);
    assert!(commit(&deletion).is_err());
    assert_eq!(fixture.read_xml(), ambiguous);
    assert!(fixture.journal.join(IMAGE).is_file());
}

#[test]
fn journal_entry_delete_keeps_image_if_an_external_writer_republishes_xml() {
    let fixture = Fixture::new();
    let deletion = fixture.draft().prepare_delete().unwrap();
    let lock = lock_source(deletion.source()).unwrap();
    let (stamp, xml) = lock.read_document().unwrap();
    let document = parse_document(&xml).unwrap();
    let entry = find_entry(&document, "target").unwrap();
    let updated = remove_entry(&xml, &document, entry).unwrap();
    let cleanup = prepare_image_cleanup(&deletion, lock.directory(), &document, entry)
        .unwrap()
        .unwrap();
    lock.publish_document(&stamp, Some(&updated), |_| Ok(()))
        .unwrap();
    let newer = updated.replace(
        "<chatSnapshot>",
        &format!("<screenshotFile>{IMAGE}</screenshotFile><chatSnapshot>"),
    );
    fixture.write_xml(&newer);

    assert!(remove_published_image(cleanup, &lock, deletion.source(), &updated).is_err());
    assert_eq!(fixture.read_xml(), newer);
    assert!(fixture.journal.join(IMAGE).is_file());
}
