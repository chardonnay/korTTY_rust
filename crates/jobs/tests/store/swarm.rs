use kortty_jobs::{
    FinishedRunMetadata, MAX_SWARM_COMPARISON_BYTES, MAX_SWARM_QUESTION_BYTES,
    MAX_SWARM_TARGET_DETAIL_BYTES, MAX_SWARM_TARGET_LABEL_BYTES, MAX_SWARM_TARGETS, ProfileRef,
    RunId, RunRecord, SwarmChatOrigin, SwarmChatSeed, SwarmReport, SwarmTargetReport,
};

use super::*;

const COMPARISON: &str =
    "| Result | Errors |\n| --- | --- |\n| Ready | - |\n| Review *** | No data |";

fn scheduler() -> Scheduler<()> {
    let job = Job::new(id("swarm"), "Scheduled Swarm", ()).unwrap();
    Scheduler::new(JobGraph::new(vec![job]).unwrap())
}

fn finish(scheduler: &mut Scheduler<()>, time: u64, outcome: RunOutcome) -> RunRecord {
    let batch = scheduler.start_manual(&id("swarm"), at(time - 1)).unwrap();
    scheduler
        .complete(batch.started[0].id, outcome, at(time))
        .unwrap()
        .finished
        .remove(0)
}

fn report(outcome: RunOutcome) -> SwarmReport {
    SwarmReport {
        question: "Inspect the reviewed hosts · ***".into(),
        targets: vec![
            SwarmTargetReport {
                target: 74,
                label: "Second saved host".into(),
                outcome: RunOutcome::Success,
                detail: "The filesystem is accessible.".into(),
            },
            SwarmTargetReport {
                target: 16,
                label: "First saved host".into(),
                outcome,
                detail: "Masked target advice.".into(),
            },
        ],
        comparison: None,
    }
}

fn extend_report(report: &mut SwarmReport, total: usize) {
    let template = report.targets[0].clone();
    report
        .targets
        .extend(
            (100..)
                .take(total - report.targets.len())
                .map(|target| SwarmTargetReport {
                    target,
                    ..template.clone()
                }),
        );
}

fn commit(
    store: &mut SchedulerStore,
    scheduler: &Scheduler<()>,
    record: &RunRecord,
    report: SwarmReport,
) -> Result<(), StoreError> {
    store.commit_records_with_swarm_reports(
        scheduler.graph(),
        std::slice::from_ref(record),
        &[],
        &[],
        &[(record.id, report)],
    )
}

fn origin(value: u64) -> SwarmChatOrigin {
    SwarmChatOrigin {
        nonce: format!("{value:032x}"),
        profile: ProfileRef::Numeric(91),
    }
}

fn commit_chat(
    store: &mut SchedulerStore,
    scheduler: &Scheduler<()>,
    record: &RunRecord,
    report: SwarmReport,
    origin: SwarmChatOrigin,
) -> Result<(), StoreError> {
    store.commit_records_with_swarm_chat_origins(
        scheduler.graph(),
        std::slice::from_ref(record),
        &[],
        &[],
        &[(record.id, report)],
        &[(record.id, origin)],
    )
}

#[test]
fn single_target_swarm_report_and_chat_seed_survive_reopen() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let record = finish(&mut scheduler, 2, RunOutcome::Success);
    let mut singleton = report(RunOutcome::Success);
    singleton.targets.truncate(1);
    let origin = origin(19);
    let mut store = SchedulerStore::open(&path).unwrap();
    commit_chat(
        &mut store,
        &scheduler,
        &record,
        singleton.clone(),
        origin.clone(),
    )
    .unwrap();
    drop(store);

    let store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal_swarm_report(record.id), Some(&singleton));
    assert_eq!(store.journal_swarm_chat_origin(record.id), Some(&origin));
    assert_eq!(
        store.pending_swarm_chat_seed(record.id),
        Some(&SwarmChatSeed {
            run: record.id,
            origin,
            report: singleton,
        })
    );
}

#[test]
fn swarm_chat_seed_survives_reopen_and_report_removal_until_exact_acknowledgement() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let record = finish(&mut scheduler, 2, RunOutcome::Success);
    let mut report = report(RunOutcome::Success);
    report.comparison = Some(COMPARISON.into());
    let seed = SwarmChatSeed {
        run: record.id,
        origin: SwarmChatOrigin {
            profile: ProfileRef::Policy("policy-SwarmFixture".into()),
            ..origin(21)
        },
        report,
    };
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_with_actions(scheduler.graph(), &[], &[])
        .unwrap();
    commit_chat(
        &mut store,
        &scheduler,
        &record,
        seed.report.clone(),
        seed.origin.clone(),
    )
    .unwrap();
    assert_eq!(
        store.journal_swarm_chat_origin(record.id),
        Some(&seed.origin)
    );
    assert_eq!(store.next_swarm_chat_seed(|_| true), Some(&seed));
    assert_eq!(
        serde_json::to_value(&seed.origin).unwrap()["profile"],
        "policy-SwarmFixture"
    );
    let initial = fs::read(&path).unwrap();
    let mut changed = seed.report.clone();
    changed.comparison = None;
    assert!(matches!(
        commit_chat(&mut store, &scheduler, &record, changed, seed.origin.clone()),
        Err(StoreError::ConflictingRecord(run)) if run == record.id
    ));
    let mut changed_origin = seed.origin.clone();
    changed_origin.profile = ProfileRef::Policy("policy-swarmfixture".into());
    assert!(matches!(
        commit_chat(&mut store, &scheduler, &record, seed.report.clone(), changed_origin),
        Err(StoreError::ConflictingRecord(run)) if run == record.id
    ));
    assert_eq!(fs::read(&path).unwrap(), initial);
    assert!(
        !store
            .acknowledge_swarm_chat_seed(&origin(22).nonce)
            .unwrap()
    );
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.next_swarm_chat_seed(|_| true), Some(&seed));
    assert_eq!(store.journal_swarm_report(record.id), Some(&seed.report));
    assert_eq!(store.delete_records(&[record.id]).unwrap(), 1);
    assert!(store.journal_swarm_report(record.id).is_none());
    assert!(store.journal_swarm_chat_origin(record.id).is_none());
    assert_eq!(store.next_swarm_chat_seed(|_| true), Some(&seed));
    let native = fs::read(&path).unwrap();
    let portable = SchedulerStore::portable_backup(&native).unwrap();
    assert!(!String::from_utf8_lossy(&portable).contains("swarm_chat_seeds"));
    assert!(!String::from_utf8_lossy(&portable).contains("Masked target advice"));
    assert!(!String::from_utf8_lossy(&portable).contains("Review ***"));
    SchedulerStore::validate_backup::<()>(&portable, |()| true).unwrap();
    assert!(matches!(
        SchedulerStore::validate_backup::<()>(&native, |()| true),
        Err(StoreError::InvalidRecord)
    ));
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.next_swarm_chat_seed(|_| true), Some(&seed));
    assert!(
        store
            .acknowledge_swarm_chat_seed(&seed.origin.nonce)
            .unwrap()
    );
    assert!(
        !store
            .acknowledge_swarm_chat_seed(&seed.origin.nonce)
            .unwrap()
    );
    assert!(store.next_swarm_chat_seed(|_| true).is_none());
    drop(store);
    assert!(
        SchedulerStore::open(&path)
            .unwrap()
            .next_swarm_chat_seed(|_| true)
            .is_none()
    );
}

#[test]
fn swarm_chat_origin_is_immutable_and_retry_does_not_requeue_a_delivered_seed() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let mut store = SchedulerStore::open(&path).unwrap();
    let report = report(RunOutcome::Success);
    let first = finish(&mut scheduler, 2, RunOutcome::Success);
    let first_origin = origin(21);
    assert_eq!(serde_json::to_value(&first_origin).unwrap()["profile"], 91);
    for _ in 0..2 {
        commit_chat(
            &mut store,
            &scheduler,
            &first,
            report.clone(),
            first_origin.clone(),
        )
        .unwrap();
    }
    assert!(
        store
            .acknowledge_swarm_chat_seed(&first_origin.nonce)
            .unwrap()
    );
    commit_chat(
        &mut store,
        &scheduler,
        &first,
        report.clone(),
        first_origin.clone(),
    )
    .unwrap();
    assert!(store.next_swarm_chat_seed(|_| true).is_none());
    assert_eq!(
        store.journal_swarm_chat_origin(first.id),
        Some(&first_origin)
    );

    assert!(matches!(
        commit_chat(&mut store, &scheduler, &first, report.clone(), origin(22)),
        Err(StoreError::ConflictingRecord(run)) if run == first.id
    ));
    let second = finish(&mut scheduler, 3, RunOutcome::Success);
    assert!(matches!(
        commit_chat(
            &mut store,
            &scheduler,
            &second,
            report.clone(),
            first_origin
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert!(store.journal_record(second.id).is_none());
    commit(&mut store, &scheduler, &second, report.clone()).unwrap();
    assert!(matches!(
        commit_chat(&mut store, &scheduler, &second, report, origin(22)),
        Err(StoreError::ConflictingRecord(run)) if run == second.id
    ));
    assert!(store.journal_swarm_chat_origin(second.id).is_none());
}

#[test]
fn swarm_chat_outbox_capacity_preserves_reports_and_origin_for_manual_open() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let mut store = SchedulerStore::open(&path).unwrap();
    let records: Vec<_> = (2..35)
        .map(|time| finish(&mut scheduler, time, RunOutcome::Success))
        .collect();
    let reports: Vec<_> = records
        .iter()
        .map(|record| (record.id, report(RunOutcome::Success)))
        .collect();
    let origins: Vec<_> = records
        .iter()
        .map(|record| (record.id, origin(record.id.value())))
        .collect();
    store
        .commit_records_with_swarm_chat_origins(
            scheduler.graph(),
            &records,
            &[],
            &[],
            &reports,
            &origins,
        )
        .unwrap();
    let overflow = records.last().unwrap().id;
    assert_eq!(
        store.journal_swarm_report(overflow),
        Some(&reports.last().unwrap().1)
    );
    assert_eq!(
        store.journal_swarm_chat_origin(overflow),
        Some(&origins.last().unwrap().1)
    );
    assert!(store.pending_swarm_chat_seed(overflow).is_none());
    assert_eq!(
        store.next_swarm_chat_seed(|_| true).unwrap().run,
        records[0].id
    );
    assert_eq!(
        store
            .next_swarm_chat_seed(|seed| seed.run != records[0].id)
            .unwrap()
            .run,
        records[1].id
    );
    assert_eq!(store.prune_before(at(35)).unwrap(), records.len());
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    for record in &records[..32] {
        let seed = store.next_swarm_chat_seed(|_| true).unwrap();
        assert_eq!(seed.run, record.id);
        let nonce = seed.origin.nonce.clone();
        assert!(store.acknowledge_swarm_chat_seed(&nonce).unwrap());
    }
    assert!(store.next_swarm_chat_seed(|_| true).is_none());
}

#[test]
fn wide_swarm_report_and_journal_links_round_trip_in_reviewed_target_order() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let record = finish(&mut scheduler, 2, RunOutcome::Success);
    let mut report = report(RunOutcome::Success);
    extend_report(&mut report, MAX_SWARM_TARGETS);
    let journal_ids = (0..MAX_SWARM_TARGETS)
        .rev()
        .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
        .collect::<Vec<_>>();
    let origin = origin(42);
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_finished_runs(
            scheduler.graph(),
            std::slice::from_ref(&record),
            FinishedRunMetadata {
                session_journals: &[(record.id, &journal_ids)],
                swarm_reports: &[(record.id, report.clone())],
                swarm_chat_origins: &[(record.id, origin.clone())],
                ..FinishedRunMetadata::default()
            },
        )
        .unwrap();
    drop(store);
    let store = SchedulerStore::open(path).unwrap();
    assert_eq!(store.journal_swarm_report(record.id), Some(&report));
    assert_eq!(
        store.session_journals(record.id),
        Some(journal_ids.as_slice())
    );
    assert_eq!(
        store.next_swarm_chat_seed(|_| true),
        Some(&SwarmChatSeed {
            run: record.id,
            origin,
            report
        }),
    );
}

#[test]
fn wide_swarm_reports_evict_only_the_oldest_runs_under_byte_pressure() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let mut wide = report(RunOutcome::Success);
    extend_report(&mut wide, MAX_SWARM_TARGETS);
    for target in &mut wide.targets {
        target.label = "\"".repeat(MAX_SWARM_TARGET_LABEL_BYTES);
        target.detail = "\\".repeat(MAX_SWARM_TARGET_DETAIL_BYTES);
    }
    let report_bytes = serde_json::to_vec(&wide).unwrap().len() as u64;
    let earlier_count = MAX_STORE_BYTES / report_bytes - 4;
    let earlier = (2..2 + earlier_count)
        .map(|time| finish(&mut scheduler, time, RunOutcome::Success))
        .collect::<Vec<_>>();
    let earlier_reports = earlier
        .iter()
        .map(|record| (record.id, wide.clone()))
        .collect::<Vec<_>>();
    let first = earlier[0].id;
    let first_journal = ["00000000-0000-4000-8000-000000000001".to_owned()];
    let seed_origin = origin(50);
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_finished_runs(
            scheduler.graph(),
            &earlier,
            FinishedRunMetadata {
                session_journals: &[(first, &first_journal)],
                swarm_reports: &earlier_reports,
                swarm_chat_origins: &[(first, seed_origin.clone())],
                ..FinishedRunMetadata::default()
            },
        )
        .unwrap();
    assert_eq!(store.journal().len(), earlier.len());

    let later = (2 + earlier_count..10 + earlier_count)
        .map(|time| finish(&mut scheduler, time, RunOutcome::Success))
        .collect::<Vec<_>>();
    let later_reports = later
        .iter()
        .map(|record| (record.id, wide.clone()))
        .collect::<Vec<_>>();
    store
        .commit_finished_runs(
            scheduler.graph(),
            &later,
            FinishedRunMetadata {
                swarm_reports: &later_reports,
                ..FinishedRunMetadata::default()
            },
        )
        .unwrap();
    assert!(fs::metadata(&path).unwrap().len() <= MAX_STORE_BYTES);
    drop(store);
    let store = SchedulerStore::open(path).unwrap();
    let retained = store.journal();
    assert!(retained.len() > later.len());
    let expected = later.iter().rev().chain(earlier.iter().rev());
    assert!(retained.iter().copied().eq(expected.take(retained.len())));
    assert!(
        retained
            .iter()
            .all(|record| store.journal_swarm_report(record.id) == Some(&wide))
    );
    assert!(store.journal_swarm_report(first).is_none());
    assert!(store.session_journals(first).is_none());
    assert!(store.journal_swarm_chat_origin(first).is_none());
    let pending = store.next_swarm_chat_seed(|_| true).unwrap();
    assert_eq!(pending.run, first);
    assert_eq!(pending.origin, seed_origin);
}

#[test]
fn swarm_report_round_trips_in_target_order_and_is_excluded_from_backup() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let outcome = RunOutcome::Failed {
        exit_code: Some(17),
    };
    let record = finish(&mut scheduler, 2, outcome);
    let report = report(outcome);
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_with_actions(scheduler.graph(), &[], &[])
        .unwrap();
    for _ in 0..2 {
        store
            .commit_records_with_swarm_reports(
                scheduler.graph(),
                std::slice::from_ref(&record),
                &[(record.id, "Swarm target failed.".into())],
                &[(record.id, [5; 16])],
                &[(record.id, report.clone())],
            )
            .unwrap();
    }
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal(), [&record]);
    assert_eq!(store.journal_swarm_report(record.id), Some(&report));
    assert_eq!(store.journal_recording(record.id), Some([5; 16]));
    let native = fs::read(&path).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&native).unwrap();
    let saved = document["swarm_reports"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert!(saved.get("comparison").is_none());
    let portable = SchedulerStore::portable_backup(&native).unwrap();
    assert!(!String::from_utf8_lossy(&portable).contains("swarm_reports"));
    assert!(!String::from_utf8_lossy(&portable).contains("Masked target advice"));
    SchedulerStore::validate_backup::<()>(&portable, |()| true).unwrap();
    assert!(matches!(
        SchedulerStore::validate_backup::<()>(&native, |()| true),
        Err(StoreError::InvalidRecord)
    ));

    assert_eq!(store.delete_records(&[record.id]).unwrap(), 1);
    assert!(store.journal_swarm_report(record.id).is_none());
    drop(store);
    assert!(
        SchedulerStore::open(&path)
            .unwrap()
            .journal_swarm_report(record.id)
            .is_none()
    );
}

#[test]
fn swarm_report_rejects_invalid_late_and_mismatched_completions_atomically() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let mut store = SchedulerStore::open(&path).unwrap();
    let first = finish(&mut scheduler, 2, RunOutcome::Blocked);
    let blocked = report(RunOutcome::Blocked);
    let mut duplicates = blocked.clone();
    duplicates.targets[1].target = duplicates.targets[0].target;
    let mut invisible_label = blocked.clone();
    invisible_label.targets[1].label = "Hidden\u{202e} direction".into();
    let mut long_detail = blocked.clone();
    long_detail.targets[0].detail = "a".repeat(MAX_SWARM_TARGET_DETAIL_BYTES + 1);
    let mut long_question = blocked.clone();
    long_question.question = "a".repeat(MAX_SWARM_QUESTION_BYTES + 1);
    let mut long_label = blocked.clone();
    long_label.targets[1].label = "a".repeat(MAX_SWARM_TARGET_LABEL_BYTES + 1);
    let mut blank = blocked.clone();
    blank.question = " ".into();
    let mut many = blocked.clone();
    extend_report(&mut many, MAX_SWARM_TARGETS + 1);
    let mut empty = blocked.clone();
    empty.targets.clear();
    for invalid in [
        duplicates,
        invisible_label,
        long_detail,
        long_question,
        long_label,
        blank,
        empty,
        many,
        report(RunOutcome::Success),
        report(RunOutcome::Cancelled),
    ] {
        assert!(matches!(
            commit(&mut store, &scheduler, &first, invalid),
            Err(StoreError::InvalidRecord)
        ));
        assert!(store.journal().is_empty());
        assert!(!path.exists());
    }
    for markdown in [
        String::new(),
        " Leading whitespace".into(),
        "Invalid\u{202e}character".into(),
        "Invalid\u{001b}escape".into(),
        "x".repeat(MAX_SWARM_COMPARISON_BYTES + 1),
    ] {
        let mut invalid = blocked.clone();
        invalid.comparison = Some(markdown);
        assert!(matches!(
            commit_chat(&mut store, &scheduler, &first, invalid, origin(31)),
            Err(StoreError::InvalidRecord)
        ));
        assert!(store.journal().is_empty());
        assert!(store.next_swarm_chat_seed(|_| true).is_none());
        assert!(!path.exists());
    }

    store
        .commit(scheduler.graph(), std::slice::from_ref(&first))
        .unwrap();
    let previous = fs::read(&path).unwrap();
    let second = finish(&mut scheduler, 3, RunOutcome::Blocked);
    assert!(matches!(
        store.commit_records_with_swarm_reports(
            scheduler.graph(),
            std::slice::from_ref(&second),
            &[],
            &[],
            &[(first.id, blocked.clone())],
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert!(matches!(
        commit(&mut store, &scheduler, &first, blocked.clone()),
        Err(StoreError::ConflictingRecord(run)) if run == first.id
    ));
    let cancelled = finish(&mut scheduler, 4, RunOutcome::Cancelled);
    assert!(matches!(
        commit(
            &mut store,
            &scheduler,
            &cancelled,
            report(RunOutcome::Cancelled)
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_eq!(store.journal(), [&first]);
    assert!(store.journal_swarm_report(first.id).is_none());

    commit(&mut store, &scheduler, &second, blocked.clone()).unwrap();
    let mut edited = blocked.clone();
    edited.targets.reverse();
    assert!(matches!(
        commit(&mut store, &scheduler, &second, edited),
        Err(StoreError::ConflictingRecord(run)) if run == second.id
    ));
    assert_eq!(store.journal_swarm_report(second.id), Some(&blocked));
}

#[test]
fn swarm_report_retention_and_native_open_reject_orphaned_metadata() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler = scheduler();
    let first = finish(&mut scheduler, 2, RunOutcome::Success);
    let second = finish(&mut scheduler, 3, RunOutcome::Success);
    let mut store = SchedulerStore::open(&path).unwrap();
    for record in [&first, &second] {
        commit(&mut store, &scheduler, record, report(RunOutcome::Success)).unwrap();
    }
    assert_eq!(store.prune_before(at(3)).unwrap(), 1);
    assert!(store.journal_swarm_report(first.id).is_none());
    assert!(store.journal_swarm_report(second.id).is_some());
    let records: Vec<_> = (4..4 + MAX_JOURNAL_RECORDS as u64)
        .map(|time| finish(&mut scheduler, time, RunOutcome::Success))
        .collect();
    store.commit(scheduler.graph(), &records).unwrap();
    assert_eq!(store.journal().len(), MAX_JOURNAL_RECORDS);
    assert!(store.journal_swarm_report(second.id).is_none());
    drop(store);

    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let orphan: RunId = second.id;
    document["swarm_reports"] =
        serde_json::json!({ (orphan.to_string()): report(RunOutcome::Success) });
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::InvalidRecord)
    ));
}
