//! Saved approvals must bind the creation semantics a reviewer approved.

use super::*;
use crate::scheduler_window::tests::Profile;
use serde::Serialize;

fn hash(value: &impl Serialize) -> [u8; 32] {
    HashAlgorithm::Sha256
        .digest(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn fixture(direction: Direction) -> (Copy, ConnectionRef) {
    let copy = Copy {
        local: "/fixture/local".into(),
        remote: serde_json::from_value(serde_json::json!("/fixture/remote")).unwrap(),
        direction,
        excludes: "*.temporary".into(),
        replace_changed: false,
        standing: None,
        approval: None,
    };
    let connection = serde_json::from_value(serde_json::json!({
        "id": 7,
        "target": {"host": "server.example", "port": 22, "username": "operator"},
    }))
    .unwrap();
    (copy, connection)
}

#[test]
fn changed_downloads_cannot_use_missing_only_receipts_or_standing_authority() {
    let (mut copy, connection) = fixture(Direction::Download);
    let legacy_json = serde_json::to_value(&copy).unwrap();
    assert!(legacy_json.get("replace_changed").is_none());
    let missing = copy.scope(&connection, None).unwrap();
    copy.replace_changed = true;
    assert_ne!(copy.scope(&connection, None).unwrap(), missing);
    assert!(copy.validate_replacement(true).is_err());
    copy.standing = Some(standing::Settings::default());
    assert!(copy.scope(&connection, None).is_err());
    copy.standing = None;
    copy.direction = Direction::Upload;
    assert!(copy.scope(&connection, None).is_err());
}

// Exact serialized domains used before ordinary local creation semantics.
fn legacy_base(
    copy: &Copy,
    connection: &ConnectionRef,
    schedule: Option<&JobSchedule>,
) -> [u8; 32] {
    hash(&(
        "kortty-scheduled-sftp-v1",
        connection,
        &copy.local,
        &copy.remote,
        copy.direction,
        &copy.excludes,
        schedule,
    ))
}

fn legacy_conditioned(
    base: [u8; 32],
    mode: DependencyMode,
    predecessors: &[JobId],
    age: Option<NonZeroU32>,
) -> [u8; 32] {
    if predecessors.is_empty() {
        base
    } else {
        hash(&(
            "kortty-scheduled-sftp-condition-v1",
            base,
            mode,
            predecessors,
            age,
        ))
    }
}

#[test]
fn historical_one_use_download_approvals_require_new_review_before_claiming() {
    let profile = Profile::temporary();
    let path = profile.file();
    let store = Store::new(path.parent().unwrap());
    let (copy, connection) = fixture(Direction::Download);
    let predecessor = [JobId::new("prepare-source").unwrap()];
    let age = NonZeroU32::new(15);
    for (index, (mode, predecessors)) in [
        (DependencyMode::Gate, &[][..]),
        (DependencyMode::Gate, predecessor.as_slice()),
        (DependencyMode::Chain, predecessor.as_slice()),
    ]
    .into_iter()
    .enumerate()
    {
        let job = JobId::new(format!("download-{index}")).unwrap();
        let legacy = legacy_conditioned(
            legacy_base(&copy, &connection, None),
            mode,
            predecessors,
            age,
        );
        let current = copy
            .conditioned_scope(&connection, None, mode, predecessors, age)
            .unwrap();
        assert_ne!(current, legacy);
        let old = Receipt::for_operation(legacy, [1; 32], vec!["pinned route".into()]).unwrap();
        store.swap(&job, old.clone(), |_| true).unwrap();
        let reopened = Store::new(path.parent().unwrap());
        assert!(reopened.load(&job, Some(&old.approval), legacy).is_ok());
        assert_eq!(
            reopened
                .load(&job, Some(&old.approval), current)
                .err()
                .unwrap()
                .to_string(),
            needs_review().to_string()
        );
        let fresh = Receipt::for_operation(current, [2; 32], old.pins).unwrap();
        reopened.swap(&job, fresh.clone(), |_| true).unwrap();
        let loaded = reopened.load(&job, Some(&fresh.approval), current).unwrap();
        reopened.claim(&job, &loaded).unwrap();
        reopened.require_claimed(&job, &fresh).unwrap();
        assert!(reopened.load(&job, Some(&fresh.approval), current).is_err());
    }
}

#[test]
fn historical_standing_download_approval_requires_new_review_after_reopening() {
    let profile = Profile::temporary();
    let path = profile.file();
    let store = Store::new(path.parent().unwrap());
    let (mut copy, connection) = fixture(Direction::Download);
    let settings = standing::Settings::default();
    copy.standing = Some(settings);
    let id = JobId::new("recurring-download").unwrap();
    let action: SshCommand =
        serde_json::from_value(serde_json::json!({ "command": "true" })).unwrap();
    let schedule = JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]);
    let job = Job::new(id.clone(), "Download", action)
        .unwrap()
        .with_schedule(schedule);
    let old_scope = hash(&(
        "kortty-scheduled-sftp-standing-v1",
        job.id(),
        legacy_base(&copy, &connection, job.schedule()),
        TimeZone::UTC.iana_name().unwrap(),
        settings,
    ));
    let current_scope = copy
        .standing_scope(&job, &connection, &TimeZone::UTC)
        .unwrap();
    assert_ne!(current_scope, old_scope);
    let now = SystemTime::now();
    let expiry = settings.expiry(now).unwrap();
    let old = standing::Approval::new(
        old_scope,
        vec!["pinned route".into()],
        [1; 32],
        expiry,
        settings.limits,
    )
    .unwrap();
    store.swap_standing(&id, old.clone(), |_| true).unwrap();
    let reopened = Store::new(path.parent().unwrap());
    assert!(
        reopened
            .load_standing(&id, Some(&old.approval), old_scope, SystemTime::now())
            .is_ok()
    );
    assert!(
        reopened
            .load_standing(&id, Some(&old.approval), current_scope, SystemTime::now())
            .is_err()
    );
    let fresh =
        standing::Approval::new(current_scope, old.pins, [2; 32], expiry, settings.limits).unwrap();
    reopened
        .swap_standing(&id, fresh.clone(), |_| true)
        .unwrap();
    let loaded = reopened
        .load_standing(&id, Some(&fresh.approval), current_scope, SystemTime::now())
        .unwrap();
    let firing = standing::Firing {
        epoch: [1; 16],
        run: serde_json::from_value(serde_json::json!(1)).unwrap(),
    };
    reopened
        .claim_standing(&id, &loaded, firing, 1, 5, &|| false)
        .unwrap();
    reopened
        .require_standing_claimed(&id, &fresh, firing, SystemTime::now())
        .unwrap();
}

#[test]
fn upload_and_privileged_approvals_keep_their_exact_historical_scopes() {
    let predecessor = [JobId::new("prepare-source").unwrap()];
    let age = NonZeroU32::new(15);
    let schedule = JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]);
    for direction in [Direction::Upload, Direction::Download] {
        let (mut copy, connection) = fixture(direction);
        for calendar in [None, Some(&schedule)] {
            let base = legacy_base(&copy, &connection, calendar);
            assert_eq!(copy.scope(&connection, calendar).unwrap(), base);
            for sudo in [SudoMode::Passwordless, SudoMode::Vault] {
                assert_eq!(
                    copy.privileged_scope(&connection, calendar, sudo).unwrap(),
                    hash(&("kortty-scheduled-privileged-tree-v1", base, sudo))
                );
            }
            if direction == Direction::Upload {
                for (mode, predecessors) in [
                    (DependencyMode::Gate, &[][..]),
                    (DependencyMode::Gate, predecessor.as_slice()),
                    (DependencyMode::Chain, predecessor.as_slice()),
                ] {
                    assert_eq!(
                        copy.conditioned_scope(&connection, calendar, mode, predecessors, age)
                            .unwrap(),
                        legacy_conditioned(base, mode, predecessors, age)
                    );
                }
            }
        }
        if direction == Direction::Upload {
            let settings = standing::Settings::default();
            copy.standing = Some(settings);
            let action: SshCommand =
                serde_json::from_value(serde_json::json!({ "command": "true" })).unwrap();
            let job = Job::new(JobId::new("upload").unwrap(), "Upload", action)
                .unwrap()
                .with_schedule(schedule.clone());
            assert_eq!(
                copy.standing_scope(&job, &connection, &TimeZone::UTC)
                    .unwrap(),
                hash(&(
                    "kortty-scheduled-sftp-standing-v1",
                    job.id(),
                    legacy_base(&copy, &connection, job.schedule()),
                    TimeZone::UTC.iana_name().unwrap(),
                    settings
                ))
            );
        }
    }
}
