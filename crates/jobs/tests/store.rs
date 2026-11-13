use std::fs::{self, File};
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use kortty_jobs::{
    BlockReason, DependencyMode, Job, JobGraph, JobId, JobSchedule, MAX_JOURNAL_RECORDS,
    MAX_STORE_BYTES, RunBatch, RunOutcome, Scheduler, SchedulerStore, StoreError, Weekday,
};

#[path = "store/session_journals.rs"]
mod session_journals;
#[path = "store/swarm.rs"]
mod swarm;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "kortty-jobs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        }
    }

    fn profile(&self) -> PathBuf {
        self.0.join("scheduler.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(name: &str) -> JobId {
    JobId::new(name).unwrap()
}

fn at(second: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(second)
}

fn complete<A>(scheduler: &mut Scheduler<A>, job: &str, finish: u64) -> RunBatch<A> {
    let started = scheduler.start_manual(&id(job), at(finish - 1)).unwrap();
    scheduler
        .complete(started.started[0].id, RunOutcome::Success, at(finish))
        .unwrap()
}

#[test]
fn definitions_and_actual_runs_round_trip_without_executor_data() {
    let directory = TestDirectory::new();
    let path = directory.0.join("profile/scheduler.json");
    // This payload deliberately implements neither serde trait.
    struct ExecutorData(&'static str);
    let build = Job::new(
        id("build"),
        "Build job",
        ExecutorData("sensitive-command-marker"),
    )
    .unwrap();
    let schedule = JobSchedule::default()
        .with_weekdays([Weekday::Monday])
        .with_window("09:00".parse().unwrap(), "18:00".parse().unwrap())
        .with_fixed_times(["09:15".parse().unwrap()])
        .with_active_dates(Some("2026-05-04".parse().unwrap()), None)
        .unwrap();
    let dependent = Job::new(
        id("dependent"),
        "Dependent",
        ExecutorData("secret-profile-marker"),
    )
    .unwrap()
    .with_schedule(schedule.clone())
    .with_dependencies(DependencyMode::Gate, vec![id("build")], NonZeroU32::new(30));
    let mut scheduler = Scheduler::new(JobGraph::new(vec![build, dependent]).unwrap());
    let blocked = scheduler.start_scheduled(&id("dependent"), at(1)).unwrap();
    let build = complete(&mut scheduler, "build", 2);
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit(scheduler.graph(), &blocked.finished).unwrap();
    store.commit(scheduler.graph(), &build.finished).unwrap();
    store.commit(scheduler.graph(), &build.finished).unwrap();
    assert_eq!(store.journal().len(), 2);
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::InUse)
    ));
    let raw = fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("sensitive-command-marker"));
    assert!(!raw.contains("secret-profile-marker"));
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.definitions()[1].schedule.as_ref(), Some(&schedule));
    assert_eq!(store.definitions()[1].max_age_minutes, NonZeroU32::new(30));
    assert_eq!(store.definitions()[1].dependencies, [id("build")]);
    assert_eq!(store.journal()[0], &blocked.finished[0]);
    assert_eq!(store.journal()[1], &build.finished[0]);
    assert!(
        matches!(store.restore::<()>(|_| None), Err(StoreError::MissingAction(job)) if job == id("build"))
    );
    let restored = store
        .restore(|_| Some(ExecutorData("restored-executor")))
        .unwrap();
    assert_eq!(
        restored.graph().job(&id("build")).unwrap().action().0,
        "restored-executor"
    );
    assert!(
        restored
            .blockers(&id("dependent"), at(3))
            .unwrap()
            .is_empty()
    );

    let empty = JobGraph::<()>::new(Vec::new()).unwrap();
    store.commit(&empty, &[]).unwrap();
    assert!(store.definitions().is_empty());
    assert_eq!(store.journal()[1].job_name, "Build job");
}

#[test]
fn optional_metadata_and_summary_round_trip_with_one_atomic_update() {
    #[derive(Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
    struct Action {
        target: String,
    }
    let directory = TestDirectory::new();
    let path = directory.profile();
    let action = Action {
        target: "test.example".into(),
    };
    let job = Job::new(id("job"), "Job", action).unwrap();
    let mut scheduler = Scheduler::new(JobGraph::new(vec![job]).unwrap());
    let first = complete(&mut scheduler, "job", 1).finished.remove(0);
    let mut store = SchedulerStore::open(&path).unwrap();
    assert!(matches!(
        store.commit_with_actions(
            scheduler.graph(),
            std::slice::from_ref(&first),
            &[(first.id, "control\u{001b}".into())]
        ),
        Err(StoreError::InvalidRecord)
    ));
    assert!(store.definitions().is_empty() && store.journal().is_empty());
    assert!(!path.exists());

    store
        .commit_with_actions(
            scheduler.graph(),
            std::slice::from_ref(&first),
            &[(first.id, "Exit status 0".into())],
        )
        .unwrap();
    let second = complete(&mut scheduler, "job", 2).finished.remove(0);
    store
        .commit_records(
            scheduler.graph(),
            std::slice::from_ref(&second),
            &[(second.id, "Second run".into())],
        )
        .unwrap();
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    let restored = store.restore_with_actions::<Action>().unwrap();
    assert_eq!(
        restored.graph().job(&id("job")).unwrap().action().target,
        "test.example"
    );
    assert_eq!(store.journal(), [&second, &first]);
    assert_eq!(store.journal_detail(first.id), Some("Exit status 0"));
    assert_eq!(store.journal_detail(second.id), Some("Second run"));
    assert_eq!(store.delete_records(&[first.id]).unwrap(), 1);
    assert!(store.journal_detail(first.id).is_none());
}

#[test]
fn disabled_additions_preserve_local_history_and_require_exact_predecessors() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let local = Job::new(id("local"), "Local job", "uptime").unwrap();
    let mut scheduler = Scheduler::new(JobGraph::new(vec![local]).unwrap());
    let record = complete(&mut scheduler, "local", 1).finished.remove(0);
    let mut store = SchedulerStore::open(&path).unwrap();
    store
        .commit_with_actions(
            scheduler.graph(),
            std::slice::from_ref(&record),
            &[(record.id, "Local detail".into())],
        )
        .unwrap();
    drop(store);
    let current = fs::read(&path).unwrap();
    let make_jobs = |root_action| {
        let root = Job::new(id("java-root"), "Imported root", root_action)
            .unwrap()
            .with_enabled(false);
        let leaf = Job::new(id("java-leaf"), "Imported leaf", "date")
            .unwrap()
            .with_enabled(false)
            .with_dependencies(DependencyMode::Chain, vec![id("java-root")], None);
        JobGraph::new(vec![leaf, root]).unwrap()
    };
    let initial = make_jobs("ls");
    let prepared = SchedulerStore::prepare_disabled_additions(Some(&current), &initial).unwrap();
    assert_eq!(prepared.added, 2);
    let bytes = prepared.bytes.unwrap();
    let before: serde_json::Value = serde_json::from_slice(&current).unwrap();
    let after: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for (key, value) in before.as_object().unwrap() {
        match key.as_str() {
            "actions" => assert_eq!(after[key]["local"], value["local"]),
            "definitions" => assert_eq!(after[key][0], value[0]),
            _ => assert_eq!(after[key], *value, "field {key}"),
        }
    }
    assert_eq!(fs::read(&path).unwrap(), current);
    let repeated = SchedulerStore::prepare_disabled_additions(Some(&bytes), &initial).unwrap();
    assert_eq!((repeated.added, repeated.unchanged), (0, 2));
    assert!(repeated.bytes.is_none());

    let conflict =
        SchedulerStore::prepare_disabled_additions(Some(&bytes), &make_jobs("pwd")).unwrap();
    assert_eq!((conflict.conflicts, conflict.missing_dependencies), (1, 1));
    assert!(conflict.bytes.is_none());
    fs::write(&path, bytes).unwrap();
    let store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal_detail(record.id), Some("Local detail"));
    let restored = store.restore_with_actions::<String>().unwrap();
    let root = restored.graph().job(&id("java-root")).unwrap();
    assert!(!root.enabled());
    assert_eq!(root.action(), "ls");
}

#[test]
fn disabled_additions_respect_job_capacity_and_reject_enabled_definitions() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let local = (0..kortty_jobs::MAX_JOBS)
        .map(|index| Job::new(id(&format!("local-{index}")), "Local job", "date").unwrap())
        .collect();
    let local = JobGraph::new(local).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&local, &[], &[])
        .unwrap();
    let current = fs::read(path).unwrap();
    let incoming = |enabled| {
        JobGraph::new(vec![
            Job::new(id("java-root"), "Imported root", "uptime")
                .unwrap()
                .with_enabled(enabled),
        ])
        .unwrap()
    };
    let result =
        SchedulerStore::prepare_disabled_additions(Some(&current), &incoming(false)).unwrap();
    assert_eq!(result.full, 1);
    assert!(result.bytes.is_none());
    assert!(matches!(
        SchedulerStore::prepare_disabled_additions(None, &incoming(true)),
        Err(StoreError::InvalidRecord)
    ));
}

#[test]
fn journal_deletion_preserves_dependency_results_and_own_last_run() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let jobs = vec![
        Job::new(id("one"), "one", ()).unwrap(),
        Job::new(id("two"), "two", ()).unwrap(),
        Job::new(id("join"), "join", ()).unwrap().with_dependencies(
            DependencyMode::Chain,
            vec![id("one"), id("two")],
            None,
        ),
    ];
    let mut scheduler = Scheduler::new(JobGraph::new(jobs).unwrap());
    let mut store = SchedulerStore::open(&path).unwrap();
    let one = complete(&mut scheduler, "one", 2);
    let two = complete(&mut scheduler, "two", 3);
    let join = scheduler
        .complete(two.started[0].id, RunOutcome::Success, at(4))
        .unwrap();
    for batch in [&one, &two, &join] {
        store.commit(scheduler.graph(), &batch.finished).unwrap();
    }
    assert_eq!(store.delete_records(&[join.finished[0].id]).unwrap(), 1);
    drop(store);

    let mut store = SchedulerStore::open(&path).unwrap();
    let mut restored = store.restore(|_| Some(())).unwrap();
    assert!(restored.last_finished(&id("join")).is_none());
    let one_new = complete(&mut restored, "one", 5);
    assert!(
        one_new.started.is_empty(),
        "old predecessor success must not retrigger the chain"
    );
    store.commit(restored.graph(), &one_new.finished).unwrap();
    assert_eq!(store.prune_before(at(5)).unwrap(), 2);
    store.apply_history(&mut restored);
    assert!(restored.last_finished(&id("two")).is_none());
    assert!(restored.blockers(&id("join"), at(6)).unwrap().is_empty());
    let two_new = complete(&mut restored, "two", 6);
    assert_eq!(two_new.started[0].job, id("join"));
}

#[test]
fn deleted_latest_failure_still_blocks_chain_after_restart() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let jobs = vec![
        Job::new(id("one"), "one", ()).unwrap(),
        Job::new(id("two"), "two", ()).unwrap(),
        Job::new(id("join"), "join", ()).unwrap().with_dependencies(
            DependencyMode::Chain,
            vec![id("one"), id("two")],
            None,
        ),
    ];
    let mut scheduler = Scheduler::new(JobGraph::new(jobs).unwrap());
    let success = complete(&mut scheduler, "one", 2);
    let failed = scheduler.start_manual(&id("one"), at(2)).unwrap();
    let failure = RunOutcome::Failed { exit_code: Some(1) };
    let failed = scheduler
        .complete(failed.started[0].id, failure, at(2))
        .unwrap();
    let mut store = SchedulerStore::open(&path).unwrap();
    for batch in [&success, &failed] {
        store.commit(scheduler.graph(), &batch.finished).unwrap();
    }
    store.delete_records(&[failed.finished[0].id]).unwrap();
    drop(store);

    let store = SchedulerStore::open(&path).unwrap();
    let mut restored = store.restore(|_| Some(())).unwrap();
    assert_eq!(
        restored.last_finished(&id("one")),
        Some(&success.finished[0])
    );
    let two = complete(&mut restored, "two", 3);
    assert!(two.started.is_empty());
    let blockers = restored.blockers(&id("join"), at(3)).unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].predecessor, id("one"));
    assert_eq!(blockers[0].reason, BlockReason::NotSuccessful(failure));
    let recovered = complete(&mut restored, "one", 4);
    assert_eq!(recovered.started[0].job, id("join"));
}

#[test]
fn old_profiles_only_restore_results_from_runs_that_are_still_latest() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let jobs = vec![
        Job::new(id("one"), "one", ()).unwrap(),
        Job::new(id("two"), "two", ()).unwrap(),
        Job::new(id("join"), "join", ()).unwrap().with_dependencies(
            DependencyMode::Gate,
            vec![id("one"), id("two")],
            None,
        ),
    ];
    let mut scheduler = Scheduler::new(JobGraph::new(jobs).unwrap());
    let first = complete(&mut scheduler, "one", 2);
    let latest = complete(&mut scheduler, "one", 3);
    let two = complete(&mut scheduler, "two", 4);
    let mut store = SchedulerStore::open(&path).unwrap();
    for batch in [&first, &latest, &two] {
        store.commit(scheduler.graph(), &batch.finished).unwrap();
    }
    store.delete_records(&[latest.finished[0].id]).unwrap();
    drop(store);

    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("dependency_results");
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let store = SchedulerStore::open(&path).unwrap();
    let restored = store.restore(|_| Some(())).unwrap();
    let blockers = restored.blockers(&id("join"), at(5)).unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].predecessor, id("one"));
    assert_eq!(blockers[0].reason, BlockReason::NeverRan);
}

#[test]
fn a_failed_write_does_not_mutate_the_store_and_can_be_retried() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let saved = directory.0.join("previous.json");
    let mut scheduler =
        Scheduler::new(JobGraph::new(vec![Job::new(id("job"), "job", ()).unwrap()]).unwrap());
    let first = complete(&mut scheduler, "job", 1);
    let second = complete(&mut scheduler, "job", 2);
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit(scheduler.graph(), &first.finished).unwrap();
    let before = fs::read(&path).unwrap();
    fs::rename(&path, &saved).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        store.commit(scheduler.graph(), &second.finished),
        Err(StoreError::UnsafePath)
    ));
    assert_eq!(store.journal(), [&first.finished[0]]);
    assert_eq!(fs::read(&saved).unwrap(), before);
    fs::remove_dir(&path).unwrap();
    fs::rename(&saved, &path).unwrap();
    store.commit(scheduler.graph(), &second.finished).unwrap();
    let mut altered = second.finished[0].clone();
    altered.job_name = "Different".into();
    assert!(matches!(
        store.commit(scheduler.graph(), &[altered]),
        Err(StoreError::ConflictingRecord(_))
    ));
    assert_eq!(store.journal().len(), 2);
    assert!(!fs::read_dir(&directory.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "tmp")
    }));
}

#[test]
fn invalid_newer_oversized_and_symlink_profiles_are_preserved() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let corrupt = b"{\"version\":1,\"secret-invalid-field\":\"do not echo\"}";
    fs::write(&path, corrupt).unwrap();
    let failure = SchedulerStore::open(&path).err().unwrap();
    assert!(matches!(failure, StoreError::InvalidJson { .. }));
    assert!(!failure.to_string().contains("secret-invalid-field"));
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    let newer = b"{\"version\":2,\"new_field\":true}";
    fs::write(&path, newer).unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::Version(2))
    ));
    assert_eq!(fs::read(&path).unwrap(), newer);
    File::create(&path)
        .unwrap()
        .set_len(MAX_STORE_BYTES + 1)
        .unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::TooLarge)
    ));
    assert_eq!(fs::metadata(&path).unwrap().len(), MAX_STORE_BYTES + 1);

    #[cfg(unix)]
    {
        let link = directory.0.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            SchedulerStore::open(&link),
            Err(StoreError::UnsafePath)
        ));
        assert_eq!(fs::read_link(link).unwrap(), path);
    }
}

#[test]
fn calendar_and_graph_validation_are_reapplied_at_open() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let job = Job::new(id("job"), "job", ())
        .unwrap()
        .with_schedule(JobSchedule::default());
    let graph = JobGraph::new(vec![job]).unwrap();
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit(&graph, &[]).unwrap();
    drop(store);
    let original = fs::read(&path).unwrap();
    let mut invalid: serde_json::Value = serde_json::from_slice(&original).unwrap();
    invalid["definitions"][0]["schedule"]["window_end"] = 1440.into();
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::InvalidJson { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let mut invalid: serde_json::Value = serde_json::from_slice(&original).unwrap();
    invalid["definitions"][0]["dependencies"] = serde_json::json!(["job"]);
    fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(matches!(
        SchedulerStore::open(&path),
        Err(StoreError::Graph(_))
    ));
}

#[test]
fn journal_bounds_and_profile_permissions_are_enforced() {
    let directory = TestDirectory::new();
    let path = directory.profile();
    let mut scheduler =
        Scheduler::new(JobGraph::new(vec![Job::new(id("job"), "job", ()).unwrap()]).unwrap());
    let mut records = Vec::new();
    for instant in 1..=MAX_JOURNAL_RECORDS as u64 + 1 {
        records.extend(complete(&mut scheduler, "job", instant).finished);
    }
    let mut store = SchedulerStore::open(&path).unwrap();
    assert!(matches!(
        store.commit(scheduler.graph(), &records),
        Err(StoreError::TooManyRecords)
    ));
    let (first, tail) = records.split_at(MAX_JOURNAL_RECORDS);
    store.commit(scheduler.graph(), first).unwrap();
    store.commit(scheduler.graph(), tail).unwrap();
    assert_eq!(store.journal().len(), MAX_JOURNAL_RECORDS);
    assert_eq!(store.journal().first().unwrap().id, tail[0].id);
    assert_eq!(store.journal().last().unwrap().id, first[1].id);

    assert_private(&path);
    drop(store);
    let store = SchedulerStore::open(&path).unwrap();
    assert_eq!(store.journal().len(), MAX_JOURNAL_RECORDS);
}

fn assert_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    #[cfg(not(unix))]
    assert!(path.is_file());
}
