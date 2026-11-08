use std::cell::Cell;
use std::fs;
use std::io::Write as _;

use flate2::write::GzEncoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::super::java_zstd_tests::JAVA_LONG_WINDOW;
use super::super::live_tests::{ID, Scratch};
use super::super::open_document;
use super::*;

const TIME: &str = "2026-10-01T11:05:00+02:00";
const LATE: &str = "2026-10-01T12:10:00+02:00";
const XML_FOOTER: &str = "</session-log>\n";
const JSON_FOOTER: &str = "{\"type\":\"end\"}\n";
const YAML_FOOTER: &str = "end: true\n";

fn header(part: u32, format: Format) -> String {
    let json = format!(
        "{{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"{ID}\",\"part\":{part},\"host\":\"host.test\",\"port\":22,\"username\":\"operator\",\"connectionName\":\"Shell\",\"appVersion\":\"2.26.0\",\"startedAt\":\"{TIME}\",\"tabSessionId\":\"tab-42\"}}\n"
    );
    match format {
        Format::Xml => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <session-log formatVersion=\"1\" journalId=\"{ID}\" part=\"{part}\">\n\
             <meta host=\"host.test\" port=\"22\" username=\"operator\" connectionName=\"Shell\" appVersion=\"2.26.0\" startedAt=\"{TIME}\" tabSessionId=\"tab-42\"/>\n"
        ),
        Format::Json => json,
        Format::Yaml => {
            format!("formatVersion: 1\njournalId: \"{ID}\"\npart: {part}\nmeta: {json}entries:\n")
        }
    }
}

fn gzip(input: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}

#[derive(Default)]
struct Captured {
    parts: Vec<(String, Vec<u8>)>,
    records: Vec<PreviewRecord>,
}

impl Captured {
    fn emit(&mut self, output: Output<'_>) -> io::Result<()> {
        match output {
            Output::Part(part) => self
                .parts
                .push((part.name.to_owned(), part.bytes.to_owned())),
            Output::Record(record) => self.records.push(record),
        }
        Ok(())
    }
}

#[test]
fn capture_bundle_decodes_the_pinned_java_long_window_and_keeps_the_cap() {
    let fixture = Scratch::new();
    let part = fixture.journal.join("session-log.json.zst");
    let expected = format!(
        "{{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"{ID}\",\"part\":1,\"tabSessionId\":\"tab-java-zstd\"}}\n\
         {{\"seq\":1,\"t\":\"{TIME}\",\"k\":\"out\",\"x\":\"long-window Java journal fixture\"}}\n{JSON_FOOTER}"
    );
    fs::write(&part, JAVA_LONG_WINDOW).unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let mut captured = Captured::default();
    let exported = export(&source, ID, 1, None, &mut 2048, &|| false, |event| {
        captured.emit(event)
    })
    .unwrap();
    assert!(!exported.partial);
    assert_eq!(
        captured.parts,
        [("session-log.json".into(), expected.into_bytes())]
    );
    assert_eq!(
        captured.records[0].record.text,
        "long-window Java journal fixture"
    );
    assert!(
        super::super::zstandard::Decoder::new(JAVA_LONG_WINDOW, || false)
            .read_to_end(&mut Vec::new())
            .is_err()
    );

    let mut excessive = JAVA_LONG_WINDOW.to_vec();
    excessive[5] = 0x89; // 144 MiB, beyond the bundle's allowed Java window.
    fs::write(part, excessive).unwrap();
    assert!(
        export(&source, ID, 1, None, &mut 2048, &|| false, |_| panic!(
            "oversized window accepted"
        ))
        .is_err()
    );
}

#[test]
fn capture_bundle_ranges_merge_overlap_adjacency_reversed_and_point_anchors() {
    let ranges = Ranges::new([
        (None, None),
        (Some(9), Some(4)),
        (Some(2), Some(3)),
        (Some(6), Some(10)),
        (None, Some(12)),
        (Some(u64::MAX - 1), Some(u64::MAX)),
        (Some(u64::MAX), None),
    ]);
    assert_eq!(ranges.0, [(2, 10), (12, 12), (u64::MAX - 1, u64::MAX)]);
    for sequence in [2, 10, 12, u64::MAX] {
        assert!(ranges.contains(sequence));
    }
    for sequence in [1, 11, 13, u64::MAX - 2] {
        assert!(!ranges.contains(sequence));
    }
}

#[test]
fn capture_bundle_unfiltered_copies_exact_decoded_bytes_and_masks_preview() {
    let fixture = Scratch::new();
    let mut xml = format!("{}{XML_FOOTER}", header(1, Format::Xml))
        .replace('\n', "\r\n")
        .into_bytes();
    xml.extend_from_slice(b"\xfftorn");
    let json = format!(
        "{}{{\"seq\":1,\"t\":\"{TIME}\",\"k\":\"in\",\"x\":\"private input\",\"redacted\":true}}\n{JSON_FOOTER}",
        header(2, Format::Json),
    );
    let yaml = format!(
        "{}- {{\"seq\":2,\"t\":\"{TIME}\",\"k\":\"screenshot\",\"file\":\"screenshots/shot-000002.orig.png\"}}\n{YAML_FOOTER}",
        header(3, Format::Yaml),
    );
    fs::write(fixture.journal.join("session-log.xml"), &xml).unwrap();
    fs::write(
        fixture.journal.join("session-log-2.json.gz"),
        gzip(json.as_bytes()),
    )
    .unwrap();
    fs::write(
        fixture.journal.join("session-log-3.yaml.zst"),
        compress_to_vec(yaml.as_bytes(), CompressionLevel::Fastest),
    )
    .unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let mut captured = Captured::default();
    let exported = export(&source, ID, 3, None, &mut (1 << 20), &|| false, |event| {
        captured.emit(event)
    })
    .unwrap();
    assert_eq!(exported.command_count, 1);
    assert!(exported.partial);
    assert!(exported.screenshot_refs.is_empty());
    assert_eq!(
        captured.parts,
        [
            ("session-log.xml".into(), xml),
            ("session-log-2.json".into(), json.into_bytes()),
            ("session-log-3.yaml".into(), yaml.into_bytes()),
        ]
    );
    assert_eq!(captured.records.len(), 2);
    assert_eq!(captured.records[0].record.sequence, 1);
    assert!(captured.records[0].record.redacted);
    assert!(captured.records[0].record.text.is_empty());
    assert!(captured.records[1].screenshot.is_none());
}

#[test]
fn capture_bundle_filtered_parts_preserve_headers_and_apply_both_constraints() {
    let fixture = Scratch::new();
    let retained_xml = format!("<in seq=\"2\" t=\"{TIME}\" redacted=\"true\">masked input</in>\n");
    let xml = format!(
        "{}<out seq=\"1\" t=\"{TIME}\">before interval</out>\n{retained_xml}\
         <out seq=\"3\" t=\"{LATE}\">outside time</out>\n<out seq=\"4\" t=\"{TIME}\">torn",
        header(1, Format::Xml)
    );
    let dropped_json = format!(
        "{}{{\"seq\":8,\"t\":\"{LATE}\",\"k\":\"out\"}}\n{JSON_FOOTER}",
        header(2, Format::Json),
    );
    let retained_yaml = format!(
        "- {{\"seq\":9,\"t\":\"{TIME}\",\"k\":\"screenshot\",\"file\":\"screenshots/shot-000009.png\"}}\n\
         - {{\"seq\":8,\"t\":\"{TIME}\",\"k\":\"in\",\"partial\":true,\"repeat\":7,\"x\":\"part\\u001bial\"}}\n"
    );
    let yaml = format!("{}{retained_yaml}{YAML_FOOTER}", header(3, Format::Yaml));
    let retained_json =
        format!("{{\"seq\":4,\"t\":\"{TIME}\",\"k\":\"out\",\"x\":\"complete at EOF\"}}");
    let json = format!("{}{retained_json}", header(4, Format::Json));
    fs::write(fixture.journal.join("session-log.xml"), xml).unwrap();
    fs::write(fixture.journal.join("session-log-2.json"), dropped_json).unwrap();
    fs::write(
        fixture.journal.join("session-log-3.yaml.gz"),
        gzip(yaml.as_bytes()),
    )
    .unwrap();
    fs::write(
        fixture.journal.join("session-log-4.json.zst"),
        compress_to_vec(json.as_bytes(), CompressionLevel::Fastest),
    )
    .unwrap();

    let ranges = Ranges::new([(Some(2), Some(4)), (Some(9), Some(8))]);
    let time = TIME.parse::<Timestamp>().unwrap();
    let selection = Selection {
        ranges: &ranges,
        matches_time: &|timestamp| timestamp == time,
    };
    let source = open_document(&fixture.journal).unwrap();
    let mut captured = Captured::default();
    let exported = export(
        &source,
        ID,
        4,
        Some(selection),
        &mut (1 << 20),
        &|| false,
        |event| captured.emit(event),
    )
    .unwrap();
    assert!(exported.partial);
    assert_eq!(exported.command_count, 1);
    assert_eq!(
        exported
            .screenshot_refs
            .iter()
            .map(ScreenshotRef::as_str)
            .collect::<Vec<_>>(),
        ["screenshots/shot-000009.png"]
    );
    assert_eq!(
        captured.parts,
        [
            (
                "session-log.xml".into(),
                format!("{}{retained_xml}{XML_FOOTER}", header(1, Format::Xml)).into_bytes()
            ),
            ("session-log-3.yaml".into(), yaml.into_bytes()),
            (
                "session-log-4.json".into(),
                format!("{}{retained_json}\n{JSON_FOOTER}", header(4, Format::Json)).into_bytes()
            ),
        ]
    );
    let sequences = captured
        .records
        .iter()
        .map(|record| record.record.sequence)
        .collect::<Vec<_>>();
    assert_eq!(sequences, [2, 9, 8, 4]);
    assert!(captured.records[0].record.text.is_empty());
    assert_eq!(
        captured.records[1].screenshot.as_ref().unwrap().as_str(),
        "screenshots/shot-000009.png"
    );
    assert_eq!(captured.records[2].record.repeat, 7);
    assert_eq!(captured.records[2].record.text, "partial");
}

#[test]
fn capture_bundle_reports_missing_parts_and_rejects_decode_limits_or_corruption() {
    let fixture = Scratch::new();
    let original = format!(
        "{}{}{JSON_FOOTER}",
        header(3, Format::Json),
        "\n".repeat(8192)
    );
    let part = fixture.journal.join("session-log-3.json.gz");
    let encoded = gzip(original.as_bytes());
    assert!(encoded.len() < 1024);
    fs::write(&part, &encoded).unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let mut captured = Captured::default();
    assert!(
        export(&source, ID, 3, None, &mut 1024, &|| false, |event| captured
            .emit(event))
        .is_err()
    );
    assert!(captured.parts.is_empty() && captured.records.is_empty());
    let exported = export(&source, ID, 3, None, &mut (1 << 20), &|| false, |event| {
        captured.emit(event)
    })
    .unwrap();
    assert!(exported.partial);
    assert_eq!(captured.parts.len(), 1);
    assert_eq!(captured.parts[0].0, "session-log-3.json");
    fs::write(&part, &encoded[..encoded.len() - 4]).unwrap();
    assert!(export(&source, ID, 3, None, &mut (1 << 20), &|| false, |_| Ok(())).is_err());
}

#[test]
fn capture_bundle_cancels_between_records_and_refuses_foreign_part_headers() {
    let fixture = Scratch::new();
    let row = format!("{{\"seq\":1,\"t\":\"{TIME}\",\"k\":\"out\"}}\n");
    let part = fixture.journal.join("session-log.json");
    let original = format!("{}{row}{row}{JSON_FOOTER}", header(1, Format::Json));
    fs::write(&part, &original).unwrap();
    let source = open_document(&fixture.journal).unwrap();
    let stopped = Cell::new(false);
    let result = export(
        &source,
        ID,
        1,
        None,
        &mut 2048,
        &|| stopped.get(),
        |event| {
            assert!(matches!(event, Output::Record(_)));
            stopped.set(true);
            Ok(())
        },
    );
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Interrupted));
    fs::write(&part, original.replace(ID, "foreign-id")).unwrap();
    assert!(
        export(&source, ID, 1, None, &mut 2048, &|| false, |_| panic!(
            "foreign log output"
        ))
        .is_err()
    );
    assert!(ScreenshotRef::parse("screenshots/../elsewhere.png").is_none());
    assert!(ScreenshotRef::parse("screenshots/shot-000001.ORIG.png").is_none());
}
