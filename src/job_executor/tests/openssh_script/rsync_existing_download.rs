use std::sync::atomic::AtomicBool;

use kortty_jobs::{Job, JobGraph, SchedulerStore};

use crate::scheduler_window::sync;

use super::*;

mod recurring_file;

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_rsync_download_merges_or_mirrors_only_the_reviewed_local_tree() {
    for named in [false, true] {
        existing_download(named);
    }
}

fn existing_download(named: bool) {
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
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    fs::write(source.join("nested/payload"), b"new rsync bytes").unwrap();
    let program_digest = crate::job_executor::rsync::program_digest(&program).unwrap();
    let configured = [root.join("first-receiver"), root.join("second-receiver")];
    let effective = configured.each_ref().map(|path| {
        if named {
            path.join("source")
        } else {
            path.clone()
        }
    });
    let settings = json!({ "delete": named, "include_source_name": named });
    let mut approvals = Vec::new();
    for (index, destination) in effective.iter().enumerate() {
        fs::create_dir_all(destination.join("nested")).unwrap();
        let original = destination.join("nested/payload");
        fs::write(&original, b"old receiver bytes").unwrap();
        fs::set_permissions(&original, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(destination.join("local-only"), b"receiver-only bytes").unwrap();
        if named {
            fs::write(configured[index].join("sibling"), b"outside reviewed tree").unwrap();
        }
        let reference = &references[index];
        let snapshot =
            crate::job_executor::rsync::review_local_existing(destination, &AtomicBool::new(false))
                .unwrap();
        let pins = sync::fingerprints(&sync::pins(reference, &trust).unwrap());
        approvals.push(json!({
            "program": program, "digest": program_digest, "reference": reference, "pins": pins,
            "directory": { "source": source, "destination": configured[index] },
            "existing_download": { "destination": destination, "snapshot": snapshot, "settings": settings },
        }));
    }
    let action: SshCommand = serde_json::from_value(json!({
        "connection": references[0], "additional_connections": [references[1]],
        "remote": {
            "kind": "rsync_download", "directory": true, "program": program,
            "local": configured[0], "remote": source, "existing_download": settings,
            "approval": approvals[0],
            "routes": [{ "profile": references[1].id, "destination": configured[1], "approval": approvals[1] }],
        },
    }))
    .unwrap();
    let jobs = root.join("rsync-existing-download-jobs.json");
    let graph = JobGraph::new(vec![
        Job::new(
            seed.job,
            "Reviewed local receivers",
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

    fs::write(effective[1].join("nested/payload"), b"changed after review").unwrap();
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
    let published = effective[0].join("nested/payload");
    assert_eq!(fs::read(&published).unwrap(), b"new rsync bytes");
    assert_eq!(fs::metadata(&published).unwrap().mode() & 0o777, 0o640);
    assert!(effective[0].join("nested/empty").is_dir());
    assert_eq!(effective[0].join("local-only").exists(), !named);
    assert!(!effective[1].join("nested/empty").exists());
    assert_eq!(
        fs::read(effective[1].join("nested/payload")).unwrap(),
        b"changed after review"
    );
    assert!(effective[1].join("local-only").is_file());
    if named {
        assert_eq!(
            fs::read(configured[0].join("sibling")).unwrap(),
            b"outside reviewed tree"
        );
    }
    let recovery = fs::read_dir(effective[0].parent().unwrap())
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
        fs::read(recovery.join("nested/payload")).unwrap(),
        b"old receiver bytes"
    );
    assert!(detail.contains(recovery.to_str().unwrap()), "{detail}");
}
