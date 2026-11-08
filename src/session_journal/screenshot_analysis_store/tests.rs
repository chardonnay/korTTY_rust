use std::fs;
use std::path::PathBuf;

use crate::private_files::Directory;
use crate::session_journal::{DOCUMENT, viewer::Source};

use super::*;

const ID: &str = "analysis-fixture";
const REFERENCE: &str = "screenshots/shot.png";
const XML: &str = r#"<session-journal id="analysis-fixture" formatVersion="1">
  <meta><title>Fixture</title></meta><entries>
    <entry><id>shot</id><kind>SCREENSHOT</kind><createdAt>2026-11-07T07:00:00Z</createdAt>
      <screenshotFile>screenshots/shot.png</screenshotFile><userNote>Original note</userNote>
      <annotations><future/></annotations><editedAt>2026-11-07T07:01:00Z</editedAt>
      <aiDescription>Earlier analysis</aiDescription>
      <aiTags format="future"><tag>old</tag><extension>&#x41;</extension></aiTags>
      <future flag="yes"><![CDATA[<unrelated>]]></future>
    </entry>
    <entry><id>note</id><kind>USER_NOTE</kind><text>Preserve other row</text></entry>
  </entries>
</session-journal>"#;

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
    png: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-screenshot-analysis-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = root.join("journal");
        fs::create_dir_all(journal.join("screenshots")).unwrap();
        let png = tiny_skia::Pixmap::new(12, 8).unwrap().encode_png().unwrap();
        fs::write(journal.join(REFERENCE), &png).unwrap();
        fs::write(journal.join(DOCUMENT), XML).unwrap();
        Self { root, journal, png }
    }

    fn review(&self) -> Screenshot {
        let directory = Directory::open(&self.journal, false).unwrap();
        let source = Source::from_open_directory(&self.journal, ID, &directory).unwrap();
        Screenshot::review(&source, "shot", REFERENCE).unwrap()
    }

    fn xml(&self) -> String {
        fs::read_to_string(self.journal.join(DOCUMENT)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn analysis() -> Analysis {
    Analysis {
        description: "Panel <ready> & healthy".into(),
        tags: vec!["ci & status".into(), "überblick".into()],
        model: "local<vision>".into(),
    }
}

#[test]
fn stopped_screenshot_analysis_preserves_latest_non_ai_fields_and_unknown_xml() {
    let fixture = Fixture::new();
    let asset = fixture.review();
    let latest = XML
        .replace("Original note", "Latest &amp; unchanged note")
        .replace(
            "<annotations><future/></annotations>",
            "<annotations><future tone=\"new\"/></annotations>",
        );
    fs::write(fixture.journal.join(DOCUMENT), latest).unwrap();

    let analysis = analysis();
    let outcome = apply(&asset, &analysis).unwrap();
    assert!(outcome.changed && !outcome.durability_warning);
    let updated = fixture.xml();
    for fragment in [
        "<userNote>Latest &amp; unchanged note</userNote>",
        "<annotations><future tone=\"new\"/></annotations>",
        "<editedAt>2026-11-07T07:01:00Z</editedAt>",
        "<future flag=\"yes\"><![CDATA[<unrelated>]]></future>",
        "<text>Preserve other row</text>",
        "<aiDescription>Panel &lt;ready&gt; &amp; healthy</aiDescription>",
        "<aiTags format=\"future\"><tag>ci &amp; status</tag><extension>&#x41;</extension><tag>überblick</tag></aiTags>",
        "<aiAnalysisModel>local&lt;vision&gt;</aiAnalysisModel>",
    ] {
        assert!(updated.contains(fragment), "missing XML field {fragment}");
    }
    assert_eq!(
        fs::read(fixture.journal.join(REFERENCE)).unwrap(),
        fixture.png
    );
    assert_eq!(apply(&asset, &analysis).unwrap(), Outcome::default());
    assert_eq!(fixture.xml(), updated);
}

#[test]
fn stopped_screenshot_analysis_rejects_same_byte_image_replacement() {
    let fixture = Fixture::new();
    let asset = fixture.review();
    let visible = fixture.journal.join(REFERENCE);
    fs::rename(&visible, visible.with_extension("previous")).unwrap();
    fs::write(&visible, &fixture.png).unwrap();
    assert!(apply(&asset, &analysis()).is_err());
    assert_eq!(fixture.xml(), XML);
    assert_eq!(fs::read(visible).unwrap(), fixture.png);
}

#[test]
fn stopped_screenshot_analysis_rejects_oversized_and_non_xml_fields() {
    let fixture = Fixture::new();
    let asset = fixture.review();
    let mut analysis = analysis();
    analysis.description = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
    assert!(apply(&asset, &analysis).is_err());
    analysis.description = "Valid description".into();
    analysis.tags[0] = "Bad\u{1}tag".into();
    assert!(apply(&asset, &analysis).is_err());
    assert_eq!(fixture.xml(), XML);
}
