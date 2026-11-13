use crate::job_executor::Completion;
use crate::scheduler_window::{Message, SchedulerWindow, SshCommand};
use crate::snippets::tests::Profile;
use kortty_jobs::{Job, JobGraph, SchedulerStore};

use super::*;

#[test]
fn desktop_notices_follow_the_committed_run_and_saved_job_settings() {
    let profile = Profile::new();
    let path = profile.0.join("jobs.json");
    let job = JobId::new("desktop-notice").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "private-host.example", "port": 22, "username": "operator",
        "key_file": profile.0.join("private-key"), "command": "exit 17",
    }))
    .unwrap();
    {
        let graph =
            JobGraph::new(vec![Job::new(job.clone(), "Maintenance", action).unwrap()]).unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
    }
    let mut window = SchedulerWindow::open(path.clone()).unwrap();
    window.set_execution_allowed(true);
    let mut view = window.new_view();
    let first = window
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let backup = profile.0.join("previous.json");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    window.update_runtime(Message::Finished(Completion {
        id: first.id,
        outcome: RunOutcome::Failed {
            exit_code: Some(17),
        },
        detail: Some("Private output from private-host.example".into()),
        recording: None,
        swarm_report: None,
    }));
    let now = Instant::now();
    assert!(window.take_desktop_notices(now, true).is_empty());

    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(backup, &path).unwrap();
    window.update_runtime(Message::Tick);
    assert!(window.store.journal_record(first.id).is_some());
    let notices = window.take_desktop_notices(now, true);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Job Maintenance");
    assert_eq!(notices[0].body, "Failed · Exit code 17 · Manual run");
    assert!(window.take_desktop_notices(now, true).is_empty());

    let repeated = window
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    window.update_runtime(Message::Finished(Completion {
        id: repeated.id,
        outcome: RunOutcome::Failed { exit_code: None },
        detail: None,
        recording: None,
        swarm_report: None,
    }));
    assert!(
        window
            .take_desktop_notices(now + Duration::from_secs(59), true)
            .is_empty()
    );

    let settings = Settings {
        recovered: true,
        ..Settings::default()
    };
    window.update_view(&mut view, Message::DesktopNotices(settings));
    window.update_view(&mut view, Message::Save);

    let recovered = window
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    window.update_runtime(Message::Finished(Completion {
        id: recovered.id,
        outcome: RunOutcome::Success,
        detail: None,
        recording: None,
        swarm_report: None,
    }));
    let notices = window.take_desktop_notices(now + COOLDOWN, true);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].body, "Recovered · Manual run");

    let denied = window
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    window.update_runtime(Message::Finished(Completion {
        id: denied.id,
        outcome: RunOutcome::Blocked,
        detail: None,
        recording: None,
        swarm_report: None,
    }));
    assert!(
        window
            .take_desktop_notices(now + 2 * COOLDOWN, false)
            .is_empty()
    );
    assert!(
        window
            .take_desktop_notices(now + 2 * COOLDOWN, true)
            .is_empty()
    );

    drop(window);
    let reopened = SchedulerWindow::open(path).unwrap();
    assert_eq!(
        reopened
            .scheduler
            .graph()
            .job(&job)
            .unwrap()
            .action()
            .desktop_notices,
        settings
    );
}
