use std::fs;
use std::path::PathBuf;

use super::*;
use crate::session_journal::{DOCUMENT, catalog, export};

const LARGE_SEQUENCE: u64 = 9_007_199_254_740_993;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-html-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn snapshot(&self) -> Snapshot {
        let directory = self.0.join("journal");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join(DOCUMENT),
            format!(r#"<session-journal id="offline-html" formatVersion="1">
 <meta><title>&lt;script&gt; Host &amp; notes</title><description>Example description</description></meta>
 <entries>
  <entry><id>same</id><kind>USER_NOTE</kind><title>Note &amp; link</title><createdAt>2026-11-07T10:00:00Z</createdAt>
    <text>https://example.org/?a=1&amp;b=2</text><userNote>javascript:alert(1)</userNote><marker>ERROR</marker>
    <logStartSeq>3</logStartSeq><logEndSeq>{LARGE_SEQUENCE}</logEndSeq></entry>
  <entry><id>same</id><kind>SCREENSHOT</kind><title>Screenshot</title><createdAt>2026-11-07T11:00:00Z</createdAt>
    <screenshotFile>screenshots/shot.png</screenshotFile><aiDescription>Analysis &lt;b&gt;</aiDescription>
    <aiTags><tag>Find screenshot</tag></aiTags></entry>
  <entry><id>private-image</id><kind>SCREENSHOT</kind><title>Unsafe screenshot</title>
    <screenshotFile>screenshots/shot.ORIG.png</screenshotFile></entry>
  <entry><id>agent</id><kind>AGENT</kind><title>Agent question</title><text>Agent answer</text>
    <agentModel>Example model</agentModel><agentTokens>4</agentTokens>
    <chatSnapshot><title>Private transcript</title><frozen>true</frozen><messages>
      <message><role>assistant</role><content>Private chat &lt;/details&gt;</content><steps>
        <step><title>Private step</title><command>ls</command><output>done</output><exitCode>0</exitCode></step>
      </steps></message>
    </messages></chatSnapshot></entry>
 </entries>
</session-journal>"#),
        )
        .unwrap();
        let catalog = catalog(&self.0).unwrap();
        let row = &catalog.journals[0];
        export::load(
            &self.0,
            &directory,
            &row.id,
            row.deletion_identity.as_ref().unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(sequence: u64, text: &str) -> CaptureRecord {
    CaptureRecord {
        sequence,
        timestamp: "2026-11-07T10:00:00Z".parse().unwrap(),
        kind: CaptureKind::Output,
        text: text.into(),
        redacted: false,
        partial: false,
        repeat: 2,
    }
}

fn stats() -> BundleStats {
    BundleStats {
        command_count: 7,
        error_count: 2,
        screenshot_count: 1,
        skipped_assets: 0,
        capture_incomplete: false,
    }
}

#[test]
fn journal_html_bundle_uses_escaped_markup_relative_images_and_policy_links() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let selection: Vec<_> = snapshot.entries.iter().collect();
    let mut options = Options::default();
    options.filter.zone = TimeZone::UTC;
    let log = LogPreview::new(&selection);
    // Test double for the assembly's inventory of actually published assets.
    let images = HashSet::from([
        "screenshots/shot.png".into(),
        "screenshots/shot.ORIG.png".into(),
    ]);
    let page = render(
        &snapshot,
        &selection,
        &options,
        &log,
        &images,
        true,
        &stats(),
    )
    .unwrap();
    assert!(page.contains("&lt;script&gt; Host &amp; notes"));
    assert!(page.contains("<dt>Commands</dt><dd>7</dd>"));
    assert!(page.contains("data-marker=\"error\""));
    assert!(page.contains("data-time=\"2026-11-07 10:00:00\""));
    assert!(page.contains("src=\"screenshots/shot.png\""));
    assert!(!page.contains("shot.ORIG.png"));
    assert!(page.contains("Analysis &lt;b&gt;"));
    assert!(page.contains("href=\"https://example.org/?a=1&amp;b=2\""));
    assert!(!page.contains("href=\"javascript:"));
    assert_eq!(page.matches("<script ").count(), 1);
    assert!(page.contains("script-src 'nonce-"));
    assert!(!page.contains("'unsafe-inline'"));
    assert!(page.contains("Private chat &lt;/details&gt;"));
    assert!(page.contains("Private step"));

    options.include_chats = false;
    let page = render(
        &snapshot,
        &selection,
        &options,
        &log,
        &images,
        false,
        &stats(),
    )
    .unwrap();
    assert!(page.contains("Agent question"));
    assert!(page.contains("Agent answer"));
    assert!(!page.contains("Private transcript"));
    assert!(!page.contains("Private chat"));
    assert!(!page.contains("Private step"));
    assert!(page.contains("https://example.org/?a=1&amp;b=2"));
    assert!(!page.contains("href=\"https://example.org/"));
}

#[test]
fn journal_html_bundle_embeds_inert_log_data_and_masks_redacted_rows() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let selection = [&snapshot.entries[0]];
    let mut options = Options::default();
    options.filter.zone = TimeZone::UTC;
    let mut log = LogPreview::new(&selection);
    let injected = "</template><script src=\"https://example.org/attack\"></script>&";
    log.push(&record(3, injected), None);
    let mut redacted = record(LARGE_SEQUENCE, "must never reach HTML");
    redacted.kind = CaptureKind::Input;
    redacted.redacted = true;
    log.push(&redacted, None);
    log.mark_incomplete();
    let page = render(
        &snapshot,
        &selection,
        &options,
        &log,
        &HashSet::new(),
        false,
        &stats(),
    )
    .unwrap();
    assert!(page.contains(&format!("data-from=\"3\" data-to=\"{LARGE_SEQUENCE}\"")));
    assert!(page.contains("Some capture data could not be included."));
    assert!(!page.contains("must never reach HTML"));
    assert!(!page.contains("<script src="));
    let (_, template) = page.split_once("<template id=\"log-data\">").unwrap();
    let (template, _) = template.split_once("</template>").unwrap();
    let xml = format!("<rows>{template}</rows>");
    let xml = roxmltree::Document::parse(&xml).unwrap();
    let data: serde_json::Value = serde_json::from_str(xml.root_element().text().unwrap()).unwrap();
    assert_eq!(data[0]["text"], injected);
    assert_eq!(data[0]["repeat"], 2);
    assert_eq!(data[1]["sequence"], LARGE_SEQUENCE.to_string());
    assert_eq!(data[1]["text"], "(hidden input)");
}

#[test]
fn journal_html_bundle_keeps_newest_referenced_rows_within_embed_budget() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let selection = [&snapshot.entries[0]];
    let mut log = LogPreview::new(&selection);
    let line = "x".repeat(256 * 1024);
    let mut record = record(1, &line);
    for sequence in 1..=40 {
        record.sequence = sequence;
        log.push(&record, None);
    }
    assert!(log.limited);
    assert_eq!(log.text_bytes, MAX_LOG_BYTES);
    assert_eq!(log.rows.front().unwrap().sequence, 9);
    assert_eq!(log.rows.back().unwrap().sequence, 40);
    record.sequence = LARGE_SEQUENCE + 1;
    log.push(&record, None);
    assert_eq!(log.rows.back().unwrap().sequence, 40);
}
