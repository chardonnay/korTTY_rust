use std::num::NonZeroU32;

use kortty_jobs::{DependencyMode, JobSchedule, TimeZone};

use crate::scheduler_window::rsync::standing;

use super::*;

const FIRING_BYTES: u64 = 2048;
const CHANGED: [&[u8]; 2] = [b"protected first firing\n", b"protected second firing\n"];

#[test]
#[ignore = "requires the isolated native OpenSSH and recurring passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_advances_direct_receivers() {
    require_isolated_root();
    let fixture = Fixture::new();
    let server = Path::new(ROOT);
    let source = fixture.profile.0.join("recurring-direct-source-file");
    let destination = server.join("replace/recurring-direct-file");
    write(&source, b"reviewed local source\n");
    write(&destination, b"reviewed protected receiver\n");
    let action = fixture.action(&Case {
        local: &source,
        path: destination.to_str().unwrap(),
        upload: true,
        directory: false,
        replace: true,
    });
    let recurring = Recurring::new(&fixture, "protected-recurring-file", action, false, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(&source, bytes).unwrap();
        let before = identity(&destination);
        let previous = fs::read(&destination).unwrap();
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        assert_ne!(identity(&destination), before);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(fs::read(recovery).unwrap(), previous);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
    }
    assert_blocked(&recurring.fire(2, false));
    assert_eq!(fs::read(&destination).unwrap(), CHANGED[1]);
    assert_handoff_cleared();

    direct_tree_upload(&fixture, server);

    let source = server.join("read/recurring-direct-tree");
    let destination = fixture.profile.0.join("recurring-direct-download");
    directory(&source);
    directory(&destination);
    write(&source.join("content"), b"reviewed protected source\n");
    write(&destination.join("removed"), b"first receiver\n");
    let action = fixture.action(&Case {
        local: &destination,
        path: source.to_str().unwrap(),
        upload: false,
        directory: true,
        replace: true,
    });
    let recurring = Recurring::new(&fixture, "protected-recurring-tree", action, true, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(source.join("content"), bytes).unwrap();
        let before = identity(&destination);
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(recovery.join("removed").exists(), iteration == 0);
        assert_eq!(fs::read(destination.join("content")).unwrap(), bytes);
        assert!(!destination.join("removed").exists());
    }

    let displaced = fixture.profile.0.join("recurring-displaced-download");
    fs::rename(&destination, &displaced).unwrap();
    directory(&destination);
    write(&destination.join("content"), CHANGED[1]);
    let occupant = identity(&destination);
    assert_blocked(&recurring.fire(2, true));
    assert_eq!(identity(&destination), occupant);
    assert_eq!(fs::read(destination.join("content")).unwrap(), CHANGED[1]);
    assert_eq!(fs::read(displaced.join("content")).unwrap(), CHANGED[1]);
    assert_handoff_cleared();
}

fn direct_tree_upload(fixture: &Fixture, server: &Path) {
    let source = fixture.profile.0.join("recurring-direct-source-tree");
    let destination = server.join("replace/recurring-direct-upload");
    directory(&source);
    directory(&destination);
    write(&source.join("content"), b"reviewed source tree\n");
    write(&destination.join("removed"), b"previous receiver\n");
    let action = fixture.action(&Case {
        local: &source,
        path: destination.to_str().unwrap(),
        upload: true,
        directory: true,
        replace: true,
    });
    let recurring = Recurring::new(fixture, "protected-recurring-tree-upload", action, false, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(source.join("content"), bytes).unwrap();
        let before = identity(&destination);
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(recovery.join("removed").exists(), iteration == 0);
        assert_eq!(fs::read(destination.join("content")).unwrap(), bytes);
        assert!(!destination.join("removed").exists());
    }
    assert_handoff_cleared();
}

#[test]
#[ignore = "requires the isolated native OpenSSH and recurring passwordless mixed-rsync fixture"]
fn openssh_passwordless_recurring_mixed_rsync_limits_losing_inputs_and_advances_receivers() {
    require_isolated_root();
    let fixture = Fixture::new();
    let server = Path::new(ROOT);
    for upload in [true, false] {
        let (name, inputs, destination) = if upload {
            (
                "protected-recurring-mixed-upload",
                fixture.profile.0.join("recurring-mixed-upload-inputs"),
                server.join("replace/recurring-mixed-upload"),
            )
        } else {
            (
                "protected-recurring-mixed-download",
                server.join("read/recurring-mixed-download-inputs"),
                fixture.profile.0.join("recurring-mixed-download"),
            )
        };
        seed_inputs(&inputs);
        seed_receiver(&destination);
        let mode = ReceiverMode::ScopedMirror;
        let target = target(&fixture, upload, &destination, sources(&inputs, mode), mode);
        let review = preview(&fixture, &target).unwrap();
        let handoff = handoff(&review).to_owned();
        let recurring = Recurring::new(&fixture, name, action(&fixture, &target, review), false, 1);
        for (iteration, bytes) in CHANGED.into_iter().enumerate() {
            fs::write(inputs.join("first/report"), bytes).unwrap();
            if iteration == 1 {
                fs::rename(
                    inputs.join("named/scope/new"),
                    inputs.join("named/scope/next"),
                )
                .unwrap();
            }
            let before = identity(&destination);
            let previous = fs::read(destination.join("report")).unwrap();
            let completion = recurring.fire(iteration, true);
            assert_success(&completion);
            let recovery = recovered(&completion, &destination, before);
            assert_eq!(fs::read(recovery.join("report")).unwrap(), previous);
            assert_eq!(fs::read(destination.join("report")).unwrap(), bytes);
            assert!(!destination.join("scope/obsolete").exists());
            assert!(destination.join("scope-more/keep").is_file());
            assert!(destination.join("outside").is_file());
            assert_eq!(destination.join("scope/next").exists(), iteration == 1);
            assert_empty_handoff(&handoff);
        }

        let before = identity(&destination);
        fs::write(
            inputs.join("losing/report"),
            vec![b'x'; FIRING_BYTES as usize + 1],
        )
        .unwrap();
        assert_blocked(&recurring.fire(2, true));
        assert_eq!(identity(&destination), before);
        assert_eq!(fs::read(destination.join("report")).unwrap(), CHANGED[1]);
        assert_empty_handoff(&handoff);
    }
}

pub(in crate::job_executor::tests::openssh_script::rsync_privileged) struct Recurring<'a> {
    fixture: &'a Fixture,
    path: PathBuf,
    id: JobId,
    issued: SystemTime,
    predecessor: Option<JobId>,
}

impl<'a> Recurring<'a> {
    pub(in crate::job_executor::tests::openssh_script::rsync_privileged) fn new(
        fixture: &'a Fixture,
        name: &str,
        action: SshCommand,
        chain: bool,
        deletes: u32,
    ) -> Self {
        let id = JobId::new(name).unwrap();
        let predecessor = chain.then(|| JobId::new("protected-recurring-predecessor").unwrap());
        let issued = SystemTime::now() - Duration::from_secs(15 * 60);
        let mut action = serde_json::to_value(action).unwrap();
        action["remote"]["standing"] = json!({
            "settings": {
                "days": 2, "firings": 4,
                "budget": {"bytes": FIRING_BYTES, "entries": 32, "deletes": deletes}
            }
        });
        let job = |action| {
            let action: SshCommand = serde_json::from_value(action).unwrap();
            let job = Job::new(id.clone(), name, action.validated().unwrap()).unwrap();
            if let Some(predecessor) = &predecessor {
                job.with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None)
            } else {
                job.with_schedule(JobSchedule::default().with_interval_minutes(NonZeroU32::new(1)))
            }
        };
        let grant = standing::Grant::new(&job(action.clone()), &TimeZone::UTC, issued).unwrap();
        action["remote"]["standing"]["approval"] = json!(grant.approval);
        let path = fixture.profile.0.join(format!("{name}.json"));
        let mut jobs = vec![job(action)];
        if let Some(predecessor) = &predecessor {
            let command = serde_json::from_value(json!({
                "connection": fixture.reference, "command": "true",
            }))
            .unwrap();
            jobs.push(Job::new(predecessor.clone(), "Successful predecessor", command).unwrap());
        }
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
        standing::Store::new(fixture.profile.0.clone())
            .swap(&id, grant, |_| true)
            .unwrap();
        Self {
            fixture,
            path,
            id,
            issued,
            predecessor,
        }
    }

    pub(in crate::job_executor::tests::openssh_script::rsync_privileged) fn fire(
        &self,
        iteration: usize,
        sudo_allowed: bool,
    ) -> Completion {
        let mut runtime = SchedulerWindow::open(self.path.clone()).unwrap();
        runtime.set_connections(Some(&self.fixture.connections));
        runtime.set_execution_allowed(true);
        let scheduled_at = self.issued + Duration::from_secs(iteration as u64 * 120);
        runtime
            .initialize_schedules_at(TimeZone::UTC, scheduled_at)
            .unwrap();
        let (mut requests, trigger, finished_at) = if let Some(predecessor) = &self.predecessor {
            let mut view = runtime.new_view();
            runtime.update_view(&mut view, Message::Select(predecessor.clone()));
            let request = runtime
                .update_view(&mut view, Message::Run(false))
                .pop()
                .unwrap();
            let completion = Completion::from_result(request.id, Ok(0));
            (
                runtime.update_runtime(Message::Finished(completion)),
                RunTrigger::Dependency,
                SystemTime::now(),
            )
        } else {
            let now = scheduled_at + Duration::from_secs(90);
            (
                runtime.update_runtime_at(Message::Tick, now),
                RunTrigger::Scheduled,
                now + Duration::from_secs(10),
            )
        };
        let request = requests.pop().unwrap();
        assert_eq!(&request.job, &self.id);
        assert_eq!(request.trigger, trigger);
        let saved = Credentials::resolve(
            &request.action,
            self.fixture
                .connections
                .get(self.fixture.reference.id)
                .unwrap(),
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .map(Some);
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.sudo_allowed = sudo_allowed;
        policy.rsync = runtime.try_rsync_run_context(&request).unwrap();
        let result = run(&request, None, None, &policy, saved, Ok(None), || {
            Ok(self.fixture.trust.clone())
        });
        runtime.update_runtime_at(Message::Finished(result.clone()), finished_at);
        result
    }
}

fn assert_success(completion: &Completion) {
    assert_eq!(completion.outcome, RunOutcome::Success, "{completion:?}");
}

fn assert_blocked(completion: &Completion) {
    assert_eq!(completion.outcome, RunOutcome::Blocked, "{completion:?}");
}

fn assert_handoff_cleared() {
    assert_empty_handoff("/test-rsync/handoff/probe");
}
