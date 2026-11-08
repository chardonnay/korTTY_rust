use std::fs;
use std::path::PathBuf;

use super::*;
use crate::session_journal::metadata::{LOCK, lock_metadata};

const ID: &str = "journal-replace-fixture";
const PART: &str = "session-log.json";
const ORIGINAL_XML: &str = r#"<session-journal formatVersion="1" id="journal-replace-fixture">
<meta><title>secret in journal metadata</title><logParts>1</logParts></meta>
<entries><entry><id>first</id><kind>USER_NOTE</kind><title>secret &amp; text</title><future>secret</future></entry></entries>
</session-journal>"#;
const CAPTURE: &str = concat!(
    "{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"journal-replace-fixture\",\"part\":1,\"tabSessionId\":\"preserved-tab\"}\n",
    "{\"seq\":1,\"t\":\"2026-11-06T14:00:00Z\",\"k\":\"out\",\"x\":\"secret in capture\"}\n",
    "{\"seq\":2,\"t\":\"2026-11-06T14:00:01Z\",\"k\":\"out\",\"x\":\"keep exact\"}\n",
    "{\"type\":\"end\"}\n"
);
const STAGE_PREFIX: &str = ".kortty-journal-replace-";

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-journal-replace-test-{}", nonce().unwrap()));
        let journal = root.join("journal");
        fs::create_dir_all(&journal).unwrap();
        fs::write(journal.join(DOCUMENT), ORIGINAL_XML).unwrap();
        fs::write(journal.join(PART), CAPTURE).unwrap();
        Self { root, journal }
    }

    fn directory(&self) -> Directory {
        Directory::open(&self.journal, false).unwrap()
    }

    fn source(&self) -> Source {
        Source::from_open_directory(&self.journal, ID, &self.directory()).unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.journal.join(name)).unwrap()
    }

    fn stamp(&self, name: &str) -> FileStamp {
        FileStamp::read(&self.directory().open_file(name).unwrap()).unwrap()
    }

    fn assert_no_stages(&self) {
        assert!(
            self.directory()
                .names(16)
                .unwrap()
                .iter()
                .all(|name| { !name.to_str().unwrap().starts_with(STAGE_PREFIX) })
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn rule() -> Rule {
    Rule::compile("secret", "[masked]", false, false).unwrap()
}

#[test]
fn journal_replace_count_is_read_only_and_apply_publishes_both_reviewed_files() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let document_stamp = fixture.stamp(DOCUMENT);
    let capture_stamp = fixture.stamp(PART);
    let preview = preview(&source, rule(), true, false, || false).unwrap();
    assert!(preview.source().same_journal(&source));
    let counts = Counts {
        fields: 1,
        records: 1,
    };
    assert_eq!(preview.counts(), counts);
    assert_eq!(fixture.stamp(DOCUMENT), document_stamp);
    assert_eq!(fixture.stamp(PART), capture_stamp);
    assert!(!fixture.journal.join(LOCK).exists());
    assert!(!format!("{preview:?}").contains("secret"));
    fixture.assert_no_stages();

    assert_eq!(
        apply(&preview).unwrap(),
        Outcome {
            counts,
            published: 2,
            ..Outcome::default()
        }
    );
    assert_eq!(
        fixture.read(DOCUMENT),
        ORIGINAL_XML.replace("secret &amp; text", "[masked] &amp; text")
    );
    assert_eq!(
        fixture.read(PART),
        CAPTURE.replace("secret in capture", "[masked] in capture")
    );
    fixture.assert_no_stages();
}

#[test]
fn journal_replace_cancelled_count_never_writes_to_the_source() {
    let fixture = Fixture::new();
    let error = preview(&fixture.source(), rule(), true, false, || true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(!fixture.journal.join(LOCK).exists());
    assert_eq!(fixture.read(DOCUMENT), ORIGINAL_XML);
    assert_eq!(fixture.read(PART), CAPTURE);
    fixture.assert_no_stages();
}

#[test]
fn journal_replace_refuses_a_replaced_document_or_capture_even_with_the_same_bytes() {
    for name in [DOCUMENT, PART] {
        let fixture = Fixture::new();
        let preview = preview(&fixture.source(), rule(), true, false, || false).unwrap();
        let original = fixture.read(name);
        fs::rename(fixture.journal.join(name), fixture.root.join("previous")).unwrap();
        fs::write(fixture.journal.join(name), &original).unwrap();

        assert!(apply(&preview).is_err());
        assert_eq!(fixture.read(DOCUMENT), ORIGINAL_XML);
        assert_eq!(fixture.read(PART), CAPTURE);
        fixture.assert_no_stages();
    }
}

#[test]
fn journal_replace_refuses_a_capture_part_added_after_count() {
    let fixture = Fixture::new();
    let preview = preview(&fixture.source(), rule(), true, false, || false).unwrap();
    let added = CAPTURE.replace("\"part\":1", "\"part\":2");
    fs::write(fixture.journal.join("session-log-2.json"), &added).unwrap();
    assert!(apply(&preview).is_err());
    assert_eq!(fixture.read(DOCUMENT), ORIGINAL_XML);
    assert_eq!(fixture.read(PART), CAPTURE);
    assert_eq!(fixture.read("session-log-2.json"), added);
    fixture.assert_no_stages();
}

#[test]
fn journal_replace_live_count_can_read_an_open_part_but_apply_requires_a_stopped_writer() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let open = CAPTURE.strip_suffix("{\"type\":\"end\"}\n").unwrap();
    fs::write(fixture.journal.join(PART), open).unwrap();
    assert!(preview(&source, rule(), true, false, || false).is_err());
    let capture_preview = preview(&source, rule(), true, true, || false).unwrap();
    assert_eq!(
        capture_preview.counts(),
        Counts {
            fields: 1,
            records: 1
        }
    );
    assert!(!fixture.journal.join(LOCK).exists());
    let lock = lock_metadata(&fixture.journal).unwrap();
    let xml_preview = preview(&source, rule(), false, true, || false).unwrap();
    assert_eq!(
        apply(&xml_preview).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(apply(&capture_preview).is_err());
    drop(lock);
    assert!(apply(&capture_preview).is_err());
    assert_eq!(fixture.read(PART), open);
    assert_eq!(fixture.read(DOCUMENT), ORIGINAL_XML);
    fixture.assert_no_stages();
}

#[test]
fn journal_replace_staging_failure_never_writes_an_original_or_keeps_owned_stages() {
    let fixture = Fixture::new();
    let directory = fixture.directory();
    let mut stage = Stage::create(&directory).unwrap();
    stage
        .write(PART, b"new capture", fixture.stamp(PART), Counts::default())
        .unwrap();
    // Exercise the production exhaustion path without generating a 512 MiB fixture.
    stage.bytes_left = 0;
    assert!(
        stage
            .write(
                DOCUMENT,
                b"new document",
                fixture.stamp(DOCUMENT),
                Counts::default()
            )
            .is_err()
    );
    drop(stage);
    assert_eq!(fixture.read(DOCUMENT), ORIGINAL_XML);
    assert_eq!(fixture.read(PART), CAPTURE);
    fixture.assert_no_stages();
}

#[test]
fn journal_replace_late_conflict_reports_only_already_published_counts() {
    let fixture = Fixture::new();
    let directory = fixture.directory();
    let mut stage = Stage::create(&directory).unwrap();
    let applied = Counts {
        fields: 0,
        records: 1,
    };
    stage
        .write(PART, b"new capture", fixture.stamp(PART), applied)
        .unwrap();
    stage
        .write(
            DOCUMENT,
            b"new document",
            fixture.stamp(DOCUMENT),
            Counts {
                fields: 1,
                records: 0,
            },
        )
        .unwrap();
    stage.require_ready().unwrap();
    // A real external update after initial preflight makes the second target stale.
    fs::write(fixture.journal.join(DOCUMENT), "newer external XML").unwrap();
    assert_eq!(
        stage.publish(|| Ok(())).unwrap(),
        Outcome {
            counts: applied,
            published: 1,
            pending: 1,
            ..Outcome::default()
        }
    );
    assert_eq!(fixture.read(DOCUMENT), "newer external XML");
    assert_eq!(fixture.read(PART), "new capture");
    fixture.assert_no_stages();
}
