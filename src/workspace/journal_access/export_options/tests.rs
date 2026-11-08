use std::fs;
use std::path::PathBuf;

use jiff::civil::Time;

use super::*;
use crate::session_journal::{catalog, export};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-export-options-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn snapshots(&self) -> Arc<[Snapshot]> {
        let directory = self.0.join("journal");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("journal.xml"),
            r#"<session-journal formatVersion="1" id="options-fixture">
              <meta><title>Options fixture</title></meta>
              <entries>
                <entry><title>Apache installed</title><marker>IMPORTANT</marker></entry>
                <entry><title>System restarted</title></entry>
              </entries>
            </session-journal>"#,
        )
        .unwrap();
        let snapshot = catalog(&self.0).unwrap();
        let row = &snapshot.journals[0];
        Arc::from([export::load(
            &self.0,
            &directory,
            &row.id,
            row.deletion_identity.as_ref().unwrap(),
        )
        .unwrap()])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn edit(draft: &mut Draft, event: Event) {
    let _ = draft.update(event);
}

fn finish_count(draft: &mut Draft, snapshots: Option<&[Snapshot]>) {
    let job = draft.preview_job.as_ref().unwrap();
    let result = count(snapshots, &draft.filter().unwrap(), &job.cancelled);
    // Test double for Iced's delivery of the actual backend preview result.
    edit(draft, Event::Counted(job.revision, result));
}

#[test]
fn journal_export_options_defaults_and_preview_limit_follow_java() {
    for selection in [1, 10, 11] {
        let mut draft = Draft::new(Format::Markdown, selection, Vec::new());
        assert_eq!(draft.wants_preview(), selection <= MAX_PREVIEW_JOURNALS);
        let choice = draft.choice().unwrap();
        assert!(choice.options.include_screenshots);
        assert!(choice.options.include_chats);
        assert!(!choice.ai_topic);
        assert_eq!(choice.options.filter.tolerance_minutes, 5);
        assert!(!choice.options.filter.is_active());
        assert!(matches!(choice.protection, ProtectionChoice::None));
        edit(&mut draft, Event::IncludeScreenshots(false));
        edit(&mut draft, Event::IncludeChats(false));
        let options = draft.choice().unwrap().options;
        assert!(!options.include_screenshots);
        assert!(!options.include_chats);
    }
    assert!(
        Draft::new(Format::Markdown, 0, Vec::new())
            .choice()
            .is_none()
    );
}

#[test]
fn journal_export_options_validate_dates_tolerance_and_stale_time_window_events() {
    let mut draft = Draft::new(Format::Markdown, 1, Vec::new());
    edit(&mut draft, Event::AddWindow);
    let first = draft.windows[0].id;
    assert!(draft.choice().unwrap().options.filter.windows.is_empty());
    edit(
        &mut draft,
        Event::Bound(first, Bound::FromTime, "2230".into()),
    );
    edit(&mut draft, Event::Bound(first, Bound::ToTime, "6.5".into()));
    let options = draft.choice().unwrap().options;
    assert_eq!(
        options.filter.windows[0].from_time,
        Some(Time::new(22, 30, 0, 0).unwrap())
    );
    assert_eq!(
        options.filter.windows[0].to_time,
        Some(Time::new(6, 5, 0, 0).unwrap())
    );

    edit(
        &mut draft,
        Event::Bound(first, Bound::FromDate, "2026-11-08".into()),
    );
    edit(
        &mut draft,
        Event::Bound(first, Bound::ToDate, "2026-11-07".into()),
    );
    assert!(draft.choice().is_none());
    edit(&mut draft, Event::RemoveWindow(first));
    edit(&mut draft, Event::AddWindow);
    let second = draft.windows[0].id;
    assert_ne!(second, first);
    edit(
        &mut draft,
        Event::Bound(first, Bound::FromTime, "not a time".into()),
    );
    assert!(draft.choice().is_some());
    edit(
        &mut draft,
        Event::Bound(second, Bound::FromTime, "25:00".into()),
    );
    assert!(draft.choice().is_none());
    edit(&mut draft, Event::ClearWindows);

    edit(&mut draft, Event::Tolerance("121".into()));
    assert!(draft.choice().is_none());
    edit(&mut draft, Event::Tolerance("0".into()));
    edit(&mut draft, Event::StepTolerance(false));
    assert_eq!(draft.choice().unwrap().options.filter.tolerance_minutes, 0);
}

#[test]
fn journal_export_options_require_equal_nonempty_passwords_and_available_keys() {
    let mut draft = Draft::new(Format::Markdown, 1, Vec::new());
    edit(&mut draft, Event::Protection(ProtectionMode::Password));
    assert!(draft.choice().is_none());
    let change = Event::Password(Password::new("example-secret".into()));
    assert!(!format!("{change:?}").contains("example-secret"));
    edit(&mut draft, change);
    edit(&mut draft, Event::Repeat(Password::new("different".into())));
    assert!(draft.choice().is_none());
    edit(
        &mut draft,
        Event::Repeat(Password::new("example-secret".into())),
    );
    assert!(matches!(draft.choice().unwrap().protection,
        ProtectionChoice::Password(password) if password.as_str() == "example-secret"));

    edit(
        &mut draft,
        Event::Password(Password::new("x".repeat(MAX_PASSWORD_BYTES + 1))),
    );
    assert!(draft.choice().is_none());
    edit(&mut draft, Event::Protection(ProtectionMode::OpenPgp));
    assert!(draft.choice().is_none());
    edit(&mut draft, Event::Protection(ProtectionMode::Password));
    assert!(draft.password.as_str().is_empty());
    assert!(draft.repeat.as_str().is_empty());
}

#[test]
fn journal_export_options_apply_current_regex_marker_filter_and_discard_stale_preview() {
    let fixture = Fixture::new();
    let snapshots = fixture.snapshots();
    let mut draft = Draft::new(Format::Markdown, 1, Vec::new());
    let _ = draft.set_preview(Ok(Arc::clone(&snapshots)));
    let old = draft.preview_job.as_ref().unwrap();
    let old_revision = old.revision;
    let old_count = count(Some(&snapshots), &draft.filter().unwrap(), &old.cancelled);
    let old_cancelled = Arc::clone(&old.cancelled);
    assert_eq!(draft.markers, [("important".into(), "Important".into())]);

    edit(&mut draft, Event::MarkerMode(MarkerMode::Selected));
    assert_eq!(draft.filter().unwrap().marker_mode, MarkerMode::All);
    edit(&mut draft, Event::Marker("unknown".into(), true));
    assert!(draft.marker_ids.is_empty());
    edit(&mut draft, Event::Marker("important".into(), true));
    edit(&mut draft, Event::TopicRegex(true));
    edit(&mut draft, Event::Topic("[".into()));
    assert!(old_cancelled.load(Ordering::Relaxed));
    assert!(draft.choice().is_none());

    // Test double for the superseded Iced worker delivery, with actual old counts.
    edit(&mut draft, Event::Counted(old_revision, old_count));
    assert!(draft.preview.is_none());
    finish_count(&mut draft, Some(&snapshots));
    assert!(matches!(draft.preview, Some(Counted::InvalidFilter)));
    assert!(draft.choice().is_none());

    edit(&mut draft, Event::Topic("^SYSTEM".into()));
    finish_count(&mut draft, Some(&snapshots));
    assert!(matches!(
        draft.preview,
        Some(Counted::Entries {
            kept: 0,
            total: 2,
            ..
        })
    ));
    assert!(draft.choice().is_none());

    edit(&mut draft, Event::Topic("^APACHE".into()));
    finish_count(&mut draft, Some(&snapshots));
    assert!(matches!(
        draft.preview,
        Some(Counted::Entries {
            kept: 1,
            total: 2,
            ..
        })
    ));
    let choice = draft.choice().unwrap();
    assert_eq!(choice.options.filter.marker_mode, MarkerMode::Selected);
    assert_eq!(
        snapshots[0]
            .preview(&choice.options.filter)
            .unwrap()
            .kept_entries,
        1
    );
}

#[test]
fn journal_export_options_ai_topic_requires_availability_and_nonblank_input() {
    let mut draft = Draft::new(Format::Pdf, 1, Vec::new());
    edit(&mut draft, Event::Topic("certificate rotation".into()));
    edit(&mut draft, Event::TopicAi(true));
    assert!(!draft.topic_ai);
    assert!(!draft.choice().unwrap().ai_topic);

    draft.set_topic_ai_available(true);
    assert!(!draft.choice().unwrap().ai_topic);
    edit(&mut draft, Event::TopicAi(true));
    assert!(draft.choice().unwrap().ai_topic);
    edit(&mut draft, Event::Topic(" \t ".into()));
    let choice = draft.choice().unwrap();
    assert!(!choice.ai_topic);
    assert!(!choice.options.filter.is_active());
}

#[test]
fn journal_export_options_ai_topic_ignores_literal_preview_and_stale_regex() {
    let fixture = Fixture::new();
    let snapshots = fixture.snapshots();
    let mut draft = Draft::new(Format::Pdf, 1, Vec::new());
    draft.set_topic_ai_available(true);
    let _ = draft.set_preview(Ok(Arc::clone(&snapshots)));
    finish_count(&mut draft, Some(&snapshots));
    edit(&mut draft, Event::MarkerMode(MarkerMode::Selected));
    edit(&mut draft, Event::Marker("important".into(), true));
    edit(&mut draft, Event::Topic("certificate rotation".into()));
    // The first worker was invalidated by the later marker and topic edits.
    finish_count(&mut draft, Some(&snapshots));
    finish_count(&mut draft, Some(&snapshots));
    assert!(matches!(
        draft.preview,
        Some(Counted::Entries { kept: 0, .. })
    ));
    assert!(draft.choice().is_none());

    edit(&mut draft, Event::Topic("TLS [certificate]".into()));
    let previous = draft.preview_job.as_ref().unwrap();
    let revision = previous.revision;
    let cancelled = Arc::clone(&previous.cancelled);
    let result = count(Some(&snapshots), &draft.filter().unwrap(), &cancelled);
    edit(&mut draft, Event::TopicRegex(true));
    edit(&mut draft, Event::TopicAi(true));
    assert!(cancelled.load(Ordering::Relaxed));
    // Test double for Iced delivering the previously computed literal count after the mode change.
    edit(&mut draft, Event::Counted(revision, result));
    edit(&mut draft, Event::TopicRegex(true));
    assert!(draft.preview_job.is_none());
    assert!(draft.preview.is_none());

    let choice = draft.choice().unwrap();
    assert!(choice.ai_topic);
    assert_eq!(choice.options.filter.topic, "TLS [certificate]");
    assert!(!choice.options.filter.topic_regex);
    assert_eq!(choice.options.filter.marker_mode, MarkerMode::Selected);
    assert!(choice.options.filter.marker_ids.contains("important"));
    assert_eq!(
        draft.preview_status(Some(&choice.options.filter)),
        Some(("Determined by the AI during the export".into(), false))
    );

    edit(&mut draft, Event::AddWindow);
    let id = draft.windows[0].id;
    edit(
        &mut draft,
        Event::Bound(id, Bound::FromTime, "12:45".into()),
    );
    let choice = draft.choice().unwrap();
    assert_eq!(
        choice.options.filter.windows[0].from_time,
        Some(Time::new(12, 45, 0, 0).unwrap())
    );
    edit(
        &mut draft,
        Event::Bound(id, Bound::FromTime, "25:00".into()),
    );
    assert!(draft.choice().is_none());

    edit(&mut draft, Event::ClearWindows);
    edit(&mut draft, Event::TopicAi(false));
    finish_count(&mut draft, Some(&snapshots));
    assert!(matches!(
        draft.preview,
        Some(Counted::Entries { kept: 0, .. })
    ));
    assert!(draft.choice().is_none());
}
