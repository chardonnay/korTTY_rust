use std::cell::Cell;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ai_manager::ProfileRef;
use crate::snippet_analysis::model::{Analysis, Finding, Provenance, ReportFormat, Source};
use crate::snippets::{Library, tests::Profile};

fn setup() -> (Profile, Store) {
    let profile = Profile::new();
    let store = Store::new(fs::canonicalize(&profile.0).unwrap());
    (profile, store)
}

fn setup_with_limit(limit: usize) -> (Profile, Store, Arc<AtomicUsize>) {
    let (profile, mut store) = setup();
    let limit = Arc::new(AtomicUsize::new(limit));
    store.retention_limit = Some(Arc::clone(&limit));
    (profile, store, limit)
}

fn record(text: &str) -> Record {
    Record::new(
        Purpose::Analysis,
        None,
        Source::new(text, "Bash", "de", "en", "Health check").unwrap(),
        Provenance {
            profile_id: ProfileRef::Numeric(12),
            profile_name: "Test profile".into(),
            model: "Test model".into(),
            duration_millis: 250,
            ..Provenance::default()
        },
        Analysis {
            summary: "Use safer arguments.".into(),
            improvements: vec![Finding {
                id: "I1".into(),
                title: "Quote the argument".into(),
                line: Some(1),
                ..Finding::default()
            }],
            ..Analysis::default()
        },
    )
    .unwrap()
}

#[test]
fn snippet_history_preserves_numeric_wire_and_requires_exact_named_apply_provenance() {
    use crate::snippet_analysis::model::Selection;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let mut before = record("echo old\n");
    assert_eq!(
        serde_json::to_value(&before.provenance).unwrap()["profileId"],
        12
    );
    assert_eq!(
        serde_json::from_str::<Provenance>("{}").unwrap(),
        Provenance::default()
    );
    let reference = ProfileRef::Policy("policy-SnippetExample".into());
    before.provenance.profile_id = reference.clone();
    let source_hash = before.source.sha256.clone();
    let pre_id = before.id;
    store.append(subject, before).unwrap();
    let mut apply = ApplyRun::new(
        "echo old\n".into(),
        "echo updated\n".into(),
        Selection {
            findings: vec!["I1".into()],
            ..Selection::default()
        },
        Vec::new(),
        1,
    )
    .unwrap();
    apply.provenance.profile_id = ProfileRef::Policy("policy-snippetexample".into());
    assert!(
        store
            .append_apply_while(subject, pre_id, &source_hash, apply.clone(), || true)
            .is_err()
    );
    apply.provenance.profile_id = reference.clone();
    store
        .append_apply_while(subject, pre_id, &source_hash, apply, || true)
        .unwrap();
    let reopened = store.load(subject).unwrap();
    let record = reopened.current().unwrap();
    assert_eq!(record.provenance.profile_id, reference);
    assert_eq!(record.apply_runs[0].provenance.profile_id, reference);
    assert_eq!(
        serde_json::to_value(record).unwrap()["applyRuns"][0]["provenance"]["profileId"],
        "policy-SnippetExample"
    );
}

#[test]
fn report_export_receipts_are_bounded_and_require_the_saved_source_and_basename() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let before = record("old source\n");
    let id = before.id;
    let hash = &before.source.sha256;
    store.append(subject, before.clone()).unwrap();
    assert!(
        store
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .exports
            .is_empty()
    );

    let receipt = |name: &str| ExportEntry::new(ReportFormat::Html, None, name);
    assert!(
        store
            .record_export(subject, id, hash, receipt("private/report.html"))
            .is_err()
    );
    assert!(
        store
            .record_export(subject, id, "stale", receipt("report.html"))
            .is_err()
    );
    let missing = ExportEntry::new(
        ReportFormat::Html,
        Some(SnippetId::new().unwrap()),
        "report.html",
    );
    assert!(store.record_export(subject, id, hash, missing).is_err());
    assert!(
        store
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .exports
            .is_empty()
    );

    store
        .update(subject, |history| {
            record_mut(history, id)?.exports = (0..MAX_EXPORT_ENTRIES)
                .map(|index| receipt(&format!("report-{index}.html")))
                .collect();
            Ok(true)
        })
        .unwrap();
    store
        .record_export(subject, id, hash, receipt("new.html"))
        .unwrap();
    let loaded = store.load(subject).unwrap();
    let latest = loaded.current().unwrap();
    assert_eq!(latest.exports.len(), MAX_EXPORT_ENTRIES);
    assert_eq!(latest.exports[0].file_name, "report-1.html");
    assert_eq!(latest.exports.last().unwrap().file_name, "new.html");
    assert_eq!(latest.updated_at, before.updated_at);
}

#[test]
fn history_round_trip_deduplicates_source_blobs_and_retains_full_hash() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let first = record("first\r\nsecond\n");
    let second = record("first\r\nsecond\n");
    assert!(store.load(subject).unwrap().records.is_empty());
    assert!(!store.profile.join(DIRECTORY_NAME).exists());

    store.append(subject, first.clone()).unwrap();
    let expected = store.append(subject, second.clone()).unwrap();
    let loaded = Store::new(store.profile.clone()).load(subject).unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.revision, 2);
    assert_eq!(loaded.current(), Some(&second));
    assert_eq!(loaded.records[1], first);
    assert_eq!(second.source.line_count, 2);
    assert!(Arc::ptr_eq(
        loaded.records[0].source.content.as_ref().unwrap(),
        loaded.records[1].source.content.as_ref().unwrap(),
    ));
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let bytes = fs::read(path).unwrap();
    let document: Document = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document.blobs.len(), 1);
    assert!(!format!("{second:?}").contains("first\\r\\nsecond"));

    let oversized = "é".repeat(super::super::model::DEFAULT_CONTENT_BYTES / 2 + 1);
    let third = record(&oversized);
    assert!(third.source.content.is_none());
    assert!(third.source.content_truncated);
    assert_eq!(third.source.sha256, content_hash(&oversized).unwrap());
    store.append(subject, third.clone()).unwrap();
    assert_eq!(store.load(subject).unwrap().current(), Some(&third));
}

#[test]
fn managed_source_limit_counts_utf8_and_removes_legacy_content_on_write() {
    let (_profile, store, limit) = setup_with_limit(4);
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let short = record("a");
    let exact = record("éé");
    let over = record("ééa");
    for entry in [&short, &exact, &over] {
        store.append(subject, entry.clone()).unwrap();
    }
    let loaded = store.load(subject).unwrap();
    assert_eq!(loaded.records[1], exact);
    assert_eq!(loaded.records[2], short);
    assert_eq!(loaded.records[0].analysis, over.analysis);
    assert_eq!(loaded.records[0].source.sha256, over.source.sha256);
    assert_eq!(loaded.records[0].source.line_count, over.source.line_count);
    assert!(loaded.records[0].source.content_truncated);
    assert!(loaded.records[0].source.content.is_none());

    limit.store(0, Ordering::Release);
    assert_eq!(store.load(subject).unwrap(), loaded);
    let scrubbed = store.set_pinned(subject, exact.id, true).unwrap();
    assert!(scrubbed.records[1].pinned);
    assert_eq!(scrubbed.revision, loaded.revision + 1);
    for (before, after) in loaded.records.iter().zip(&scrubbed.records) {
        assert_eq!(after.analysis, before.analysis);
        assert_eq!(after.source.sha256, before.source.sha256);
        assert_eq!(after.source.line_count, before.source.line_count);
        assert!(after.source.content_truncated);
        assert!(after.source.content.is_none());
    }
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let on_disk: Document = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(on_disk.blobs.is_empty());
    assert_eq!(store.load(subject).unwrap(), scrubbed);

    limit.store(MAX_CONTENT_BYTES, Ordering::Release);
    let expanded = store.set_pinned(subject, exact.id, false).unwrap();
    assert!(
        expanded
            .records
            .iter()
            .all(|entry| entry.source.content.is_none())
    );
}

#[test]
fn a_tighter_policy_during_publication_leaves_the_previous_history_intact() {
    let (_profile, store, limit) = setup_with_limit(MAX_CONTENT_BYTES);
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let previous = store.append(subject, record("legacy source")).unwrap();
    let next = record("new source");
    let checks = Cell::new(0);
    let error = store
        .update_while(
            subject,
            || {
                if checks.replace(checks.get() + 1) == 1 {
                    limit.store(0, Ordering::Release);
                }
                true
            },
            |history| insert_record(history, next.clone()),
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(store.load(subject).unwrap(), previous);
    let committed = store.clone().append(subject, next).unwrap();
    assert_eq!(committed.records.len(), 2);
    assert!(
        committed
            .records
            .iter()
            .all(|entry| entry.source.content.is_none())
    );
}

#[test]
fn apply_retention_keeps_hashes_and_prevents_omitted_source_from_being_accepted() {
    use crate::snippet_analysis::model::{ChangeReason, Selection};

    let (_profile, store, limit) = setup_with_limit(MAX_CONTENT_BYTES);
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let before = record("a");
    store.append(subject, before.clone()).unwrap();
    let proposed = ApplyRun::new(
        "a".into(),
        "éé".into(),
        Selection {
            findings: vec!["I1".into()],
            ..Selection::default()
        },
        vec![ChangeReason {
            id: "I1".into(),
            reason: "Quote the argument".into(),
            anchor: None,
        }],
        1,
    )
    .unwrap();
    store
        .append_apply_while(
            subject,
            before.id,
            &before.source.sha256,
            proposed.clone(),
            || true,
        )
        .unwrap();
    assert!(store.load(subject).unwrap().records[0].apply_runs[0].has_review_source());

    limit.store(1, Ordering::Release);
    let saved = store.set_pinned(subject, before.id, true).unwrap();
    let run = &saved.records[0].apply_runs[0];
    assert_eq!(run.before, "a");
    assert!(run.after.is_empty());
    assert!(!run.has_review_source());
    assert_eq!(run.before_sha256, proposed.before_sha256);
    assert_eq!(run.after_sha256, proposed.after_sha256);
    assert_eq!(run.reasons, proposed.reasons);
    assert_eq!(store.load(subject).unwrap(), saved);
    assert_eq!(
        store
            .decide_apply_while(
                subject,
                before.id,
                proposed.id,
                &proposed.after_sha256,
                ApplyStatus::Accepted,
                || true
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(store.load(subject).unwrap(), saved);

    limit.store(0, Ordering::Release);
    let rejected = store
        .decide_apply_while(
            subject,
            before.id,
            proposed.id,
            &proposed.after_sha256,
            ApplyStatus::Rejected,
            || true,
        )
        .unwrap();
    let run = &rejected.records[0].apply_runs[0];
    assert_eq!(run.status, ApplyStatus::Rejected);
    assert!(run.before.is_empty());
    assert!(run.after.is_empty());
    assert_eq!(run.reasons, proposed.reasons);
    assert_eq!(store.load(subject).unwrap(), rejected);
}

#[test]
fn zero_cap_preserves_live_review_and_acceptance_without_retaining_source() {
    use crate::ai_manager::CodeTextLanguage;
    use crate::snippet_analysis::model::hardening::{HeaderRef, Requirement};
    use crate::snippet_analysis::model::{ChangeReason, Checkpoint, Selection};
    use crate::snippet_analysis::request::{Snapshot, apply::Request};

    let (_profile, store, limit) = setup_with_limit(MAX_CONTENT_BYTES);
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let original = "echo unsafe";
    let header = "# exact fixture header";
    let improved = "# exact fixture header\necho improved";
    let before = record(original);
    store.append(subject, before.clone()).unwrap();
    let snapshot = Snapshot::snippet("Health check".into(), "Bash".into(), original.into());
    let request = Request::new(
        snapshot.clone(),
        &before.analysis,
        Selection {
            findings: vec!["I1".into()],
            requirements: vec![
                Requirement::resolved_header(
                    HeaderRef::User(SnippetId::new().unwrap()),
                    header,
                    None,
                )
                .unwrap(),
            ],
            ..Selection::default()
        },
        String::new(),
        String::new(),
        "en",
        CodeTextLanguage::English,
    )
    .unwrap();
    let mut recovery = ApplyRecovery {
        revision: SnippetId::new().unwrap(),
        started_at: 1,
        elapsed_millis: 0,
        profile_name: before.provenance.profile_name.clone(),
        model: before.provenance.model.clone(),
        request: request.recovery_snapshot().unwrap(),
        checkpoint: None,
    };
    let save = |previous, recovery| {
        store.save_apply_recovery_while(
            subject,
            before.id,
            &before.source.sha256,
            previous,
            recovery,
            || true,
        )
    };
    limit.store(0, Ordering::Release);
    save(None, recovery.clone()).unwrap();
    let previous = recovery.revision;
    recovery.revision = SnippetId::new().unwrap();
    let checkpoint = Arc::new(Checkpoint {
        code: improved.into(),
        code_omitted: false,
        code_sha256: content_hash(improved).unwrap(),
        reasons: ["I1", "SCRIPT-HEADER"]
            .into_iter()
            .map(|id| ChangeReason {
                id: id.into(),
                reason: "Use the reviewed change".into(),
                anchor: (id == "SCRIPT-HEADER").then(|| header.into()),
            })
            .collect(),
        completed: 1,
    });
    recovery.checkpoint = Some(Arc::clone(&checkpoint));
    let saved = save(Some(previous), recovery.clone()).unwrap();
    let point = saved.records[0].apply_recovery.as_ref().unwrap();
    let omitted = point.checkpoint.as_ref().unwrap();
    assert!(!omitted.has_source());
    assert!(omitted.code.is_empty());
    assert_eq!(omitted.code_sha256, checkpoint.code_sha256);
    assert_eq!(omitted.reasons.len(), checkpoint.reasons.len());
    assert_eq!(omitted.reasons[1].reason, checkpoint.reasons[1].reason);
    assert!(omitted.reasons[1].anchor.is_none());
    assert_eq!(omitted.completed(), checkpoint.completed());
    assert!(!point.request.selection.has_required_source());
    assert!(Request::from_recovery(snapshot, &before.analysis, point).is_err());
    assert!(request.preview_from(Some(omitted)).is_err());
    assert!(request.completed(omitted, 1).is_err());
    assert_eq!(store.load(subject).unwrap(), saved);
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let check_disk = || {
        let raw = fs::read_to_string(&path).unwrap();
        let document: Document = serde_json::from_str(&raw).unwrap();
        assert!(document.blobs.is_empty());
        for omitted in [original, "echo improved", header] {
            assert!(!raw.contains(omitted));
        }
    };
    check_disk();

    let mut live = request.completed(&checkpoint, 1).unwrap();
    live.id = recovery.revision;
    live.provenance = before.provenance.clone();
    live.provenance.duration_millis = 0;
    assert_eq!(live.after, improved);
    let proposed = store
        .append_apply_recovery_while(
            subject,
            before.id,
            &before.source.sha256,
            recovery.revision,
            live.clone(),
            || true,
        )
        .unwrap();
    let receipt = &proposed.records[0].apply_runs[0];
    assert!(receipt.matches_live(&live));
    assert!(!receipt.has_review_source());
    assert!(
        store
            .decide_apply_while(
                subject,
                before.id,
                live.id,
                &live.after_sha256,
                ApplyStatus::Accepted,
                || true
            )
            .is_err()
    );
    let mut altered = live.clone();
    altered.after.push_str("\necho unexpected");
    assert!(
        store
            .decide_reviewed_apply_while(
                subject,
                before.id,
                &altered,
                ApplyStatus::Accepted,
                || true
            )
            .is_err()
    );
    let accepted = store
        .decide_reviewed_apply_while(subject, before.id, &live, ApplyStatus::Accepted, || true)
        .unwrap();
    let receipt = &accepted.records[0].apply_runs[0];
    assert_eq!(receipt.status, ApplyStatus::Accepted);
    assert_eq!(receipt.after_sha256, live.after_sha256);
    assert!(!receipt.has_review_source());
    assert_eq!(store.load(subject).unwrap(), accepted);
    check_disk();
}

#[test]
fn staged_apply_recovery_reopens_and_atomically_converts_the_latest_checkpoint() {
    use crate::ai_manager::CodeTextLanguage;
    use crate::snippet_analysis::model::{ChangeReason, Checkpoint, Selection};
    use crate::snippet_analysis::request::{Snapshot, apply::Request};

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let original = "echo unsafe\r\n";
    let intermediate = "printf '%s' safe\r\n";
    let finished = "printf '%s\\n' safe\r\n";
    let snapshot =
        |text: &str| Snapshot::snippet("Health check".into(), "Bash".into(), text.into());
    let mut pre = record(original);
    for index in 2..=4 {
        pre.analysis.improvements.push(Finding {
            id: format!("I{index}"),
            title: format!("Finding {index}"),
            ..Finding::default()
        });
    }
    store.append(subject, pre.clone()).unwrap();
    let request = Request::new(
        snapshot(original),
        &pre.analysis,
        Selection {
            findings: (1..=4).map(|index| format!("I{index}")).collect(),
            ..Selection::default()
        },
        "Keep the final newline".into(),
        "Use the health-check conventions".into(),
        "de",
        CodeTextLanguage::English,
    )
    .unwrap();
    let mut recovery = ApplyRecovery {
        revision: SnippetId::new().unwrap(),
        started_at: 1,
        elapsed_millis: 0,
        profile_name: pre.provenance.profile_name.clone(),
        model: pre.provenance.model.clone(),
        request: request.recovery_snapshot().unwrap(),
        checkpoint: None,
    };
    let initial = recovery.revision;
    let save = |previous, recovery: &ApplyRecovery| {
        store.save_apply_recovery_while(
            subject,
            pre.id,
            &pre.source.sha256,
            previous,
            recovery.clone(),
            || true,
        )
    };
    let checkpoint = |code: &str, completed, items| {
        Arc::new(Checkpoint {
            code: code.into(),
            code_omitted: false,
            code_sha256: content_hash(code).unwrap(),
            reasons: (1..=items)
                .map(|index| ChangeReason {
                    id: format!("I{index}"),
                    reason: format!("Change {index}"),
                    anchor: None,
                })
                .collect(),
            completed,
        })
    };
    save(None, &recovery).unwrap();
    let mut skipped = recovery.clone();
    skipped.revision = SnippetId::new().unwrap();
    skipped.checkpoint = Some(checkpoint(finished, 2, 4));
    assert!(save(Some(initial), &skipped).is_err());

    recovery.revision = SnippetId::new().unwrap();
    recovery.elapsed_millis = 20;
    recovery.checkpoint = Some(checkpoint(intermediate, 1, 3));
    let first_stage = recovery.revision;
    let history = save(Some(initial), &recovery).unwrap();
    assert_eq!(
        save(Some(initial), &recovery).unwrap().revision,
        history.revision
    );
    let reopened = Store::new(store.profile.clone());
    let history = reopened.load(subject).unwrap();
    let saved = history.current().unwrap().apply_recovery.as_ref().unwrap();
    assert_eq!(saved, &recovery);
    assert_eq!(saved.request.instructions(), "Keep the final newline");
    let document: serde_json::Value = serde_json::from_slice(
        &fs::read(
            store
                .profile
                .join(DIRECTORY_NAME)
                .join(file_name(subject).unwrap()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        document["blobs"][content_hash(intermediate).unwrap()],
        intermediate
    );
    assert!(
        document["history"]["records"][0]["applyRecovery"]["checkpoint"]
            .get("code")
            .is_none()
    );
    assert!(Request::from_recovery(snapshot("echo changed"), &pre.analysis, saved).is_err());
    let mut changed_plan = pre.analysis.clone();
    changed_plan.improvements[3].title = "A changed finding".into();
    assert!(Request::from_recovery(snapshot(original), &changed_plan, saved).is_err());
    let restored = Request::from_recovery(snapshot(original), &pre.analysis, saved).unwrap();
    let preview = restored.preview_from(saved.checkpoint.as_deref()).unwrap();
    assert!(preview.contains("Stage 2 user prompt") && preview.contains(intermediate.trim()));
    assert!(preview.contains("I4") && !preview.contains("I3"));

    recovery.revision = SnippetId::new().unwrap();
    recovery.elapsed_millis = 40;
    recovery.checkpoint = Some(checkpoint(finished, 2, 4));
    assert!(save(Some(initial), &recovery).is_err());
    assert!(
        reopened
            .remove_apply_recovery(subject, pre.id, initial)
            .is_err()
    );
    save(Some(first_stage), &recovery).unwrap();
    let history = reopened.load(subject).unwrap();
    let saved = history.current().unwrap().apply_recovery.as_ref().unwrap();
    let restored = Request::from_recovery(snapshot(original), &pre.analysis, saved).unwrap();
    let mut proposal = restored
        .completed(saved.checkpoint.as_deref().unwrap(), saved.started_at)
        .unwrap();
    proposal.id = saved.revision;
    proposal.provenance = Provenance {
        additional_instructions: saved.request.instructions().into(),
        duration_millis: saved.elapsed_millis,
        ..pre.provenance.clone()
    };
    let publish = |revision, run| {
        reopened.append_apply_recovery_while(
            subject,
            pre.id,
            &pre.source.sha256,
            revision,
            run,
            || true,
        )
    };
    assert!(publish(first_stage, proposal.clone()).is_err());
    let mut different = proposal.clone();
    different.reasons[0].reason = "Unapproved change".into();
    assert!(publish(saved.revision, different).is_err());
    let published = publish(saved.revision, proposal.clone()).unwrap();
    assert_eq!(
        publish(saved.revision, proposal.clone()).unwrap(),
        published
    );
    let actual = reopened.load(subject).unwrap();
    let record = actual.current().unwrap();
    assert!(record.apply_recovery.is_none());
    assert_eq!(record.apply_runs, vec![proposal]);
}

#[test]
fn saved_flowchart_updates_only_the_named_record_and_preserves_source_navigation() {
    use crate::diagram_window::Chart;
    use kortty_diagram::{SourceRange, parse_mermaid};
    use std::collections::HashMap;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let old = record("echo old\necho done\n");
    let new = record("echo new");
    store.append(subject, old.clone()).unwrap();
    store.append(subject, new.clone()).unwrap();
    let chart = Chart::stored(
        parse_mermaid("flowchart TD\nstart[Start] --> endNode[Finish]").unwrap(),
        HashMap::from([("start".into(), SourceRange::new(1, 2).unwrap())]),
        false,
        None,
    );
    let saved = AnalysisDiagram::capture(&chart, old.source.line_count).unwrap();
    for (source_hash, allowed) in [
        (new.source.sha256.as_str(), true),
        (old.source.sha256.as_str(), false),
    ] {
        assert!(
            store
                .set_diagram_while(subject, old.id, source_hash, None, saved.clone(), || {
                    allowed
                })
                .is_err()
        );
    }
    store
        .set_diagram_while(
            subject,
            old.id,
            &old.source.sha256,
            None,
            saved.clone(),
            || true,
        )
        .unwrap();
    assert!(
        store
            .set_diagram_while(
                subject,
                old.id,
                &old.source.sha256,
                None,
                saved.clone(),
                || true
            )
            .is_err()
    );
    let loaded = Store::new(store.profile.clone()).load(subject).unwrap();
    assert_eq!(loaded.current(), Some(&new));
    let saved = loaded.records[1].diagram.as_ref().unwrap();
    let restored = saved.chart(old.source.line_count, true).unwrap();
    let start = &restored.diagram().nodes()[0];
    assert_eq!(restored.source_range(start), SourceRange::new(1, 2));
    let without_source = saved.chart(old.source.line_count, false).unwrap();
    assert!(without_source.source_range(start).is_none());
    assert_eq!(loaded.revision, 3);
}

#[test]
fn folder_history_survives_rename_and_move_and_is_separate_from_snippet_history() {
    let (profile, store) = setup();
    let mut library = Library::open(profile.path()).unwrap();
    let folder = library.create_folder(None, "Ops").unwrap();
    let destination = library.create_folder(None, "Infrastructure").unwrap();
    let subject = Subject::Folder(folder);
    let first = record("echo folder");
    store.append(subject, first.clone()).unwrap();
    library.rename_folder(folder, "Production").unwrap();
    library.move_folder(folder, Some(destination)).unwrap();
    assert_eq!(
        library.folder(folder).unwrap().path,
        "Infrastructure/Production"
    );

    let folder_key = file_name(subject).unwrap();
    let id: SnippetId = folder_key
        .strip_prefix("folder-")
        .unwrap()
        .strip_suffix(".json")
        .unwrap()
        .to_owned()
        .try_into()
        .unwrap();
    assert!(store.load(Subject::Snippet(id)).unwrap().records.is_empty());
    store
        .append(Subject::Snippet(id), record("echo snippet"))
        .unwrap();
    assert_eq!(store.load(subject).unwrap().current(), Some(&first));
}

#[test]
fn history_mutations_use_latest_revision_preserve_pins_and_allow_idempotent_retry() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let retained = record("oldest");
    let pinned = store.append(subject, retained.clone()).unwrap();
    assert_eq!(pinned.revision, 1);
    let pinned = store.set_pinned(subject, retained.id, true).unwrap();
    assert_eq!(pinned.revision, 2);

    let writes: Vec<_> = (0..RETENTION + 2)
        .map(|index| {
            let store = store.clone();
            std::thread::spawn(move || {
                store
                    .append(subject, record(&format!("run {index}")))
                    .unwrap()
            })
        })
        .collect();
    for write in writes {
        write.join().unwrap();
    }
    let history = store.load(subject).unwrap();
    assert_eq!(history.records.len(), RETENTION);
    assert_eq!(history.revision, (RETENTION + 4) as u64);
    assert!(history.records.last().unwrap().pinned);
    assert_eq!(history.records.last().unwrap().id, retained.id);
    assert_eq!(store.append(subject, retained.clone()).unwrap(), history);
    let updated = store.remove(subject, retained.id).unwrap();
    assert_eq!(updated.revision, history.revision + 1);
    assert!(
        !updated
            .records
            .iter()
            .any(|record| record.id == retained.id)
    );
    assert_eq!(store.remove(subject, retained.id).unwrap(), updated);
}

#[test]
fn apply_decisions_require_the_original_proposal_and_a_current_review() {
    use std::cell::Cell;

    use crate::snippet_analysis::model::Selection;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let pre = record("echo original");
    let initial = store.append(subject, pre.clone()).unwrap();
    let run = ApplyRun::new(
        "echo $updated".into(),
        "echo \"$updated\"".into(),
        Selection {
            findings: vec!["I1".into()],
            ..Selection::default()
        },
        Vec::new(),
        now_millis(),
    )
    .unwrap();
    assert!(
        store
            .append_apply_while(subject, pre.id, &run.before_sha256, run.clone(), || true)
            .is_err()
    );
    assert_eq!(store.load(subject).unwrap(), initial);

    let proposed = store
        .append_apply_while(subject, pre.id, &pre.source.sha256, run.clone(), || true)
        .unwrap();
    let decide =
        |hash, status| store.decide_apply_while(subject, pre.id, run.id, hash, status, || true);
    assert!(decide(&run.before_sha256, ApplyStatus::Accepted).is_err());
    let checked = Cell::new(false);
    let cancelled = store.decide_apply_while(
        subject,
        pre.id,
        run.id,
        &run.after_sha256,
        ApplyStatus::Accepted,
        || !checked.replace(true),
    );
    assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(store.load(subject).unwrap(), proposed);

    let rejected = decide(&run.after_sha256, ApplyStatus::Rejected).unwrap();
    assert_eq!(
        rejected.current().unwrap().apply_runs[0].status,
        ApplyStatus::Rejected
    );
    assert_eq!(
        decide(&run.after_sha256, ApplyStatus::Rejected).unwrap(),
        rejected
    );
    assert!(decide(&run.after_sha256, ApplyStatus::Accepted).is_err());
    assert_eq!(store.load(subject).unwrap(), rejected);
}

#[test]
fn clearing_history_removes_blobs_and_rejects_late_analysis_writes() {
    use std::fs::TryLockError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::sync_channel;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let other = Subject::Snippet(SnippetId::new().unwrap());
    store.clear(subject).unwrap();
    assert!(!store.profile.join(DIRECTORY_NAME).exists());
    let empty_directory = store.directory(true).unwrap();
    store.clear(subject).unwrap();
    assert!(!empty_directory.has_entry(LOCK_NAME).unwrap());
    drop(empty_directory);

    store
        .append(subject, record("echo source to delete"))
        .unwrap();
    store.append(other, record("echo independent")).unwrap();

    let current = Arc::new(AtomicBool::new(true));
    let (guard_entered, wait_for_guard) = sync_channel(0);
    let (resume, wait_for_resume) = sync_channel(0);
    let append = {
        let store = store.clone();
        let current = Arc::clone(&current);
        std::thread::spawn(move || {
            let first_check = AtomicBool::new(true);
            store.append_while(subject, record("echo queued"), || {
                if first_check.swap(false, Ordering::AcqRel) {
                    let still_current = current.load(Ordering::Acquire);
                    guard_entered.send(()).unwrap();
                    wait_for_resume.recv().unwrap();
                    still_current
                } else {
                    current.load(Ordering::Acquire)
                }
            })
        })
    };
    wait_for_guard.recv().unwrap();
    let competing = store
        .directory(false)
        .unwrap()
        .open_lock(LOCK_NAME)
        .unwrap();
    assert!(matches!(
        competing.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    drop(competing);

    current.store(false, Ordering::Release);
    resume.send(()).unwrap();
    store.clear(subject).unwrap();
    assert_eq!(
        append.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );

    let error = store
        .append_while(subject, record("echo late"), || {
            current.load(Ordering::Acquire)
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    assert!(!path.exists());
    assert!(store.load(subject).unwrap().records.is_empty());
    assert_eq!(store.load(other).unwrap().records.len(), 1);
    store.clear(subject).unwrap();
    assert!(!path.exists());
}

#[test]
fn corrupt_future_or_foreign_histories_are_never_replaced() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    store.append(subject, record("echo original")).unwrap();
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let original = fs::read(&path).unwrap();
    let mut foreign: Document = serde_json::from_slice(&original).unwrap();
    foreign.history.subject = Subject::Snippet(SnippetId::new().unwrap());
    let mut future: Document = serde_json::from_slice(&original).unwrap();
    future.schema_version += 1;
    let mut corrupt: Document = serde_json::from_slice(&original).unwrap();
    *corrupt.blobs.values_mut().next().unwrap() = Arc::from("corrupt source");
    for bytes in [
        b"{invalid json".to_vec(),
        serde_json::to_vec(&foreign).unwrap(),
        serde_json::to_vec(&future).unwrap(),
        serde_json::to_vec(&corrupt).unwrap(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(store.load(subject).is_err());
        assert!(store.append(subject, record("echo new")).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn history_rejects_symlinks_and_hard_links() {
    use std::os::unix::fs::symlink;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let directory = store.directory(true).unwrap();
    let external = store.profile.join("external.txt");
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    drop(directory);
    fs::write(&external, b"outside").unwrap();

    symlink(&external, &path).unwrap();
    assert!(store.load(subject).is_err());
    assert!(store.append(subject, record("echo new")).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());

    fs::remove_file(&path).unwrap();
    fs::hard_link(&external, &path).unwrap();
    assert!(store.load(subject).is_err());
    assert!(store.append(subject, record("echo new")).is_err());
    assert_eq!(fs::read(external).unwrap(), b"outside");
}
