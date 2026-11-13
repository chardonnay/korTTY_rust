use kortty_jobs::{FinishedRunMetadata, MAX_SESSION_JOURNALS_PER_RUN, RunId, RunRecord};

use super::*;

const FIRST_TARGET: &str = "a0000000-0000-4000-8000-000000000001";
const SECOND_TARGET: &str = "b0000000-0000-4000-8000-000000000002";
const THIRD_TARGET: &str = "c0000000-0000-4000-8000-000000000003";

fn scheduler() -> Scheduler<()> {
    let job = Job::new(id("job"), "Job", ()).unwrap();
    Scheduler::new(JobGraph::new(vec![job]).unwrap())
}

fn attach(
    store: &mut SchedulerStore,
    scheduler: &Scheduler<()>,
    records: &[RunRecord],
    links: &[(RunId, &[String])],
) -> Result<(), StoreError> {
    store.commit_finished_runs(
        scheduler.graph(),
        records,
        FinishedRunMetadata {
            session_journals: links,
            ..FinishedRunMetadata::default()
        },
    )
}

#[test]
fn scheduler_session_journals_round_trip_in_target_order_and_expire_with_the_run() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let first = complete(&mut scheduler, "job", 2).finished.remove(0);
    let second = complete(&mut scheduler, "job", 3).finished.remove(0);
    let first_links = [SECOND_TARGET.into(), FIRST_TARGET.into()];
    let second_links = [THIRD_TARGET.into()];
    let links = [
        (first.id, first_links.as_slice()),
        (second.id, second_links.as_slice()),
    ];
    let records = [first.clone(), second.clone()];
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_with_actions(scheduler.graph(), &[], &[])
        .unwrap();
    attach(&mut store, &scheduler, &records, &links).unwrap();
    let native = fs::read(&path).unwrap();
    attach(&mut store, &scheduler, &records, &links).unwrap();
    assert_eq!(fs::read(&path).unwrap(), native);
    let portable = SchedulerStore::portable_backup(&native).unwrap();
    let portable_text = String::from_utf8(portable.clone()).unwrap();
    assert!(!portable_text.contains("session_journals"));
    assert!(!portable_text.contains(FIRST_TARGET));
    SchedulerStore::validate_backup::<()>(&portable, |()| true).unwrap();
    assert!(matches!(
        SchedulerStore::validate_backup::<()>(&native, |()| true),
        Err(StoreError::InvalidRecord)
    ));
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(
        store.session_journals(first.id),
        Some(first_links.as_slice())
    );
    assert_eq!(
        store.session_journals(second.id),
        Some(second_links.as_slice())
    );
    assert_eq!(store.delete_records(&[first.id]).unwrap(), 1);
    assert_eq!(store.session_journals(first.id), None);
    assert_eq!(store.prune_before(at(3)).unwrap(), 0);
    assert_eq!(
        store.session_journals(second.id),
        Some(second_links.as_slice())
    );
    assert_eq!(store.prune_before(at(4)).unwrap(), 1);
    assert_eq!(store.session_journals(second.id), None);
    assert!(store.journal().is_empty());
}

#[test]
fn scheduler_session_journals_reject_ambiguous_or_unbound_references_atomically() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let first = complete(&mut scheduler, "job", 2).finished.remove(0);
    let second = complete(&mut scheduler, "job", 3).finished.remove(0);
    let first_links = [FIRST_TARGET.into(), SECOND_TARGET.into()];
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit(scheduler.graph(), &[]).unwrap();
    let initial = fs::read(&path).unwrap();
    for invalid in [
        Vec::new(),
        vec!["../journal.xml".into()],
        vec![FIRST_TARGET.into(), FIRST_TARGET.to_ascii_uppercase()],
        (0..=MAX_SESSION_JOURNALS_PER_RUN)
            .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
            .collect(),
    ] {
        assert!(matches!(
            attach(
                &mut store,
                &scheduler,
                std::slice::from_ref(&first),
                &[(first.id, &invalid)]
            ),
            Err(StoreError::InvalidRecord)
        ));
    }
    assert!(matches!(
        attach(
            &mut store,
            &scheduler,
            &[first.clone(), second.clone()],
            &[(first.id, &first_links), (first.id, &first_links)],
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert!(matches!(
        attach(
            &mut store,
            &scheduler,
            std::slice::from_ref(&first),
            &[(second.id, &first_links)],
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert!(store.journal().is_empty());
    assert_eq!(fs::read(&path).unwrap(), initial);

    attach(
        &mut store,
        &scheduler,
        std::slice::from_ref(&first),
        &[(first.id, &first_links)],
    )
    .unwrap();
    let saved = fs::read(&path).unwrap();
    let alias = [FIRST_TARGET.to_ascii_uppercase()];
    assert!(matches!(
        attach(
            &mut store,
            &scheduler,
            std::slice::from_ref(&second),
            &[(second.id, &alias)],
        ),
        Err(StoreError::ConflictingRecord(run)) if run == second.id
    ));
    let changed = [SECOND_TARGET.into(), FIRST_TARGET.into()];
    assert!(matches!(
        attach(
            &mut store,
            &scheduler,
            std::slice::from_ref(&first),
            &[(first.id, &changed)],
        ),
        Err(StoreError::ConflictingRecord(run)) if run == first.id
    ));
    assert_eq!(store.journal(), [&first]);
    assert_eq!(store.session_journals(second.id), None);
    assert_eq!(fs::read(&path).unwrap(), saved);
    drop(store);

    let mut tampered: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    *tampered["session_journals"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap() = serde_json::json!([FIRST_TARGET, FIRST_TARGET.to_ascii_uppercase()]);
    let tampered = serde_json::to_vec(&tampered).unwrap();
    fs::write(&path, &tampered).unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::InvalidRecord)
    ));
    assert_eq!(fs::read(&path).unwrap(), tampered);
}

#[test]
fn scheduler_session_journals_follow_the_rolling_run_limit() {
    let fixture = TestDirectory::new();
    let path = fixture.profile();
    let mut scheduler = scheduler();
    let first = complete(&mut scheduler, "job", 2).finished.remove(0);
    let mut store = SchedulerStore::open(&path).unwrap();
    attach(
        &mut store,
        &scheduler,
        std::slice::from_ref(&first),
        &[(first.id, &[FIRST_TARGET.into()])],
    )
    .unwrap();
    let records: Vec<_> = (3..3 + MAX_JOURNAL_RECORDS as u64)
        .map(|second| complete(&mut scheduler, "job", second).finished.remove(0))
        .collect();
    let newest = records.last().unwrap().id;
    attach(
        &mut store,
        &scheduler,
        &records,
        &[(newest, &[SECOND_TARGET.into()])],
    )
    .unwrap();
    drop(store);
    let store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal().len(), MAX_JOURNAL_RECORDS);
    assert!(store.journal_record(first.id).is_none());
    assert!(store.session_journals(first.id).is_none());
    assert_eq!(store.session_journals(newest).unwrap(), [SECOND_TARGET]);
}
