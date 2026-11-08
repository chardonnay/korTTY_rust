use std::fmt::Write as _;
use std::fs;

use image::ImageEncoder as _;
use image::codecs::png::PngEncoder;
use jiff::tz::TimeZone;

use super::*;

const ID: &str = "00000000-0000-4000-8000-000000000001";

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
}

impl Fixture {
    fn new(entries: &str, markers: &str) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-export-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = root.join(format!("journal-{ID}"));
        fs::create_dir_all(&journal).unwrap();
        fs::write(
            journal.join("journal.xml"),
            format!("<session-journal formatVersion=\"1\" id=\"{ID}\">\
                <meta><title>Session &lt;demo&gt;</title><description>Night shift</description>\
                <host>host.test</host><port>22</port><username>operator</username><connectionName>Shell</connectionName>\
                <startedAt>2026-10-01T10:00:00+02:00</startedAt><endedAt>2026-10-01T09:32:00Z</endedAt>\
                <commandCount>9</commandCount><errorCount>1</errorCount><screenshotCount>1</screenshotCount>\
                </meta><entries>{entries}</entries><markerDefinitions>{markers}</markerDefinitions></session-journal>"),
        )
        .unwrap();
        Self { root, journal }
    }

    fn identity(&self) -> DeletionIdentity {
        super::super::catalog(&self.root).unwrap().journals[0]
            .deletion_identity
            .clone()
            .unwrap()
    }

    fn load(&self) -> Snapshot {
        load(&self.root, &self.journal, ID, &self.identity()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn window(from: &str, to: &str) -> TimeWindow {
    TimeWindow {
        from_time: parse_time_of_day(from).unwrap(),
        to_time: parse_time_of_day(to).unwrap(),
        ..TimeWindow::default()
    }
}

fn options() -> Options {
    Options {
        filter: Filter {
            zone: TimeZone::UTC,
            ..Filter::default()
        },
        ..Options::default()
    }
}

#[test]
fn journal_exports_withhold_uncommitted_original_while_screenshot_edit_rolls_back() {
    use crate::session_journal::annotation::{Annotation, Kind};
    use crate::session_journal::entry_edit::lock_source;
    use crate::session_journal::export_branding::PdfBranding;
    use crate::session_journal::screenshot_edit::{Draft, XmlWrite, apply};
    use crate::session_journal::viewer::{Screenshot, Source};
    use std::sync::Arc;

    const ENTRY_ID: &str = "shot";
    const REFERENCE: &str = "screenshots/shot.png";
    let fixture = Fixture::new(
        "<entry><id>shot</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/shot.png</screenshotFile></entry>",
        "",
    );
    fs::create_dir(fixture.journal.join("screenshots")).unwrap();
    let visible = fixture.journal.join(REFERENCE);
    let mut image = tiny_skia::Pixmap::new(24, 16).unwrap();
    image.fill(tiny_skia::Color::WHITE);
    let original = image.encode_png().unwrap();
    fs::write(&visible, &original).unwrap();
    let source =
        Source::catalog(&super::super::catalog(&fixture.root).unwrap().journals[0]).unwrap();
    let draft = Arc::new(Draft::read(&source, ENTRY_ID, REFERENCE).unwrap());
    let mark = Annotation {
        kind: Kind::Box,
        points: vec![[4.0, 3.0], [12.0, 9.0]],
        ..Annotation::default()
    };
    apply(&draft.edit(vec![mark], String::new()).unwrap()).unwrap();
    let annotated = fs::read(&visible).unwrap();
    assert_ne!(annotated, original);

    let snapshot =
        load_for_bundle(&fixture.root, &fixture.journal, ID, &fixture.identity()).unwrap();
    let options = options();
    let draft = Arc::new(Draft::read(&source, ENTRY_ID, REFERENCE).unwrap());
    let clear = draft.edit(Vec::new(), String::new()).unwrap();
    let lock = lock_source(&source).unwrap();
    let (stamp, original_xml) = lock.read_document().unwrap();
    let error = clear
        .publish_with(lock.directory(), |operation| match operation {
            XmlWrite::Check => lock.require_document(&stamp).map(|()| false),
            XmlWrite::Commit(verify) => {
                verify()?;
                // The real image transaction has installed the original, but the old XML is current.
                assert_eq!(fs::read(&visible).unwrap(), original);
                let markdown = snapshot.render(&options, "report-files").unwrap();
                assert!(markdown.assets.is_empty());
                assert_eq!(markdown.skipped_assets, 1);
                let pdf = snapshot
                    .render_pdf(&options, &PdfBranding::default(), &|| false)
                    .unwrap();
                assert_eq!(pdf.skipped_assets, 1);
                let bundle = snapshot
                    .write_html_bundle(
                        &options,
                        false,
                        |name, _| {
                            assert!(!name.starts_with("screenshots/"));
                            Ok(())
                        },
                        &|| false,
                    )
                    .unwrap();
                assert_eq!(bundle.skipped_assets, 1);
                assert_eq!(
                    Screenshot::review(&source, ENTRY_ID, REFERENCE)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::WouldBlock
                );
                // Test double rejects the XML commit after using the real guarded readers.
                Err(io::Error::other(
                    "Injected screenshot XML publication failure",
                ))
            }
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Injected screenshot XML publication failure"
    );
    assert_eq!(lock.read_document().unwrap().1, original_xml);
    assert_eq!(fs::read(visible).unwrap(), annotated);
    assert_eq!(
        snapshot.render(&options, "report-files").unwrap().assets[0].bytes,
        annotated
    );
}

#[test]
fn export_reads_all_entries_and_preserves_offset_ties_and_undated_order() {
    let mut entries = String::from(
        "<entry><title>Undated entry</title></entry>\
        <entry><createdAt>2026-10-01T10:00:00+02:00</createdAt><title>Equal instant later offset</title></entry>\
        <entry><createdAt>2026-10-01T08:00:00Z</createdAt><title>Equal instant earlier offset</title></entry>",
    );
    for index in 0..super::super::MAX_TIMELINE_ROWS + 1 {
        write!(entries, "<entry><title>Extra entry {index}</title></entry>").unwrap();
    }
    let fixture = Fixture::new(&entries, "");
    let snapshot = fixture.load();
    let options = options();
    let preview = snapshot.preview(&options.filter).unwrap();
    let markdown = snapshot.render(&options, "report-files").unwrap();
    assert_eq!(preview.total_entries, super::super::MAX_TIMELINE_ROWS + 4);
    assert_eq!(preview.total_entries, preview.kept_entries);
    let first = markdown.text.find("Equal instant earlier offset").unwrap();
    let second = markdown.text.find("Equal instant later offset").unwrap();
    let last = markdown.text.find("Undated entry").unwrap();
    assert!(first < second && second < last);
    assert!(
        markdown
            .text
            .contains(&format!("Extra entry {}", super::super::MAX_TIMELINE_ROWS))
    );
    assert!(markdown.text.starts_with("# Session &lt;demo&gt;\n\n"));
    assert!(
        markdown
            .text
            .contains("- **Started:** 2026-10-01 10:00 · **Duration:** 1h 32m")
    );
}

#[test]
fn export_preview_combines_daily_windows_markers_regex_and_drop_reasons() {
    let fixture = Fixture::new(
        "<entry><createdAt>2026-10-01T07:00:00Z</createdAt><title>Outside</title></entry>\
         <entry><createdAt>2026-10-01T14:01:00Z</createdAt><title>Day window</title><markerId>deploy</markerId>\
           <inputExcerpt><line>Deploy Prod</line></inputExcerpt></entry>\
         <entry><createdAt>2026-10-01T23:50:00Z</createdAt><title>Night window</title><markerId>deploy</markerId>\
           <outputExcerpt><line>DEPLOY prod</line></outputExcerpt></entry>\
         <entry><createdAt>2026-10-02T00:12:00Z</createdAt><title>Unmarked</title><text>deploy prod</text></entry>\
         <entry><createdAt>2026-10-02T00:23:00Z</createdAt><title>Different subject</title><markerId>deploy</markerId></entry>\
         <entry><title>Undated</title><markerId>deploy</markerId></entry>",
        "<marker><id>deploy</id><name>Deployment</name><legacyMarker>INFO</legacyMarker></marker>",
    );
    let snapshot = fixture.load();
    assert_eq!(
        snapshot.used_markers(),
        [Marker {
            id: "deploy".into(),
            name: "Deployment".into()
        }]
    );
    let options = Options {
        filter: Filter {
            zone: TimeZone::UTC,
            windows: vec![window("14", "14:05"), window("2345", "00.25")],
            tolerance_minutes: 0,
            topic: " deploy\\s+prod ".into(),
            topic_regex: true,
            marker_mode: MarkerMode::Selected,
            marker_ids: ["deploy".to_owned()].into(),
        },
        ..Options::default()
    };
    let preview = snapshot.preview(&options.filter).unwrap();
    assert_eq!(
        preview,
        Preview {
            total_entries: 6,
            kept_entries: 2,
            dropped_by_time: 1,
            dropped_by_marker: 1,
            dropped_by_topic: 1,
            dropped_undated: 1,
        }
    );
    let markdown = snapshot.render(&options, "report-files").unwrap();
    assert!(
        markdown
            .text
            .contains("Markers: Deployment · 2 of 6 entries")
    );
    assert!(markdown.text.contains("`[Deployment]`"));
    assert!(!markdown.text.contains("Different subject"));
}

#[test]
fn export_time_windows_use_span_overlap_tolerance_and_a_bounded_lookback() {
    let fixture = Fixture::new(
        "<entry><createdAt>2026-10-01T07:00:00Z</createdAt></entry>\
         <entry><createdAt>2026-10-01T12:03:00Z</createdAt></entry>\
         <entry><createdAt>2026-10-01T17:00:00Z</createdAt></entry>",
        "",
    );
    let snapshot = fixture.load();
    let mut filter = Filter {
        windows: vec![TimeWindow {
            from_date: Some("2026-10-01".parse().unwrap()),
            to_date: Some("2026-10-01".parse().unwrap()),
            ..window("1205", "1205")
        }],
        tolerance_minutes: 2,
        zone: TimeZone::UTC,
        ..Filter::default()
    };
    assert_eq!(snapshot.preview(&filter).unwrap().kept_entries, 1);
    filter.tolerance_minutes = 0;
    assert_eq!(snapshot.preview(&filter).unwrap().kept_entries, 0);
    filter.windows = vec![window("11 50", "11:55")];
    assert_eq!(snapshot.preview(&filter).unwrap().kept_entries, 1);
    filter.windows = vec![window("10:00", "10:05")];
    assert_eq!(snapshot.preview(&filter).unwrap().kept_entries, 0);
}

#[test]
fn export_validates_filter_input_and_refuses_empty_filtered_output() {
    for time in ["8", "08", "8:00", "8.0", "8 00", "0800", "800"] {
        assert_eq!(
            parse_time_of_day(time).unwrap(),
            Some("08:00".parse().unwrap())
        );
    }
    assert!(parse_time_of_day("25:00").is_err());
    assert!(parse_time_of_day("8  30").is_err());
    let fixture = Fixture::new("<entry><title>Case [literal]</title></entry>", "");
    let snapshot = fixture.load();
    let mut options = options();
    options.filter.topic = "[LITERAL]".into();
    assert_eq!(snapshot.preview(&options.filter).unwrap().kept_entries, 1);
    options.filter.topic = "[".into();
    options.filter.topic_regex = true;
    assert!(options.filter.validate().is_err());
    options.filter.topic_regex = false;
    options.filter.topic = "missing".into();
    assert!(snapshot.render(&options, "report-files").is_err());
    let empty = Fixture::new("", "");
    assert!(
        empty
            .load()
            .render(&Options::default(), "report-files")
            .unwrap()
            .text
            .starts_with("# Session &lt;demo&gt;")
    );
}

#[test]
fn export_copies_png_and_chat_with_safe_links_fences_and_options() {
    let fixture = Fixture::new(
        "<entry><kind>SCREENSHOT</kind><title>Capture</title><screenshotFile>screenshots/shot-000001.png</screenshotFile>\
         <aiDescription>A &lt;screen&gt;</aiDescription><aiTags><tag>test</tag><tag>demo</tag></aiTags></entry>\
         <entry><id>agent-1</id><kind>AGENT</kind><title>Agent run</title><agentModel>local model</agentModel>\
         <agentDurationMillis>1200</agentDurationMillis><agentTokens>1500</agentTokens>\
         <text>&lt;script&gt;hidden()&lt;/script&gt; [x](javascript:unsafe)</text><outputExcerpt><line>````</line></outputExcerpt>\
         <chatSnapshot><title>Task [test]</title><capturedAt>2026-10-01T09:00:00Z</capturedAt><frozen>true</frozen>\
           <messages><message><role>user</role><content>What happened?</content></message>\
             <message><role>assistant</role><aiProfileName>Local</aiProfileName><content>Done</content>\
             <steps><step><status>DONE</status><title>Read</title><command>ls</command><exitCode>0</exitCode>\
               <output>```</output><truncated>true</truncated></step></steps></message></messages>\
         </chatSnapshot></entry>",
        "",
    );
    let screenshot_dir = fixture.journal.join("screenshots");
    fs::create_dir(&screenshot_dir).unwrap();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&[0, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(screenshot_dir.join("shot-000001.png"), &png).unwrap();
    fs::write(screenshot_dir.join("shot-000001-original.png"), &png).unwrap();
    let snapshot = fixture.load();
    let mut options = options();
    let markdown = snapshot.render(&options, "report (1)-files").unwrap();
    assert_eq!(markdown.skipped_assets, 0);
    assert_eq!(markdown.assets.len(), 2);
    assert!(
        markdown
            .text
            .contains("![Screenshot](report%20%281%29-files/shot-000001.png)")
    );
    assert!(
        markdown
            .text
            .contains("report%20%281%29-files/chat-agent-1.md)")
    );
    assert!(markdown.text.contains("local model · 1 s · 1.5k tokens"));
    assert!(markdown.text.contains("`````text\n````\n`````"));
    assert!(markdown.text.contains("&lt;script&gt;"));
    assert!(!markdown.text.contains("[x](javascript:"));
    assert_eq!(markdown.assets[0].name, "shot-000001.png");
    assert_eq!(markdown.assets[0].bytes, png);
    assert_eq!(markdown.assets[1].name, "chat-agent-1.md");
    let transcript = std::str::from_utf8(&markdown.assets[1].bytes).unwrap();
    assert!(transcript.contains("As of:** 2026-10-01 09:00 (final)"));
    assert!(transcript.contains("## You:"));
    assert!(transcript.contains("### Steps (1)"));
    assert!(transcript.contains("  ````text\n  $ ls\n  ```\n  ````"));
    assert!(transcript.contains("_Output truncated_"));

    #[cfg(unix)]
    {
        let annotated = screenshot_dir.join("shot-000001.png");
        fs::remove_file(&annotated).unwrap();
        std::os::unix::fs::symlink(screenshot_dir.join("shot-000001-original.png"), annotated)
            .unwrap();
        let without_link = snapshot.render(&options, "report-files").unwrap();
        assert_eq!(without_link.skipped_assets, 1);
        assert_eq!(without_link.assets.len(), 1);
        assert!(!without_link.text.contains("![Screenshot]"));
    }

    options.include_screenshots = false;
    options.include_chats = false;
    let markdown = snapshot.render(&options, "report-files").unwrap();
    assert!(markdown.assets.is_empty());
    assert!(
        markdown
            .text
            .contains("**AI description:** A &lt;screen&gt;")
    );
    assert!(!markdown.text.contains("**AI chat:**"));
}

#[test]
fn export_checks_snapshot_identity_and_blocks_asset_traversal() {
    let fixture = Fixture::new(
        "<entry><kind>SCREENSHOT</kind><screenshotFile>screenshots/../elsewhere.png</screenshotFile></entry>",
        "",
    );
    let identity = fixture.identity();
    let snapshot = fixture.load();
    let markdown = snapshot
        .render(&Options::default(), "report-files")
        .unwrap();
    assert!(markdown.assets.is_empty());
    assert_eq!(markdown.skipped_assets, 1);
    assert!(
        snapshot
            .render(&Options::default(), "../report-files")
            .is_err()
    );
    assert!(load(&fixture.root, &fixture.journal, "wrong", &identity).is_err());
    let moved = fixture.root.join("moved");
    fs::rename(&fixture.journal, &moved).unwrap();
    fs::create_dir(&fixture.journal).unwrap();
    fs::copy(
        moved.join("journal.xml"),
        fixture.journal.join("journal.xml"),
    )
    .unwrap();
    assert!(load(&fixture.root, &fixture.journal, ID, &identity).is_err());
    assert!(
        snapshot
            .render(&Options::default(), "report-files")
            .is_err()
    );
}
