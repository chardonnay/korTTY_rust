use std::fs;
use std::path::PathBuf;

use super::*;
use crate::session_journal::markers::{Compiled, Rule, Settings};
use crate::session_journal::{DOCUMENT, metadata::lock_metadata};

const DOCUMENT_ID: &str = "entry-fixture";
const CHAT: &str =
    "<chatSnapshot><chatTitle>Current &amp; exact</chatTitle><!-- untouched --></chatSnapshot>";
const ORIGINAL: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<session-journal id="entry-fixture" formatVersion="1">
  <meta><title>Original journal</title><logEntryCount>80</logEntryCount><errorCount>17</errorCount>
    <future attr="keep">&#x41;</future>
  </meta>
  <entries>
    <entry><id>target</id><kind>AGENT</kind><marker>INFO</marker><markerSource>AI</markerSource>
      <createdAt>2026-11-06T14:00:00Z</createdAt><editedAt>2026-11-06T15:00:00Z</editedAt>
      <title><![CDATA[Original & raw]]></title>
      <text>before<!-- preserve -->&#x3c;after</text>
      <userNote>first<?keep parse?> note</userNote>
      <logStartSeq>1</logStartSeq><logEndSeq>4</logEndSeq>
      <screenshotFile>screenshots/shot-000001.png</screenshotFile>
      <chatSnapshot><chatTitle>Old</chatTitle></chatSnapshot>
      <extension keep="yes"><child /></extension>
    </entry>
    <entry><id>other</id><kind>USER_NOTE</kind><marker>ERROR</marker><text>Keep &#x41;</text></entry>
  </entries>
  <markerDefinitions>
    <marker><id>review</id><name>Needs review</name><color>#315a82</color><builtIn>false</builtIn><legacyMarker>ERROR</legacyMarker></marker>
  </markerDefinitions>
</session-journal>
"##;

struct Fixture {
    root: PathBuf,
    directory: PathBuf,
}

impl Fixture {
    fn new(xml: &str) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-entry-edit-{}",
            crate::private_files::nonce().unwrap()
        ));
        let directory = root.join("journal");
        fs::create_dir_all(&directory).unwrap();
        let fixture = Self { root, directory };
        fixture.write(xml);
        fixture
    }

    fn write(&self, xml: &str) {
        fs::write(self.directory.join(DOCUMENT), xml).unwrap();
    }

    fn read(&self) -> String {
        fs::read_to_string(self.directory.join(DOCUMENT)).unwrap()
    }

    fn source(&self) -> Source {
        let directory = Directory::open(&self.directory, false).unwrap();
        Source::from_open_directory(&self.directory, DOCUMENT_ID, &directory).unwrap()
    }

    fn draft(&self) -> Draft {
        Draft::read(&self.source(), "target").unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn choice(draft: &Draft, id: &str) -> MarkerChoice {
    draft
        .markers()
        .iter()
        .find(|marker| marker.id() == id)
        .unwrap()
        .clone()
}

#[test]
fn journal_note_translation_source_checks_the_original_note_and_journal() {
    let fixture = Fixture::new(ORIGINAL);
    let draft = fixture.draft();
    draft.require_translation_source().unwrap();
    let new_entry = ORIGINAL.replace(
        "</entries>",
        "<entry><id>later</id><kind>USER_NOTE</kind></entry></entries>",
    );
    fixture.write(&new_entry);
    draft.require_translation_source().unwrap();
    fixture.write(&new_entry.replace("first<?keep parse?> note", "Changed note"));
    assert!(draft.require_translation_source().is_err());
    let old = fixture.root.join("old");
    fs::rename(&fixture.directory, old).unwrap();
    fs::create_dir(&fixture.directory).unwrap();
    fixture.write(ORIGINAL);
    assert!(draft.require_translation_source().is_err());
}

fn deploy_marker() -> Definition {
    Definition {
        id: "deploy".into(),
        name: "Deploy & check".into(),
        color: Some([0x42, 0x63, 0x85]),
        legacy: LegacyMarker::Error,
    }
}

fn deployment_rules() -> Compiled {
    let rule = Rule {
        marker_id: "deploy".into(),
        pattern: "run deployment\nready".into(),
        ..Rule::default()
    };
    Compiled::new(&Settings {
        definitions: vec![deploy_marker()],
        rules: vec![
            rule.clone(),
            Rule {
                marker_id: "info".into(),
                ..rule
            },
        ],
        enabled: false,
    })
    .unwrap()
}

#[test]
fn journal_marker_picker_refreshes_globals_and_keeps_portable_choices() {
    let fixture = Fixture::new(ORIGINAL);
    let review = Definition {
        id: "review".into(),
        name: "Review (current profile)".into(),
        color: Some([1, 2, 3]),
        legacy: LegacyMarker::Important,
    };
    let deploy = deploy_marker();
    let draft =
        Draft::read_with_markers(&fixture.source(), "target", &[deploy.clone(), review]).unwrap();
    assert_eq!(choice(&draft, "review").name(), "Review (current profile)");
    assert_eq!(fixture.read(), ORIGINAL);
    let draft = draft.with_markers(std::slice::from_ref(&deploy));
    assert_eq!(draft.fields().title, "Original & raw");
    assert_eq!(choice(&draft, "review").name(), "Needs review");

    let marker = choice(&draft, "deploy");
    assert!(marker.is_profile_definition());
    save(&draft.edit(Changes::Marker(marker)).unwrap()).unwrap();

    let imported = fixture.draft();
    assert_eq!(imported.marker().definition(), deploy);
    assert!(!imported.marker().is_profile_definition());
    assert_eq!(choice(&imported, "review").name(), "Needs review");
    let xml = fixture.read();
    assert!(xml.contains("<name>Deploy &amp; check</name><color>#426385</color>"));
    assert!(xml.contains("<markerSource>USER</markerSource>"));
    assert!(xml.contains("<errorCount>2</errorCount>"));
}

#[test]
fn journal_marker_picker_keeps_every_global_choice_at_the_profile_limit() {
    let fixture = Fixture::new(ORIGINAL);
    let settings = Settings {
        definitions: (0..MAX_DEFINITIONS)
            .map(|index| Definition {
                id: format!("global-{index}"),
                name: format!("Global {index}"),
                color: Some([1, 2, 3]),
                legacy: LegacyMarker::Info,
            })
            .collect(),
        ..Settings::default()
    };
    let rules = Compiled::new(&settings).unwrap();
    let draft = fixture.draft().with_markers(rules.registry());

    assert_eq!(draft.markers().len(), rules.registry().len() + 1);
    assert!(choice(&draft, &format!("global-{}", MAX_DEFINITIONS - 1)).is_profile_definition());
    assert_eq!(choice(&draft, "review").name(), "Needs review");
}

#[test]
fn journal_marker_rules_apply_first_match_and_preserve_user_none_until_overwritten() {
    let xml = ORIGINAL
        .replace(
            "<logStartSeq>1</logStartSeq>",
            "<inputExcerpt><line>RUN DEPLOYMENT</line></inputExcerpt>\
             <outputExcerpt><line>ready</line></outputExcerpt><logStartSeq>1</logStartSeq>",
        )
        .replace(
            "  </entries>",
            "    <entry><id>manual-none</id><kind>USER_NOTE</kind><marker>NONE</marker>\
             <markerSource>USER</markerSource><title>run deployment&#10;ready</title></entry>\n  </entries>",
        );
    let fixture = Fixture::new(&xml);
    let capture = fixture.directory.join("session-log.json");
    let capture_bytes = concat!(
        "{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"entry-fixture\",\"part\":1}\n",
        "{\"seq\":1,\"t\":\"2026-11-06T14:00:00Z\",\"k\":\"out\",\"x\":\"unchanged capture bytes\"}\n",
        "{\"type\":\"end\"}\n",
    );
    fs::write(&capture, capture_bytes).unwrap();
    let source = fixture.source();
    let rules = deployment_rules();
    assert!(!rules.enabled());
    assert_eq!(
        apply_rules(&source, &rules, false).unwrap(),
        MarkerOutcome {
            changed: 1,
            durability_warning: false,
        }
    );

    let updated = fixture.read();
    let document = parse_document(&updated).unwrap();
    let target = find_entry(&document, "target").unwrap();
    assert_eq!(bounded_field(target, "marker").unwrap(), "ERROR");
    assert_eq!(bounded_field(target, "markerId").unwrap(), "deploy");
    assert_eq!(bounded_field(target, "markerSource").unwrap(), "RULE");
    assert_eq!(
        bounded_field(target, "editedAt").unwrap(),
        "2026-11-06T15:00:00Z"
    );
    let manual = find_entry(&document, "manual-none").unwrap();
    assert_eq!(bounded_field(manual, "marker").unwrap(), "NONE");
    assert_eq!(bounded_field(manual, "markerSource").unwrap(), "USER");
    for unchanged in [
        "<text>before<!-- preserve -->&#x3c;after</text>",
        "<extension keep=\"yes\"><child /></extension>",
        "<chatSnapshot><chatTitle>Old</chatTitle></chatSnapshot>",
        "<future attr=\"keep\">&#x41;</future>",
        "<text>Keep &#x41;</text>",
    ] {
        assert!(updated.contains(unchanged));
    }
    assert!(updated.contains("<errorCount>2</errorCount>"));
    assert_eq!(fixture.draft().marker().definition(), deploy_marker());
    assert_eq!(
        apply_rules(&source, &rules, false).unwrap(),
        MarkerOutcome::default()
    );
    assert_eq!(fixture.read(), updated);

    assert_eq!(apply_rules(&source, &rules, true).unwrap().changed, 1);
    let updated = fixture.read();
    let document = parse_document(&updated).unwrap();
    let manual = find_entry(&document, "manual-none").unwrap();
    assert_eq!(bounded_field(manual, "markerSource").unwrap(), "RULE");
    assert_eq!(bounded_field(manual, "markerId").unwrap(), "deploy");
    assert!(updated.contains("<errorCount>3</errorCount>"));
    assert_eq!(updated.matches("<id>deploy</id>").count(), 1);
    assert_eq!(fs::read(&capture).unwrap(), capture_bytes.as_bytes());
}

#[test]
fn journal_marker_rules_cannot_modify_a_locked_journal() {
    let fixture = Fixture::new(ORIGINAL);
    let source = fixture.source();
    let _lock = lock_metadata(&fixture.directory).unwrap();
    let error = apply_rules(&source, &deployment_rules(), false).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(fixture.read(), ORIGINAL);
}

#[test]
fn journal_marker_rules_report_postpublication_sync_failure_as_a_warning() {
    let fixture = Fixture::new(&ORIGINAL.replace(
        "<title><![CDATA[Original & raw]]></title>",
        "<title>run deployment&#10;ready</title>",
    ));
    let outcome =
        marker_rules::apply_rules_with_sync(&fixture.source(), &deployment_rules(), false, |_| {
            // Test double injects the sync failure after the actual atomic XML replacement.
            assert_eq!(fixture.draft().marker().id(), "deploy");
            Err(io::Error::other("Injected journal directory sync failure"))
        })
        .unwrap();
    assert_eq!(
        outcome,
        MarkerOutcome {
            changed: 1,
            durability_warning: true
        }
    );
}

#[test]
fn journal_entry_edit_preserves_current_unrelated_imported_xml_and_updates_error_count() {
    let fixture = Fixture::new(ORIGINAL);
    let draft = fixture.draft();
    assert_eq!(draft.fields().title, "Original & raw");
    assert_eq!(draft.fields().text, "before<after");
    assert_eq!(draft.fields().note, "first note");
    let review = choice(&draft, "review");
    assert_eq!(review.name(), "Needs review");
    assert_eq!(review.color(), Some([0x31, 0x5a, 0x82]));
    assert_eq!(review.legacy(), LegacyMarker::Error);
    assert!(!review.is_builtin());
    let edit = draft
        .edit(Changes::Fields {
            fields: Fields {
                title: "\u{2000}Review & <approve> \u{3000}".into(),
                text: "\u{00a0}first\rsecond\u{00a0}".into(),
                note: "\t\u{2000}\n".into(),
            },
            marker: review,
        })
        .unwrap();
    let newer = ORIGINAL
        .replace(
            "<chatSnapshot><chatTitle>Old</chatTitle></chatSnapshot>",
            CHAT,
        )
        .replace("<logEndSeq>4</logEndSeq>", "<logEndSeq>81</logEndSeq>")
        .replace(
            "<logEntryCount>80</logEntryCount>",
            "<logEntryCount>81</logEntryCount>",
        );
    fixture.write(&newer);

    save(&edit).unwrap();
    let updated = fixture.read();
    let document = parse_document(&updated).unwrap();
    let entry = find_entry(&document, "target").unwrap();
    let edited_at = bounded_field(entry, "editedAt").unwrap();
    assert_ne!(edited_at, "2026-11-06T15:00:00Z");
    assert!(edited_at.parse::<Timestamp>().is_ok());
    let expected = newer
        .replacen("<marker>INFO</marker>", "<marker>ERROR</marker>", 1)
        .replace(
            "<markerSource>AI</markerSource>",
            "<markerSource>USER</markerSource>",
        )
        .replace(
            "<editedAt>2026-11-06T15:00:00Z</editedAt>",
            &format!("<editedAt>{edited_at}</editedAt>"),
        )
        .replace(
            "<title><![CDATA[Original & raw]]></title>",
            "<title>Review &amp; &lt;approve&gt;</title>",
        )
        .replace(
            "<text>before<!-- preserve -->&#x3c;after</text>",
            "<text>\u{00a0}first&#13;second\u{00a0}</text>",
        )
        .replace("<userNote>first<?keep parse?> note</userNote>", "")
        .replacen("    </entry>", "    <markerId>review</markerId></entry>", 1)
        .replace("<errorCount>17</errorCount>", "<errorCount>2</errorCount>");
    assert_eq!(updated, expected);
    assert!(!format!("{draft:?} {edit:?}").contains("Original"));
    assert!(!format!("{edit:?}").contains("approve"));
}

#[test]
fn journal_entry_marker_only_keeps_scalar_source_and_clears_custom_id() {
    let xml = ORIGINAL.replacen(
        "<marker>INFO</marker>",
        "<marker>ERROR</marker><markerId>review</markerId>",
        1,
    );
    let fixture = Fixture::new(&xml);
    let draft = fixture.draft();
    assert_eq!(draft.marker().id(), "review");
    let edit = draft.edit(Changes::Marker(choice(&draft, "none"))).unwrap();
    save(&edit).unwrap();
    let updated = fixture.read();
    for unchanged in [
        "<title><![CDATA[Original & raw]]></title>",
        "<text>before<!-- preserve -->&#x3c;after</text>",
        "<userNote>first<?keep parse?> note</userNote>",
        "<logStartSeq>1</logStartSeq><logEndSeq>4</logEndSeq>",
        "<screenshotFile>screenshots/shot-000001.png</screenshotFile>",
    ] {
        assert!(updated.contains(unchanged));
    }
    assert!(!updated.contains("<markerId>"));
    assert!(updated.contains("<marker>NONE</marker><markerSource>USER</markerSource>"));
    assert!(updated.contains("<errorCount>1</errorCount>"));
}

#[test]
fn journal_entry_edit_refuses_stale_ambiguous_and_missing_rows() {
    let fixture = Fixture::new(ORIGINAL);
    let draft = fixture.draft();
    let edit = draft
        .edit(Changes::Marker(choice(&draft, "error")))
        .unwrap();
    for changed in [
        ORIGINAL.replace("first<?keep parse?> note", "Someone else's note"),
        ORIGINAL.replace(
            "<markerSource>AI</markerSource>",
            "<markerSource>RULE</markerSource>",
        ),
        ORIGINAL.replace("<id>other</id>", "<id>target</id>"),
        ORIGINAL.replace("<id>target</id>", "<id>removed</id>"),
    ] {
        fixture.write(&changed);
        assert!(save(&edit).is_err());
        assert_eq!(fixture.read(), changed);
    }
    assert!(Draft::read(draft.source(), "target").is_err());
    fixture.write(&ORIGINAL.replace("<id>other</id>", "<id>target</id>"));
    assert!(Draft::read(draft.source(), "target").is_err());
}

#[test]
fn journal_entry_edit_checks_live_lock_and_rejects_a_same_id_path_replacement() {
    let fixture = Fixture::new(ORIGINAL);
    let draft = fixture.draft();
    let edit = draft.edit(Changes::Marker(choice(&draft, "info"))).unwrap();
    let lock = lock_metadata(&fixture.directory).unwrap();
    assert!(save(&edit).is_err());
    assert_eq!(fixture.read(), ORIGINAL);
    drop(lock);

    fs::rename(&fixture.directory, fixture.root.join("previous")).unwrap();
    fs::create_dir(&fixture.directory).unwrap();
    fixture.write(ORIGINAL);
    assert!(save(&edit).is_err());
    assert!(Draft::read(draft.source(), "target").is_err());
    assert!(
        !fixture
            .directory
            .join(crate::session_journal::metadata::LOCK)
            .exists()
    );
    assert_eq!(fixture.read(), ORIGINAL);
}

#[test]
fn journal_entry_edit_uses_the_original_opaque_id_and_validates_fields() {
    let id = "tar\u{202e}get";
    let xml = ORIGINAL.replace("<id>other</id>", &format!("<id>{id}</id>"));
    let fixture = Fixture::new(&xml);
    let journal = fixture.source().read(None).unwrap().unwrap();
    assert!(journal.timeline.iter().any(|entry| entry.id == id));
    let draft = Draft::read(&fixture.source(), id).unwrap();
    assert_eq!(draft.fields().text, "Keep A");
    for note in ["bad\0text".into(), "A".repeat(MAX_FIELD_BYTES + 1)] {
        assert!(
            draft
                .edit(Changes::Fields {
                    fields: Fields {
                        note,
                        ..Fields::default()
                    },
                    marker: draft.marker().clone(),
                })
                .is_err()
        );
    }
    let edit = draft.edit(Changes::Marker(choice(&draft, "none"))).unwrap();
    save(&edit).unwrap();
    assert!(fixture.read().contains(&format!(
        "<id>{id}</id><kind>USER_NOTE</kind><marker>NONE</marker>"
    )));
    assert_eq!(fixture.draft().marker().id(), "info");
}

#[test]
fn journal_entry_edit_rejects_namespaced_scalar_collisions_without_rewriting_xml() {
    let fixture = Fixture::new(ORIGINAL);
    let draft = fixture.draft();
    let edit = draft
        .edit(Changes::Marker(choice(&draft, "error")))
        .unwrap();
    let collision = ORIGINAL.replace(
        "<errorCount>17</errorCount>",
        "<future:errorCount xmlns:future=\"urn:extension\">17</future:errorCount>",
    );
    fixture.write(&collision);
    assert!(save(&edit).is_err());
    assert_eq!(fixture.read(), collision);

    let collision = ORIGINAL.replace(
        "<id>target</id>",
        "<future:id xmlns:future=\"urn:extension\">target</future:id>",
    );
    fixture.write(&collision);
    assert!(Draft::read(draft.source(), "target").is_err());
    assert_eq!(fixture.read(), collision);
}
