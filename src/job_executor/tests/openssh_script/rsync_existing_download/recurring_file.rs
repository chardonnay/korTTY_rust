use std::num::NonZeroU32;
use std::sync::atomic::Ordering;

use kortty_jobs::{DependencyMode, JobSchedule, TimeZone};

use crate::job_executor::rsync;
use crate::scheduler_window::rsync::standing;
use crate::sftp_window::ReviewedFileReceiver;

use super::*;

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_rsync_download_reviews_each_confirmed_local_file() {
    let program = PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
        .canonicalize()
        .unwrap();
    assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
    for chain in [false, true] {
        recurring_download(&program, chain);
    }
}

fn recurring_download(program: &Path, chain: bool) {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    configure_rsync_command_server(&fixture, program, "  true) exit 0 ;;\n");
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", vec![]);
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let source = root.join("file-on-server");
    let destination = root.join("reviewed-local-file");
    let path = root.join("recurring-file-download-jobs.json");
    let id = JobId::new("recurring-file-download").unwrap();
    let predecessor = JobId::new("recurring-file-predecessor").unwrap();
    let issued = SystemTime::now() - Duration::from_secs(15 * 60);
    let pins = sync::fingerprints(&sync::pins(reference, &fixture.trust).unwrap());
    let digest = rsync::program_digest(program).unwrap();
    let review = || {
        let mut approval = json!({
            "program": program, "digest": digest, "reference": reference, "pins": pins,
            "download": { "source": source, "destination": destination },
        });
        let exists = destination.is_file();
        let mut remote = json!({
            "kind": "rsync_download", "program": program, "local": destination, "remote": source,
            "standing": {"settings": {"days": 2, "firings": 4,
                "budget": {"bytes": 4 * 1024 * 1024, "entries": 8, "deletes": 0}}},
        });
        if exists {
            let snapshot = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
            approval["existing_file_download"] =
                json!({"destination": destination, "snapshot": snapshot});
            remote["existing_download"] = json!({"include_source_name": false, "delete": false});
        } else {
            let snapshot =
                rsync::review_local_file_vacancy(&destination, &AtomicBool::new(false)).unwrap();
            approval["vacant_file_download"] =
                json!({"destination": destination, "snapshot": snapshot});
            remote["allow_vacant_file_download"] = json!(true);
        }
        remote["approval"] = approval;
        remote
    };
    let job = |remote| {
        let action: SshCommand =
            serde_json::from_value(json!({"connection": reference, "remote": remote})).unwrap();
        let job = Job::new(
            id.clone(),
            "Recurring file download",
            action.validated().unwrap(),
        )
        .unwrap();
        if chain {
            job.with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None)
        } else {
            job.with_schedule(JobSchedule::default().with_interval_minutes(NonZeroU32::new(1)))
        }
    };
    let approve = |mut reviewed: serde_json::Value, issued_at| {
        let grant =
            standing::Grant::new(&job(reviewed.clone()), &TimeZone::UTC, issued_at).unwrap();
        reviewed["standing"]["approval"] = json!(grant.approval);
        let mut jobs = vec![job(reviewed)];
        if chain {
            let command =
                serde_json::from_value(json!({ "connection": reference, "command": "true" }))
                    .unwrap();
            jobs.push(Job::new(predecessor.clone(), "Real SSH predecessor", command).unwrap());
        }
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
        standing::Store::new(root.clone())
            .swap(&id, grant, |_| true)
            .unwrap();
    };
    let execute = |request: &RunRequest<SshCommand>, policy: &ExecutionPolicy| {
        let saved = Credentials::resolve(
            &request.action,
            profile,
            &policy.server_access,
            |_, _, _| Err("unexpected credential"),
        )
        .map(Some);
        run(request, None, None, policy, saved, Ok(None), || {
            Ok(fixture.trust.clone())
        })
    };
    let fire = |iteration: u64, expected| {
        let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
        runtime.set_connections(Some(&connections));
        runtime.set_execution_allowed(true);
        let due = issued + Duration::from_secs(iteration * 120);
        runtime.initialize_schedules_at(TimeZone::UTC, due).unwrap();
        let (fired, trigger, finished_at) = if chain {
            let preceding = execute_predecessor(&fixture, &mut runtime, &predecessor, execute);
            (
                runtime.update_runtime(Message::Finished(preceding)),
                RunTrigger::Dependency,
                SystemTime::now(),
            )
        } else {
            let now = due + Duration::from_secs(90);
            (
                runtime.update_runtime_at(Message::Tick, now),
                RunTrigger::Scheduled,
                now + Duration::from_secs(10),
            )
        };
        let [request] = fired.as_slice() else {
            panic!("expected one reviewed recurring run");
        };
        assert_eq!(request.job, id);
        assert_eq!(request.trigger, trigger);
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.rsync = runtime.try_rsync_run_context(request).unwrap();
        assert!(policy.rsync.is_some());
        let (completion, trace) = serve_if_needed(&fixture, || execute(request, &policy));
        assert_eq!(completion.outcome, expected, "{completion:?}\n{trace}");
        if expected == RunOutcome::Success {
            assert!(trace.contains("Accepted publickey"), "{trace}");
        }
        runtime.update_runtime_at(Message::Finished(completion.clone()), finished_at);
        completion
    };

    fs::write(&source, b"first native firing").unwrap();
    approve(review(), issued);
    let completion = fire(0, RunOutcome::Success);
    assert_eq!(fs::read(&destination).unwrap(), b"first native firing");
    assert!(!completion.detail.unwrap().contains("original is retained"));
    let first = identity(&destination);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o600);

    // Cross the remote shell pipe's capacity during a real transfer.
    let second = vec![0xa5; 2 * 1024 * 1024];
    fs::write(&source, &second).unwrap();
    let completion = fire(1, RunOutcome::Success);
    let recovered = recovery_path(&completion);
    assert_eq!(identity(&recovered), first);
    assert_eq!(fs::read(&recovered).unwrap(), b"first native firing");
    assert_eq!(fs::read(&destination).unwrap(), second);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o600);

    let displaced = root.join("manually-displaced");
    fs::rename(&destination, &displaced).unwrap();
    fs::copy(&displaced, &destination).unwrap();
    let substitute = identity(&destination);
    assert_ne!(substitute, identity(&displaced));
    fs::write(&source, b"third after explicit new review").unwrap();
    let completion = fire(2, RunOutcome::Blocked);
    assert_eq!(identity(&destination), substitute);
    assert_eq!(fs::read(&destination).unwrap(), second);
    assert!(!completion.detail.unwrap().contains("original is retained"));

    approve(review(), issued + Duration::from_secs(5 * 60));
    let completion = fire(3, RunOutcome::Success);
    let retained = recovery_path(&completion);
    assert_eq!(identity(&retained), substitute);
    assert_eq!(fs::read(retained).unwrap(), second);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"third after explicit new review"
    );
    println!(
        "{}: publication, restart, substituted receiver refusal and renewed approval verified",
        if chain {
            "Chain over native OpenSSH"
        } else {
            "Calendar over native OpenSSH"
        }
    );
}

fn execute_predecessor(
    fixture: &Sshd,
    runtime: &mut SchedulerWindow,
    predecessor: &JobId,
    execute: impl Fn(&RunRequest<SshCommand>, &ExecutionPolicy) -> Completion,
) -> Completion {
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Select(predecessor.clone()));
    let request = runtime
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let daemon = fixture.spawn();
    let result = execute(&request, &ExecutionPolicy::unrestricted(true));
    let trace = daemon.join().unwrap();
    assert_eq!(
        result.outcome,
        RunOutcome::Success,
        "{result:?}\n{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    result
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn recovery_path(completion: &Completion) -> PathBuf {
    let detail = completion.detail.as_deref().unwrap();
    let path = detail
        .split(" · ")
        .find_map(|line| line.strip_prefix("The original is retained at "))
        .unwrap();
    PathBuf::from(path.strip_suffix('.').unwrap())
}

fn serve_if_needed(fixture: &Sshd, execute: impl FnOnce() -> Completion) -> (Completion, String) {
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let daemon = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                match fixture.listener.accept() {
                    Ok((stream, _)) => {
                        return String::from_utf8_lossy(
                            &Sshd::serve_stream(&fixture.config, stream).stderr,
                        )
                        .into_owned();
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if finished.load(Ordering::Acquire) {
                            return String::new();
                        }
                        assert!(Instant::now() < deadline, "expected an rsync SSH route");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("could not accept a fixture SSH route: {error}"),
                }
            }
        });
        let result = execute();
        finished.store(true, Ordering::Release);
        (result, daemon.join().unwrap())
    })
}
