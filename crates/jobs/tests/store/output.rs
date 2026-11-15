use kortty_jobs::{FinishedRunMetadata, MAX_RUN_OUTPUT_BYTES, RunId, RunOutput, RunRecord};
use serde_json::json;

use super::*;

fn scheduler() -> Scheduler<()> {
    let job = Job::new(id("job"), "Job", ()).unwrap();
    Scheduler::new(JobGraph::new(vec![job]).unwrap())
}

fn commit(
    store: &mut SchedulerStore,
    scheduler: &Scheduler<()>,
    records: &[RunRecord],
    output: &[(RunId, RunOutput)],
) -> Result<(), StoreError> {
    store.commit_finished_runs(
        scheduler.graph(),
        records,
        FinishedRunMetadata {
            output,
            ..FinishedRunMetadata::default()
        },
    )
}

#[test]
fn scheduler_run_output_survives_restart_and_stays_out_of_backups() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let record = complete(&mut scheduler, "job", 2).finished.remove(0);
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_with_actions(scheduler.graph(), std::slice::from_ref(&record), &[])
        .unwrap();
    assert!(store.journal_output(record.id).is_none());
    let output = RunOutput {
        stdout: "stdout output\n東京\tready\n".into(),
        stderr: "stderr output\ncafé\n".into(),
    };
    let attached = [(record.id, output.clone())];
    commit(&mut store, &scheduler, &[], &attached).unwrap();
    let native = fs::read(&path).unwrap();
    commit(
        &mut store,
        &scheduler,
        std::slice::from_ref(&record),
        &attached,
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), native);
    let portable = SchedulerStore::portable_backup(&native).unwrap();
    assert!(!String::from_utf8_lossy(&portable).contains("stdout output"));
    assert!(!String::from_utf8_lossy(&portable).contains("journal_output"));
    SchedulerStore::validate_backup::<()>(&portable, |()| true).unwrap();
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal_output(record.id), Some(&output));
    assert_eq!(store.prune_before(at(3)).unwrap(), 1);
    assert!(store.journal_output(record.id).is_none());
    assert!(matches!(
        commit(&mut store, &scheduler, &[], &attached),
        Err(StoreError::InvalidRecord)
    ));
    assert!(!fs::read_to_string(path).unwrap().contains("journal_output"));
}

#[test]
fn scheduler_run_output_rejects_unbound_and_unsafe_data_atomically() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let record = complete(&mut scheduler, "job", 2).finished.remove(0);
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit(scheduler.graph(), &[]).unwrap();
    let original = fs::read(&path).unwrap();
    let output = |stdout: String| RunOutput {
        stdout,
        ..RunOutput::default()
    };
    let valid = output("ready\n".into());
    for invalid in [
        vec![(record.id, RunOutput::default())],
        vec![(record.id, output("x".repeat(MAX_RUN_OUTPUT_BYTES + 1)))],
        vec![(record.id, output("\x1b[31m".into()))],
        vec![(record.id, output("control\u{202e}text".into()))],
        vec![(record.id, valid.clone()), (record.id, valid.clone())],
    ] {
        assert!(matches!(
            commit(
                &mut store,
                &scheduler,
                std::slice::from_ref(&record),
                &invalid
            ),
            Err(StoreError::InvalidRecord)
        ));
    }
    assert!(matches!(
        commit(&mut store, &scheduler, &[], &[(record.id, valid.clone())]),
        Err(StoreError::InvalidRecord)
    ));
    let mut cancelled = record.clone();
    cancelled.outcome = RunOutcome::Cancelled;
    assert!(matches!(
        commit(
            &mut store,
            &scheduler,
            &[cancelled],
            &[(record.id, valid.clone())],
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
    commit(
        &mut store,
        &scheduler,
        std::slice::from_ref(&record),
        &[(record.id, valid)],
    )
    .unwrap();
    let original = fs::read(&path).unwrap();
    drop(store);
    let parsed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let mut changed = parsed.clone();
    changed["journal_output"][record.id.to_string()]["output"]["stdout"] = json!("changed");
    let mut orphaned = parsed;
    orphaned["journal"] = json!([]);
    for invalid in [changed, orphaned] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            SchedulerStore::open(&path),
            Err(StoreError::InvalidRecord)
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn scheduler_keeps_partial_output_only_when_the_blocked_run_started() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let job = Job::new(id("job"), "Job", ()).unwrap();
    let dependent = Job::new(id("dependent"), "Dependent", ())
        .unwrap()
        .with_dependencies(DependencyMode::Gate, vec![id("job")], None);
    let mut scheduler = Scheduler::new(JobGraph::new(vec![job, dependent]).unwrap());
    let skipped = scheduler
        .start_scheduled(&id("dependent"), at(1))
        .unwrap()
        .finished
        .remove(0);
    let started = scheduler
        .start_manual(&id("job"), at(2))
        .unwrap()
        .started
        .remove(0);
    let partial = scheduler
        .complete(started.id, RunOutcome::Blocked, at(3))
        .unwrap()
        .finished
        .remove(0);
    assert_eq!(partial.outcome, skipped.outcome);
    let output = RunOutput {
        stdout: "One target finished before another target was blocked.\n".into(),
        ..RunOutput::default()
    };
    let mut store = SchedulerStore::open(&path).unwrap();
    assert!(matches!(
        commit(
            &mut store,
            &scheduler,
            std::slice::from_ref(&skipped),
            &[(skipped.id, output.clone())],
        ),
        Err(StoreError::InvalidRecord)
    ));
    let records = [skipped.clone(), partial.clone()];
    let outputs = [(partial.id, output.clone())];
    commit(&mut store, &scheduler, &records, &outputs).unwrap();
    commit(&mut store, &scheduler, &records, &outputs).unwrap();
    drop(store);
    let store = SchedulerStore::open(path).unwrap();
    assert_eq!(store.journal_output(partial.id), Some(&output));
    assert_eq!(store.journal_output(skipped.id), None);
}

#[test]
fn scheduler_output_budget_retains_newest_text_and_old_receipts() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let records = (2..12)
        .map(|finish| complete(&mut scheduler, "job", finish).finished.remove(0))
        .collect::<Vec<_>>();
    // Every byte in both streams needs a two-byte escape on disk.
    let output = RunOutput {
        stdout: "\\".repeat(MAX_RUN_OUTPUT_BYTES),
        stderr: "\n".repeat(MAX_RUN_OUTPUT_BYTES),
    };
    let attached = records
        .iter()
        .rev()
        .map(|record| (record.id, output.clone()))
        .collect::<Vec<_>>();
    let first = &records[0];
    let last = records.last().unwrap();
    let mut store = SchedulerStore::open(&path).unwrap();
    commit(&mut store, &scheduler, &records, &attached).unwrap();
    assert_eq!(store.journal().len(), records.len());
    assert_eq!(store.journal_output(first.id), None);
    assert_eq!(store.journal_output(last.id), Some(&output));
    let persisted = fs::read(&path).unwrap();
    assert!(persisted.len() < 2 * 1024 * 1024 + 64 * 1024);
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    commit(&mut store, &scheduler, &records, &attached).unwrap();
    assert_eq!(fs::read(&path).unwrap(), persisted);
    let changed = RunOutput {
        stdout: "changed".into(),
        ..RunOutput::default()
    };
    assert!(matches!(
        commit(&mut store, &scheduler, &[], &[(first.id, changed)]),
        Err(StoreError::ConflictingRecord(id)) if id == first.id
    ));
    assert_eq!(fs::read(&path).unwrap(), persisted);
    assert_eq!(store.delete_records(&[first.id, last.id]).unwrap(), 2);
    assert!(store.journal_output(last.id).is_none());
    assert!(matches!(
        commit(&mut store, &scheduler, &[], &[(first.id, output)]),
        Err(StoreError::InvalidRecord)
    ));
}
