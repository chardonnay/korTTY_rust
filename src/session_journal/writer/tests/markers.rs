use crate::session_journal::markers::{Compiled, Definition, Rule, Settings};
use crate::terminal_logging::{Policy, Redactor, Secrets};

use super::*;

fn definition(id: &str, name: &str, color: [u8; 3], legacy: LegacyMarker) -> Definition {
    Definition {
        id: id.into(),
        name: name.into(),
        color: Some(color),
        legacy,
    }
}

fn rule(marker_id: &str, pattern: &str) -> Rule {
    Rule {
        marker_id: marker_id.into(),
        pattern: pattern.into(),
        ..Rule::default()
    }
}

fn compiled(settings: &Settings) -> Arc<Compiled> {
    Arc::new(Compiled::new(settings).unwrap())
}

fn note(capture: &Capture, text: impl Into<String>) {
    capture
        .sink()
        .try_write(RecordKind::Note, Zeroizing::new(text.into()), false)
        .unwrap();
}

fn xml(capture: &Capture) -> String {
    fs::read_to_string(capture.directory().join("journal.xml")).unwrap()
}

#[test]
fn live_marker_rules_use_current_profile_and_see_only_already_redacted_new_entries() {
    let fixture = Fixture::new();
    let mut profile = Settings {
        enabled: true,
        definitions: vec![
            definition("secret", "Secret", [0, 0, 0], LegacyMarker::Error),
            definition("deploy", "Deployment", [1, 2, 3], LegacyMarker::Info),
        ],
        rules: vec![rule("secret", "private-token"), rule("deploy", "deploy")],
    };
    let capture = Capture::start_with_marker_rules(
        fixture.journals(),
        Metadata::default(),
        compiled(&profile),
    )
    .unwrap();
    let secrets = Secrets::default();
    secrets.add("private-token");
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
    let redacted = redactor
        .journal_text("private-token", Instant::now() + Duration::from_secs(1))
        .unwrap();
    note(&capture, redacted.as_str());
    wait_timeline(&capture, 1);
    assert!(!xml(&capture).contains("private-token"));
    assert!(!xml(&capture).contains("<markerId>secret</markerId>"));
    assert!(
        read(capture.directory()).unwrap().timeline[0]
            .marker_key
            .is_none()
    );

    note(&capture, "Deploy first");
    wait_timeline(&capture, 2);
    let first = read(capture.directory()).unwrap();
    assert_eq!(first.timeline[1].marker_name, "Deployment");
    assert_eq!(first.timeline[1].marker_color, Some([1, 2, 3]));

    profile.definitions[1].name = "New deployment label".into();
    profile.definitions[1].color = Some([4, 5, 6]);
    capture.set_marker_rules(compiled(&profile));
    note(&capture, "DEPLOY second");
    wait_timeline(&capture, 3);
    profile.enabled = false;
    capture.set_marker_rules(compiled(&profile));
    note(&capture, "Deploy disabled");
    wait_timeline(&capture, 4);
    wait(capture.finish());

    let document = read(capture.directory()).unwrap();
    for entry in &document.timeline[1..=2] {
        assert_eq!(entry.marker_name, "New deployment label");
        assert_eq!(entry.marker_color, Some([4, 5, 6]));
    }
    assert!(document.timeline[3].marker_key.is_none());
    let persisted = xml(&capture);
    assert_eq!(
        persisted
            .matches("<markerSource>RULE</markerSource>")
            .count(),
        2
    );
    assert_eq!(persisted.matches("<id>deploy</id>").count(), 1);
    assert!(persisted.contains("<color>#040506</color>"));
}

#[test]
fn live_marker_rules_apply_fifo_without_touching_capture_log_and_protect_manual_markers() {
    let fixture = Fixture::new();
    let profile = compiled(&Settings {
        enabled: false,
        definitions: vec![
            definition("outage", "Outage", [9, 8, 7], LegacyMarker::Error),
            definition(
                "manual",
                "Manual choice",
                [2, 3, 4],
                LegacyMarker::Important,
            ),
        ],
        rules: vec![rule("outage", "outage")],
    });
    let capture = Capture::start_with_marker_rules(
        fixture.journals(),
        Metadata::default(),
        Arc::clone(&profile),
    )
    .unwrap();
    let source = viewer::Source::capture(&capture);
    note(&capture, "Manual outage");
    note(&capture, "Automatic outage");
    wait_timeline(&capture, 2);
    let first = read(capture.directory()).unwrap().timeline[0].id.clone();
    let draft = Draft::read_with_markers(&source, &first, profile.registry()).unwrap();
    let marker = draft
        .markers()
        .iter()
        .find(|choice| choice.id() == "manual")
        .unwrap();
    wait_edit(
        capture
            .edit_entry(draft.edit(Changes::Marker(marker.clone())).unwrap())
            .unwrap(),
    )
    .unwrap();
    let before = xml(&capture);
    assert!(before.contains("<markerId>manual</markerId>\n<markerSource>USER</markerSource>"));
    assert!(before.contains("<id>manual</id>\n<name>Manual choice</name>"));
    let edited_at = before
        .lines()
        .find(|line| line.starts_with("<editedAt>"))
        .unwrap()
        .to_owned();
    let log = capture.directory().join("session-log.json");
    let before_log = fs::read(&log).unwrap();

    assert_eq!(
        wait_edit(
            capture
                .apply_marker_rules(&source, Arc::clone(&profile), false)
                .unwrap()
        )
        .unwrap(),
        MarkerOutcome {
            changed: 1,
            durability_warning: false,
        }
    );
    let current = read(capture.directory()).unwrap();
    assert_eq!(current.timeline[0].marker_key.as_deref(), Some("manual"));
    assert_eq!(current.timeline[1].marker_key.as_deref(), Some("outage"));
    assert_eq!(
        wait_edit(
            capture
                .apply_marker_rules(&source, Arc::clone(&profile), true)
                .unwrap()
        )
        .unwrap(),
        MarkerOutcome {
            changed: 1,
            durability_warning: false,
        }
    );
    let updated = xml(&capture);
    assert!(updated.contains("<errorCount>2</errorCount>"));
    assert_eq!(
        updated.matches("<markerSource>RULE</markerSource>").count(),
        2
    );
    assert_eq!(
        updated
            .lines()
            .filter(|line| line.starts_with("<editedAt>"))
            .collect::<Vec<_>>(),
        vec![edited_at.as_str()]
    );
    assert_eq!(
        wait_edit(
            capture
                .apply_marker_rules(&source, Arc::clone(&profile), true)
                .unwrap()
        )
        .unwrap(),
        MarkerOutcome::default()
    );
    assert_eq!(xml(&capture), updated);

    let deletion = Draft::read(&source, &first)
        .unwrap()
        .prepare_delete()
        .unwrap();
    wait_edit(capture.delete_entry(Arc::new(deletion)).unwrap()).unwrap();
    let pruned = xml(&capture);
    assert!(!pruned.contains("<id>manual</id>"));
    assert!(pruned.contains("<id>outage</id>"));
    assert!(pruned.contains("<errorCount>1</errorCount>"));
    assert_eq!(fs::read(&log).unwrap(), before_log);
    wait(capture.finish());
}

#[test]
fn live_manual_marker_rejects_a_profile_definition_changed_since_review() {
    let fixture = Fixture::new();
    let mut profile = Settings {
        definitions: vec![definition(
            "review",
            "Before",
            [1, 1, 1],
            LegacyMarker::Info,
        )],
        ..Settings::default()
    };
    let original = compiled(&profile);
    let capture = Capture::start_with_marker_rules(
        fixture.journals(),
        Metadata::default(),
        Arc::clone(&original),
    )
    .unwrap();
    let source = viewer::Source::capture(&capture);
    note(&capture, "Choose a marker");
    wait_timeline(&capture, 1);
    let id = read(capture.directory()).unwrap().timeline[0].id.clone();
    let edit = |profile: &Compiled| {
        let draft = Draft::read_with_markers(&source, &id, profile.registry()).unwrap();
        let marker = draft
            .markers()
            .iter()
            .find(|choice| choice.id() == "review")
            .unwrap();
        draft.edit(Changes::Marker(marker.clone())).unwrap()
    };
    let old = edit(&original);
    let before = xml(&capture);
    profile.definitions[0].name = "After".into();
    let current = compiled(&profile);
    capture.set_marker_rules(Arc::clone(&current));
    assert!(wait_edit(capture.edit_entry(old).unwrap()).is_err());
    assert_eq!(xml(&capture), before);
    wait_edit(capture.edit_entry(edit(&current)).unwrap()).unwrap();
    assert!(xml(&capture).contains("<name>After</name>"));
    wait(capture.finish());
}

#[test]
fn live_marker_application_reports_published_count_when_directory_sync_fails() {
    let fixture = Fixture::new();
    let profile = compiled(&Settings {
        definitions: vec![definition(
            "review",
            "Review",
            [4, 5, 6],
            LegacyMarker::Error,
        )],
        rules: vec![rule("review", "review")],
        ..Settings::default()
    });
    let capture = Capture::start_with_marker_rules(
        fixture.journals(),
        Metadata::default(),
        Arc::clone(&profile),
    )
    .unwrap();
    let source = viewer::Source::capture(&capture);
    note(&capture, "Review this");
    wait_timeline(&capture, 1);
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    let result = wait_edit(capture.apply_marker_rules(&source, profile, false).unwrap()).unwrap();
    assert_eq!(
        result,
        MarkerOutcome {
            changed: 1,
            durability_warning: true,
        }
    );
    assert!(xml(&capture).contains("<markerId>review</markerId>"));
    note(&capture, "Capture continues");
    wait_timeline(&capture, 3);
    wait(capture.finish());
    assert!(xml(&capture).contains("<markerId>review</markerId>"));
    assert!(xml(&capture).contains("Capture continues"));
}
