use std::fs;
use std::path::PathBuf;

use tiny_skia::Color;

use crate::session_journal::{DOCUMENT, annotation::Kind};

use super::*;

const ID: &str = "screenshot-fixture";
const REFERENCE: &str = "screenshots/shot.png";
const ORIGINAL: &str = "screenshots/shot.orig.png";
const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<session-journal id="screenshot-fixture" formatVersion="1">
  <meta><title>Screenshot fixture</title></meta><entries>
    <entry><id>shot</id><kind>SCREENSHOT</kind><createdAt>2026-11-07T07:00:00Z</createdAt>
      <title><![CDATA[Preserve <summary>]]></title><screenshotFile>screenshots/shot.png</screenshotFile>
      <logPart>1</logPart><logStartSeq>13</logStartSeq><logEndSeq>17</logEndSeq>
      <userNote>Original note</userNote><future flag="yes">&#x41;</future>
    </entry>
  </entries>
</session-journal>"#;

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
    png: Vec<u8>,
}

impl Fixture {
    fn new(xml: &str) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-screenshot-edit-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = root.join("journal");
        fs::create_dir_all(journal.join(SCREENSHOTS)).unwrap();
        let mut raster = Pixmap::new(24, 16).unwrap();
        raster.fill(Color::from_rgba8(10, 40, 70, 255));
        let png = raster.encode_png().unwrap();
        fs::write(journal.join(REFERENCE), &png).unwrap();
        fs::write(journal.join(DOCUMENT), xml).unwrap();
        Self { root, journal, png }
    }

    fn source(&self) -> Source {
        let opened = Directory::open(&self.journal, false).unwrap();
        Source::from_open_directory(&self.journal, ID, &opened).unwrap()
    }

    fn draft(&self) -> Arc<Draft> {
        Arc::new(Draft::read(&self.source(), "shot", REFERENCE).unwrap())
    }

    fn visible(&self) -> Vec<u8> {
        fs::read(self.journal.join(REFERENCE)).unwrap()
    }

    fn xml(&self) -> String {
        fs::read_to_string(self.journal.join(DOCUMENT)).unwrap()
    }

    fn stage(&self) -> PathBuf {
        self.journal
            .join(SCREENSHOTS)
            .join(annotation_stage_name("shot.png"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn mark(color: [u8; 3]) -> Annotation {
    Annotation {
        kind: Kind::Box,
        points: vec![[4.0, 3.0], [12.0, 9.0]],
        stroke_width: 2.0,
        color,
        ..Annotation::default()
    }
}

#[test]
fn screenshot_note_translation_source_rejects_changed_note_or_screenshot_row() {
    let fixture = Fixture::new(XML);
    let draft = fixture.draft();
    draft.require_translation_source().unwrap();
    let document = fixture.journal.join(DOCUMENT);
    fs::write(&document, XML.replace("Preserve", "Updated title")).unwrap();
    draft.require_translation_source().unwrap();
    fs::write(&document, XML.replace("Original note", "Changed note")).unwrap();
    assert!(draft.require_translation_source().is_err());
    fs::write(&document, XML.replace(REFERENCE, "screenshots/other.png")).unwrap();
    assert!(draft.require_translation_source().is_err());
}

#[test]
fn journal_screenshot_edit_bakes_from_private_original_and_restores_exact_bytes() {
    let fixture = Fixture::new(XML);
    let first = fixture.draft();
    assert_eq!(first.original(), Some(fixture.png.as_slice()));
    assert!(first.annotations().is_empty());
    let red = mark([230, 35, 24]);
    let edit = first
        .edit(vec![red.clone()], "  First & exact  ".into())
        .unwrap();
    assert_eq!(apply(&edit).unwrap(), Outcome::default());
    assert_ne!(fixture.visible(), fixture.png);
    let opened = Directory::open(&fixture.journal, false).unwrap();
    let screenshots = screenshot_folder(&opened).unwrap();
    screenshots.open_private_file("shot.orig.png").unwrap();
    assert_eq!(
        fs::read(fixture.journal.join(ORIGINAL)).unwrap(),
        fixture.png
    );
    let second = fixture.draft();
    assert_eq!(second.annotations(), [red]);
    assert_eq!(second.note(), "First & exact");
    assert_eq!(second.original(), Some(fixture.png.as_slice()));

    let green = mark([31, 210, 47]);
    let expected = annotation::render(
        &Pixmap::decode_png(&fixture.png).unwrap(),
        std::slice::from_ref(&green),
    )
    .unwrap()
    .encode_png()
    .unwrap();
    assert_eq!(
        apply(&second.edit(vec![green], second.note().into()).unwrap()).unwrap(),
        Outcome::default()
    );
    assert_eq!(fixture.visible(), expected);
    let third = fixture.draft();
    assert_eq!(
        apply(&third.edit(Vec::new(), third.note().into()).unwrap()).unwrap(),
        Outcome::default()
    );
    assert_eq!(fixture.visible(), fixture.png);
    assert!(!fixture.journal.join(ORIGINAL).exists());
    assert!(!fixture.stage().exists());
    let final_xml = fixture.xml();
    for unchanged in [
        "<title><![CDATA[Preserve <summary>]]></title>",
        "<logPart>1</logPart><logStartSeq>13</logStartSeq><logEndSeq>17</logEndSeq>",
        "<future flag=\"yes\">&#x41;</future>",
    ] {
        assert!(final_xml.contains(unchanged));
    }
    assert!(final_xml.contains("<editedAt>"));
    assert!(final_xml.contains("<userNote>First &amp; exact</userNote>"));
    assert!(!final_xml.contains("<annotations>"));
}

#[test]
fn journal_screenshot_edit_baked_import_and_shared_alias_allow_only_note_edits() {
    let red = mark([230, 35, 24]);
    let annotations = annotation::xml(std::slice::from_ref(&red)).unwrap();
    let import = Fixture::new(&XML.replace("</entry>", &format!("{annotations}</entry>")));
    let shared = Fixture::new(&XML.replace(
        "</entries>",
        "<entry><id>another</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/SHOT.png</screenshotFile></entry></entries>",
    ));
    for fixture in [&import, &shared] {
        let draft = fixture.draft();
        assert!(!draft.can_annotate());
        let attempted_marks = if draft.annotations().is_empty() {
            vec![red.clone()]
        } else {
            Vec::new()
        };
        assert!(draft.edit(attempted_marks, draft.note().into()).is_err());
        let folder = screenshot_folder(&Directory::open(&fixture.journal, false).unwrap()).unwrap();
        let stamp = FileStamp::read(&open_image(&folder, "shot.png").unwrap()).unwrap();
        let edit = draft
            .edit(draft.annotations().to_vec(), "Imported note".into())
            .unwrap();
        assert_eq!(apply(&edit).unwrap(), Outcome::default());
        stamp
            .require_unchanged(&open_image(&folder, "shot.png").unwrap())
            .unwrap();
        assert_eq!(fixture.visible(), fixture.png);
        assert!(!fixture.journal.join(ORIGINAL).exists());
        assert_eq!(fixture.draft().note(), "Imported note");
    }
    assert!(import.xml().contains(&annotations));
}

#[test]
fn journal_screenshot_edit_rolls_back_when_xml_cannot_publish() {
    let fixture = Fixture::new(XML);
    let edit = fixture
        .draft()
        .edit(vec![mark([230, 35, 24])], "New note".into())
        .unwrap();
    let lock = lock_source(edit.source()).unwrap();
    let (stamp, _) = lock.read_document().unwrap();
    let result = edit.publish_with(lock.directory(), |operation| match operation {
        XmlWrite::Check => lock.require_document(&stamp).map(|()| false),
        XmlWrite::Commit(verify) => {
            verify()?;
            assert_ne!(fixture.visible(), fixture.png);
            assert_eq!(
                fs::read(fixture.journal.join(ORIGINAL)).unwrap(),
                fixture.png
            );
            // Test double rejects XML publication after the actual checked PNG rename.
            Err(io::Error::other("Injected XML publication failure"))
        }
    });
    assert_eq!(
        result.unwrap_err().to_string(),
        "Injected XML publication failure"
    );
    assert_eq!(fixture.visible(), fixture.png);
    assert_eq!(fixture.xml(), XML);
    assert!(!fixture.journal.join(ORIGINAL).exists());
    assert!(!fixture.stage().exists());
}

#[test]
fn journal_screenshot_edit_retains_recovery_and_does_not_overwrite_a_substitute() {
    let fixture = Fixture::new(XML);
    let edit = fixture
        .draft()
        .edit(vec![mark([230, 35, 24])], "New note".into())
        .unwrap();
    let lock = lock_source(edit.source()).unwrap();
    let (stamp, _) = lock.read_document().unwrap();
    let substitute = Pixmap::new(10, 10).unwrap().encode_png().unwrap();
    let result = edit.publish_with(lock.directory(), |operation| match operation {
        XmlWrite::Check => lock.require_document(&stamp).map(|()| false),
        XmlWrite::Commit(verify) => {
            // Test double interleaves an actual external file replacement before XML publication.
            fs::rename(
                fixture.journal.join(REFERENCE),
                fixture.root.join("moved.png"),
            )?;
            fs::write(fixture.journal.join(REFERENCE), &substitute)?;
            verify().map(|()| false)
        }
    });
    assert!(result.unwrap_err().to_string().contains("needs recovery"));
    assert_eq!(fixture.visible(), substitute);
    assert_eq!(fixture.xml(), XML);
    assert_eq!(
        fs::read(fixture.stage().join("previous/shot.png")).unwrap(),
        fixture.png
    );
    assert_eq!(
        Draft::read(&fixture.source(), "shot", REFERENCE)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_screenshot_edit_never_clobbers_a_file_arriving_in_either_publication_gap() {
    for during_rollback in [false, true] {
        let fixture = Fixture::new(XML);
        let edit = fixture
            .draft()
            .edit(vec![mark([230, 35, 24])], "New note".into())
            .unwrap();
        let lock = lock_source(edit.source()).unwrap();
        let (stamp, _) = lock.read_document().unwrap();
        let intruder = Pixmap::new(11, 10).unwrap().encode_png().unwrap();
        let visible = fixture.journal.join(REFERENCE);
        let mut reached_commit = false;
        let mut filled_gap = false;
        let result = edit.publish_with(lock.directory(), |operation| match operation {
            XmlWrite::Check => {
                lock.require_document(&stamp)?;
                if reached_commit == during_rollback && !visible.exists() && !filled_gap {
                    // Test double interleaves a real external file creation after the slot claim.
                    fs::write(&visible, &intruder)?;
                    filled_gap = true;
                }
                Ok(false)
            }
            XmlWrite::Commit(verify) => {
                verify()?;
                reached_commit = true;
                // Test double rejects XML publication so the real rollback path runs.
                Err(io::Error::other("Injected XML publication failure"))
            }
        });
        assert!(filled_gap);
        assert_eq!(reached_commit, during_rollback);
        assert!(result.unwrap_err().to_string().contains("needs recovery"));
        assert_eq!(fixture.visible(), intruder);
        assert_eq!(fixture.xml(), XML);
        assert_eq!(
            fs::read(fixture.stage().join("previous/shot.png")).unwrap(),
            fixture.png
        );
    }
}
