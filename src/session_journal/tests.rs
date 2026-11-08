use std::fs;
use std::io::Write as _;

use flate2::Compression;
use flate2::write::GzEncoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-java-journal-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn journal(&self, folder: &str, document: &str) -> PathBuf {
        let directory = self.0.join(folder);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(DOCUMENT), document).unwrap();
        directory
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Deterministic on-disk shapes from the pinned JAXB v1 model and log serializer.
const DOCUMENT_V1: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<session-journal formatVersion="1" id="a7b616c3-13b1-4547-bce8-95fed1109df4">
  <meta>
    <title>A &amp; B</title><description>Offline fixture</description>
    <connectionName>Shell</connectionName><host>localhost</host><port>22</port><username>operator</username>
    <startedAt>2026-11-06T13:00:00+01:00</startedAt><endedAt>2026-11-06T13:00:02+01:00</endedAt>
    <logFormat>XML</logFormat><logParts>3</logParts>
  </meta>
  <entries>
    <entry>
      <id>208e6569-57bf-40ad-9c74-eecb689f9a27</id><kind>SCREENSHOT</kind><marker>INFO</marker><markerId>work-done</markerId>
      <createdAt>2026-11-06T13:00:01+01:00</createdAt><title>Seen</title><text>&lt;b&gt;literal&lt;/b&gt;</text>
      <userNote>Look here</userNote><screenshotFile>screenshots/shot-000009.png</screenshotFile>
      <logPart>2</logPart><logStartSeq>3</logStartSeq><logEndSeq>4</logEndSeq>
      <inputExcerpt><line>ls</line></inputExcerpt><outputExcerpt><line>ALPHA</line></outputExcerpt>
    </entry>
  </entries>
</session-journal>
"##;
const ID: &str = "a7b616c3-13b1-4547-bce8-95fed1109df4";
const TIME: &str = "2026-11-06T13:00:01+01:00";

#[test]
fn imports_java_automation_metadata_and_preserves_empty_present_ids() {
    let fixture = Fixture::new();
    let xml = DOCUMENT_V1.replace("<logFormat>", r#"<sourceKind>JOB</sourceKind><sourceId/><sourceName>Nightly &amp; check</sourceName><runId></runId><runStartedAt>2026-11-06T12:59:00+01:00</runStartedAt><automationAction>AI_SWARM</automationAction><runStatus>CANCELLED</runStatus><pinned>1</pinned><expiresAt>2026-12-01T00:00:00Z</expiresAt><storageBytes>123</storageBytes><logEntryCount>456</logEntryCount><duplicateRunCount>2</duplicateRunCount><aiKeywords><keyword>nginx.conf</keyword><keyword>Disk Full</keyword></aiKeywords><aiPromptTokens>10</aiPromptTokens><aiCompletionTokens>20</aiCompletionTokens><aiTotalTokens>30</aiTotalTokens><aiCallCount>4</aiCallCount><aiCost>0.125</aiCost><aiCostCurrency>EUR</aiCostCurrency><aiProfileLocal>true</aiProfileLocal><logFormat>"#);
    let journal = read(&fixture.journal("automation", &xml)).unwrap();
    let meta = &journal.summary;
    assert_eq!(meta.source, "JOB");
    assert_eq!(meta.source_kind, tree::SourceKind::Job);
    assert_eq!(meta.source_id.as_deref(), Some(""));
    assert_eq!(meta.run_id.as_deref(), Some(""));
    assert_eq!(meta.source_name.as_deref(), Some("Nightly & check"));
    assert_eq!(
        meta.run_started_at,
        Some("2026-11-06T12:59:00+01:00".parse().unwrap())
    );
    assert_eq!(meta.automation_action.as_deref(), Some("AI_SWARM"));
    assert_eq!(meta.run_status, Some(tree::RunStatus::Cancelled));
    assert!(meta.pinned);
    assert_eq!(
        meta.expires_at,
        Some("2026-12-01T00:00:00Z".parse().unwrap())
    );
    assert_eq!(meta.storage_bytes, 123);
    assert_eq!(meta.log_entry_count, 456);
    assert_eq!(meta.duplicate_run_count, 2);
    assert_eq!(meta.keywords, ["nginx.conf", "Disk Full"]);
    assert_eq!(meta.usage.prompt_tokens, 10);
    assert_eq!(meta.usage.completion_tokens, 20);
    assert_eq!(meta.usage.total_tokens, 30);
    assert_eq!(meta.usage.call_count, 4);
    assert_eq!(meta.usage.cost, 0.125);
    assert_eq!(meta.usage.currency.as_deref(), Some("EUR"));
    assert!(meta.usage.local);
    assert!(meta.host_present);
}

#[test]
fn imports_legacy_and_unknown_automation_enums_without_granting_automation_identity() {
    let fixture = Fixture::new();
    let legacy = r#"<session-journal formatVersion="1" id="legacy"><meta><title>Legacy</title></meta><entries/></session-journal>"#;
    let meta = read(&fixture.journal("legacy", legacy)).unwrap().summary;
    assert_eq!(meta.source_kind, tree::SourceKind::Interactive);
    assert_eq!(meta.source, "INTERACTIVE");
    assert!(meta.source_id.is_none());
    assert!(meta.source_name.is_none());
    assert!(meta.run_id.is_none());
    assert!(meta.run_started_at.is_none());
    assert!(meta.run_status.is_none());
    assert!(!meta.pinned);
    assert!(meta.keywords.is_empty());
    assert_eq!(meta.log_entry_count, 0);
    assert_eq!(meta.usage.total_tokens, 0);
    assert!(!meta.host_present);
    let unknown = legacy.replace(
        "</title>",
        "</title><sourceKind>FUTURE</sourceKind><runStatus>FUTURE_STATUS</runStatus>",
    );
    let meta = read(&fixture.journal("unknown", &unknown)).unwrap().summary;
    assert_eq!(meta.source, "FUTURE");
    assert_eq!(meta.source_kind, tree::SourceKind::Interactive);
    assert_eq!(meta.run_status, None);
    let concealed = legacy.replace("</title>", "</title><sourceKind>JO&#x202e;B</sourceKind>");
    let meta = read(&fixture.journal("concealed", &concealed))
        .unwrap()
        .summary;
    assert_eq!(meta.source, "JOB");
    assert_eq!(meta.source_kind, tree::SourceKind::Interactive);
}

#[test]
fn imported_automation_identity_and_keywords_keep_raw_scalar_semantics() {
    let fixture = Fixture::new();
    let base = r#"<session-journal formatVersion="1" id="raw"><meta><sourceKind>JOB</sourceKind><sourceId>ab</sourceId><runId>run</runId><sourceName>Earlier name</sourceName><automationAction>AI_SWARM</automationAction><startedAt>2026-11-06T10:00:00Z</startedAt></meta><entries/></session-journal>"#;
    let other_source = base.replace("<sourceId>ab</sourceId>", "<sourceId>a&#x202e;b</sourceId>");
    let other_run = base.replace("<runId>run</runId>", "<runId>r&#x202e;un</runId>")
        .replace("<sourceName>Earlier name</sourceName>", "<sourceName>&#x202e;</sourceName>")
        .replace("2026-11-06T10:00:00Z", "2026-11-06T11:00:00Z")
        .replace("<automationAction>AI_SWARM</automationAction>", "<automationAction>AI_&#x202e;SWARM</automationAction><aiKeywords><keyword>a&#x202e;b</keyword></aiKeywords>");
    let original = read(&fixture.journal("original", base)).unwrap();
    let different_source = read(&fixture.journal("source", &other_source)).unwrap();
    let different_run = read(&fixture.journal("run", &other_run)).unwrap();
    assert_eq!(
        different_source.summary.source_id.as_deref(),
        Some("a\u{202e}b")
    );
    assert_eq!(different_run.summary.run_id.as_deref(), Some("r\u{202e}un"));
    assert_eq!(
        different_run.summary.automation_action.as_deref(),
        Some("AI_\u{202e}SWARM")
    );
    assert_eq!(different_run.summary.keywords, ["a\u{202e}b"]);
    assert!(!tree::matches_metadata(&different_run.summary, "ab"));
    let catalog = Catalog {
        journals: vec![
            original.summary,
            different_source.summary,
            different_run.summary,
        ],
        ..Catalog::default()
    };
    let roots = tree::build(&catalog, tree::Filter::All, "");
    assert_eq!(roots.len(), 2);
    let original_group = roots
        .iter()
        .find(|node| node.journals.contains(&0))
        .unwrap();
    assert_eq!(original_group.run_count, 2);
    assert_eq!(original_group.source_name.as_deref(), Some("\u{202e}"));
    assert!(!original_group.is_scheduled_swarm());
}

#[test]
fn imported_scalar_fields_join_xml_text_around_comments_processing_instructions_and_cdata() {
    let fixture = Fixture::new();
    let xml = r#"<session-journal formatVersion="1" id="segments"><meta>
      <title>Fi<!--comment-->rst<![CDATA[ title]]></title>
      <sourceKind>JO<?part value?>B</sourceKind><sourceId>sou<!--comment-->rce<![CDATA[-id]]></sourceId>
      <sourceName>Source<?part value?> name</sourceName><runId>ru<!--comment-->n<![CDATA[-id]]></runId>
      <runStatus>CANCEL<!--comment-->LED</runStatus><logEntryCount>12<?part value?>34</logEntryCount>
      <aiKeywords><keyword>nginx<!--comment-->.<![CDATA[conf]]></keyword></aiKeywords>
      </meta><entries><entry><id>note</id><kind>USER<!--comment-->_NOTE</kind><text>First<?part value?> and <![CDATA[second]]></text>
      <inputExcerpt><line>in<!--comment-->put</line></inputExcerpt></entry></entries></session-journal>"#;
    let document = read(&fixture.journal("segments", xml)).unwrap();
    assert_eq!(document.summary.title, "First title");
    assert_eq!(document.summary.source_kind, tree::SourceKind::Job);
    assert_eq!(document.summary.source_id.as_deref(), Some("source-id"));
    assert_eq!(document.summary.source_name.as_deref(), Some("Source name"));
    assert_eq!(document.summary.run_id.as_deref(), Some("run-id"));
    assert_eq!(
        document.summary.run_status,
        Some(tree::RunStatus::Cancelled)
    );
    assert_eq!(document.summary.log_entry_count, 1234);
    assert_eq!(document.summary.keywords, ["nginx.conf"]);
    assert_eq!(document.timeline[0].kind, "USER_NOTE");
    assert_eq!(document.timeline[0].text, "First and second");
    assert_eq!(document.timeline[0].input, ["input"]);
}

#[test]
fn imported_scalar_text_limit_applies_to_all_xml_text_segments_before_joining() {
    let fixture = Fixture::new();
    let first = "a".repeat(128 * 1024);
    let second = "b".repeat(128 * 1024);
    for (index, field) in ["sourceId", "runId", "title", "description", "aiKeywords"]
        .into_iter()
        .enumerate()
    {
        let content = if field == "aiKeywords" {
            format!("<keyword>{first}<!--comment-->{second}</keyword>")
        } else {
            format!("{first}<?part value?>{second}")
        };
        let prefix = format!("<session-journal formatVersion=\"1\" id=\"limit\"><meta><{field}>");
        let suffix = format!("</{field}></meta><entries/></session-journal>");
        let directory = fixture.journal(
            &format!("limit-{index}"),
            &format!("{prefix}{content}{suffix}"),
        );
        assert!(read(&directory).is_ok());
        let extra = if field == "aiKeywords" {
            content.replace("</keyword>", "x</keyword>")
        } else {
            format!("{content}x")
        };
        fs::write(directory.join(DOCUMENT), format!("{prefix}{extra}{suffix}")).unwrap();
        assert!(read(&directory).is_err());
    }
}

#[test]
fn journal_catalog_offset_tie_orders_the_first_run_action_like_java() {
    let fixture = Fixture::new();
    let low = r#"<session-journal formatVersion="1" id="low"><meta><sourceKind>JOB</sourceKind><sourceId>job</sourceId><runId>run</runId><startedAt>2026-06-01T11:00:00+01:00</startedAt><automationAction>COMMAND</automationAction></meta><entries/></session-journal>"#;
    let high = low
        .replace("id=\"low\"", "id=\"high\"")
        .replace("11:00:00+01:00", "12:00:00+02:00")
        .replace("COMMAND", "AI_SWARM");
    let low_path = fixture.journal("a-low", low);
    let high_path = fixture.journal("z-high", &high);
    let catalog = catalog(&fixture.0).unwrap();
    assert_eq!(catalog.journals.len(), 2);
    assert_eq!(
        catalog.journals[0].started_at,
        catalog.journals[1].started_at
    );
    assert_eq!(catalog.journals[0].directory, high_path);
    assert_eq!(catalog.journals[1].directory, low_path);
    let roots = tree::build(&catalog, tree::Filter::All, "");
    assert!(roots[0].is_scheduled_swarm());
    assert_eq!(
        roots[0].children[0].automation_action.as_deref(),
        Some("AI_SWARM")
    );
}

#[test]
fn imported_automation_fields_reject_duplicate_nested_and_unbounded_values() {
    let fixture = Fixture::new();
    for (index, field) in [
        "<runId>a</runId><runId>b</runId>".into(),
        "<sourceId><nested/></sourceId>".into(),
        "<aiKeywords/><aiKeywords/>".into(),
        "<aiKeywords><keyword><nested/></keyword></aiKeywords>".into(),
        format!(
            "<aiKeywords><keyword>{}</keyword></aiKeywords>",
            "a".repeat(256 * 1024 + 1)
        ),
        "<pinned>unrecognized</pinned>".into(),
        "<aiCost>NaN</aiCost>".into(),
    ]
    .into_iter()
    .enumerate()
    {
        let xml = DOCUMENT_V1.replace("</meta>", &format!("{field}</meta>"));
        assert!(read(&fixture.journal(&format!("invalid-{index}"), &xml)).is_err());
    }
}

fn journal_with_entries(fixture: &Fixture, entries: &str) -> Journal {
    let document = DOCUMENT_V1.replace("<entries>", &format!("<entries>{entries}"));
    read(&fixture.journal("links", &document)).unwrap()
}

fn entry_by_id<'a>(journal: &'a Journal, id: &str) -> &'a TimelineEntry {
    journal
        .timeline
        .iter()
        .find(|entry| entry.id == id)
        .unwrap()
}

fn linked_labels(entry: &TimelineEntry, field: LinkField) -> Vec<(&str, &str)> {
    entry
        .link_spans(field, 8192)
        .into_iter()
        .filter_map(|span| span.target.map(|target| (span.text, target.uri())))
        .collect()
}

#[test]
fn journal_link_import_distinguishes_notes_summaries_and_literal_fields() {
    let fixture = Fixture::new();
    let contents = journal_with_entries(
        &fixture,
        r#"
        <entry><id>note</id><kind>USER_NOTE</kind>
          <text>See (HTTP://example.test/a_(b)). www.example.test/draft stays text.</text>
        </entry>
        <entry><id>ordinary</id><kind>COMMAND</kind>
          <title>https://title.test</title><text>https://body.test</text>
          <userNote>Read https://example.test/a?x=1&amp;y=2 and https://[2001:db8::1]/docs!</userNote>
          <inputExcerpt><line>https://input.test</line></inputExcerpt>
          <outputExcerpt><line>https://output.test</line></outputExcerpt>
        </entry>
        <entry><id>summary</id><kind>SESSION_SUMMARY</kind>
          <text>- Start `links www.heise.de` then www.kortty.test/docs. script.pl is a filename.</text>
          <userNote>www.example.test/note</userNote>
        </entry>
        <entry><id>ai</id><kind>AI_SUMMARY</kind>
          <text>https://example.test/a and WWW.EXAMPLE.TEST/b, but xwww.example.test is embedded.</text>
        </entry>
        "#,
    );
    let note = entry_by_id(&contents, "note");
    let ordinary = entry_by_id(&contents, "ordinary");
    let summary = entry_by_id(&contents, "summary");
    let ai = entry_by_id(&contents, "ai");
    assert_eq!(
        linked_labels(note, LinkField::Text),
        [("HTTP://example.test/a_(b)", "http://example.test/a_(b)")]
    );
    let rendered = note
        .link_spans(LinkField::Text, 8192)
        .into_iter()
        .map(|span| span.text)
        .collect::<String>();
    assert_eq!(rendered, note.text);
    assert!(linked_labels(ordinary, LinkField::Text).is_empty());
    assert_eq!(
        linked_labels(ordinary, LinkField::Note),
        [
            (
                "https://example.test/a?x=1&y=2",
                "https://example.test/a?x=1&y=2"
            ),
            ("https://[2001:db8::1]/docs", "https://[2001:db8::1]/docs"),
        ]
    );
    assert_eq!(ordinary.title, "https://title.test");
    assert_eq!(ordinary.input, ["https://input.test"]);
    assert_eq!(ordinary.output, ["https://output.test"]);
    assert_eq!(
        linked_labels(summary, LinkField::Text),
        [
            ("www.heise.de", "https://www.heise.de/"),
            ("www.kortty.test/docs", "https://www.kortty.test/docs"),
        ]
    );
    assert!(linked_labels(summary, LinkField::Note).is_empty());
    assert_eq!(
        linked_labels(ai, LinkField::Text),
        [
            ("https://example.test/a", "https://example.test/a"),
            ("WWW.EXAMPLE.TEST/b", "https://www.example.test/b"),
        ]
    );
}

#[test]
fn journal_link_import_keeps_whole_bare_web_addresses() {
    let fixture = Fixture::new();
    let contents = journal_with_entries(
        &fixture,
        r#"
        <entry><id>summary</id><kind>AI_SUMMARY</kind><text>
          www.example.test:8443/private www.example.test?token=abc
          www.example.test#section www.example.testé/path
          www.example.test:invalid/private www.example.test@unrelated.test
          www.example/private www.example.test?x=a&amp;y=b#section
        </text></entry>
        "#,
    );
    let summary = entry_by_id(&contents, "summary");
    assert_eq!(
        linked_labels(summary, LinkField::Text),
        [
            (
                "www.example.test:8443/private",
                "https://www.example.test:8443/private"
            ),
            (
                "www.example.test?token=abc",
                "https://www.example.test/?token=abc"
            ),
            (
                "www.example.test#section",
                "https://www.example.test/#section"
            ),
            (
                "www.example.test?x=a&y=b#section",
                "https://www.example.test/?x=a&y=b#section"
            ),
        ]
    );
    assert_eq!(
        summary
            .link_spans(LinkField::Text, 8192)
            .iter()
            .map(|span| span.text)
            .collect::<String>(),
        summary.text
    );
}

#[test]
fn journal_link_import_rejects_sanitized_fragments_and_unsafe_destinations() {
    let fixture = Fixture::new();
    let contents = journal_with_entries(
        &fixture,
        r#"
        <entry><id>note</id><kind>USER_NOTE</kind><text>
          ht&#x202e;tps://fabricated.test https://exa&#x202e;mple.test/path
          https://prefix.test&#x85;.extra.test
          https://trusted.test@real.test/path https://. http:///missing-host
          https://malformed.test/%xx https://bad\host.test ftp://ftp.test
          file:///tmp/example mailto:user@example.test data:text/html,x javascript:alert(1)
          A removed marker: &#x202e; then https://valid.test/read.
        </text></entry>
        <entry><id>kind</id><kind>USER&#x202e;_NOTE</kind><text>https://plain.test</text></entry>
        "#,
    );
    let note = entry_by_id(&contents, "note");
    assert_eq!(
        linked_labels(note, LinkField::Text),
        [("https://valid.test/read", "https://valid.test/read")]
    );
    assert!(note.text.contains("https://fabricated.test"));
    assert!(note.text.contains("https://example.test/path"));
    assert!(note.text.contains("https://prefix.test.extra.test"));
    let kind = entry_by_id(&contents, "kind");
    assert_eq!(kind.kind, "USER_NOTE");
    assert!(linked_labels(kind, LinkField::Text).is_empty());
}

#[test]
fn journal_link_spans_keep_utf8_and_leave_clipped_urls_plain() {
    let fixture = Fixture::new();
    let contents = journal_with_entries(
        &fixture,
        r#"<entry><id>note</id><kind>USER_NOTE</kind><text>漢 🦀 https://example.test/a_(b) rest</text></entry>"#,
    );
    let note = entry_by_id(&contents, "note");
    let clipped = note.link_spans(LinkField::Text, 10);
    assert_eq!(
        clipped.iter().map(|span| span.text).collect::<String>(),
        "漢 🦀 https:…"
    );
    assert!(clipped.iter().all(|span| span.target.is_none()));
    let visible = note.link_spans(LinkField::Text, 4 + "https://example.test/a_(b)".len());
    assert_eq!(visible[0].text, "漢 🦀 ");
    assert_eq!(visible[1].text, "https://example.test/a_(b)");
    assert_eq!(
        visible[1].target.unwrap().uri(),
        "https://example.test/a_(b)"
    );
    assert_eq!(visible[2].text, "…");
    assert!(visible[2].target.is_none());
}

fn json_header(part: u32) -> String {
    format!(
        "{{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"{ID}\",\"part\":{part},\"host\":\"localhost\",\"port\":22,\"tabSessionId\":\"fixture\"}}\n"
    )
}

#[test]
fn reads_v1_metadata_timeline_and_each_rotated_capture_encoding() {
    let fixture = Fixture::new();
    let journal = fixture.journal("newer", DOCUMENT_V1);
    fixture.journal("older", &DOCUMENT_V1.replace("2026-11-06", "2026-10-01"));
    fixture.journal("bad", "<not-a-journal/>");
    let catalog = catalog(&fixture.0).unwrap();
    assert_eq!(catalog.skipped, 1);
    assert!(!catalog.limited);
    assert_eq!(catalog.journals.len(), 2);
    assert_eq!(catalog.journals[0].directory, journal);

    let contents = read(&journal).unwrap();
    assert!(!contents.timeline_limited);
    assert_eq!(contents.summary.id, ID);
    assert_eq!(contents.summary.title, "A & B");
    assert_eq!(contents.summary.description, "Offline fixture");
    assert_eq!(contents.summary.connection_name, "Shell");
    assert_eq!(contents.summary.host, "localhost");
    assert_eq!(contents.summary.port, Some(22));
    assert_eq!(contents.summary.username, "operator");
    assert_eq!(contents.summary.source, "INTERACTIVE");
    assert_eq!(contents.summary.log_parts, 3);
    assert!(contents.summary.ended_at > contents.summary.started_at);
    let entry = &contents.timeline[0];
    assert_eq!(entry.id, "208e6569-57bf-40ad-9c74-eecb689f9a27");
    assert_eq!(entry.kind, "SCREENSHOT");
    assert_eq!(entry.title, "Seen");
    assert_eq!(entry.text, "<b>literal</b>");
    assert_eq!(entry.note, "Look here");
    assert_eq!(entry.marker, "work-done");
    assert_eq!(entry.created_at, Some(TIME.parse().unwrap()));
    assert_eq!(
        entry.screenshot.as_deref(),
        Some("screenshots/shot-000009.png")
    );
    assert_eq!(
        (entry.log_part, entry.log_start, entry.log_end),
        (Some(2), Some(3), Some(4))
    );
    assert_eq!(entry.input, ["ls"]);
    assert_eq!(entry.output, ["ALPHA"]);

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<session-log formatVersion=\"1\" journalId=\"{ID}\" part=\"1\">\n<meta host=\"localhost\"/>\n<out seq=\"1\" t=\"{TIME}\" repeat=\"2\">x &amp; y</out>\n<note seq=\"5\" t=\"{TIME}\">continued in part 2</note>\n</session-log>\n"
    );
    fs::write(journal.join("session-log.xml"), xml).unwrap();
    let json = format!(
        "{}{{\"seq\":3,\"t\":\"{TIME}\",\"k\":\"in\",\"redacted\":true}}\n{{\"type\":\"end\"}}\n",
        json_header(2)
    );
    let zstd = compress_to_vec(json.as_bytes(), CompressionLevel::Fastest);
    fs::write(journal.join("session-log-2.json.zst"), zstd).unwrap();
    let yaml = format!(
        "formatVersion: 1\njournalId: \"{ID}\"\npart: 3\nmeta: {}entries:\n- {{\"seq\":4,\"t\":\"{TIME}\",\"k\":\"out\",\"x\":\"Alpha\\u001b\",\"partial\":true}}\nend: true\n",
        json_header(3)
    );
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(yaml.as_bytes()).unwrap();
    fs::write(
        journal.join("session-log-3.yaml.gz"),
        gzip.finish().unwrap(),
    )
    .unwrap();

    let page = capture_page(&journal, None, None, None).unwrap();
    assert_eq!(page.status.notice(), None);
    assert!(page.next.is_none());
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        [1, 5, 3, 4]
    );
    assert_eq!(page.records[0].repeat, 2);
    assert_eq!(page.records[0].text, "x & y");
    assert_eq!(page.records[1].kind, CaptureKind::Note);
    assert_eq!(page.records[2].kind, CaptureKind::Input);
    assert!(page.records[2].redacted);
    assert!(page.records[3].partial);
    assert_eq!(page.records[3].timestamp, TIME.parse().unwrap());

    let page = capture_page(&journal, None, Some(3..=4), Some("ALPHA")).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].sequence, 4);
    assert_eq!(page.records[0].text, "Alpha");
}

#[test]
fn cursor_resumes_physical_lines_and_reports_missing_and_torn_parts() {
    let fixture = Fixture::new();
    let journal = fixture.journal("paged", DOCUMENT_V1);
    let mut json = json_header(1);
    for sequence in (1..=270).rev() {
        json.push_str(&format!(
            "{{\"seq\":{sequence},\"t\":\"{TIME}\",\"k\":\"seed\",\"x\":\"line {sequence}\"}}\n"
        ));
    }
    json.push_str("{\"seq\":");
    fs::write(journal.join("session-log.json"), json).unwrap();
    fs::write(journal.join("session-log-3.json"), json_header(2)).unwrap();
    let first = capture_page(&journal, None, None, None).unwrap();
    assert_eq!(first.records.len(), 256);
    assert_eq!(first.records[255].sequence, 15);
    assert_eq!(first.status.missing_parts, 1);
    let second = capture_page(&journal, first.next, None, None).unwrap();
    assert_eq!(second.records.len(), 14);
    assert_eq!(second.records[0].sequence, 14);
    assert_eq!(second.records[13].sequence, 1);
    assert_eq!(second.status.skipped_lines, 1);
    assert_eq!(second.status.unclosed_parts, 1);
    assert_eq!(second.status.unreadable_parts, 1);
    assert!(second.status.notice().is_some());
    assert!(second.next.is_none());
}

#[test]
fn refuses_dtds_and_confines_screenshot_references() {
    let fixture = Fixture::new();
    let dtd = DOCUMENT_V1.replace(
        "<session-journal",
        "<!DOCTYPE session-journal SYSTEM \"file:///etc/passwd\"><session-journal",
    );
    let external = fixture.journal("external-entity", &dtd);
    assert!(read(&external).is_err());

    let unsafe_reference =
        DOCUMENT_V1.replace("screenshots/shot-000009.png", "screenshots/../outside.png");
    let directory = fixture.journal("screens", &unsafe_reference);
    let contents = read(&directory).unwrap();
    assert!(contents.timeline_limited);
    assert!(contents.timeline[0].screenshot.is_none());
    assert!(read_screenshot(&directory, "screenshots/../outside.png").is_err());
    assert!(read_screenshot(&directory, "screenshots/C:unsafe.png").is_err());
    fs::create_dir(directory.join("screenshots")).unwrap();
    let png = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
    fs::write(directory.join("screenshots/shot-000009.png"), &png).unwrap();
    assert_eq!(
        read_screenshot(&directory, "screenshots/shot-000009.png").unwrap(),
        png
    );
}

#[cfg(unix)]
#[test]
fn does_not_follow_symlinked_document_parts_or_screenshots() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let original = fixture.journal("original", DOCUMENT_V1);
    let linked = fixture.0.join("linked");
    symlink(&original, &linked).unwrap();
    assert!(read(&linked).is_err());
    assert!(catalog(&linked).is_err());

    let outside = fixture.0.join("outside");
    fs::write(&outside, json_header(1)).unwrap();
    symlink(&outside, original.join("session-log.json")).unwrap();
    symlink(&fixture.0, original.join("screenshots")).unwrap();
    let page = capture_page(&original, None, None, None).unwrap();
    assert_eq!(page.status.unreadable_parts, 1);
    assert!(page.records.is_empty());
    assert!(read_screenshot(&original, "screenshots/outside.png").is_err());
}
