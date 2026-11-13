use iced::futures::{StreamExt as _, executor::block_on};
use iced::window;
use kortty_jobs::{Job, JobGraph, JobId, RunOutcome, SchedulerStore};
use serde_json::json;

use crate::scheduler_window::{JournalDetails, Message as SchedulerMessage, Page, SchedulerWindow};
use crate::session_journal::automation::{Destination, Recorder};
use crate::session_journal::{self, CaptureKind, tree::RunStatus};
use crate::snippets::tests::Profile;
use crate::terminal_logging::{Redactor, Secrets};

use super::*;

fn job_finished(task: Task<Desktop>) -> (crate::job_executor::Completion, Vec<Artifact>) {
    let mut actions = iced_runtime::task::into_stream(task).expect("scheduler execution task");
    loop {
        if let iced_runtime::Action::Output(Desktop::JobFinished(outcome, artifacts)) =
            block_on(actions.next()).expect("scheduler outcome")
        {
            return (outcome, artifacts);
        }
    }
}

pub(in crate::workspace) fn capture_job_journals(
    workspace: &mut Workspace,
    job: &JobId,
    action: &'static str,
    targets: &[&str],
) -> Vec<Artifact> {
    let (settings, run) = workspace
        .automation_capture(
            Kind::Job,
            job.as_str().into(),
            "Job fixture".into(),
            action,
            Timestamp::now(),
            Options {
                enabled: true,
                ..Options::default()
            },
        )
        .unwrap();
    let redactor = Arc::new(Redactor::for_journal(
        Secrets::default().seal().unwrap(),
        workspace.clipboard.policy.terminal_logging.clone(),
    ));
    targets
        .iter()
        .map(|target| {
            let recorder = Recorder::start(
                &settings,
                run.clone(),
                Destination {
                    key: session_journal::automation::identity_digest(target.as_bytes()).unwrap(),
                    label: (*target).into(),
                    host: "fixture.example".into(),
                    port: Some(22),
                    username: "operator".into(),
                },
                Arc::clone(&redactor),
            )
            .unwrap();
            recorder.command("fixture command");
            recorder.finish(RunStatus::Success, Some(0))
        })
        .collect()
}

#[test]
fn scheduler_run_journals_open_exact_targets_after_reopen_and_recheck_policy_and_root() {
    verify_run_journal_routing(2);
}

#[test]
fn scheduler_large_run_lists_every_journal_and_opens_only_the_requested_target() {
    verify_run_journal_routing(kortty_jobs::MAX_SWARM_TARGETS);
}

fn verify_run_journal_routing(target_count: usize) {
    let scratch = Profile::new();
    let base = scratch.0.canonicalize().unwrap();
    let root = base.join("journals");
    let database = base.join("jobs.json");
    let job = JobId::new("run-journal-routing").unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "host": "fixture.example", "port": 22, "username": "operator",
        "key_file": base.join("unused-key"), "command": "exit 0"
    }))
    .unwrap();
    let graph = JobGraph::new(vec![Job::new(job.clone(), "Job fixture", action).unwrap()]).unwrap();
    SchedulerStore::open(&database)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let setup = |hosted| {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.desktop_notifications_allowed = false;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let mut scheduler = SchedulerWindow::open(database.clone()).unwrap();
        scheduler.set_execution_allowed(true);
        workspace.scheduler = Some(scheduler);
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_scheduler(owner);
        let view = *workspace.scheduler_views.keys().next().unwrap();
        (workspace, view)
    };
    let (mut original, original_view) = setup(false);
    let run = original
        .scheduler
        .as_mut()
        .unwrap()
        .update_view(
            original.scheduler_views.get_mut(&original_view).unwrap(),
            SchedulerMessage::Run(false),
        )
        .pop()
        .unwrap()
        .id;
    let targets: Vec<_> = (1..=target_count)
        .map(|index| format!("Target {index}"))
        .collect();
    let targets: Vec<_> = targets.iter().map(String::as_str).collect();
    let artifacts = capture_job_journals(&mut original, &job, "COMMAND", &targets);
    let expected = artifacts
        .iter()
        .map(|artifact| artifact.source.id().to_owned())
        .collect::<Vec<_>>();
    let _ = original.update(Desktop::JobFinished(
        crate::job_executor::Completion {
            id: run,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            swarm_report: None,
        },
        artifacts,
    ));
    let _ = capture_job_journals(&mut original, &job, "COMMAND", &["Unrelated"]);
    drop(original);

    let (mut workspace, view) = setup(true);
    let _ = workspace.update(Desktop::Scheduler(
        view,
        SchedulerMessage::Page(Page::Journal),
    ));
    let select = || Desktop::Scheduler(view, SchedulerMessage::SelectRecord(run));
    let bulk = || Desktop::Scheduler(view, SchedulerMessage::OpenSessionJournals(run));
    let large = target_count > crate::session_journal::automation::run_links::MAX_BULK_OPEN;
    let single = |id| Desktop::Scheduler(view, SchedulerMessage::OpenSessionJournal(run, id));
    let open = || {
        if large {
            single(expected.last().unwrap().clone())
        } else {
            bulk()
        }
    };
    let opened_count = if large { 1 } else { target_count };
    let complete_lookup = |task| {
        let mut events = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(event @ Desktop::AutomationJournal(_))) =
            block_on(events.next())
        else {
            panic!("expected a journal lookup result");
        };
        event
    };
    let preview = workspace.update(select());
    assert!(matches!(
        workspace.scheduler_views[&view].journal_details(run, &root),
        Some(JournalDetails::Loading)
    ));
    let stale = complete_lookup(preview);
    workspace.window_settings.session_journal.directory =
        base.join("different").to_str().unwrap().into();
    let _ = workspace.update(stale);
    assert!(workspace.journals.is_empty());
    assert!(iced_runtime::task::into_stream(workspace.update(single("unknown".into()))).is_none());
    if large {
        assert!(iced_runtime::task::into_stream(workspace.update(bulk())).is_none());
    }
    workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();

    let denied = complete_lookup(workspace.update(open()));
    workspace.clipboard.policy.session_journal_allowed = false;
    let _ = workspace.update(denied);
    assert!(workspace.journals.is_empty());
    assert!(iced_runtime::task::into_stream(workspace.update(open())).is_none());
    workspace.clipboard.policy.session_journal_allowed = true;

    let found = complete_lookup(workspace.update(select()));
    let Desktop::AutomationJournal(Message::RunJournalsLoaded(_, Ok(report))) = &found else {
        panic!("expected both exact retained target journals");
    };
    assert_eq!(report.missing(), 0);
    assert_eq!(
        report
            .entries()
            .map(|journal| journal.source.id())
            .collect::<Vec<_>>(),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    let _ = workspace.update(found);
    let Some(JournalDetails::Ready(details)) =
        workspace.scheduler_views[&view].journal_details(run, &root)
    else {
        panic!("expected inline journal details from selection");
    };
    assert!(
        details
            .entries()
            .all(|journal| journal.status == Some(RunStatus::Success))
    );
    assert!(workspace.journals.is_empty());
    let pending = workspace.update(select());
    assert!(iced_runtime::task::into_stream(workspace.update(open())).is_none());
    let opened = complete_lookup(pending);
    let _ = workspace.update(opened);
    assert_eq!(workspace.journals.len(), opened_count);
    if large {
        let id = *workspace.journals.keys().next().unwrap();
        assert!(
            workspace
                .journal_title(id)
                .unwrap()
                .contains(*targets.last().unwrap())
        );
    }
    let again = complete_lookup(workspace.update(open()));
    let _ = workspace.update(again);
    assert_eq!(workspace.journals.len(), opened_count);
    if large {
        let pending = workspace.update(single(expected[0].clone()));
        assert!(
            iced_runtime::task::into_stream(workspace.update(single(expected[1].clone())))
                .is_none()
        );
        let opened = complete_lookup(pending);
        let _ = workspace.update(opened);
        assert_eq!(workspace.journals.len(), 3);
    }
}

#[test]
fn scheduler_normal_run_records_the_enabled_target_and_reports_unavailable_capture() {
    for (enabled, allowed, automation_allowed) in [
        (false, true, true),
        (true, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        let fixture = Profile::new();
        let scratch = fixture.0.canonicalize().unwrap();
        let root = scratch.join("journals");
        let database = scratch.join("scheduler.json");
        let id = JobId::new("automation-test").unwrap();
        let mut action = json!({
            "host": "127.0.0.1", "port": 22, "username": "operator",
            "key_file": scratch.join("unused-key"), "command": "exit 0"
        });
        if enabled {
            action["automation_journal"] = json!({"enabled": true, "capture_input": true});
        }
        let action: SshCommand = serde_json::from_value(action).unwrap();
        let graph = JobGraph::new(vec![Job::new(id, "Blocked run", action).unwrap()]).unwrap();
        SchedulerStore::open(&database)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy = crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['127.0.0.1']\n[rule.session-journal]\nautomation-allowed = {automation_allowed}",
        ))
        .unwrap();
        workspace.clipboard.policy.session_journal_allowed = allowed;
        workspace.clipboard.policy.desktop_notifications_allowed = false;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        let mut scheduler = SchedulerWindow::open(database).unwrap();
        scheduler.set_execution_allowed(true);
        let view = window::Id::unique();
        workspace.scheduler_views.insert(view, scheduler.new_view());
        workspace.scheduler = Some(scheduler);

        let task = workspace.update(Desktop::Scheduler(view, SchedulerMessage::Run(false)));
        let (outcome, artifacts) = job_finished(task);
        assert_eq!(outcome.outcome, RunOutcome::Blocked);
        let detail = outcome.detail.as_deref().unwrap();
        assert!(detail.contains("Managed policy blocks"));
        assert_eq!(detail.contains("Automation journal"), enabled && !allowed);
        let captured = enabled && allowed && automation_allowed;
        assert_eq!(artifacts.len(), usize::from(captured));
        if let Some(artifact) = artifacts.first() {
            let journal = artifact.source.read(None).unwrap().unwrap();
            assert_eq!(journal.summary.source_kind, SourceKind::Job);
            assert_eq!(
                journal.summary.source_id.as_deref(),
                Some("automation-test")
            );
            assert_eq!(journal.summary.run_status, Some(RunStatus::Blocked));
            assert!(
                artifact
                    .source
                    .capture_page(None, None, None)
                    .unwrap()
                    .records
                    .iter()
                    .all(|record| record.kind != CaptureKind::Input)
            );
        }
        let _ = workspace.update(Desktop::JobFinished(outcome, artifacts));
        if captured {
            let task = workspace.update(Desktop::RuntimeTick);
            let mut actions = iced_runtime::task::into_stream(task).expect("retention task");
            let event = loop {
                if let iced_runtime::Action::Output(event @ Desktop::AutomationJournal(_)) =
                    block_on(actions.next()).expect("retention outcome")
                {
                    break event;
                }
            };
            let _ = workspace.update(event);
            let catalog = session_journal::catalog(&root).unwrap();
            assert_eq!(catalog.journals.len(), 1);
            assert!(catalog.journals[0].storage_bytes > 0);
        } else {
            assert!(!root.exists());
        }
    }
}

#[test]
fn automation_policy_revocation_rotates_capture_without_stopping_retention() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap().join("journals");
    let mut workspace = Workspace::default();
    workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
    let artifacts = capture_job_journals(
        &mut workspace,
        &JobId::new("retained-job").unwrap(),
        "COMMAND",
        &["Existing target"],
    );
    let capture_stop = Arc::clone(&workspace.journal_automation.stopped);
    let retention_stop = Arc::new(AtomicBool::new(false));
    let (_done, done) = async_channel::bounded(1);
    workspace.journal_automation.completed = artifacts;
    workspace.journal_automation.active = Some(Active {
        ticket: 1,
        stopped: Arc::clone(&retention_stop),
        done,
        completed: Vec::new(),
        recovering: true,
    });
    let next = workspace.journal_automation.next;

    workspace.clipboard.policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[rule.session-journal]\nautomation-allowed = false",
    )
    .unwrap();
    workspace.revalidate_automation_journals();

    assert!(capture_stop.load(Ordering::Acquire));
    assert!(!retention_stop.load(Ordering::Acquire));
    assert_eq!(workspace.journal_directory().unwrap(), root);
    assert_eq!(workspace.journal_automation.next, next);
    assert_eq!(workspace.journal_automation.completed.len(), 1);
    assert_eq!(session_journal::catalog(&root).unwrap().journals.len(), 1);

    workspace.clipboard.policy = ClipboardPolicy::default();
    workspace.revalidate_automation_journals();
    assert!(workspace.automation_journal_capture_allowed());
    assert!(capture_stop.load(Ordering::Acquire));
    assert!(!workspace.journal_automation.stopped.load(Ordering::Acquire));
    assert!(!retention_stop.load(Ordering::Acquire));
}

#[test]
fn scheduler_archive_journal_records_blocked_target_without_capture_input() {
    let fixture = Profile::new();
    let scratch = fixture.0.canonicalize().unwrap();
    let root = scratch.join("journals");
    let database = scratch.join("scheduler.json");
    let id = JobId::new("archive-journal").unwrap();
    let credential = "4".repeat(32);
    let action: SshCommand = serde_json::from_value(json!({
        "host": "127.0.0.1", "port": 22, "username": "operator",
        "key_file": scratch.join("unused-key"),
        "remote": {
            "kind": "archive", "format": "zip_password", "level": 6,
            "sources": ["/private/archive-source"], "target": "/private/archive-result.zip",
            "password_credential": credential,
        },
        "automation_journal": {"enabled": true},
    }))
    .unwrap();
    let graph = JobGraph::new(vec![Job::new(id, "Encrypted archive", action).unwrap()]).unwrap();
    SchedulerStore::open(&database)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut workspace = Workspace::default();
    workspace.clipboard.policy.desktop_notifications_allowed = false;
    workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
    let mut scheduler = SchedulerWindow::open(database).unwrap();
    scheduler.set_execution_allowed(true);
    let view = window::Id::unique();
    workspace.scheduler_views.insert(view, scheduler.new_view());
    workspace.scheduler = Some(scheduler);

    let (outcome, artifacts) =
        job_finished(workspace.update(Desktop::Scheduler(view, SchedulerMessage::Run(false))));
    assert_eq!(outcome.outcome, RunOutcome::Blocked);
    assert!(
        outcome
            .detail
            .as_deref()
            .unwrap()
            .contains("Unlock the vault")
    );
    let [artifact] = artifacts.as_slice() else {
        panic!("expected one archive target journal");
    };
    let journal = artifact.source.read(None).unwrap().unwrap();
    assert_eq!(journal.summary.source_kind, SourceKind::Job);
    assert_eq!(
        journal.summary.automation_action.as_deref(),
        Some("SFTP_ARCHIVE")
    );
    assert_eq!(journal.summary.run_status, Some(RunStatus::Blocked));
    let capture = artifact.source.capture_page(None, None, None).unwrap();
    assert!(capture.records.iter().all(|record| {
        record.kind != CaptureKind::Input
            && !record.text.contains("/private/archive-source")
            && !record.text.contains(credential.as_str())
    }));
    assert!(capture.records.iter().any(|record| {
        record.kind == CaptureKind::Note && record.text.contains("Unlock the vault")
    }));
    let _ = workspace.update(Desktop::JobFinished(outcome, artifacts));
}
