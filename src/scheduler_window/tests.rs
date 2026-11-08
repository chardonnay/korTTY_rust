use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use kortty_jobs::{DependencyMode, Job};

use super::*;

struct TestWindow {
    runtime: SchedulerWindow,
    view: SchedulerView,
}

impl TestWindow {
    fn open(path: PathBuf) -> Result<Self, String> {
        let runtime = SchedulerWindow::open(path)?;
        let view = runtime.new_view();
        Ok(Self { runtime, view })
    }

    fn update(&mut self, message: Message) -> Vec<RunRequest<SshCommand>> {
        if matches!(message, Message::Tick | Message::Finished(_)) {
            let requests = self.runtime.update_runtime(message);
            self.runtime.sync_view(&mut self.view);
            requests
        } else {
            self.runtime.update_view(&mut self.view, message)
        }
    }

    fn selected_job(&self) -> Option<&Job<SshCommand>> {
        self.runtime.selected_job(&self.view)
    }

    fn request_close(&mut self) -> bool {
        self.view.request_close()
    }

    fn close_pending(&self) -> bool {
        self.view.close_pending()
    }

    fn confirm_close(&mut self) -> bool {
        self.runtime.confirm_close(&mut self.view)
    }

    fn status(&self) -> Option<&str> {
        self.view
            .status
            .as_deref()
            .or(self.runtime.status.as_deref())
    }
}

struct Profile(PathBuf);

impl Profile {
    fn temporary() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let index = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kortty-scheduler-window-{}-{index}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create temporary scheduler profile: {error}"),
            }
        }
    }

    fn file(&self) -> PathBuf {
        self.0.join("scheduler.json")
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn journal_open_job_selects_the_saved_job_without_discarding_another_draft() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("journal-first").unwrap();
    let second = JobId::new("journal-second").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(first.clone(), "First job", action.clone()).unwrap(),
            Job::new(second.clone(), "Second job", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut runtime = SchedulerWindow::open(path).unwrap();
    let mut view = runtime.new_view();
    view.page = Page::Journal;
    runtime.select_journal_job(&mut view, &second);
    assert_eq!(view.draft.id.as_ref(), Some(&second));
    assert_eq!(view.page, Page::Job);
    runtime.update_view(&mut view, Message::Edit(Field::Name, "Unsaved name".into()));
    runtime.select_journal_job(&mut view, &first);
    assert_eq!(view.draft.id.as_ref(), Some(&second));
    assert!(view.dirty());
    assert_eq!(view.draft.build(second).unwrap().name(), "Unsaved name");
}

#[test]
fn scheduler_views_keep_independent_drafts_and_share_job_execution() {
    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("shared-job").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id.clone(), "Original", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    let mut first = runtime.new_view();
    let mut second = runtime.new_view();
    runtime.update_view(&mut first, Message::Edit(Field::Name, "First edit".into()));
    runtime.update_view(
        &mut second,
        Message::Edit(Field::Name, "Second edit".into()),
    );
    runtime.update_view(&mut first, Message::Save);
    assert_eq!(runtime.selected_job(&first).unwrap().name(), "First edit");
    runtime.sync_view(&mut second);
    assert_eq!(second.draft.build(id).unwrap().name(), "Second edit");
    assert!(second.dirty());

    runtime.update_view(&mut second, Message::Save);
    assert!(
        second
            .status
            .as_deref()
            .unwrap()
            .contains("changed in another view")
    );
    assert_eq!(runtime.selected_job(&second).unwrap().name(), "First edit");
    assert!(second.dirty());
    let runs = runtime.update_view(&mut first, Message::Run(false));
    assert_eq!(runs.len(), 1);
    let run = runs[0].id;
    drop(first);
    drop(second);
    // Test-only worker completion. Runtime scheduling and journal persistence are real.
    assert!(
        runtime
            .update_runtime(Message::Finished(Completion {
                id: run,
                outcome: RunOutcome::Success,
                detail: None,
                recording: None,
            }))
            .is_empty()
    );
    assert!(!runtime.has_active_runs());
    assert_eq!(runtime.store.journal().len(), 1);
    assert_eq!(runtime.store.journal()[0].id, run);
    drop(runtime);

    let restored = SchedulerWindow::open(path).unwrap();
    assert_eq!(restored.store.journal().len(), 1);
    assert_eq!(
        restored.selected_job(&restored.new_view()).unwrap().name(),
        "First edit"
    );
}

#[test]
fn jobs_menu_cancels_only_the_current_exact_run() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("menu-first").unwrap();
    let second = JobId::new("menu-second").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(first.clone(), "Duplicate display name", action.clone()).unwrap(),
            Job::new(second.clone(), "Duplicate display name", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path).unwrap();
    let runtime = &mut window.runtime;
    runtime.set_execution_allowed(true);
    let now = SystemTime::now();
    assert!(runtime.menu_status(now, 5).is_none());
    let mut first_view = runtime.new_view();
    runtime.update_view(&mut first_view, Message::Select(first));
    let first_run = runtime
        .update_view(&mut first_view, Message::Run(false))
        .pop()
        .unwrap();
    let mut second_view = runtime.new_view();
    runtime.update_view(&mut second_view, Message::Select(second));
    let second_run = runtime
        .update_view(&mut second_view, Message::Run(false))
        .pop()
        .unwrap();
    let status = runtime.menu_status(now, 5).unwrap();
    assert_eq!(status.running.len(), 2);
    assert!(status.running.iter().all(|run| !run.cancelling));

    assert!(runtime.cancel_menu_run(first_run.id.value()));
    assert!(!runtime.cancel_menu_run(first_run.id.value()));
    assert!(first_run.cancellation.is_requested());
    assert!(!second_run.cancellation.is_requested());
    let status = runtime.menu_status(now, 5).unwrap();
    assert_eq!(
        status.running.iter().filter(|run| run.cancelling).count(),
        1,
    );

    // Test-only executor completion through the real scheduler and journal.
    runtime.update_runtime(Message::Finished(Completion {
        id: first_run.id,
        outcome: RunOutcome::Cancelled,
        detail: None,
        recording: None,
    }));
    let rerun = runtime
        .update_view(&mut first_view, Message::Run(false))
        .pop()
        .unwrap();
    assert_ne!(rerun.id, first_run.id);
    assert!(!runtime.cancel_menu_run(first_run.id.value()));
    assert!(!rerun.cancellation.is_requested());
    assert!(!second_run.cancellation.is_requested());
}

#[test]
fn jobs_menu_lists_five_enabled_deadlines_in_local_time_order() {
    let profile = Profile::temporary();
    let path = profile.file();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let mut jobs = (1..=7)
        .rev()
        .map(|hour| {
            let schedule =
                kortty_jobs::JobSchedule::default()
                    .with_fixed_times([format!("{hour:02}:00").parse::<Time>().unwrap()]);
            Job::new(
                JobId::new(format!("hour-{hour}")).unwrap(),
                format!("At hour {hour}"),
                action.clone(),
            )
            .unwrap()
            .with_schedule(schedule)
        })
        .collect::<Vec<_>>();
    let early =
        kortty_jobs::JobSchedule::default().with_fixed_times(["00:30".parse::<Time>().unwrap()]);
    jobs.push(
        Job::new(
            JobId::new("job-disabled").unwrap(),
            "Disabled",
            action.clone(),
        )
        .unwrap()
        .with_schedule(early.clone())
        .with_enabled(false),
    );
    jobs.push(
        Job::new(
            JobId::new("schedule-disabled").unwrap(),
            "Schedule disabled",
            action,
        )
        .unwrap()
        .with_schedule(early.with_enabled(false)),
    );
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        store
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
    }
    let now = SystemTime::UNIX_EPOCH;
    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.zone = TimeZone::UTC;
    runtime
        .scheduler
        .initialize_schedules(TimeZone::UTC, now)
        .unwrap();
    runtime.set_execution_allowed(true);
    let status = runtime.menu_status(now, 5).unwrap();
    assert!(status.running.is_empty());
    assert_eq!(
        status
            .upcoming
            .iter()
            .map(|job| job.name)
            .collect::<Vec<_>>(),
        [
            "At hour 1",
            "At hour 2",
            "At hour 3",
            "At hour 4",
            "At hour 5"
        ]
    );
    assert_eq!(status.upcoming[0].local_time, "1970-01-01 01:00:00");
    assert_eq!(status.upcoming[0].due, now + Duration::from_secs(3600));
}

#[test]
fn virtual_terminal_attachment_survives_reopen_and_is_removed_with_its_record() {
    let profile = Profile::temporary();
    let id = JobId::new("virtual").unwrap();
    let path = profile.file();
    let options = virtual_terminal::Options {
        max_frames: 2,
        ..virtual_terminal::Options::default()
    };
    let action = SshCommand {
        host: "example.test".into(),
        port: 22,
        username: "test".into(),
        key_file: profile.0.join("fixture-key"),
        connection: None,
        additional_connections: Vec::new(),
        command: "true".into(),
        script: None,
        snippet: None,
        remote: None,
        virtual_terminal: Some(options),
    };
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id, "Virtual terminal", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.action.virtual_terminal, Some(options));
    let token = [17; 16];
    let directory = &window.runtime.recording_directory;
    fs::create_dir(directory).unwrap();
    let file = virtual_terminal::path_for(directory, request.id, token);
    // A real recording artifact, standing in for output from the SSH fixture.
    let mut recorder = kortty_recording::Recorder::create(&file, Default::default()).unwrap();
    let mut terminal = kortty_terminal::Terminal::new(40, 10);
    terminal.advance(b"stale\r\x1b[2K\x1b[31mfinal\x1b[0m");
    recorder.start(Duration::ZERO).unwrap();
    recorder
        .record_screen(Duration::ZERO, 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    recorder.finish(Duration::ZERO).unwrap();
    use crate::recording_window::screenshots;
    screenshots::attach(&file, || false).unwrap();
    let attached = screenshots::attachment_dir(&file);
    let orphan = virtual_terminal::path_for(directory, request.id, [18; 16]);
    fs::copy(&file, &orphan).unwrap();
    screenshots::attach(&orphan, || false).unwrap();
    window.update(Message::Finished(Completion {
        id: request.id,
        outcome: RunOutcome::Success,
        detail: None,
        recording: Some(token),
    }));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert!(!orphan.exists());
    assert!(!screenshots::attachment_dir(&orphan).exists());
    assert!(attached.join("001.png").is_file());
    assert_eq!(
        window.runtime.recording_path(request.id),
        Some(file.clone())
    );
    let mut replay = kortty_recording::Replay::open(&file).unwrap();
    assert!(replay.is_complete());
    let frame = replay.frame(0).unwrap().unwrap();
    let visible: String = frame
        .screen
        .row(0)
        .unwrap()
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert_eq!(visible, "final");
    drop(replay);
    window.update(Message::Page(Page::Journal));
    window.update(Message::SelectRecord(request.id));
    assert_eq!(
        window
            .runtime
            .selected_recording_path(&window.view, request.id),
        Some(file.clone())
    );
    window.update(Message::DeleteRecord);
    assert!(matches!(
        window.view.confirm,
        Some(Confirmation::DeleteRecord(_))
    ));
    window.update(Message::JournalSearch("no matching run".into()));
    window.update(Message::SelectRecord(request.id));
    window.update(Message::Confirm);
    assert_eq!(window.view.selected_run, None);
    assert!(
        window
            .runtime
            .selected_recording_path(&window.view, request.id)
            .is_none()
    );
    assert!(file.is_file());
    window.update(Message::JournalSearch(String::new()));
    window.update(Message::SelectRecord(request.id));
    window.update(Message::DeleteRecord);
    window.update(Message::Confirm);
    assert!(window.runtime.recording_path(request.id).is_none());
    assert!(!file.exists());
    assert!(!attached.exists());
}

#[test]
fn scheduler_journal_search_uses_real_run_fields_and_view_specific_columns() {
    let profile = Profile::temporary();
    let path = profile.file();
    let job = JobId::new("internal-id").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(job, "Backlog upload", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut window = TestWindow::open(path).unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    window.update(Message::Page(Page::Journal));
    window.update(Message::JournalSearch("back*up running manual".into()));
    assert!(
        window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalSearchMode(
        JournalSearchMode::SelectedColumns,
    ));
    assert!(
        !window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalColumns);
    window.update(Message::JournalColumn(JournalColumn::Trigger, true));
    assert!(
        window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );

    // Test double for the remote executor's completion through the real runtime and store.
    window.update(Message::Finished(Completion {
        id: request.id,
        outcome: RunOutcome::Failed {
            exit_code: Some(42),
        },
        detail: Some("Permission denied".into()),
        recording: None,
    }));
    // A clock value containing "42" must not satisfy the exit-code check.
    window.update(Message::JournalColumn(JournalColumn::Started, false));
    window.update(Message::JournalSearch("back*up per*denied 42".into()));
    assert!(
        !window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalColumn(JournalColumn::ExitCode, true));
    assert!(
        window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    window.update(Message::SelectRecord(request.id));
    assert_eq!(window.view.selected_run, Some(request.id));

    let mut other = window.runtime.new_view();
    other.page = Page::Journal;
    other
        .journal_search
        .set_text(format!("internal-id {} 42", request.id));
    assert!(other.journal_record_visible(&window.runtime, request.id));
    window.update(Message::JournalColumn(JournalColumn::ExitCode, false));
    assert_eq!(window.view.selected_run, None);
    assert!(
        !window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    assert!(other.journal_record_visible(&window.runtime, request.id));
}

#[test]
fn a_chain_waits_for_the_predecessor_journal_commit() {
    let profile = Profile::temporary();
    let root = JobId::new("first").unwrap();
    let leaf = JobId::new("second").unwrap();
    let action = SshCommand {
        host: "example.test".into(),
        port: 22,
        username: "test".into(),
        key_file: profile.0.join("fixture-key"),
        connection: None,
        additional_connections: Vec::new(),
        command: "true".into(),
        script: None,
        snippet: None,
        remote: None,
        virtual_terminal: None,
    };
    let jobs = vec![
        Job::new(root.clone(), "First", action.clone()).unwrap(),
        Job::new(leaf.clone(), "Second", action)
            .unwrap()
            .with_dependencies(DependencyMode::Chain, vec![root.clone()], None),
    ];
    let path = profile.file();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        store
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    assert!(window.update(Message::Run(false)).is_empty());
    window.runtime.set_execution_allowed(true);
    let run = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(run.job, root);

    let backup = profile.0.join("previous.json");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    // Simulated executor completion. All runtime, dependency and file operations are real.
    let completion = Message::Finished(Completion {
        id: run.id,
        outcome: RunOutcome::Success,
        detail: Some("Exit status 0".into()),
        recording: None,
    });
    assert!(window.update(completion).is_empty());
    assert!(window.runtime.has_active_runs());
    assert!(window.runtime.next_due().is_some());
    assert!(window.runtime.store.journal().is_empty());

    fs::remove_dir(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    let leaf_run = window.update(Message::Tick).pop().unwrap();
    assert_eq!(leaf_run.job, leaf);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
    assert!(window.update(Message::Tick).is_empty());
    let failed = RunOutcome::Failed { exit_code: Some(5) };
    assert!(
        window
            .update(Message::Finished(Completion {
                id: leaf_run.id,
                outcome: failed,
                detail: None,
                recording: None
            }))
            .is_empty()
    );
    assert!(!window.runtime.has_active_runs());
    drop(window);

    let window = TestWindow::open(path).unwrap();
    assert_eq!(window.runtime.store.journal().len(), 2);
    assert_eq!(window.runtime.store.journal()[0].outcome, failed);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
}

#[test]
fn native_form_reopens_the_command_and_calendar() {
    use std::sync::Arc;

    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Maintenance".to_owned()),
        (Field::Host, "TEST.Example.".to_owned()),
        (Field::Username, "operator".to_owned()),
        (
            Field::KeyFile,
            profile.0.join("reference-key").display().to_string(),
        ),
        (Field::From, "2026-11-07".to_owned()),
        (Field::Until, "2026-11-20".to_owned()),
        (Field::WindowStart, "21:30".to_owned()),
        (Field::WindowEnd, "02:00".to_owned()),
        (Field::Interval, "30".to_owned()),
        (Field::Age, "45".to_owned()),
    ] {
        assert!(window.update(Message::Edit(field, value)).is_empty());
    }
    window.update(Message::FixedTimeInput("23:15, 21:37, 23:59, 21:37".into()));
    window.update(Message::AddFixedTimes);
    let retained: Time = "21:37".parse().unwrap();
    let removed: Time = "23:59".parse().unwrap();
    window.update(Message::SelectFixedTime(removed));
    window.update(Message::RemoveFixedTime(retained));
    window.update(Message::RemoveFixedTime(removed));
    window.update(Message::FixedTimeInput("12:00, 24:00".into()));
    window.update(Message::AddFixedTimes);
    assert_eq!(window.status(), Some("Enter schedule times as HH:MM"));
    window.update(Message::ScheduleEnabled(true));
    window.update(Message::AllWeekdays(true));
    window.update(Message::Weekday(Weekday::Saturday, false));
    window.update(Message::Mode(Mode::Chain));
    let command = "printf '%s\\n' first\nprintf '%s\\n' second".to_owned();
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(command.clone())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let original = window.selected_job().unwrap();
    let original_definition = original.definition();
    let original_action = original.action().clone();
    let schedule = original.schedule().unwrap();
    assert_eq!(
        schedule.fixed_times().collect::<Vec<_>>(),
        [retained, "23:15".parse::<Time>().unwrap()]
    );
    assert_eq!(schedule.weekdays().count(), 6);
    assert!(!schedule.weekdays().any(|day| day == Weekday::Saturday));
    assert_eq!(original_action.host, "test.example.");
    assert_eq!(original_action.command, command);
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    window.update(Message::Save);
    let reopened = window.selected_job().unwrap();
    assert_eq!(reopened.definition(), original_definition);
    assert_eq!(reopened.action(), &original_action);
    window.update(Message::FixedTimeInput("21:37".into()));
    window.update(Message::AddFixedTimes);
    assert!(!window.view.dirty());
    window.update(Message::FixedTimeInput("x".repeat(4097)));
    window.update(Message::AddFixedTimes);
    assert!(!window.view.dirty());
    window.update(Message::AllWeekdays(false));
    window.update(Message::Edit(Field::Interval, "0".into()));
    window.update(Message::Save);
    let saved = window.selected_job().unwrap();
    let saved_definition = saved.definition();
    let schedule = saved.schedule().unwrap();
    assert_eq!(schedule.weekdays().count(), 0);
    assert!(schedule.interval_minutes().is_none());
    assert_eq!(schedule.fixed_times().count(), 2);

    assert!(window.request_close());
    assert!(!window.confirm_close());
    window.update(Message::Edit(Field::Name, "Unsaved name".into()));
    assert!(!window.request_close());
    assert!(window.close_pending());
    window.update(Message::Save);
    assert!(window.close_pending());
    assert_eq!(window.selected_job().unwrap().name(), "Maintenance");
    window.update(Message::Dismiss);
    assert!(!window.close_pending());
    assert!(!window.confirm_close());
    assert!(!window.request_close());
    assert!(window.confirm_close());
    assert!(window.request_close());
    window.update(Message::Save);
    assert_eq!(
        window.selected_job().unwrap().definition(),
        saved_definition
    );
}

#[test]
fn native_form_reopens_each_sftp_action_and_honors_the_scheduler_policy() {
    for (source, kind) in [
        (Source::RemoteMkdir, "mkdir"),
        (Source::RemoteRename, "rename"),
        (Source::RemoteChmod, "chmod"),
        (Source::RemoteChown, "chown"),
        (Source::RemoteCopy, "copy_remote"),
        (Source::RemoteUpload, "upload"),
        (Source::RemoteDownload, "download"),
        (Source::RemoteUploadFolder, "upload"),
        (Source::RemoteDownloadFolder, "download"),
    ] {
        let profile = Profile::temporary();
        let path = profile.file();
        let mut window = TestWindow::open(path.clone()).unwrap();
        window.update(Message::VirtualTerminal(true));
        window.update(Message::CommandSource(source));
        for (field, value) in [
            (Field::Name, "SFTP metadata".to_owned()),
            (Field::Host, "example.test".to_owned()),
            (Field::Username, "operator".to_owned()),
            (Field::KeyFile, profile.0.join("key").display().to_string()),
            (Field::RemotePath, "/canonical/remote path".to_owned()),
            (Field::RemoteDestination, "/canonical/renamed".to_owned()),
            (Field::RemoteMode, "0640".to_owned()),
            (Field::RemoteUid, "1001".to_owned()),
            (
                Field::LocalFile,
                profile.0.join("configured-file").display().to_string(),
            ),
        ] {
            window.update(Message::Edit(field, value));
        }
        window.update(Message::Save);
        assert_eq!(window.status(), Some("Job saved"));
        let action = window.selected_job().unwrap().action().clone();
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["remote"]["kind"], kind);
        assert_eq!(
            json["remote"]["folder"].as_bool().unwrap_or(false),
            matches!(
                source,
                Source::RemoteUploadFolder | Source::RemoteDownloadFolder
            )
        );
        assert!(action.virtual_terminal.is_none());
        assert!(action.command_for_exec(None).is_err());
        drop(window);

        let mut window = TestWindow::open(path).unwrap();
        assert_eq!(window.view.draft.source, source);
        window.update(Message::Save);
        assert_eq!(window.selected_job().unwrap().action(), &action);
        assert!(window.update(Message::Run(false)).is_empty());
        window.runtime.set_execution_allowed(true);
        let ticket = window.update(Message::Run(false)).pop().unwrap();
        assert_eq!(ticket.action.as_ref(), &action);
    }
}

#[test]
fn scheduled_delete_requires_exact_confirmation_after_scope_or_endpoint_changes() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteDelete));
    let target = "/canonical/stale output";
    for (field, value) in [
        (Field::Name, "Remove stale output".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, target.into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    let save_without_confirmation = |window: &mut TestWindow| {
        window.update(Message::Save);
        assert_eq!(
            window.status(),
            Some("Repeat the exact remote path to authorize deletion")
        );
    };
    let confirm_and_save = |window: &mut TestWindow| {
        window.update(Message::Edit(
            Field::RemoteDeleteConfirmation,
            target.into(),
        ));
        window.update(Message::Save);
        assert_eq!(window.status(), Some("Job saved"));
    };
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    assert!(matches!(
        window.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Delete {
            recursive: false,
            ..
        })
    ));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    window.update(Message::RecursiveDelete(true));
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    assert!(matches!(
        window.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Delete {
            recursive: true,
            ..
        })
    ));

    window.update(Message::Edit(Field::RemotePath, "/other/path".into()));
    window.update(Message::Edit(Field::RemotePath, target.into()));
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    window.update(Message::Edit(Field::Host, "other.test".into()));
    save_without_confirmation(&mut window);
}

#[test]
fn native_archive_form_saves_and_reopens_sources_compression_and_optional_local_copy() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteArchive));
    window.update(Message::ArchiveFormat(ArchiveFormat::TarBz2));
    for (field, value) in [
        (Field::Name, "Remote backup".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, "/canonical/result.tar.bz2".into()),
        (
            Field::ArchiveDownload,
            profile.0.join("local copy.tar.bz2").display().to_string(),
        ),
        (Field::ArchiveLevel, "7".into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditArchiveSources(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("/canonical/one file\n/canonical/second".into())),
    )));
    window.update(Message::EditArchiveExcludes(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(" *.tmp \ncache/?.txt\n".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action().clone();
    let remote = serde_json::to_value(&action).unwrap()["remote"].clone();
    assert_eq!(remote["kind"], "archive");
    assert_eq!(remote["format"], "tar_bz2");
    assert_eq!(
        remote["excludes"],
        serde_json::json!(["*.tmp", "cache/?.txt"])
    );
    assert_eq!(
        remote["sources"],
        serde_json::json!(["/canonical/one file", "/canonical/second"])
    );
    assert_eq!(remote["level"], 7);
    assert!(action.remote_action().unwrap().transfers_files());
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.view.draft.source, Source::RemoteArchive);
    assert_eq!(window.view.draft.archive_format, ArchiveFormat::TarBz2);
    window.update(Message::Save);
    assert_eq!(window.selected_job().unwrap().action(), &action);
    window.update(Message::Edit(Field::ArchiveLevel, "0".into()));
    window.update(Message::Save);
    assert_eq!(
        window.status(),
        Some("Choose ZIP compression 0–9 or bzip2 compression 1–9")
    );

    for sources in [
        vec!["/a/report", "/b/report"],
        vec!["/canonical/result.tar.bz2"],
        vec![],
    ] {
        let mut invalid = remote.clone();
        invalid["sources"] = serde_json::json!(sources);
        assert!(
            serde_json::from_value::<RemoteAction>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    let mut invalid = remote;
    invalid["excludes"] = serde_json::json!(["unclosed["]);
    assert!(
        serde_json::from_value::<RemoteAction>(invalid)
            .unwrap()
            .validate()
            .is_err()
    );
    let legacy: RemoteAction = serde_json::from_value(serde_json::json!({
        "kind": "archive_zip", "sources": ["/source"], "target": "/target.zip", "level": 6,
    }))
    .unwrap();
    assert!(legacy.validate().is_ok());
    assert!(matches!(legacy, RemoteAction::Archive(Archive {
        format: ArchiveFormat::Zip, excludes, ..
    }) if excludes.is_empty()));
}

#[test]
fn encrypted_zip_form_persists_only_a_vault_reference() {
    use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

    let profile = Profile::temporary();
    let path = profile.file();
    let mut vault = Vault::create(profile.0.join("vault"), b"test master password").unwrap();
    let password = "test-only distinct AES ZIP password";
    let id = vault
        .save(
            CredentialInput {
                label: "Backup password".into(),
                kind: CredentialKind::ArchivePassword,
                ..Default::default()
            },
            Secret::new(password.into()),
        )
        .unwrap();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteArchive));
    window.update(Message::ArchiveFormat(ArchiveFormat::ZipPassword));
    for (field, value) in [
        (Field::Name, "Encrypted backup".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, "/target.zip".into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditArchiveSources(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("/source".into())),
    )));
    window.update(Message::Save);
    assert_eq!(
        window.status(),
        Some("Choose a ZIP archive password from the vault")
    );
    let choice = CredentialChoice {
        id: id.clone(),
        label: "Backup password".into(),
        username: String::new(),
    };
    window.runtime.set_archive_passwords(vec![choice.clone()]);
    window.update(Message::ArchivePassword(choice));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains(&id));
    assert!(!persisted.contains(password));
    let mut reopened = TestWindow::open(path).unwrap();
    assert_eq!(
        reopened.view.draft.archive_password.as_deref(),
        Some(id.as_str())
    );
    reopened.update(Message::ArchiveFormat(ArchiveFormat::Zip));
    reopened.update(Message::Save);
    assert!(matches!(
        reopened.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Archive(Archive {
            format: ArchiveFormat::Zip,
            password_credential: None,
            ..
        }))
    ));
}

#[test]
fn scheduler_persists_a_reviewed_saved_route_without_authentication_details() {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;

    let profile = Profile::temporary();
    let path = profile.file();
    let connections_path = profile.0.join("connections.json");
    let mut store = ConnectionStore::open(connections_path).unwrap();
    let mut saved = Connection {
        id: 0,
        name: "Target profile".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "target.example.test.".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::VaultPassword("vault-reference-test".into()),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: Default::default(),
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
    };
    let id = store.save(saved.clone()).unwrap();
    saved.id = id;
    let reference = ConnectionRef::from(&saved);
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_connections(Some(&store));
    window.update(Message::ConnectionSelected(
        window.runtime.connection_choices[1].clone(),
    ));
    window.update(Message::Edit(Field::Name, "Saved connection job".into()));
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action();
    assert_eq!(action.connection.as_ref(), Some(&reference));
    let json = serde_json::to_value(action).unwrap();
    assert!(json.get("key_file").is_none());
    assert!(!json.to_string().contains("vault-reference-test"));
    drop(window);

    let mut window = TestWindow::open(path.clone()).unwrap();
    saved.login.endpoint.port = 2222;
    store.save(saved).unwrap();
    window.runtime.set_connections(Some(&store));
    assert_ne!(
        window.runtime.connection_choices[1].reference(),
        Some(&reference)
    );
    window.update(Message::Save);
    assert_eq!(
        window.selected_job().unwrap().action().connection.as_ref(),
        Some(&reference)
    );
    window.update(Message::ConnectionSelected(
        window.runtime.connection_choices[1].clone(),
    ));
    window.update(Message::Save);
    assert_eq!(
        window
            .selected_job()
            .unwrap()
            .action()
            .connection
            .as_ref()
            .unwrap()
            .target
            .port,
        2222
    );

    let mut target = store.get(id).unwrap().clone();
    target.id = 0;
    target.name = "Second target".into();
    target.login.endpoint.host = "second.example.test".into();
    let second = store.save(target.clone()).unwrap();
    target.name = "Third target".into();
    target.login.endpoint.host = "third.example.test".into();
    let third = store.save(target).unwrap();
    window.runtime.set_connections(Some(&store));
    let choice = |window: &TestWindow, id| {
        window
            .runtime
            .connection_choices
            .iter()
            .find(|choice| {
                choice
                    .reference()
                    .is_some_and(|reference| reference.id == id)
            })
            .unwrap()
            .clone()
    };
    window.update(Message::ConnectionAdded(choice(&window, third)));
    window.update(Message::ConnectionAdded(choice(&window, second)));
    window.update(Message::ConnectionAdded(choice(&window, third)));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    let action = window.selected_job().unwrap().action().clone();
    assert_eq!(
        action
            .targets()
            .map(|reference| reference.id)
            .collect::<Vec<_>>(),
        [id, third, second]
    );
    let mut invalid = action.clone();
    invalid
        .additional_connections
        .push(invalid.connection.clone().unwrap());
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("once")
    );
    invalid.additional_connections.pop();
    invalid.virtual_terminal = Some(virtual_terminal::Options::default());
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("recordings")
    );
    invalid.virtual_terminal = None;
    invalid.remote = Some(RemoteAction::Download {
        local: profile.0.join("destination"),
        remote: "/remote/source".to_owned().try_into().unwrap(),
        folder: true,
    });
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("download")
    );

    let mut changed = store.get(second).unwrap().clone();
    changed.login.endpoint.port = 9922;
    store.save(changed).unwrap();
    assert!(
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, |_, _, _| panic!("a stale selection must block before vault lookup")), Err(error) if error.contains("route changed"))
    );
    store.delete(second).unwrap();
    assert!(
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, |_, _, _| panic!("a missing selection must block before vault lookup")), Err(error) if error.contains("missing"))
    );
    window.update(Message::ConnectionRemoved(second));
    window.update(Message::Save);
    assert_eq!(
        window
            .selected_job()
            .unwrap()
            .action()
            .targets()
            .map(|reference| reference.id)
            .collect::<Vec<_>>(),
        [id, third]
    );
}

#[test]
fn sftp_actions_reject_ambiguous_paths_and_incompatible_shell_settings() {
    for path in [
        "relative",
        "/",
        "/parent/..",
        "/parent//target",
        "/parent/\\..\\target",
        "/parent/CON",
        "/parent/target.",
        "/file:stream",
    ] {
        let document = serde_json::json!({"kind": "mkdir", "path": path});
        assert!(
            serde_json::from_value::<RemoteAction>(document).is_err(),
            "{path}"
        );
    }
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "host":"example.test", "port":22, "username":"operator",
        "key_file":Profile::temporary().0.join("key"),
        "remote":{"kind":"chmod", "path":"/allowed/file", "mode":4096},
    }))
    .unwrap();
    assert!(action.remote_action().is_err());
    action.remote =
        serde_json::from_value(serde_json::json!({"kind":"mkdir", "path":"/allowed/path"}))
            .unwrap();
    assert!(action.remote_action().is_ok());
    action.virtual_terminal = Some(virtual_terminal::Options::default());
    assert!(action.remote_action().is_err());
    action.virtual_terminal = None;
    action.command = "echo conflicting action".into();
    assert!(action.remote_action().is_err());

    for remote in [
        serde_json::json!({"kind":"copy_remote", "from":"/allowed/file", "to":"/allowed/file"}),
        serde_json::json!({"kind":"chown", "path":"/allowed/file"}),
        serde_json::json!({"kind":"chown", "path":"/allowed/file", "uid":4294967295u32}),
    ] {
        action.command.clear();
        action.remote = serde_json::from_value(remote).unwrap();
        assert!(action.remote_action().is_err());
    }
    for invalid in ["root", "+1", "4294967295"] {
        assert!(remote::owner_id_from_text(invalid).is_err());
    }
    assert_eq!(remote::owner_id_from_text(" 0 "), Ok(Some(0)));
    assert_eq!(remote::owner_id_from_text(""), Ok(None));
}

#[test]
fn native_form_saves_and_reopens_the_script_source_and_separate_argv() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Script job".to_owned()),
        (Field::Host, "example.test".to_owned()),
        (Field::Username, "operator".to_owned()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::CommandSource(Source::Script));
    window.update(Message::ScriptLanguage(Language::Python));
    let body = "import sys\nprint(sys.argv[1:])";
    let paste =
        |value: &str| text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(value.into())));
    window.update(Message::EditCommand(paste(body)));
    window.update(Message::EditScriptArguments(paste(
        "two words\n\n apostrophe' \n--flag",
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action();
    let script = action.script.as_ref().unwrap();
    assert_eq!(script.language, Language::Python);
    assert_eq!(script.body, body);
    assert_eq!(script.arguments, ["two words", " apostrophe' ", "--flag"]);
    assert!(action.command.is_empty());
    let expected = action.clone();
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.view.draft.source, Source::Script);
    assert_eq!(window.view.draft.script_language, Language::Python);
    window.update(Message::Save);
    assert_eq!(window.selected_job().unwrap().action(), &expected);
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.action.as_ref(), &expected);
    let encoded = request.action.command_for_exec(None).unwrap();
    assert!(encoded.starts_with("exec python3 - 'two words'"));
}

#[test]
fn native_form_reopens_stable_snippet_link_and_missing_library_does_not_break_other_jobs() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    let mut library = Library::open(window.runtime.snippet_path.clone()).unwrap();
    let snippet = |name: &str, content: &str| crate::snippets::Input {
        name: name.into(),
        language: "bash".into(),
        category: String::new(),
        content: content.into(),
    };
    let id = library.save(None, snippet("Probe", "exit 99")).unwrap();
    window.runtime.refresh_snippets();
    for (field, value) in [
        (Field::Name, "Shared snippet job".to_owned()),
        (Field::Host, "example.test".to_owned()),
        (Field::Username, "operator".to_owned()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::CommandSource(Source::Snippet));
    window.update(Message::SnippetSelected(
        window.runtime.snippet_choices[0].clone(),
    ));
    window.update(Message::EditScriptArguments(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("one literal argument".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);

    let mut window = TestWindow::open(path.clone()).unwrap();
    assert_eq!(window.view.draft.source, Source::Snippet);
    assert_eq!(window.view.draft.snippet_id, Some(id));
    library
        .save(Some(id), snippet("Renamed", "exit 23"))
        .unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    let command = request
        .action
        .command_for_exec(Some(window.runtime.snippet_path()))
        .unwrap();
    assert!(command.starts_with("exec bash -s -- 'one literal argument'"));
    assert!(command.contains("\nexit 23\n"));
    assert!(!command.contains("exit 99"));
    library.remove(id).unwrap();
    drop(window);

    let window = TestWindow::open(path).unwrap();
    assert!(window.runtime.snippet_choices.is_empty());
    let action = window.selected_job().unwrap().action();
    assert_eq!(action.snippet.as_ref().unwrap().id, id);
    assert_eq!(
        action
            .command_for_exec(Some(window.runtime.snippet_path()))
            .unwrap_err(),
        "The linked snippet is missing"
    );
}
