use kortty_jobs::{DependencyMode, Job, JobGraph, SchedulerStore};

use crate::job_executor::JobAuth;
use crate::scheduler_window::{Page, SudoMode, sync};

use super::*;

fn replacement_fixture() -> (Connection, Vec<(Endpoint, HostKey)>, TrustStore) {
    assert_eq!(
        std::env::var("KORTTY_SSH_PRIVILEGED_UPLOAD_FIXTURE").as_deref(),
        Ok("isolated-v1")
    );
    assert_eq!(
        fs::read_to_string("/fixtures/.kortty-privileged-upload-fixture").unwrap(),
        "isolated-v1\n"
    );
    assert!(
        crate::profile::ssh_known_hosts()
            .unwrap()
            .starts_with("/profiles")
    );
    let (vault_account, pins) = privileged_fixture_route();
    let trust = crate::ssh_connection::trust_store().unwrap();
    for (endpoint, key) in &pins {
        assert_eq!(trust.remember(endpoint, key).unwrap(), HostKeyStatus::Known);
    }
    (vault_account, pins, trust)
}

fn replacement_credentials(
    reference: &ConnectionRef,
    account: &Connection,
    mode: SudoMode,
) -> Credentials {
    let credentials = Credentials::resolve_directory_browser(
        reference,
        account,
        &ServerAccess::default(),
        |_, _, _| panic!("The synthetic fixture key has no passphrase"),
    )
    .unwrap();
    if mode == SudoMode::Passwordless {
        return credentials;
    }
    let JobAuth::Saved(auth) = JobAuth::Saved(Ok(Box::new(credentials)))
        .with_sudo_passwords(|_| Ok(Secret::new("kortty-file-fixture".into())))
    else {
        unreachable!();
    };
    *auth.unwrap()
}

#[test]
#[ignore = "requires the isolated OpenSSH and installed sudo file replacement fixture"]
fn openssh_scheduled_privileged_upload_reviews_exact_bytes_and_preserves_the_original() {
    let (vault_account, pins, trust) = replacement_fixture();

    for (mode, name) in [
        (SudoMode::Vault, "vault-upload.bin"),
        (SudoMode::Passwordless, "passwordless-upload.bin"),
    ] {
        let profile = Profile::new();
        let mut account = vault_account.clone();
        account.id = 0;
        if mode == SudoMode::Passwordless {
            account.login.endpoint.username =
                std::env::var("KORTTY_SSH_PRIVILEGED_PASSWORDLESS_USER").unwrap();
        }
        let mut connections = Store::open(crate::profile::connections_path_at(&profile.0)).unwrap();
        account.id = connections.save(account.clone()).unwrap();
        let reference = ConnectionRef::from(&account);
        let destination = Path::new("/fixtures").join(name);
        let original = b"reviewed server file";
        fs::write(&destination, original).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o640)).unwrap();
        let source = profile.0.join("checked-upload");
        let bytes: Vec<_> = (0..196_610).map(|index| (index % 251) as u8).collect();
        fs::write(&source, &bytes).unwrap();
        let id = JobId::new("protected-upload").unwrap();
        let path = profile.0.join("jobs.json");
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {"kind":"upload", "local":source, "remote":destination}
        }))
        .unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Protected upload", action.clone()).unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let credentials = || replacement_credentials(&reference, &account, mode);
        let approve = || {
            let mut scheduler = SchedulerWindow::open(path.clone()).unwrap();
            scheduler.set_execution_allowed(true);
            let mut view = scheduler.new_view();
            scheduler.update_view(&mut view, Message::Select(id.clone()));
            scheduler.update_view(&mut view, Message::Page(Page::Action));
            scheduler.update_view(&mut view, Message::Sudo(mode));
            scheduler.update_view(&mut view, Message::PrivilegedReplaceExisting(true));
            let preview = scheduler
                .review_privileged(&mut view, |_| Ok(sync::fingerprints(&pins)))
                .unwrap()
                .unwrap();
            assert_eq!(
                preview.upload().unwrap().path.as_str(),
                destination.to_str().unwrap()
            );
            let completed =
                iced::futures::executor::block_on(preview.inspect(Some(Ok(credentials()))));
            scheduler.update_view(
                &mut view,
                Message::PrivilegedReplacementPreviewed(completed),
            );
            scheduler.update_view(&mut view, Message::ApprovePrivileged);
            scheduler.update_view(&mut view, Message::Save);
            let request = scheduler
                .update_view(&mut view, Message::Run(false))
                .pop()
                .expect("The actual protected upload review must enable the saved job");
            let mut policy = ExecutionPolicy::unrestricted(true);
            policy.privileged = scheduler.privileged_run_context(&request);
            (request, policy)
        };
        let execute = |request: &RunRequest<SshCommand>, policy: &ExecutionPolicy| {
            run(
                request,
                None,
                None,
                policy,
                Ok(Some(credentials())),
                Ok(None),
                || Ok(trust.clone()),
            )
        };
        let (request, policy) = approve();
        fs::write(&source, b"unreviewed bytes").unwrap();
        let changed = execute(&request, &policy);
        assert_eq!(changed.outcome, RunOutcome::Blocked);
        assert!(
            changed
                .detail
                .unwrap()
                .contains("approved transfer changed")
        );
        assert_eq!(fs::read(&destination).unwrap(), original);
        fs::write(&source, &bytes).unwrap();

        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(execute(&request, &policy).outcome, RunOutcome::Blocked);
        assert_eq!(fs::read(&destination).unwrap(), original);
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o640)).unwrap();

        let (request, policy) = approve();
        let metadata = fs::metadata(&destination).unwrap();
        let published = execute(&request, &policy);
        assert_eq!(
            published.outcome,
            RunOutcome::Success,
            "{:?}",
            published.detail
        );
        let journal = published.detail.unwrap();
        let recovery: PathBuf = serde_json::from_str(
            journal
                .split_once("Original retained at ")
                .unwrap()
                .1
                .trim_end_matches('.'),
        )
        .unwrap();
        assert!(recovery.starts_with("/fixtures"));
        assert_eq!(fs::read(&recovery).unwrap(), original);
        assert_eq!(fs::metadata(recovery).unwrap().ino(), metadata.ino());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        let current = fs::metadata(&destination).unwrap();
        assert_eq!(current.mode(), metadata.mode());
        assert_eq!(current.uid(), metadata.uid());
        assert_eq!(current.gid(), metadata.gid());
        assert_eq!(current.mtime(), metadata.mtime());
        assert_eq!(execute(&request, &policy).outcome, RunOutcome::Blocked);
    }
}

#[test]
#[ignore = "requires the isolated OpenSSH and installed sudo file replacement fixture"]
fn openssh_recurring_protected_replacement_advances_only_confirmed_receivers() {
    let (vault_account, pins, trust) = replacement_fixture();
    for (mode, user) in [
        (
            SudoMode::Vault,
            vault_account.login.endpoint.username.clone(),
        ),
        (
            SudoMode::Passwordless,
            std::env::var("KORTTY_SSH_PRIVILEGED_PASSWORDLESS_USER").unwrap(),
        ),
    ] {
        let profile = Profile::new();
        let mut account = vault_account.clone();
        account.id = 0;
        account.login.endpoint.username = user.clone();
        let mut connections = Store::open(crate::profile::connections_path_at(&profile.0)).unwrap();
        account.id = connections.save(account.clone()).unwrap();
        let reference = ConnectionRef::from(&account);
        let credentials = || replacement_credentials(&reference, &account, mode);
        for upload in [false, true] {
            let direction = if upload { "upload" } else { "download" };
            let path = profile.0.join(format!("{direction}-jobs.json"));
            let local = profile.0.join(format!("{direction}-file"));
            let remote = Path::new("/fixtures").join(format!("{user}-{direction}-recurring"));
            let (source, receiver) = if upload {
                (&local, &remote)
            } else {
                (&remote, &local)
            };
            let original = b"initial receiver".as_slice();
            fs::write(source, b"initial sender").unwrap();
            fs::set_permissions(source, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(receiver, original).unwrap();
            fs::set_permissions(receiver, fs::Permissions::from_mode(0o640)).unwrap();
            let id = JobId::new(format!("{direction}-replacement")).unwrap();
            let prepare_id = JobId::new("prepare").unwrap();
            let transfer: SshCommand = serde_json::from_value(json!({
                "connection": reference, "sudo": mode,
                "remote": {
                    "kind": "privileged", "direction": direction, "local": local, "remote": remote,
                    "replace_existing": true,
                    "standing": {"days": 2, "limits": {"max_firings": 3, "max_actions": 1, "max_bytes": 1_048_576}},
                },
            }))
            .unwrap();
            let prepare: SshCommand = serde_json::from_value(json!({
                "connection": reference, "command": "true",
            }))
            .unwrap();
            let graph = JobGraph::new(vec![
                Job::new(prepare_id.clone(), "Prepare", prepare).unwrap(),
                Job::new(id.clone(), "Recurring protected replacement", transfer)
                    .unwrap()
                    .with_enabled(false)
                    .with_dependencies(DependencyMode::Chain, vec![prepare_id.clone()], None),
            ])
            .unwrap();
            SchedulerStore::open(&path)
                .unwrap()
                .commit_with_actions(&graph, &[], &[])
                .unwrap();

            let mut scheduler = SchedulerWindow::open(path.clone()).unwrap();
            scheduler.set_execution_allowed(true);
            let mut view = scheduler.new_view();
            scheduler.update_view(&mut view, Message::Select(id.clone()));
            scheduler.update_view(&mut view, Message::Page(Page::Action));
            let preview = scheduler
                .review_privileged(&mut view, |_| Ok(sync::fingerprints(&pins)))
                .unwrap()
                .unwrap();
            let completed = iced::futures::executor::block_on(
                preview.inspect(upload.then(|| Ok(credentials()))),
            );
            scheduler.update_view(
                &mut view,
                Message::PrivilegedReplacementPreviewed(completed),
            );
            scheduler.update_view(&mut view, Message::ApprovePrivileged);
            scheduler.update_view(&mut view, Message::Enabled(true));
            scheduler.update_view(&mut view, Message::Save);
            drop(scheduler);

            let mut scheduler = SchedulerWindow::open(path).unwrap();
            scheduler.set_execution_allowed(true);
            let fire = |scheduler: &mut SchedulerWindow| {
                let mut view = scheduler.new_view();
                scheduler.update_view(&mut view, Message::Select(prepare_id.clone()));
                let predecessor = scheduler
                    .update_view(&mut view, Message::Run(false))
                    .pop()
                    .unwrap();
                let request = scheduler
                    .update_runtime(Message::Finished(Completion::from_result(
                        predecessor.id,
                        Ok(0),
                    )))
                    .pop()
                    .expect("the current predecessor round starts the transfer");
                assert_eq!(request.job, id);
                assert_eq!(request.trigger, RunTrigger::Dependency);
                let mut policy = ExecutionPolicy::unrestricted(true);
                policy.privileged = Some(
                    scheduler
                        .privileged_run_context(&request)
                        .expect("approved Chain firing"),
                );
                (request, policy)
            };
            let execute = |request: &RunRequest<SshCommand>, policy: &ExecutionPolicy| {
                run(
                    request,
                    None,
                    None,
                    policy,
                    Ok(Some(credentials())),
                    Ok(None),
                    || Ok(trust.clone()),
                )
            };
            fs::write(source, vec![42; 1_048_577]).unwrap();
            let (request, policy) = fire(&mut scheduler);
            let over_budget = execute(&request, &policy);
            assert_ne!(over_budget.outcome, RunOutcome::Success);
            assert_eq!(fs::read(receiver).unwrap(), original);
            scheduler.update_runtime(Message::Finished(over_budget));

            let mut previous = original;
            for bytes in [
                b"first firing payload".as_slice(),
                b"changed source on the second firing",
            ] {
                fs::write(source, bytes).unwrap();
                let (request, policy) = fire(&mut scheduler);
                let completion = execute(&request, &policy);
                assert_eq!(
                    completion.outcome,
                    RunOutcome::Success,
                    "{direction} {mode:?}: {:?}",
                    completion.detail
                );
                let recovery: PathBuf = serde_json::from_str(
                    completion
                        .detail
                        .as_ref()
                        .unwrap()
                        .split_once("Original retained at ")
                        .unwrap()
                        .1
                        .trim_end_matches('.'),
                )
                .unwrap();
                assert_eq!(fs::read(recovery).unwrap(), previous);
                assert_eq!(fs::read(receiver).unwrap(), bytes);
                assert_eq!(fs::metadata(receiver).unwrap().mode() & 0o7777, 0o640);
                assert_eq!(execute(&request, &policy).outcome, RunOutcome::Blocked);
                scheduler.update_runtime(Message::Finished(completion));
                previous = bytes;
            }

            fs::write(receiver, b"external change").unwrap();
            let (request, policy) = fire(&mut scheduler);
            let changed = execute(&request, &policy);
            assert_eq!(
                changed.outcome,
                RunOutcome::Blocked,
                "{direction} {mode:?}: {:?}",
                changed.detail
            );
            assert_eq!(fs::read(receiver).unwrap(), b"external change");
            scheduler.update_runtime(Message::Finished(changed));
        }
    }
}

#[test]
#[ignore = "requires the isolated OpenSSH sudo transfer fixture"]
fn openssh_protected_folder_download_merge_preserves_the_existing_tree() {
    protected_folder_merge(sync::Direction::Download);
}

#[test]
#[ignore = "requires the isolated OpenSSH and installed sudo folder merge fixture"]
fn openssh_protected_folder_upload_merge_preserves_the_existing_tree() {
    protected_folder_merge(sync::Direction::Upload);
}

fn protected_folder_merge(direction: sync::Direction) {
    use crate::job_executor::remote::sync::preview_with_trust;

    let upload = direction == sync::Direction::Upload;
    let (vault_account, _, trust) = replacement_fixture();
    for mode in [SudoMode::Vault, SudoMode::Passwordless] {
        let profile = Profile::new();
        let mut account = vault_account.clone();
        account.id = 0;
        if mode == SudoMode::Passwordless {
            account.login.endpoint.username =
                std::env::var("KORTTY_SSH_PRIVILEGED_PASSWORDLESS_USER").unwrap();
        }
        let mut connections = Store::open(crate::profile::connections_path_at(&profile.0)).unwrap();
        account.id = connections.save(account.clone()).unwrap();
        let reference = ConnectionRef::from(&account);
        let credentials = || replacement_credentials(&reference, &account, mode);
        let local = profile.0.join("receiver");
        let remote = Path::new("/fixtures").join(format!(
            "{}-merge-{}",
            account.login.endpoint.username,
            if upload { "receiver" } else { "source" }
        ));
        let (source, destination) = if upload {
            (&local, &remote)
        } else {
            (&remote, &local)
        };
        for directory in [&local, &remote] {
            fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
            fs::DirBuilder::new()
                .mode(0o700)
                .create(directory.join("existing"))
                .unwrap();
        }
        let payload = source.join("existing/report.txt");
        fs::write(&payload, b"protected replacement").unwrap();
        fs::write(source.join("existing/same.txt"), b"identical").unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(source.join("new-folder"))
            .unwrap();
        fs::write(source.join("new-folder/data.txt"), b"protected additions").unwrap();
        let changed = destination.join("existing/report.txt");
        let unchanged = destination.join("existing/same.txt");
        fs::write(&changed, b"old receiver").unwrap();
        fs::set_permissions(&changed, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(&unchanged, b"identical").unwrap();
        fs::write(
            destination.join("local-note"),
            b"keep destination-only content",
        )
        .unwrap();
        let original_nodes = [destination, &destination.join("existing"), &unchanged]
            .map(|path| fs::metadata(path).unwrap().ino());

        let kind = if upload {
            "privileged_tree_upload"
        } else {
            "privileged_tree_download"
        };
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "sudo": mode,
            "remote": {"kind": kind, "local": local,
                "remote": remote, "direction": direction, "replace_changed": true},
        }))
        .unwrap();
        let id = JobId::new("protected-folder-merge").unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Merge a protected folder", action)
                .unwrap()
                .with_enabled(false),
        ])
        .unwrap();
        let jobs = profile.0.join("jobs.json");
        SchedulerStore::open(&jobs)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut runtime = SchedulerWindow::open(jobs).unwrap();
        runtime.set_execution_allowed(true);
        let mut view = runtime.new_view();
        runtime.update_view(&mut view, Message::Select(id));
        runtime.update_view(&mut view, Message::Page(Page::Action));

        let approve = |runtime: &mut SchedulerWindow, view: &mut _| {
            let request = runtime.begin_sync_review(view).unwrap();
            let ticket = request.ticket.clone();
            let pinned = trust.clone();
            let review = preview_with_trust(request, credentials(), move || Ok(pinned))
                .recv_blocking()
                .unwrap()
                .unwrap();
            assert_eq!(review.preview.replacements.len(), 1);
            assert_eq!(review.preview.replacements[0].target, "existing/report.txt");
            assert_eq!(
                review.preview.paths,
                [
                    (true, "new-folder".into()),
                    (false, "new-folder/data.txt".into())
                ]
            );
            runtime.update_view(view, Message::SyncPreviewed(ticket, Ok(Arc::new(review))));
            runtime.update_view(view, Message::ApproveSync);
            runtime.update_view(view, Message::Enabled(true));
            runtime.update_view(view, Message::Save);
        };
        let execute = |runtime: &mut SchedulerWindow, view: &mut _| {
            let request = runtime
                .update_view(view, Message::Run(false))
                .pop()
                .unwrap();
            let mut policy = ExecutionPolicy::unrestricted(true);
            policy.sync = Some(runtime.sync_run_context(&request).unwrap());
            run(
                &request,
                None,
                None,
                &policy,
                Ok(Some(credentials())),
                Ok(None),
                || Ok(trust.clone()),
            )
        };

        approve(&mut runtime, &mut view);
        fs::write(&changed, b"concurrent local edit").unwrap();
        let stale = execute(&mut runtime, &mut view);
        assert_eq!(
            stale.outcome,
            RunOutcome::Blocked,
            "{mode:?}: {:?}",
            stale.detail
        );
        assert_eq!(fs::read(&changed).unwrap(), b"concurrent local edit");
        assert!(!destination.join("new-folder").exists());
        runtime.update_runtime(Message::Finished(stale));
        runtime.update_view(&mut view, Message::Page(Page::Action));
        approve(&mut runtime, &mut view);
        if upload {
            fs::write(&payload, b"unapproved local bytes").unwrap();
            let stale = execute(&mut runtime, &mut view);
            assert_eq!(
                stale.outcome,
                RunOutcome::Blocked,
                "{mode:?}: {:?}",
                stale.detail
            );
            assert_eq!(fs::read(&changed).unwrap(), b"concurrent local edit");
            assert!(!destination.join("new-folder").exists());
            runtime.update_runtime(Message::Finished(stale));
            fs::write(&payload, b"protected replacement").unwrap();
            runtime.update_view(&mut view, Message::Page(Page::Action));
            approve(&mut runtime, &mut view);
        }
        let completed = execute(&mut runtime, &mut view);
        assert_eq!(
            completed.outcome,
            RunOutcome::Success,
            "{mode:?}: {:?}",
            completed.detail
        );
        let detail = completed.detail.as_ref().unwrap();
        assert!(detail.contains("replaced 1 files"));
        let prefix = detail
            .split_once(".kortty-")
            .unwrap()
            .1
            .split('*')
            .next()
            .unwrap();
        let backup = fs::read_dir(destination.join("existing"))
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!(".kortty-{prefix}"))
            })
            .unwrap();
        let original = if upload {
            assert_eq!(
                fs::read(backup.path().join("replaced-name")).unwrap(),
                b"report.txt"
            );
            "original"
        } else {
            "report.txt"
        };
        assert_eq!(
            fs::read(backup.path().join(original)).unwrap(),
            b"concurrent local edit"
        );
        assert_eq!(fs::read(&changed).unwrap(), b"protected replacement");
        assert_eq!(fs::metadata(&changed).unwrap().mode() & 0o7777, 0o640);
        assert_eq!(
            fs::read(destination.join("new-folder/data.txt")).unwrap(),
            b"protected additions"
        );
        assert_eq!(
            fs::read(destination.join("local-note")).unwrap(),
            b"keep destination-only content"
        );
        assert_eq!(
            [destination, &destination.join("existing"), &unchanged]
                .map(|path| fs::metadata(path).unwrap().ino()),
            original_nodes
        );
        runtime.update_runtime(Message::Finished(completed));
        let replay = execute(&mut runtime, &mut view);
        assert_eq!(replay.outcome, RunOutcome::Blocked);
        runtime.update_runtime(Message::Finished(replay));
    }
}
