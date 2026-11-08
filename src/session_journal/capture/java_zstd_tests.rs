use std::fs;

use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::live_tests::{ID, Scratch};
use super::{MAX_SCAN_LINES, capture_page, content_match, open_document};
use crate::session_journal::{catalog, content_search, tree};

// Produced by the pinned Java zstd-jni 1.5.7-25 writer: level 3, setLong(27).
// Window descriptor 0x88 means 128 MiB, even for this 231-byte decoded log.
pub(super) const JAVA_LONG_WINDOW: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x88, 0x4d, 0x05, 0x00, 0xe2, 0x4a, 0x23, 0x21, 0x50, 0x8b, 0x9a,
    0x03, 0xc0, 0x68, 0x1f, 0xb5, 0x70, 0x07, 0x49, 0x5b, 0x18, 0xd4, 0xee, 0x57, 0x57, 0xf6, 0xd8,
    0x00, 0xbd, 0xc4, 0x89, 0xd8, 0x7e, 0x20, 0x82, 0xa7, 0xdd, 0x4c, 0xd8, 0x55, 0x0d, 0x49, 0xc4,
    0x89, 0x29, 0xe5, 0x9a, 0x7d, 0x2c, 0x84, 0xa2, 0x68, 0x43, 0x0b, 0x14, 0x47, 0x4b, 0x6c, 0x39,
    0x30, 0x81, 0x35, 0x85, 0xd3, 0x0f, 0x1f, 0x6f, 0xd3, 0x9c, 0x56, 0xf0, 0xf1, 0x9f, 0xcb, 0x0f,
    0x7f, 0x06, 0x3c, 0x11, 0x3a, 0x40, 0xc7, 0x23, 0x14, 0x58, 0xbe, 0x38, 0xbd, 0xa0, 0x9e, 0x21,
    0x49, 0xc4, 0x69, 0x5f, 0x03, 0x6a, 0x43, 0x13, 0xc8, 0xe8, 0xb2, 0xb6, 0xde, 0x53, 0x06, 0x8c,
    0xee, 0x3a, 0x65, 0xf8, 0x18, 0x0d, 0xe0, 0x7f, 0x00, 0xfc, 0x7f, 0xbc, 0xd3, 0x89, 0x40, 0xa1,
    0xcc, 0x35, 0x2d, 0xe1, 0x83, 0xd2, 0xac, 0xad, 0xe7, 0x14, 0xc2, 0xea, 0xc2, 0x35, 0x84, 0x8f,
    0x75, 0xd5, 0xc2, 0x69, 0xc5, 0x60, 0x58, 0x43, 0x02, 0x09, 0x00, 0x5b, 0x4b, 0xe0, 0x45, 0xe3,
    0x7e, 0x32, 0x0e, 0x66, 0x8a, 0x4c, 0x08, 0x0d, 0x7d, 0x25, 0x18, 0xe4, 0x20, 0x93, 0xa4, 0x67,
    0xa0, 0x0c,
];

#[test]
fn java_long_window_capture_viewer_reads_open_and_archived_documents() {
    let fixture = Scratch::new();
    fs::write(
        fixture.journal.join("session-log.json.zst"),
        JAVA_LONG_WINDOW,
    )
    .unwrap();
    for ended_at in ["", "<endedAt>2026-10-01T10:00:00Z</endedAt>"] {
        let document = format!(
            r#"<session-journal formatVersion="1" id="{ID}"><meta>{ended_at}</meta></session-journal>"#
        );
        fs::write(fixture.journal.join("journal.xml"), document).unwrap();
        let page = capture_page(&fixture.journal, None, Some(1..=1), Some("JAVA JOURNAL")).unwrap();
        assert_eq!(page.status.notice(), None);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].text, "long-window Java journal fixture");
        assert!(page.next.is_none());
    }
}

#[test]
fn java_long_window_capture_viewer_resumes_within_a_compressed_part() {
    let fixture = Scratch::new();
    // Leading blank lines exhaust the first page's work limit. The next zstd
    // frame is the unchanged Java fixture in the same physical log part.
    let mut bytes = compress_to_vec(
        vec![b'\n'; MAX_SCAN_LINES as usize].as_slice(),
        CompressionLevel::Fastest,
    );
    bytes.extend_from_slice(JAVA_LONG_WINDOW);
    fs::write(fixture.journal.join("session-log.json.zst"), bytes).unwrap();

    let first = capture_page(&fixture.journal, None, None, None).unwrap();
    assert!(first.records.is_empty());
    assert_eq!(first.status.notice(), None);
    let cursor = first
        .next
        .expect("first page should pause at its line limit");
    let second = capture_page(&fixture.journal, Some(cursor), None, None).unwrap();
    assert_eq!(second.status.notice(), None);
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].sequence, 1);
    assert_eq!(second.records[0].text, "long-window Java journal fixture");
    assert!(second.next.is_none());
}

#[test]
fn java_long_window_capture_matches_catalog_fulltext_and_keeps_reader_limits() {
    let fixture = Scratch::new();
    let file = fixture.journal.join("session-log.json.zst");
    fs::write(&file, JAVA_LONG_WINDOW).unwrap();
    let root = fixture.journal.parent().unwrap();
    let snapshot = catalog(root).unwrap();
    let query = tree::prepare_query("LONG-WINDOW Java");
    let search =
        || content_search::scan(root, &snapshot, tree::Filter::All, &query, &|| false).unwrap();
    let result = search();
    assert!(result.matching_indices.contains(&0));
    assert!(!result.partial);

    let source = open_document(&fixture.journal).unwrap();
    let mut budget = 128;
    let limited = content_match(&source, ID, 1, &query, &mut budget, &|| false).unwrap();
    assert!(!limited.found && limited.partial);
    assert_eq!(budget, 0);

    let mut excessive = JAVA_LONG_WINDOW.to_vec();
    excessive[5] = 0x89; // The frame asks for a 144 MiB window.
    fs::write(&file, excessive).unwrap();
    let result = search();
    assert!(result.matching_indices.is_empty());
    assert!(result.partial);
    let page = capture_page(&fixture.journal, None, None, None).unwrap();
    assert!(page.records.is_empty());
    assert_eq!(page.status.unreadable_parts, 1);
}
