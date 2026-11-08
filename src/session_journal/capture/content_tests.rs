use std::cell::Cell;
use std::fs;
use std::io::Write as _;

use flate2::write::GzEncoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::live_tests::{ID, Scratch, header, record};
use super::{MAX_LINE_BYTES, MAX_PAGE_RECORDS, content_match, open_document};

const TIME: &str = "2026-10-01T01:02:03Z";
const END: &str = "{\"type\":\"end\"}\n";

fn gzip(input: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn content_search_reads_rotated_encodings_and_reaches_beyond_a_viewer_page() {
    let fixture = Scratch::new();
    fs::write(
        fixture.journal.join("session-log.xml"),
        format!(
            "<session-log formatVersion=\"1\" journalId=\"{ID}\" part=\"1\">\n\
             <out seq=\"1\" t=\"{TIME}\">Stra<!-- marker -->ße &amp; Äpfel</out>\n\
             </session-log>\n"
        ),
    )
    .unwrap();
    let mut json = header(2);
    for index in 1..=MAX_PAGE_RECORDS {
        json.push_str(&record(index as u64, "out", "filler"));
    }
    json.push_str(&record(MAX_PAGE_RECORDS as u64 + 1, "note", "Second part"));
    json.push_str(END);
    fs::write(
        fixture.journal.join("session-log-2.json.zst"),
        compress_to_vec(json.as_bytes(), CompressionLevel::Fastest),
    )
    .unwrap();
    let yaml = format!(
        "formatVersion: 1\njournalId: \"{ID}\"\npart: 3\nmeta: {}entries:\n- {}end: true\n",
        header(3),
        record(1, "in", "Third part")
    );
    fs::write(
        fixture.journal.join("session-log-3.yaml.gz"),
        gzip(yaml.as_bytes()),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    for query in ["straße & äpfel", "second part", "third part"] {
        let result = content_match(&source, ID, 3, query, &mut (1 << 20), &|| false).unwrap();
        assert!(result.found, "{query}");
        assert!(!result.partial, "{query}");
    }
}

#[test]
fn content_search_uses_persisted_text_and_excludes_partial_records() {
    let fixture = Scratch::new();
    fs::write(
        fixture.journal.join("session-log.json"),
        format!(
            "{}{{\"seq\":1,\"t\":\"{TIME}\",\"k\":\"out\",\"x\":\"needle\",\"partial\":true}}\n\
             {{\"seq\":2,\"t\":\"{TIME}\",\"k\":\"screenshot\",\"file\":\"screenshots/needle.png\"}}\n\
             {}{END}",
            header(1),
            record(3, "out", "nee\u{1b}dle")
        ),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let result = content_match(&source, ID, 1, "needle", &mut 2048, &|| false).unwrap();
    assert!(!result.found && !result.partial);
}

#[test]
fn content_search_stops_after_the_first_qualifying_record() {
    let fixture = Scratch::new();
    let json = format!(
        "{}{}{}malformed trailing line\n{END}",
        header(1),
        record(1, "out", "needle"),
        record(2, "out", &"filler".repeat(8192))
    );
    fs::write(
        fixture.journal.join("session-log.json.gz"),
        gzip(json.as_bytes()),
    )
    .unwrap();
    fs::write(fixture.journal.join("session-log-2.json"), b"corrupt").unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let initial = 1 << 20;
    let mut remaining = initial;
    let result = content_match(&source, ID, 2, "needle", &mut remaining, &|| false).unwrap();
    assert!(result.found && !result.partial);
    assert!(initial - remaining < json.len() as u64);
}

#[test]
fn content_search_bounds_raw_and_decoded_reads_across_journals() {
    let fixture = Scratch::new();
    let later = Scratch::new();
    fs::write(
        later.journal.join("session-log.json"),
        format!("{}{}{END}", header(1), record(1, "out", "needle")),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let later_source = open_document(&later.journal).unwrap();
    let decoded = format!("{}{}{END}", header(1), record(1, "out", &"x".repeat(4096)));
    let packed = gzip(decoded.as_bytes());
    assert!(packed.len() < 256 && decoded.len() > 256);

    let mut many_members = gzip(format!("{}{END}", header(1)).as_bytes());
    many_members.extend(gzip(b"").repeat(32));
    assert!(many_members.len() > 256);

    for compressed in [packed, many_members] {
        fs::write(fixture.journal.join("session-log.json.gz"), compressed).unwrap();
        let mut budget = 256;
        let result = content_match(&source, ID, 1, "needle", &mut budget, &|| false).unwrap();
        assert!(!result.found && result.partial);
        assert_eq!(budget, 0);
        let result = content_match(&later_source, ID, 1, "needle", &mut budget, &|| false).unwrap();
        assert!(!result.found && result.partial);
    }
}

#[test]
fn content_search_recovers_from_missing_parts_and_overlong_lines() {
    let fixture = Scratch::new();
    fs::write(
        fixture.journal.join("session-log.json"),
        format!(
            "{}{}\n{{\"torn\":",
            header(1),
            "x".repeat(MAX_LINE_BYTES as usize + 16)
        ),
    )
    .unwrap();
    fs::write(
        fixture.journal.join("session-log-3.json"),
        format!("{}{}{END}", header(3), record(1, "out", "needle")),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let result = content_match(&source, ID, 3, "needle", &mut (1 << 20), &|| false).unwrap();
    assert!(result.found && result.partial);
}

#[test]
fn content_search_cancellation_interrupts_a_line_drain() {
    let fixture = Scratch::new();
    fs::write(
        fixture.journal.join("session-log.json"),
        format!(
            "{}{}\n{END}",
            header(1),
            "x".repeat(2 * MAX_LINE_BYTES as usize)
        ),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let polls = Cell::new(0);
    let mut budget = 1 << 20;
    let cancelled = || {
        polls.set(polls.get() + 1);
        polls.get() > 75
    };
    let result = content_match(&source, ID, 1, "needle", &mut budget, &cancelled);
    assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::Interrupted));
    assert!((1 << 20) - budget > MAX_LINE_BYTES);
    assert!((1 << 20) - budget < 2 * MAX_LINE_BYTES);
}

#[cfg(unix)]
#[test]
fn content_search_refuses_linked_parts() {
    let fixture = Scratch::new();
    let external = fixture.journal.join("unrelated.json");
    fs::write(
        &external,
        format!("{}{}{END}", header(1), record(1, "out", "needle")),
    )
    .unwrap();
    std::os::unix::fs::symlink(external, fixture.journal.join("session-log.json")).unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let result = content_match(&source, ID, 1, "needle", &mut 2048, &|| false).unwrap();
    assert!(!result.found && result.partial);
}
