use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::session_journal::{DOCUMENT, MAX_TIMELINE_ROWS, catalog, read, tree::prepare_query};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-content-search-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn journal(&self, name: &str, id: &str, meta: &str, entries: &str) -> PathBuf {
        let directory = self.0.join(name);
        fs::create_dir(&directory).unwrap();
        write_document(&directory, id, meta, entries);
        directory
    }

    fn scan(&self, snapshot: &Catalog, filter: Filter, query: &str) -> SearchResult {
        scan(&self.0, snapshot, filter, &prepare_query(query), &|| false).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_document(directory: &Path, id: &str, meta: &str, entries: &str) {
    let contents = format!(
        r#"<session-journal id="{id}" formatVersion="1"><meta>{meta}</meta><entries>{entries}</entries></session-journal>"#
    );
    fs::write(directory.join(DOCUMENT), contents).unwrap();
}

#[test]
fn journal_content_search_matches_java_xml_fields_as_literal_case_folded_text() {
    let fixture = Fixture::new();
    fixture.journal(
        "notes",
        "notes-id",
        "<title>Metadata needle</title><logParts>0</logParts>",
        "<entry><title>Übersicht [*]</title><text>Output Ω</text><userNote>My note</userNote>\
         <aiDescription>Screenshot view</aiDescription><aiTags><tag>first</tag><tag>Second tag</tag></aiTags>\
         <inputExcerpt><line>Excerpt only</line></inputExcerpt></entry>",
    );
    let snapshot = catalog(&fixture.0).unwrap();
    for query in [
        "  ÜBERSICHT [*]  ",
        "OUTPUT Ω",
        "MY NOTE",
        "SCREENSHOT VIEW",
        "SECOND TAG",
    ] {
        let result = fixture.scan(&snapshot, Filter::All, query);
        assert_eq!(result.matching_indices, HashSet::from([0]), "{query}");
        assert!(!result.partial, "{result:?}");
    }
    for query in ["Excerpt only", "Metadata needle", "Übersicht .*", ""] {
        let result = fixture.scan(&snapshot, Filter::All, query);
        assert!(result.matching_indices.is_empty(), "{query}");
        assert_eq!(result.skipped, 0);
        assert!(!result.partial, "{result:?}");
    }
}

#[test]
fn journal_content_search_reads_beyond_the_viewer_limit_and_keeps_catalog_indices() {
    let fixture = Fixture::new();
    fixture.journal(
        "interactive",
        "interactive-id",
        "<logParts>0</logParts>",
        "<entry><text>Deep needle</text></entry>",
    );
    let mut entries = "<entry><text>Ordinary</text></entry>".repeat(MAX_TIMELINE_ROWS);
    entries.push_str("<entry><userNote>Deep needle</userNote></entry>");
    let job = fixture.journal(
        "scheduled",
        "job-id",
        "<sourceKind>JOB</sourceKind><logParts>0</logParts>",
        &entries,
    );
    let viewer = read(&job).unwrap();
    assert!(viewer.timeline_limited);
    assert_eq!(viewer.timeline.len(), MAX_TIMELINE_ROWS);
    assert!(viewer.timeline.iter().all(|entry| entry.note.is_empty()));

    let snapshot = catalog(&fixture.0).unwrap();
    let result = fixture.scan(&snapshot, Filter::Jobs, "deep needle");
    let index = snapshot
        .journals
        .iter()
        .position(|row| row.directory == job)
        .unwrap();
    assert_eq!(result.matching_indices, HashSet::from([index]));
    assert!(!result.partial, "{result:?}");
}

#[test]
fn journal_content_search_accepts_same_journal_publication_and_rejects_replacements() {
    let fixture = Fixture::new();
    let meta = "<logParts>0</logParts>";
    let matching = "<entry><text>New needle</text></entry>";
    let updated = fixture.journal("updated", "retained-id", meta, "");
    let changed_id = fixture.journal("changed-id", "original-id", meta, "");
    let changed_directory = fixture.journal("changed-directory", "copied-id", meta, "");
    let snapshot = catalog(&fixture.0).unwrap();

    // A live writer can publish a new XML version in the same journal directory.
    fs::rename(updated.join(DOCUMENT), updated.join("previous.xml")).unwrap();
    write_document(&updated, "retained-id", meta, matching);
    write_document(&changed_id, "replacement-id", meta, matching);
    fs::rename(&changed_directory, fixture.0.join("previous-directory")).unwrap();
    fixture.journal("changed-directory", "copied-id", meta, matching);

    let result = fixture.scan(&snapshot, Filter::All, "new needle");
    let index = snapshot
        .journals
        .iter()
        .position(|row| row.directory == updated)
        .unwrap();
    assert_eq!(result.matching_indices, HashSet::from([index]));
    assert_eq!(result.skipped, 2);
    assert!(result.partial);
}

#[test]
fn journal_content_search_rejects_a_replaced_catalog_root_containing_the_original_journal() {
    let fixture = Fixture::new();
    let root = fixture.0.join("catalog");
    let original_root = fixture.0.join("previous-catalog");
    let directory = root.join("journal");
    fs::create_dir_all(&directory).unwrap();
    write_document(
        &directory,
        "kept-id",
        "<logParts>0</logParts>",
        "<entry><text>Needle</text></entry>",
    );
    let snapshot = catalog(&root).unwrap();
    fs::rename(&root, &original_root).unwrap();
    fs::create_dir(&root).unwrap();
    fs::rename(original_root.join("journal"), &directory).unwrap();

    let result = scan(&root, &snapshot, Filter::All, "needle", &|| false).unwrap();
    assert!(result.matching_indices.is_empty());
    assert_eq!(result.skipped, 1);
    assert!(result.partial);
}

#[test]
fn journal_content_search_reports_exhausted_xml_budget_and_aborts_mid_scan() {
    let fixture = Fixture::new();
    let meta = "<logParts>0</logParts>";
    let entries = "<entry><text>Never find this</text></entry>".repeat(40);
    let first = fixture.journal("a-first", "first-id", meta, &entries);
    fixture.journal("b-next", "second-id", meta, &entries);
    let snapshot = catalog(&fixture.0).unwrap();
    let budget = Budget {
        xml: fs::metadata(first.join(DOCUMENT)).unwrap().len() + 1,
        log: MAX_SEARCH_LOG_BYTES,
    };
    let result = scan_bounded(
        &fixture.0,
        &snapshot,
        Filter::All,
        "different",
        budget,
        &|| false,
    )
    .unwrap();
    assert!(result.matching_indices.is_empty());
    assert_eq!(result.skipped, 1);
    assert!(result.partial);

    // Test double for cancellation arriving during the real XML scan.
    let checks = Cell::new(0);
    let result = scan(&fixture.0, &snapshot, Filter::All, "different", &|| {
        checks.set(checks.get() + 1);
        checks.get() > 12
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
}
