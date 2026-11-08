use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::PathBuf;

use image::ImageEncoder as _;
use image::codecs::png::PngEncoder;
use jiff::tz::TimeZone;

use super::super::{Filter, MarkerMode, Options, Snapshot, load};
use super::{PdfBranding, PdfExport};
use crate::session_journal::catalog;
use crate::session_journal::export_branding::{BrandText, Watermark};

const ID: &str = "00000000-0000-4000-8000-000000000004";

struct Fixture {
    root: PathBuf,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-pdf-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            directory: root.join(format!("journal-{ID}")),
            root,
        };
        let screenshots = fixture.directory.join("screenshots");
        fs::create_dir_all(&screenshots).unwrap();
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                &vec![92; 1301 * 9 * 4],
                1301,
                9,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        fs::write(screenshots.join("capture.png"), &png).unwrap();
        fs::write(screenshots.join("capture.orig.png"), &png).unwrap();
        fs::write(screenshots.join("backup.ORIG.png"), &png).unwrap();
        fs::write(screenshots.join("broken.png"), b"truncated screenshot").unwrap();
        fs::write(fixture.directory.join("journal.xml"), fixture_xml()).unwrap();
        fixture
    }

    fn load(&self) -> Snapshot {
        let catalog = catalog(&self.root).unwrap();
        let identity = catalog.journals[0].deletion_identity.as_ref().unwrap();
        load(&self.root, &self.directory, ID, identity).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture_xml() -> String {
    let mut output = String::new();
    for index in 0..120 {
        write!(
            output,
            "<line>terminal-row-{index:03} with multiple spaces</line>"
        )
        .unwrap();
    }
    let mut chat = String::new();
    for index in 0..145 {
        writeln!(chat, "chat-output-row-{index:03}").unwrap();
    }
    format!(
        r#"
<session-journal formatVersion="1" id="{ID}">
 <meta><title>Journäl Café Ελληνικά Кириллица</title><description>Night shift</description>
  <host>host.test</host><port>22</port><username>operator</username><connectionName>Shell</connectionName>
  <startedAt>2026-10-01T10:00:00+02:00</startedAt><endedAt>2026-10-02T09:32:00Z</endedAt>
  <commandCount>9</commandCount><errorCount>1</errorCount><screenshotCount>4</screenshotCount>
 </meta>
 <entries>
  <entry><kind>SESSION_SUMMARY</kind><createdAt>2026-10-02T10:03:00Z</createdAt>
   <title>After midnight</title><runAsUser>root</runAsUser><markerId>review</markerId><marker>ERROR</marker>
   <text>* ran `printf`
Unsupported {unsupported} · https://journal.example/path</text><userNote>Actual shift note</userNote>
   <inputExcerpt><line>printf test</line></inputExcerpt><outputExcerpt>{output}</outputExcerpt>
  </entry>
  <entry><kind>AGENT</kind><createdAt>2026-10-02T10:04:00Z</createdAt><title>Automated check</title>
   <agentModel>Test model</agentModel><agentDurationMillis>62000</agentDurationMillis><agentTokens>1200</agentTokens>
   <chatSnapshot><title>First chat</title><capturedAt>2026-10-02T10:05:00Z</capturedAt><frozen>true</frozen><messages>
    <message><role>user</role><content>Inspect the journal</content></message>
    <message><role>assistant</role><aiProfileName>Operator assistant</aiProfileName><content>Here is the result</content><steps>
     <step><status>DONE</status><title>Inspect</title><summary>Checked</summary><command>stat log</command>
      <exitCode>0</exitCode><output>{chat}</output><truncated>true</truncated></step>
    </steps></message>
   </messages></chatSnapshot>
  </entry>
  <entry><kind>AGENT</kind><createdAt>2026-10-02T10:06:00Z</createdAt><title>Second chat</title>
   <chatSnapshot><messages><message><role>user</role><content>Second conversation</content></message></messages></chatSnapshot>
  </entry>
  <entry><kind>SCREENSHOT</kind><createdAt>2026-10-01T11:00:00Z</createdAt><title>Before midnight</title>
   <screenshotFile>screenshots/capture.png</screenshotFile><aiDescription>Screenshot analysis</aiDescription>
   <aiTags><tag>dashboard</tag><tag>metrics</tag></aiTags>
  </entry>
  <entry><kind>SCREENSHOT</kind><title>Image backup</title><screenshotFile>screenshots/capture.orig.png</screenshotFile></entry>
  <entry><kind>SCREENSHOT</kind><title>Uppercase backup</title><screenshotFile>screenshots/backup.ORIG.png</screenshotFile></entry>
  <entry><kind>SCREENSHOT</kind><title>Broken image</title><screenshotFile>screenshots/broken.png</screenshotFile></entry>
 </entries>
 <markerDefinitions><marker><id>review</id><name>Review custom</name><color>#3264a9</color><legacyMarker>ERROR</legacyMarker></marker></markerDefinitions>
</session-journal>"#,
        unsupported = '\u{10fffd}'
    )
}

fn full_options() -> Options {
    Options {
        filter: Filter {
            zone: TimeZone::UTC,
            ..Filter::default()
        },
        ..Options::default()
    }
}

fn render_fixture() -> (PdfExport, PdfExport) {
    let fixture = Fixture::new();
    let snapshot = fixture.load();
    let full = snapshot
        .render_pdf(&full_options(), &PdfBranding::default(), &|| false)
        .unwrap();
    let mut filtered = full_options();
    filtered.include_screenshots = false;
    filtered.include_chats = false;
    filtered.filter.marker_mode = MarkerMode::Selected;
    filtered.filter.marker_ids.insert("review".to_owned());
    let branding = PdfBranding {
        footer: None,
        watermark: Some(Watermark {
            brand: BrandText {
                text: "Review watermark".to_owned(),
                repository_link: false,
            },
            color: [18, 52, 86],
        }),
    };
    let filtered = snapshot
        .render_pdf(&filtered, &branding, &|| false)
        .unwrap();
    (full, filtered)
}

#[test]
fn journal_pdf_renders_real_multi_page_fixture() {
    let (full, filtered) = render_fixture();
    assert_eq!(full.skipped_assets, 3);
    assert_eq!(filtered.skipped_assets, 0);
    let full = String::from_utf8_lossy(&full.bytes);
    let filtered = String::from_utf8_lossy(&filtered.bytes);
    assert!(full.starts_with("%PDF-") && full.trim_end().ends_with("%%EOF"));
    assert!(full.matches("/MediaBox").count() >= 6);
    assert!(full.contains("/ToUnicode") && full.contains("/FontFile2"));
    assert!(full.contains("/Subtype /Image") && full.contains("/Width 1200"));
    assert!(full.contains("/SMask"));
    assert!(full.contains("/Outlines") && full.matches("/XYZ").count() >= 3);
    assert!(full.contains("https://github.com/chardonnay/kortty"));
    assert!(!full.contains("https://journal.example/path"));
    assert!(!filtered.contains("/Subtype /Image"));
    assert!(!filtered.contains("/Outlines"));
    assert!(!filtered.contains("/URI"));
    assert!(filtered.contains("/ca 0.08"));
}

#[test]
fn journal_pdf_rejects_empty_filter_cancellation_and_changed_source() {
    let fixture = Fixture::new();
    let snapshot = fixture.load();
    let mut options = full_options();
    options.filter.topic = "absent report subject".to_owned();
    let branding = PdfBranding::default();
    let error = snapshot
        .render_pdf(&options, &branding, &|| false)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let error = snapshot
        .render_pdf(&full_options(), &branding, &|| true)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    fs::write(
        fixture.directory.join("journal.xml"),
        fixture_xml().replace("Night shift", "Edited shift"),
    )
    .unwrap();
    assert!(
        snapshot
            .render_pdf(&full_options(), &branding, &|| false)
            .is_err()
    );
}

#[test]
#[ignore = "writes PDF fixtures into the test scratch directory for Poppler"]
fn journal_pdf_smoke_fixture() {
    let (full, filtered) = render_fixture();
    let directory = std::env::temp_dir().join("journal-pdf-fixture");
    fs::create_dir(&directory).unwrap();
    File::create_new(directory.join("journal.pdf"))
        .unwrap()
        .write_all(&full.bytes)
        .unwrap();
    File::create_new(directory.join("journal-filtered.pdf"))
        .unwrap()
        .write_all(&filtered.bytes)
        .unwrap();
}
