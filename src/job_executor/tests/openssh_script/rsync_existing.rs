use std::sync::atomic::AtomicBool;

use kortty_jobs::{Job, JobGraph, SchedulerStore};

use crate::scheduler_window::{RemotePath, sync};

use super::*;

#[test]
#[ignore = "requires native OpenSSH, rsync, and the current kortty server helper"]
fn openssh_existing_rsync_reviews_each_receiver_and_publishes_only_the_reviewed_tree() {
    for named in [false, true] {
        existing_upload(named);
    }
}

fn existing_upload(named: bool) {
    let first = Sshd::new();
    let second = Sshd::new();
    let servers = [&first, &second];
    let root = &first.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &servers, "true", vec![]);
    let references: Vec<_> = seed.action.targets().cloned().collect();
    let trust = TrustStore::from_pins(servers.map(Sshd::pin).into()).unwrap();
    let program = PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
        .canonicalize()
        .unwrap();
    assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
    let source = root.join("source");
    fs::create_dir_all(source.join("empty")).unwrap();
    fs::write(source.join("same-name"), b"new rsync bytes").unwrap();
    let inventory = crate::job_executor::rsync::upload_tree_inventory(&source).unwrap();
    let digest = crate::job_executor::rsync::program_digest(&program).unwrap();
    let configured = [root.join("first-receiver"), root.join("second-receiver")];
    let effective = configured.each_ref().map(|path| {
        if named {
            path.join("source")
        } else {
            path.clone()
        }
    });
    let markers = servers.map(|server| server.profile.0.join("existing-helper-enabled"));
    let settings = json!({ "delete": named, "include_source_name": named });
    let mut approvals = Vec::new();
    for (index, fixture) in servers.into_iter().enumerate() {
        fs::write(&markers[index], b"enable fixture helper").unwrap();
        configure_rsync_fixture(fixture, Some(&markers[index]));
        let destination = &effective[index];
        fs::create_dir_all(destination).unwrap();
        fs::write(destination.join("same-name"), b"old receiver bytes").unwrap();
        fs::set_permissions(
            destination.join("same-name"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        fs::write(destination.join("remote-only"), b"receiver-only bytes").unwrap();
        if named {
            fs::write(
                configured[index].join("sibling"),
                b"outside the reviewed tree",
            )
            .unwrap();
        }
        let reference = &references[index];
        let creds = Credentials::resolve_directory_browser(
            reference,
            connections.get(reference.id).unwrap(),
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .unwrap();
        let daemon = fixture.spawn();
        let remote = RemotePath::try_from(destination.to_str().unwrap().to_owned()).unwrap();
        let reviewed = crate::job_executor::rsync::preview_existing(
            creds,
            reference,
            &remote,
            &trust,
            Arc::new(AtomicBool::new(false)),
            || Ok(()),
        );
        let server = daemon.join().unwrap();
        let snapshot = reviewed.unwrap_or_else(|error| {
            panic!(
                "Could not review receiver {index} ({error})\n{}",
                String::from_utf8_lossy(&server.stderr)
            )
        });
        let pins = sync::fingerprints(&sync::pins(reference, &trust).unwrap());
        approvals.push(json!({
            "program": program, "digest": digest, "reference": reference, "pins": pins,
            "directory_upload": { "source": source, "destination": configured[index], "inventory": inventory },
            "existing": { "destination": effective[index], "snapshot": snapshot, "settings": settings },
        }));
    }
    let action: SshCommand = serde_json::from_value(json!({
        "connection": references[0], "additional_connections": [references[1]],
        "remote": {
            "kind": "rsync_upload", "directory": true, "program": program, "local": source,
            "remote": configured[0], "existing": settings, "approval": approvals[0],
            "routes": [{ "profile": references[1].id, "destination": configured[1], "approval": approvals[1] }],
        },
    })).unwrap();
    let jobs = root.join("rsync-existing-jobs.json");
    let graph = JobGraph::new(vec![
        Job::new(
            seed.job,
            "Reviewed existing receivers",
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
    policy.rsync = scheduler.rsync_run_context(&request);
    assert!(policy.rsync.is_some());

    if named {
        fs::write(effective[1].join("same-name"), b"changed after review").unwrap();
    } else {
        fs::remove_file(&markers[1]).unwrap();
    }
    let running = servers.map(Sshd::spawn);
    let completion = batch::run(
        &request,
        None,
        &policy,
        Credentials::resolve_batch(
            &request.action,
            &connections,
            &policy.server_access,
            |_, _, _| Err("unexpected credential"),
        ),
        Ok(None),
        || Ok(trust),
    );
    let traces = running.map(|server| server.join().unwrap().stderr);
    let detail = completion.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("1 of 2 targets succeeded"),
        "{completion:?}\n{}\n{}",
        String::from_utf8_lossy(&traces[0]),
        String::from_utf8_lossy(&traces[1])
    );
    assert_eq!(
        fs::read(effective[0].join("same-name")).unwrap(),
        b"new rsync bytes"
    );
    assert_eq!(
        fs::metadata(effective[0].join("same-name")).unwrap().mode() & 0o777,
        0o640
    );
    assert!(effective[0].join("empty").is_dir());
    assert_eq!(effective[0].join("remote-only").exists(), !named);
    assert!(!effective[1].join("empty").exists());
    assert!(effective[1].join("remote-only").exists());
    let unchanged: &[u8] = if named {
        b"changed after review"
    } else {
        b"old receiver bytes"
    };
    assert_eq!(fs::read(effective[1].join("same-name")).unwrap(), unchanged);
    if named {
        assert_eq!(
            fs::read(configured[0].join("sibling")).unwrap(),
            b"outside the reviewed tree"
        );
    }
    let parent = effective[0].parent().unwrap();
    let recovery = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(kortty_server_archive::rsync_tree::valid_stage)
        })
        .unwrap()
        .join("original");
    assert_eq!(
        fs::read(recovery.join("same-name")).unwrap(),
        b"old receiver bytes"
    );
    assert!(detail.contains(recovery.to_str().unwrap()), "{detail}");
}
