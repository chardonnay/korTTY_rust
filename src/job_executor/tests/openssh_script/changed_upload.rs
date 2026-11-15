use jiff::Timestamp;
use kortty_jobs::{Job, JobGraph, JobSchedule, SchedulerStore, TimeZone};

use super::*;

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_changed_upload_keeps_private_recovery_and_spends_one_approval() {
    const ORIGINAL: &[u8] = b"previous server";
    const COMPETING: &[u8] = b"external change";
    const INCOMING: &[u8] = b"new local file";
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", Vec::new());
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let local = root.join("changed-local");
    let remote = root.join("changed-remote");
    for directory in [&local, &remote] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    let source = local.join("changed");
    let target = remote.join("changed");
    fs::write(&source, INCOMING).unwrap();
    fs::write(local.join("added"), b"new entry").unwrap();
    let set_original = |contents, mode| {
        fs::write(&target, contents).unwrap();
        let file = fs::OpenOptions::new().write(true).open(&target).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(60))
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(mode))
            .unwrap();
    };
    set_original(ORIGINAL, 0o600);
    let original = fs::metadata(&target).unwrap();

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let due = SystemTime::UNIX_EPOCH + Duration::from_secs((now.as_secs() / 60 + 10) * 60);
    let local_due = Timestamp::try_from(due).unwrap().to_zoned(TimeZone::UTC);
    let schedule = JobSchedule::default()
        .with_active_dates(Some(local_due.date()), Some(local_due.date()))
        .unwrap()
        .with_fixed_times([local_due.time()]);
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "remote": { "kind": "sync", "direction": "upload", "local": local,
                    "remote": remote, "replace_changed": true },
    }))
    .unwrap();
    let id = JobId::new("changed-upload").unwrap();
    let path = root.join("changed-upload.json");
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Reviewed changed upload", action)
            .unwrap()
            .with_schedule(schedule),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    approve_pinned_copy(&mut runtime, &id, &fixture, profile);
    drop(runtime);

    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.set_execution_allowed(true);
    runtime
        .initialize_schedules_at(TimeZone::UTC, due - Duration::from_secs(1))
        .unwrap();
    let execute = |runtime: &SchedulerWindow, request: &RunRequest<SshCommand>| {
        let daemon = fixture.spawn();
        let outcome = run_saved_route(
            &fixture,
            profile,
            request,
            runtime.sync_run_context(request),
        );
        let log = daemon.join().unwrap();
        (outcome, log)
    };
    for (contents, mode, expected) in [
        (COMPETING, 0o600, "file content changed"),
        (ORIGINAL, 0o640, "owner-only permissions"),
    ] {
        set_original(contents, mode);
        let request = start_copy_manually(&mut runtime, &id);
        let (outcome, log) = execute(&runtime, &request);
        assert_eq!(
            outcome.outcome,
            RunOutcome::Blocked,
            "{:?}\n{}",
            outcome.detail,
            String::from_utf8_lossy(&log.stderr)
        );
        assert!(
            outcome
                .detail
                .as_deref()
                .is_some_and(|note| note.contains(expected)),
            "{:?}",
            outcome.detail
        );
        assert_eq!(fs::read(&target).unwrap(), contents);
        assert!(!remote.join("added").exists());
        assert_eq!(fs::read_dir(&remote).unwrap().count(), 1);
        assert!(
            runtime
                .update_runtime(Message::Finished(outcome))
                .is_empty()
        );
    }
    set_original(ORIGINAL, 0o600);
    let request = only_copy_start(
        runtime.update_runtime_at(Message::Tick, due),
        &id,
        RunTrigger::Scheduled,
    );
    let (outcome, log) = execute(&runtime, &request);
    assert_eq!(
        outcome.outcome,
        RunOutcome::Success,
        "{:?}\n{}",
        outcome.detail,
        String::from_utf8_lossy(&log.stderr)
    );
    let detail = outcome.detail.as_deref().unwrap();
    assert!(detail.contains("Replaced 1 server files."), "{detail}");
    assert!(detail.contains("Copied 1 new files"), "{detail}");
    assert_eq!(fs::read(&target).unwrap(), INCOMING);
    assert_eq!(fs::read(remote.join("added")).unwrap(), b"new entry");
    let installed = fs::metadata(&target).unwrap();
    assert_eq!(installed.permissions().mode() & 0o7777, 0o600);
    assert_eq!(installed.uid(), original.uid());
    assert_eq!(installed.gid(), original.gid());
    let backups = fs::read_dir(&remote)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".kortty-")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = &backups[0];
    let name = backup.file_name();
    let name = name.to_str().unwrap();
    assert!(detail.contains(&name[..24]), "{detail}");
    assert_eq!(backup.metadata().unwrap().mode() & 0o7777, 0o700);
    assert_eq!(fs::read(backup.path().join("changed")).unwrap(), ORIGINAL);
    let replay = run_saved_route(
        &fixture,
        profile,
        &request,
        runtime.sync_run_context(&request),
    );
    assert_eq!(replay.outcome, RunOutcome::Blocked);
    assert_eq!(
        fixture.listener.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_changed_upload_restores_after_authority_revocation() {
    changed_upload_restores_after_authority_revocation(false);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_standing_changed_upload_restores_after_authority_revocation() {
    changed_upload_restores_after_authority_revocation(true);
}

fn changed_upload_restores_after_authority_revocation(standing: bool) {
    use std::cell::Cell;
    use std::io;

    use crate::sftp_window::Side;
    use crate::sftp_window::scheduled::{Comparison, StandingLimits, UploadContext};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", Vec::new());
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let local = root.join("revoked-local");
    let remote = root.join("revoked-remote");
    for directory in [&local, &remote] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    fs::write(local.join("changed"), b"replace the original").unwrap();
    fs::write(local.join("added"), b"publish after replacements").unwrap();
    let target = remote.join("changed");
    fs::write(&target, b"restore the original").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::metadata(&target).unwrap();
    let credentials = Credentials::resolve_directory_browser(
        reference,
        profile,
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    let (endpoint, jumps) = credentials
        .into_route()
        .unwrap_or_else(|error| panic!("{}", error.detail));
    let pins = crate::scheduler_window::sync::pins(reference, &fixture.trust).unwrap();
    let daemon = fixture.spawn();
    use crate::job_executor::remote::sync::{open_pinned, supervise};
    let mut session = open_pinned(endpoint, jumps, &pins, || false)
        .unwrap_or_else(|error| panic!("{}", error.detail));
    let route = crate::workspace::SshTarget::scheduled_route(reference);
    let peer = session.verified_host_key().unwrap().clone();
    let compare = || Comparison {
        local: local.clone(),
        remote: remote.as_os_str().as_encoded_bytes().to_vec(),
        excludes: "",
        replace_changed: true,
        to: Side::Remote,
        upload: Some(UploadContext {
            route: &route,
            peer: &peer,
            profile: root,
        }),
    };
    let result = supervise(&mut session, &|| false, |client, stopped, disconnected| {
        let claimed = Cell::new(false);
        let revoked = Cell::new(false);
        let authorize = || {
            if revoked.get() || !target.exists() {
                revoked.set(true);
                return Err(io::Error::other("fixture authorization revoked"));
            }
            Ok(())
        };
        let result = if standing {
            let limits = StandingLimits {
                actions: 2,
                bytes: 64,
            };
            let preview = compare()
                .preview_standing(client, limits, &stopped)
                .unwrap();
            compare().apply_standing(
                client,
                preview.root_binding,
                limits,
                &stopped,
                &disconnected,
                |actions, bytes| {
                    assert_eq!(actions, 2);
                    assert_eq!(
                        bytes,
                        u64::try_from(
                            b"replace the original".len() + b"publish after replacements".len()
                        )
                        .unwrap()
                    );
                    claimed.set(true);
                    authorize()
                },
            )
        } else {
            let preview = compare().preview(client, &stopped).unwrap();
            compare().apply_authorized(
                client,
                preview.proof,
                &stopped,
                &disconnected,
                || {
                    claimed.set(true);
                    Ok(())
                },
                &authorize,
            )
        }
        .unwrap();
        assert!(claimed.get() && revoked.get());
        result
    })
    .unwrap_or_else(|error| panic!("{}", error.detail));
    drop(session);
    daemon.join().unwrap();
    assert_eq!(result.replaced, 0);
    assert_eq!(result.copied, 0);
    let note = result.error.unwrap();
    assert!(note.contains("fixture authorization revoked"), "{note}");
    assert!(note.contains("The displaced file was restored"), "{note}");
    assert_eq!(fs::read(&target).unwrap(), b"restore the original");
    assert_eq!(fs::metadata(&target).unwrap().ino(), original.ino());
    assert!(!remote.join("added").exists());
}
