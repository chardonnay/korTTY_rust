use std::{num::NonZeroU32, sync::atomic::AtomicBool};

use kortty_jobs::{DependencyMode, Job, JobGraph, JobSchedule, SchedulerStore, TimeZone};
use kortty_server_archive::rsync_tree::{DeleteScope, projection::CollisionKind, valid_stage};

use crate::job_executor::rsync;
use crate::scheduler_window::rsync::{
    MixedDestination, MixedReceiver, MixedReviewTarget, standing,
};
use crate::scheduler_window::{RemotePath, sync};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    Existing,
    Vacant,
    StaleInput,
    Revoked,
    Recurring,
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, the current korTTY bridge and v2 helper"]
fn openssh_mixed_rsync_publishes_existing_and_vacant_receivers() {
    for upload in [true, false] {
        mixed_transfer(upload, Case::Existing);
        mixed_transfer(upload, Case::Vacant);
    }
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, the current korTTY bridge and v2 helper"]
fn openssh_mixed_rsync_blocks_stale_losing_sources_and_revoked_approvals() {
    for upload in [true, false] {
        mixed_transfer(upload, Case::StaleInput);
        mixed_transfer(upload, Case::Revoked);
    }
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, the current korTTY bridge and v3 helper"]
fn openssh_recurring_rsync_advances_reviewed_receivers_and_limits_removals() {
    mixed_transfer(true, Case::Recurring);
    mixed_transfer(false, Case::Recurring);
}

fn mixed_transfer(upload: bool, case: Case) {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", vec![]);
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let program = PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
        .canonicalize()
        .unwrap();
    assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
    let existing = case != Case::Vacant;
    if upload {
        let marker = root.join("mixed-v2-enabled");
        if existing {
            fs::write(&marker, b"enable the existing helper").unwrap();
        }
        configure_rsync_fixture(&fixture, existing.then_some(marker.as_path()));
    }

    let destination = root.join("receiver");
    if existing {
        fs::create_dir_all(destination.join("scope")).unwrap();
        fs::create_dir(destination.join("scope-sibling")).unwrap();
        fs::write(destination.join("scope/obsolete"), b"prune only in scope").unwrap();
        fs::write(
            destination.join("scope-sibling/keep"),
            b"similar prefix preserved",
        )
        .unwrap();
        fs::write(destination.join("unrelated"), b"receiver-only entry").unwrap();
        let replaced = destination.join("report");
        fs::write(&replaced, b"reviewed original file").unwrap();
        fs::set_permissions(&replaced, fs::Permissions::from_mode(0o640)).unwrap();
    }

    let file_root = root.join("first-file");
    fs::create_dir(&file_root).unwrap();
    let first = file_root.join("report");
    fs::write(&first, b"first source wins").unwrap();
    let named = root.join("named/scope");
    fs::create_dir_all(named.join("empty")).unwrap();
    fs::write(named.join("new"), b"named directory contents").unwrap();
    let last = root.join(if existing { "last-file" } else { "contents" });
    fs::create_dir(&last).unwrap();
    let losing_file = last.join("report");
    fs::write(&losing_file, b"later source loses").unwrap();
    let last_source = if existing {
        losing_file.to_str().unwrap().to_owned()
    } else {
        fs::write(last.join("flattened"), b"flattened directory contents").unwrap();
        format!("{}/", last.display())
    };
    let sources = vec![
        first.to_str().unwrap().to_owned(),
        named.to_str().unwrap().to_owned(),
        last_source,
    ];
    let reviewed_destination = if upload {
        MixedDestination::Server(
            RemotePath::try_from(destination.to_str().unwrap().to_owned()).unwrap(),
        )
    } else {
        MixedDestination::Local(destination.clone())
    };
    let target = MixedReviewTarget {
        reference: reference.clone(),
        sources: sources.clone(),
        destination: reviewed_destination,
        delete: true,
        elevation: crate::scheduler_window::rsync::Elevation::Ordinary,
    };
    let credentials = Credentials::resolve_directory_browser(
        reference,
        profile,
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    let daemon = fixture.spawn();
    let reviewed = rsync::preview_mixed(
        credentials,
        &target,
        &fixture.trust,
        Arc::new(AtomicBool::new(false)),
        || Ok(()),
    );
    let trace = daemon.join().unwrap().stderr;
    let reviewed = reviewed.unwrap_or_else(|error| {
        panic!(
            "Mixed preview failed (upload={upload}, {case:?}): {error}\n{}",
            String::from_utf8_lossy(&trace)
        );
    });
    let has_snapshot = match &reviewed.receiver {
        MixedReceiver::Server { snapshot, .. } if upload => snapshot.is_some(),
        MixedReceiver::Local { snapshot, .. } if !upload => snapshot.is_some(),
        _ => panic!("wrong receiver side in the review"),
    };
    assert_eq!(has_snapshot, existing);
    let scope = if existing {
        DeleteScope::Named(vec!["scope".into()])
    } else {
        DeleteScope::All
    };
    assert_eq!(reviewed.plan.delete, scope);
    assert_eq!(reviewed.plan.collision_count, 1);
    let collision = &reviewed.plan.collisions[0];
    assert_eq!(collision.relative, "report");
    assert_eq!((collision.winner, collision.loser), (0, 2));
    assert_eq!(collision.kind, CollisionKind::EarlierFile);

    let pins = sync::fingerprints(&sync::pins(reference, &fixture.trust).unwrap());
    let digest = rsync::program_digest(&program).unwrap();
    let settings = json!({ "delete": true, "include_source_name": false });
    let (kind, local, remote, settings_key) = if upload {
        ("rsync_upload", &first, &destination, "existing")
    } else {
        ("rsync_download", &destination, &first, "existing_download")
    };
    let mut remote = json!({
        "kind": kind, "directory": true, "program": program, "local": local, "remote": remote,
        "mixed": sources, "approval": { "program": program, "digest": digest, "reference": reference,
            "pins": pins, "mixed": reviewed },
    });
    remote[settings_key] = settings;
    if case == Case::Recurring {
        remote["standing"] = json!({
            "settings": { "days": 2, "firings": 4,
                "budget": { "bytes": 4096, "entries": 32, "deletes": 1 } }
        });
        recurring_mixed(
            &fixture,
            &connections,
            profile,
            seed.job,
            remote,
            upload,
            (&destination, &first, &named),
        );
        return;
    }
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference, "remote": remote,
    }))
    .unwrap();
    let jobs = root.join("rsync-mixed-jobs.json");
    let graph = JobGraph::new(vec![
        Job::new(
            seed.job,
            "Reviewed mixed rsync",
            action.validated().unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs).unwrap();
    scheduler.set_connections(Some(&connections));
    scheduler.set_execution_allowed(true);
    let mut view = scheduler.new_view();
    let request = scheduler
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let mut policy = ExecutionPolicy::unrestricted(true);
    policy.rsync = Some(scheduler.rsync_run_context(&request).unwrap());

    if case == Case::StaleInput {
        // This losing input does not affect the projected output. Its approved bytes still matter.
        fs::write(&losing_file, b"changed after preview").unwrap();
    }
    let daemon = (case != Case::Revoked).then(|| fixture.spawn());
    let scheduler = (case != Case::Revoked).then_some(scheduler);
    let completion = run(
        &request,
        None,
        None,
        &policy,
        Credentials::resolve(
            &request.action,
            profile,
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .map(Some),
        Ok(None),
        || Ok(fixture.trust.clone()),
    );
    drop(scheduler);
    let trace = daemon.map_or_else(Vec::new, |daemon| daemon.join().unwrap().stderr);
    let stages = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(valid_stage)
        })
        .collect::<Vec<_>>();
    let detail = completion.detail.as_deref().unwrap_or_default();
    if matches!(case, Case::StaleInput | Case::Revoked) {
        assert_eq!(
            completion.outcome,
            RunOutcome::Blocked,
            "upload={upload} {case:?}: {completion:?}\n{}",
            String::from_utf8_lossy(&trace)
        );
        if case == Case::Revoked {
            assert!(detail.contains("no longer active"), "{detail}");
            assert_eq!(
                fixture.listener.accept().unwrap_err().kind(),
                ErrorKind::WouldBlock
            );
        } else {
            assert!(detail.contains("inputs changed"), "{detail}");
        }
        assert!(stages.is_empty());
        assert_eq!(
            fs::read(destination.join("report")).unwrap(),
            b"reviewed original file"
        );
        assert_eq!(
            fs::read(destination.join("scope/obsolete")).unwrap(),
            b"prune only in scope"
        );
        assert!(!destination.join("scope/new").exists());
        return;
    }

    assert_eq!(
        completion.outcome,
        RunOutcome::Success,
        "upload={upload} {case:?}: {completion:?}\n{}",
        String::from_utf8_lossy(&trace)
    );
    assert!(detail.contains("3 mixed sources verified"), "{detail}");
    assert_eq!(
        fs::read(destination.join("report")).unwrap(),
        b"first source wins"
    );
    assert_eq!(
        fs::read(destination.join("scope/new")).unwrap(),
        b"named directory contents"
    );
    assert!(destination.join("scope/empty").is_dir());
    assert!(!destination.join("new").exists());
    if existing {
        assert!(!destination.join("scope/obsolete").exists());
        assert_eq!(
            fs::read(destination.join("scope-sibling/keep")).unwrap(),
            b"similar prefix preserved"
        );
        assert_eq!(
            fs::read(destination.join("unrelated")).unwrap(),
            b"receiver-only entry"
        );
        assert_eq!(
            fs::metadata(destination.join("report")).unwrap().mode() & 0o777,
            0o640
        );
        let [stage] = stages.as_slice() else {
            panic!("expected exactly one private recovery stage: {stages:?}");
        };
        let recovery = stage.join("original");
        assert!(detail.contains(recovery.to_str().unwrap()), "{detail}");
        assert_eq!(
            fs::read(recovery.join("report")).unwrap(),
            b"reviewed original file"
        );
        assert_eq!(
            fs::read(recovery.join("scope/obsolete")).unwrap(),
            b"prune only in scope"
        );
    } else {
        assert!(stages.is_empty());
        assert_eq!(
            fs::read(destination.join("flattened")).unwrap(),
            b"flattened directory contents"
        );
        assert!(!destination.join("contents").exists());
    }
}

fn recurring_mixed(
    fixture: &Sshd,
    connections: &Store,
    profile: &Connection,
    id: JobId,
    mut remote: serde_json::Value,
    upload: bool,
    (destination, first, named): (&Path, &Path, &Path),
) {
    let root = &fixture.profile.0;
    let reference = remote["approval"]["reference"].clone();
    let predecessor = JobId::new("recurring-rsync-predecessor").unwrap();
    let job = |remote| {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference, "remote": remote,
        }))
        .unwrap();
        let job = Job::new(
            id.clone(),
            "Recurring mixed rsync",
            action.validated().unwrap(),
        )
        .unwrap();
        if upload {
            job.with_schedule(JobSchedule::default().with_interval_minutes(NonZeroU32::new(1)))
        } else {
            job.with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None)
        }
    };
    let issued = SystemTime::now() - Duration::from_secs(15 * 60);
    let grant = standing::Grant::new(&job(remote.clone()), &TimeZone::UTC, issued).unwrap();
    remote["standing"]["approval"] = json!(grant.approval);
    let mut jobs = vec![job(remote)];
    if !upload {
        let action =
            serde_json::from_value(json!({"connection": reference, "command": "true"})).unwrap();
        jobs.push(Job::new(predecessor.clone(), "Successful predecessor", action).unwrap());
    }
    let graph = JobGraph::new(jobs).unwrap();
    let path = root.join("recurring-mixed-jobs.json");
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    standing::Store::new(root.clone())
        .swap(&id, grant, |_| true)
        .unwrap();

    let read_only = named.join("read-only");
    if !upload {
        fs::create_dir(&read_only).unwrap();
        fs::write(read_only.join("content"), b"ordinary readable source").unwrap();
    }
    for iteration in 0..3 {
        match iteration {
            0 => fs::write(first, b"current first firing").unwrap(),
            1 => {
                fs::write(first, b"current second firing").unwrap();
                fs::rename(named.join("new"), named.join("next")).unwrap();
            }
            2 => {
                fs::write(first, b"over budget").unwrap();
                fs::remove_file(named.join("next")).unwrap();
                fs::remove_dir(named.join("empty")).unwrap();
            }
            _ => unreachable!(),
        }
        let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
        runtime.set_connections(Some(connections));
        runtime.set_execution_allowed(true);
        let scheduled_at = issued + Duration::from_secs(iteration * 120);
        runtime
            .initialize_schedules_at(TimeZone::UTC, scheduled_at)
            .unwrap();
        let (mut dispatched, finished_at) = if upload {
            let now = scheduled_at + Duration::from_secs(90);
            (
                runtime.update_runtime_at(Message::Tick, now),
                now + Duration::from_secs(10),
            )
        } else {
            let mut view = runtime.new_view();
            runtime.update_view(&mut view, Message::Select(predecessor.clone()));
            let preceding = runtime
                .update_view(&mut view, Message::Run(false))
                .pop()
                .unwrap();
            let preceding = Completion::from_result(preceding.id, Ok(0));
            (
                runtime.update_runtime(Message::Finished(preceding)),
                SystemTime::now(),
            )
        };
        let request = dispatched.pop().unwrap();
        assert_eq!(request.job, id);
        assert_eq!(
            request.trigger,
            if upload {
                RunTrigger::Scheduled
            } else {
                RunTrigger::Dependency
            }
        );
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.rsync = runtime.try_rsync_run_context(&request).unwrap();
        let daemon = fixture.spawn();
        let scratch_before = scratch_stages();
        if !upload {
            fs::set_permissions(&read_only, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let completion = run(
            &request,
            None,
            None,
            &policy,
            Credentials::resolve(
                &request.action,
                profile,
                &ServerAccess::default(),
                |_, _, _| Err("unexpected credential"),
            )
            .map(Some),
            Ok(None),
            || Ok(fixture.trust.clone()),
        );
        if !upload {
            assert_eq!(
                fs::metadata(&read_only).unwrap().permissions().mode() & 0o777,
                0o500
            );
            fs::set_permissions(&read_only, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let trace = daemon.join().unwrap().stderr;
        assert_eq!(scratch_stages(), scratch_before);
        let expected = if iteration == 2 {
            RunOutcome::Blocked
        } else {
            RunOutcome::Success
        };
        assert_eq!(
            completion.outcome,
            expected,
            "upload={upload}, firing={iteration}: {completion:?}\n{}",
            String::from_utf8_lossy(&trace)
        );
        let expected: &[u8] = if iteration == 0 {
            b"current first firing"
        } else {
            b"current second firing"
        };
        assert_eq!(fs::read(destination.join("report")).unwrap(), expected);
        assert!(!destination.join("scope/obsolete").exists());
        assert!(destination.join("scope-sibling/keep").is_file());
        assert!(destination.join("scope/empty").is_dir());
        assert_eq!(destination.join("scope/next").is_file(), iteration != 0);
        if iteration == 2 {
            assert!(
                completion
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("removal limit")
            );
        }
        runtime.update_runtime_at(Message::Finished(completion), finished_at);
    }
}

fn scratch_stages() -> std::collections::BTreeSet<std::ffi::OsString> {
    fs::read_dir(std::env::temp_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.as_encoded_bytes().starts_with(b"kr-"))
        .collect()
}
