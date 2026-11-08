use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use flate2::write::GzEncoder;
use jiff::tz::TimeZone;

use super::*;
use crate::session_journal::export::{Filter, MarkerMode, Snapshot, TimeWindow, load_for_bundle};
use crate::session_journal::{capture_page, catalog, read};

// The retained and omitted entries deliberately share an ID and appear out of time order.
const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- original-journal-layout -->
<session-journal formatVersion="1" id="bundle">
  <meta origin="fixture">
    <title>Bundle fixture</title><logFormat>JSON</logFormat><logParts>2</logParts>
    <commandCount old=">">99</commandCount><errorCount old="yes"/><screenshotCount>99</screenshotCount>
    <customMeta>A &amp; B</customMeta>
  </meta>
  <markerDefinitions>
    <marker><id>keep</id><name>Keep marker</name><color>#425678</color></marker>
    <marker><id>drop</id><name>Omitted marker</name></marker>
  </markerDefinitions>
  <entries layout="retained">
    <entry>
      <id>duplicate</id><kind>USER_NOTE</kind><createdAt>2026-11-06T16:00:00Z</createdAt>
      <markerId>drop</markerId><title>Omitted entry</title><logStartSeq>4</logStartSeq><logEndSeq>5</logEndSeq>
      <screenshotFile>screenshots/dropped-entry.png</screenshotFile>
    </entry>
    <!-- entry separator -->
    <entry>
      <id>duplicate</id><kind>AGENT</kind><createdAt>2026-11-06T09:00:00Z</createdAt>
      <markerId>keep</markerId><marker>ERROR</marker><title>Retained entry</title>
      <logStartSeq>3</logStartSeq><logEndSeq>1</logEndSeq>
      <screenshotFile>screenshots/entry.png</screenshotFile><customEntry flag="yes">preserved</customEntry>
      <chatId>private-chat-id</chatId><chatLinkedAt>2026-11-06T09:00:00Z</chatLinkedAt>
      <chatSnapshot><title>Private chat title</title><messages>
        <message><role>user</role><content>private chat message</content></message>
      </messages></chatSnapshot>
    </entry>
    <entry>
      <id>private-shot</id><kind>SCREENSHOT</kind><createdAt>2026-11-06T09:02:00Z</createdAt>
      <markerId>keep</markerId><title>Edited image</title><logEndSeq>3</logEndSeq>
      <screenshotFile>screenshots/private.orig.png</screenshotFile>
    </entry>
  </entries>
</session-journal>
"#;

const FIRST: &str = r#"{"type":"meta","formatVersion":1,"journalId":"bundle","part":1,"tabSessionId":"fixture-tab"}
{"seq":1,"t":"2026-11-06T09:00:00Z","k":"in","x":"retained command"}
{"seq":2,"t":"2026-11-06T11:00:00Z","k":"out","x":"outside period"}
{"seq":3,"t":"2026-11-06T09:01:00Z","k":"screenshot","file":"screenshots/log.png"}
{"seq":4,"t":"2026-11-06T09:03:00Z","k":"out","x":"outside referenced range"}
{"type":"end"}
"#;
const SECOND: &str = r#"{"type":"meta","formatVersion":1,"journalId":"bundle","part":2,"tabSessionId":"fixture-tab"}
{"seq":5,"t":"2026-11-06T16:00:00Z","k":"screenshot","file":"screenshots/dropped-log.png"}
{"type":"end"}
"#;

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
    png: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-bundle-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = root.join("fixture");
        let screenshots = journal.join("screenshots");
        fs::create_dir_all(&screenshots).unwrap();
        fs::write(journal.join(DOCUMENT), XML).unwrap();
        fs::write(journal.join("session-log-2.json"), SECOND).unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(FIRST.as_bytes()).unwrap();
        fs::write(journal.join("session-log.json.gz"), gzip.finish().unwrap()).unwrap();
        let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
        for image in [
            "entry.png",
            "log.png",
            "dropped-entry.png",
            "dropped-log.png",
            "private.orig.png",
            "unreferenced.png",
        ] {
            fs::write(screenshots.join(image), &png).unwrap();
        }
        fs::write(journal.join("journal.html"), b"stale-foreign-html").unwrap();
        Self { root, journal, png }
    }

    fn load(&self) -> Snapshot {
        let catalog = catalog(&self.root).unwrap();
        let row = &catalog.journals[0];
        load_for_bundle(
            &self.root,
            &row.directory,
            &row.id,
            row.deletion_identity.as_ref().unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn bundle(fixture: &Fixture, options: &Options) -> (BundleStats, BTreeMap<String, Vec<u8>>) {
    let mut members = BTreeMap::new();
    let stats = fixture
        .load()
        .write_html_bundle(
            options,
            true,
            |name, bytes| {
                assert!(members.insert(name.into(), bytes.to_vec()).is_none());
                Ok(())
            },
            &|| false,
        )
        .unwrap();
    (stats, members)
}

#[test]
#[ignore = "writes a browser smoke fixture into the test scratch directory"]
fn journal_html_bundle_browser_fixture() {
    let fixture = Fixture::new();
    let (_, members) = bundle(&fixture, &Options::default());
    let output = std::env::temp_dir().join("journal-browser-fixture");
    fs::create_dir(&output).unwrap();
    for (name, bytes) in members {
        let member = output.join(name);
        fs::create_dir_all(member.parent().unwrap()).unwrap();
        fs::write(member, bytes).unwrap();
    }
}

#[test]
fn html_bundle_keeps_valid_original_xml_decompressed_logs_and_visible_images() {
    let fixture = Fixture::new();
    let shots = fixture.journal.join("screenshots");
    fs::write(shots.join("damaged.png"), b"not a PNG").unwrap();
    let outside = fixture.root.join("outside.png");
    fs::write(&outside, &fixture.png).unwrap();
    fs::hard_link(&outside, shots.join("linked.png")).unwrap();
    fs::write(shots.join("other.ORIG.png"), &fixture.png).unwrap();
    let (stats, members) = bundle(&fixture, &Options::default());
    assert_eq!(stats.command_count, 99);
    assert_eq!(stats.skipped_assets, 3);
    assert!(!stats.capture_incomplete);
    assert_eq!(members[DOCUMENT], XML.as_bytes());
    assert_eq!(members["session-log.json"], FIRST.as_bytes());
    assert_eq!(members["session-log-2.json"], SECOND.as_bytes());
    assert_eq!(members["screenshots/entry.png"], fixture.png);
    assert_eq!(members["screenshots/log.png"], fixture.png);
    assert!(members.contains_key("screenshots/dropped-entry.png"));
    assert!(members.contains_key("screenshots/dropped-log.png"));
    assert!(!members.contains_key("screenshots/private.orig.png"));
    assert!(!members.contains_key("screenshots/other.ORIG.png"));
    assert!(!members.contains_key("screenshots/damaged.png"));
    assert!(!members.contains_key("screenshots/linked.png"));
    assert_eq!(members["screenshots/unreferenced.png"], fixture.png);
    assert!(!members.contains_key("session-log.json.gz"));
    let html = std::str::from_utf8(&members["journal.html"]).unwrap();
    assert!(html.contains("Bundle fixture"));
    assert!(html.contains("private chat message"));
    assert!(!html.contains("stale-foreign-html"));
}

#[test]
fn html_bundle_filters_selected_ranges_time_and_screenshots_and_recounts_xml() {
    let fixture = Fixture::new();
    let options = Options {
        include_chats: false,
        filter: Filter {
            marker_mode: MarkerMode::Selected,
            marker_ids: HashSet::from(["keep".into()]),
            windows: vec![TimeWindow {
                from_time: Some("09:00:00".parse().unwrap()),
                to_time: Some("09:05:00".parse().unwrap()),
                ..TimeWindow::default()
            }],
            tolerance_minutes: 0,
            zone: TimeZone::UTC,
            ..Filter::default()
        },
        ..Options::default()
    };
    let (stats, members) = bundle(&fixture, &options);
    assert_eq!(stats.command_count, 1);
    assert_eq!(stats.error_count, 1);
    assert_eq!(stats.screenshot_count, 1);
    assert_eq!(stats.skipped_assets, 1);
    let xml = std::str::from_utf8(&members[DOCUMENT]).unwrap();
    for kept in [
        "<!-- original-journal-layout -->",
        "<!-- entry separator -->",
        "<entries layout=\"retained\">",
        "<customMeta>A &amp; B</customMeta>",
        "<customEntry flag=\"yes\">preserved</customEntry>",
        "<commandCount old=\">\">1</commandCount>",
        "<errorCount old=\"yes\">1</errorCount>",
        "<screenshotCount>1</screenshotCount>",
        "<color>#425678</color>",
        "Retained entry",
    ] {
        assert!(xml.contains(kept), "{kept}");
    }
    for omitted in [
        "Omitted entry",
        "Omitted marker",
        "chatId",
        "chatLinkedAt",
        "chatSnapshot",
    ] {
        assert!(!xml.contains(omitted), "{omitted}");
    }
    let part = std::str::from_utf8(&members["session-log.json"]).unwrap();
    assert!(part.contains("\"tabSessionId\":\"fixture-tab\""));
    assert!(part.contains("retained command"));
    assert!(!part.contains("outside period"));
    assert!(!part.contains("outside referenced range"));
    assert!(part.ends_with("{\"type\":\"end\"}\n"));
    assert!(!members.contains_key("session-log-2.json"));
    assert_eq!(members["screenshots/entry.png"], fixture.png);
    assert_eq!(members["screenshots/log.png"], fixture.png);
    assert!(
        !members
            .keys()
            .any(|name| name.contains("dropped") || name.ends_with(".orig.png"))
    );
    assert!(!members.contains_key("screenshots/unreferenced.png"));
    let html = std::str::from_utf8(&members["journal.html"]).unwrap();
    for omitted in [
        "Omitted entry",
        "outside period",
        "outside referenced range",
        "private chat message",
    ] {
        assert!(!html.contains(omitted), "{omitted}");
    }
    // Extract through the test's private directory and exercise the normal v1 readers.
    let extracted = fixture.root.join("extracted");
    fs::create_dir_all(extracted.join("screenshots")).unwrap();
    for (name, bytes) in members {
        fs::write(extracted.join(name), bytes).unwrap();
    }
    assert_eq!(read(&extracted).unwrap().timeline.len(), 2);
    let page = capture_page(&extracted, None, None, None).unwrap();
    assert_eq!(
        page.records
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn html_bundle_removes_all_chat_fields_when_requested_without_other_xml_changes() {
    let fixture = Fixture::new();
    let options = Options {
        include_chats: false,
        ..Options::default()
    };
    let (_, members) = bundle(&fixture, &options);
    let xml = std::str::from_utf8(&members[DOCUMENT]).unwrap();
    let before = parse_document(XML).unwrap();
    let patches: Vec<_> = before
        .descendants()
        .filter(|node| {
            ["chatId", "chatLinkedAt", "chatSnapshot"]
                .iter()
                .any(|tag| node.has_tag_name(*tag))
        })
        .map(|node| node.range())
        .collect();
    let mut expected = XML.to_owned();
    for range in patches.into_iter().rev() {
        expected.replace_range(range, "");
    }
    assert_eq!(xml, expected);
    assert_eq!(members["session-log.json"], FIRST.as_bytes());
    let html = std::str::from_utf8(&members["journal.html"]).unwrap();
    assert!(!html.contains("private chat message"));
    assert!(!html.contains("private-chat-id"));
}

#[test]
fn html_bundle_rejects_source_change_after_an_archive_member_has_been_emitted() {
    let fixture = Fixture::new();
    let snapshot = fixture.load();
    let mut changed = false;
    let error = snapshot
        .write_html_bundle(
            &Options::default(),
            true,
            |_, _| {
                if !changed {
                    // Test double for an external save arriving while the staged archive is written.
                    fs::write(
                        fixture.journal.join(DOCUMENT),
                        XML.replace("Bundle fixture", "Changed bundle fixture"),
                    )?;
                    changed = true;
                }
                Ok(())
            },
            &|| false,
        )
        .unwrap_err();
    assert!(changed);
    assert!(error.to_string().contains("changed"));
}
