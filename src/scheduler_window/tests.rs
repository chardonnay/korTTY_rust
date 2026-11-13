use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use kortty_jobs::{DependencyMode, Job};

use super::*;

fn grouped_connection(host: &str) -> crate::connection_store::Connection {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;
    Connection {
        id: 0,
        name: host.into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: host.into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::StoredKey(1),
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
        window_geometry: None,
        terminal: Default::default(),
    }
}

#[test]
fn scheduler_target_groups_persist_source_and_block_changed_membership_until_refresh() {
    let scratch = Profile::temporary();
    let path = scratch.file();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let parent = store.create_group(None, "Fleet").unwrap();
    let child = store.create_group(Some(&parent), "Testing").unwrap();
    let prefix_sibling = store.create_group(None, "Fleet-old").unwrap();
    let direct = store
        .save_in_group(grouped_connection("a.example.test"), Some(&parent))
        .unwrap();
    let nested = store
        .save_in_group(grouped_connection("b.example.test"), Some(&child))
        .unwrap();
    let _unrelated = store
        .save_in_group(grouped_connection("c.example.test"), Some(&prefix_sibling))
        .unwrap();
    let choice = ConnectionChoice::Saved {
        reference: store.get(nested).unwrap().into(),
        name: "Explicit first".into(),
    };
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_connections(Some(&store));
    app.update(Message::Edit(Field::Name, "Group job".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    app.update(Message::ConnectionSelected(choice));
    app.update(Message::ConnectionGroupAdded(parent.clone()));
    app.update(Message::ConnectionGroupAdded(child.clone()));
    app.update(Message::Save);
    let job = app.selected_job().expect("saved group job");
    let id = job.id().clone();
    let action = job.action().clone();
    assert_eq!(
        action.targets().map(|target| target.id).collect::<Vec<_>>(),
        [nested, direct]
    );
    let source = action.target_groups.as_ref().unwrap();
    assert_eq!(source.groups, [parent.clone(), child.clone()]);
    assert_eq!(source.explicit[0].id, nested);
    assert!(action.require_current_target_groups(&store).is_ok());
    drop(app);

    let added = store
        .save_in_group(grouped_connection("d.example.test"), Some(&child))
        .unwrap();
    assert!(action.require_current_target_groups(&store).is_err());
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_connections(Some(&store));
    assert_eq!(app.selected_job().unwrap().action(), &action);
    app.update(Message::Edit(Field::Name, "Changed job".into()));
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("group targets changed"));
    app.update(Message::RefreshConnectionGroups);
    app.update(Message::Save);
    let job = app.selected_job().unwrap();
    assert_eq!(job.id(), &id);
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [nested, direct, added]
    );
    assert_eq!(
        job.action().target_groups.as_ref().unwrap().groups,
        [parent, child]
    );
}

#[test]
fn scheduler_target_groups_allow_group_only_and_refuse_empty_or_oversized_groups() {
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let group = store.create_group(None, "Fleet").unwrap();
    let empty = store.create_group(None, "Empty").unwrap();
    let mut draft = Draft::default();
    assert!(draft.add_connection_group(empty, &store).is_err());
    assert!(!draft.has_connection_groups());
    let id = store
        .save_in_group(grouped_connection("only.example.test"), Some(&group))
        .unwrap();
    draft.set(Field::Name, "Group-only job".into());
    draft.edit_command(text_editor::Action::Edit(text_editor::Edit::Paste(
        Arc::new("true".into()),
    )));
    assert!(draft.add_connection_group(group.clone(), &store).unwrap());
    let job_id = JobId::new("group-only").unwrap();
    let action = draft.build(job_id.clone()).unwrap().action().clone();
    assert!(action.target_groups.as_ref().unwrap().explicit.is_empty());
    assert_eq!(
        action.targets().map(|target| target.id).collect::<Vec<_>>(),
        [id]
    );
    for number in 1..=MAX_TARGETS {
        store
            .save_in_group(
                grouped_connection(&format!("host-{number}.example.test")),
                Some(&group),
            )
            .unwrap();
    }
    assert!(draft.refresh_connection_groups(&store).is_err());
    assert_eq!(draft.build(job_id).unwrap().action(), &action);
}

#[test]
fn scheduler_target_groups_refresh_bootstraps_and_removes_stale_selections() {
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let mut app = TestWindow::open(scratch.file()).unwrap();
    app.runtime.set_connections(Some(&store));
    assert!(app.runtime.connection_group_choices.is_empty());
    let first = store.create_group(None, "First").unwrap();
    let second = store.create_group(None, "Second").unwrap();
    let a = store
        .save_in_group(grouped_connection("one.example.test"), Some(&first))
        .unwrap();
    let b = store
        .save_in_group(grouped_connection("two.example.test"), Some(&second))
        .unwrap();
    app.update(Message::RefreshConnectionGroups);
    assert_eq!(
        app.runtime.connection_group_choices,
        [first.clone(), second.clone()]
    );
    assert!(!app.view.dirty());
    app.update(Message::Edit(Field::Name, "Group recovery".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    app.update(Message::ConnectionGroupAdded(first.clone()));
    app.update(Message::ConnectionGroupAdded(second.clone()));
    let choice = ConnectionChoice::Saved {
        reference: store.get(b).unwrap().into(),
        name: "Explicit".into(),
    };
    app.update(Message::ConnectionAdded(choice));
    let job_id = JobId::new("group-recovery").unwrap();
    let job = app.view.draft.build(job_id.clone()).unwrap();
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [b, a]
    );
    app.update(Message::ConnectionRemoved(b));
    let job = app.view.draft.build(job_id.clone()).unwrap();
    assert!(
        job.action()
            .target_groups
            .as_ref()
            .unwrap()
            .explicit
            .is_empty()
    );
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [a, b]
    );

    let one = store.get(a).unwrap().clone();
    let two = store.get(b).unwrap().clone();
    store.save_in_group(one, None).unwrap();
    store.save_in_group(two, None).unwrap();
    app.update(Message::ConnectionGroupRemoved(first));
    assert!(app.status().unwrap().contains("no saved SSH targets"));
    assert!(app.view.draft.build(job_id.clone()).is_err());
    app.update(Message::ConnectionGroupRemoved(second));
    assert!(!app.view.draft.has_connection_groups());
    let choice = ConnectionChoice::Saved {
        reference: store.get(a).unwrap().into(),
        name: "Restored selection".into(),
    };
    app.update(Message::ConnectionSelected(choice));
    let job = app.view.draft.build(job_id).unwrap();
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [a]
    );
}

#[test]
fn swarm_scheduler_handoff_builds_disabled_unapproved_draft() {
    use crate::ai_manager::ProfileRef;
    use crate::ssh_dialog::LoginTarget;

    let targets = [1, 2].map(|id| ConnectionRef {
        id,
        target: LoginTarget {
            host: format!("host-{id}.example.test"),
            port: 22,
            username: "operator".into(),
        },
        jump: None,
    });
    let make = |read_only| SwarmHandoff {
        name: "Swarm fixture".into(),
        goal: "Check disk space".into(),
        profile: ProfileRef::Numeric(1),
        targets: targets.to_vec(),
        read_only,
        journal: Some(crate::session_journal::automation::Options {
            enabled: true,
            ..crate::session_journal::automation::Options::default()
        }),
    };
    for (read_only, probe) in [
        (true, agent::Probe::SwarmReadOnly),
        (false, agent::Probe::SwarmIterative),
    ] {
        let draft = Draft::from_swarm_handoff(make(read_only)).unwrap();
        assert!(draft.id.is_none());
        let job = draft.build(JobId::new("swarm-fixture").unwrap()).unwrap();
        assert!(!job.enabled());
        assert!(!job.schedule().unwrap().enabled());
        assert!(job.action().automation_journal.as_ref().unwrap().enabled);
        assert_eq!(
            job.action().targets().collect::<Vec<_>>(),
            targets.iter().collect::<Vec<_>>()
        );
        let action = job.action().agent.as_ref().unwrap();
        assert_eq!(action.probe, probe);
        assert_eq!(action.profile, ProfileRef::Numeric(1));
        assert_eq!(action.goal, "Check disk space");
        assert!(action.approval.is_none());
    }
    let mut duplicate = make(true);
    duplicate.targets[1] = duplicate.targets[0].clone();
    assert!(Draft::from_swarm_handoff(duplicate).is_err());
}

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
        if matches!(
            message,
            Message::Tick | Message::Finished(_) | Message::FinishedWithSessionJournals(..)
        ) {
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

pub(super) struct Profile(PathBuf);

impl Profile {
    pub(super) fn temporary() -> Self {
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

    pub(super) fn file(&self) -> PathBuf {
        self.0.join("scheduler.json")
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn save_reviewed_folder_copy(app: &mut TestWindow, standing_roots: Option<[u8; 32]>) {
    app.update(Message::Page(Page::Action));
    let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
    app.update(Message::SyncPreviewed(
        pending.ticket,
        Ok(Arc::new(sync::Review {
            preview: crate::sftp_window::scheduled::Preview {
                roots: ["Local reviewed".into(), "Server reviewed".into()],
                replacements: Vec::new(),
                paths: vec![(false, "new.txt".into())],
                proof: [3; 32],
                bytes: 4,
            },
            pins: vec!["server.example SHA256:fixture".into()],
            standing_roots,
        })),
    ));
    app.update(Message::ApproveSync);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
}

#[test]
fn reviewed_sftp_approval_round_trips_through_editor_and_claims_once() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("reviewed-sftp").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "upload", "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph =
            JobGraph::new(vec![Job::new(id.clone(), "Reviewed copy", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, None);
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    assert!(context.permits(&request));
    let replacement = RunRequest {
        id: request.id,
        job: request.job.clone(),
        action: Arc::new(request.action.as_ref().clone()),
        trigger: request.trigger,
        started_at: request.started_at,
        cancellation: request.cancellation.clone(),
    };
    assert!(!context.permits(&replacement));
    let replacement = RunRequest {
        action: Arc::clone(&request.action),
        cancellation: kortty_jobs::Cancellation::default(),
        ..replacement
    };
    assert!(app.runtime.sync_run_context(&replacement).is_none());
    assert!(!context.permits(&replacement));
    let RemoteAction::Sync(copy) = request.action.remote_action().unwrap() else {
        panic!("expected reviewed copy")
    };
    let approval = copy.approval.as_deref();
    assert!(approval.is_some());
    let receipt = context.store.load(&id, approval, context.scope).unwrap();
    assert_eq!(receipt.proof, [3; 32]);
    let wrong_scope = [255; 32];
    assert!(context.store.load(&id, approval, wrong_scope).is_err());
    context.store.claim(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
    assert!(context.store.load(&id, approval, context.scope).is_err());
}

#[cfg(windows)]
#[test]
fn scheduler_changed_download_edit_and_reopen_use_the_new_exact_approval() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("reviewed-replacement").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "download", "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Reviewed replacement", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    save_reviewed_folder_copy(&mut app, None);
    let old = app.selected_job().unwrap().action().clone();
    let RemoteAction::Sync(old_copy) = old.remote_action().unwrap() else {
        panic!("expected sync")
    };
    let old_nonce = old_copy.approval.clone().unwrap();

    app.update(Message::Page(Page::Action));
    app.update(Message::SyncReplaceChanged(true));
    let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
    assert!(pending.copy.replace_changed && pending.copy.approval.is_none());
    app.update(Message::SyncPreviewed(
        pending.ticket,
        Ok(Arc::new(sync::Review {
            preview: crate::sftp_window::scheduled::Preview {
                proof: [5; 32],
                roots: ["Remote reviewed".into(), "Local reviewed".into()],
                paths: vec![],
                replacements: vec![crate::sftp_window::scheduled::ReplacementPreview {
                    source: "/reviewed/changed.txt".into(),
                    target: "local/changed.txt".into(),
                    source_sha256: "a".repeat(64),
                    target_sha256: "b".repeat(64),
                }],
                bytes: 4,
            },
            pins: vec!["server.example SHA256:fixture".into()],
            standing_roots: None,
        })),
    ));
    app.update(Message::ApproveSync);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let RemoteAction::Sync(copy) = request.action.remote_action().unwrap() else {
        panic!("expected sync")
    };
    assert!(copy.replace_changed);
    assert!(context.load_receipt(Some(&old_nonce)).is_err());
    let receipt = context.load_receipt(copy.approval.as_deref()).unwrap();
    assert_eq!(receipt.proof, [5; 32]);
    context.store.claim(&id, &receipt).unwrap();
    context.store.require_claimed(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
}

#[test]
fn legacy_download_review_reopens_but_execution_requires_a_fresh_editor_review() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("legacy-download").unwrap();
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "download", "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let Some(RemoteAction::Sync(copy)) = action.remote.as_mut() else {
        panic!("expected ordinary download");
    };
    let old_scope = copy
        .scope(action.connection.as_ref().unwrap(), None)
        .unwrap();
    let old =
        sync::Receipt::for_operation(old_scope, [1; 32], vec!["pinned route".into()]).unwrap();
    copy.approval = Some(old.approval.clone());
    sync::Store::new(&root)
        .swap(&id, old.clone(), |_| true)
        .unwrap();
    let graph = JobGraph::new(vec![Job::new(id.clone(), "Download", action).unwrap()]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        let request = app.update(Message::Run(false)).pop().unwrap();
        let context = app.runtime.sync_run_context(&request).unwrap();
        assert_ne!(context.scope, old_scope);
        assert_eq!(
            context
                .load_receipt(Some(&old.approval))
                .err()
                .unwrap()
                .to_string(),
            "This scheduled SFTP copy needs a fresh folder review."
        );
    }

    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, None);
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let Ok(RemoteAction::Sync(copy)) = request.action.remote_action() else {
        panic!("expected reviewed download");
    };
    assert_ne!(copy.approval.as_deref(), Some(old.approval.as_str()));
    let fresh = context.load_receipt(copy.approval.as_deref()).unwrap();
    assert_eq!(fresh.proof, [3; 32]);
    context.store.claim(&id, &fresh).unwrap();
    assert!(context.load_receipt(copy.approval.as_deref()).is_err());
}

#[test]
fn recurring_sftp_editor_reopens_and_authorizes_only_its_calendar() {
    let profile = Profile::temporary();
    let id = JobId::new("recurring-copy").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "upload", "local": profile.0.join("local"), "remote": "/reviewed",
            "standing": {"days": 7, "limits": {"max_firings": 3, "max_actions": 8, "max_bytes": 1048576}}}
    })).unwrap();
    let path = profile.file();
    let schedule = kortty_jobs::JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]);
    {
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Recurring copy", action)
                .unwrap()
                .with_schedule(schedule),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.zone = TimeZone::UTC;
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, Some([4; 32]));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.zone = TimeZone::UTC;
    app.runtime.set_execution_allowed(true);
    app.runtime
        .scheduler
        .initialize_schedules(TimeZone::UTC, SystemTime::now())
        .unwrap();
    app.update(Message::Select(id.clone()));
    for cascade in [false, true] {
        assert!(app.update(Message::Run(cascade)).is_empty());
        assert_eq!(
            app.status(),
            Some("Recurring SFTP copies start through their approved calendar.")
        );
    }
    let due = app.runtime.scheduler.scheduled_at(&id).unwrap();
    let request = app
        .runtime
        .update_runtime_at(Message::Tick, due)
        .pop()
        .unwrap();
    assert_eq!(request.trigger, RunTrigger::Scheduled);
    let context = app.runtime.sync_run_context(&request).unwrap();
    let (approval, _) = context.standing_approval().unwrap();
    assert_eq!(approval.anchors, [4; 32]);
    assert_eq!(approval.limits.max_firings, 3);
}

#[test]
fn reviewed_sftp_cascade_refreshes_a_spent_receipt_and_keeps_the_other_branch() {
    let profile = Profile::temporary();
    let root = JobId::new("cascade-root").unwrap();
    let copy = JobId::new("cascade-copy").unwrap();
    let sibling = JobId::new("cascade-sibling").unwrap();
    let prerequisite = JobId::new("cascade-prerequisite").unwrap();
    let route = serde_json::json!({"id": 1, "target": {
        "host": "server.example", "port": 22, "username": "operator"
    }});
    let command: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route, "command": "true"
    }))
    .unwrap();
    let transfer: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route,
        "remote": {"kind": "sync", "direction": "upload", "local": profile.0.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let path = profile.file();
    {
        let graph = JobGraph::new(vec![
            Job::new(root.clone(), "Root", command.clone())
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![prerequisite.clone()], None),
            Job::new(copy.clone(), "Copy", transfer)
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![root.clone()], None),
            Job::new(sibling.clone(), "Sibling", command.clone())
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![root.clone()], None),
            Job::new(prerequisite.clone(), "Prerequisite", command).unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(copy.clone()));
    save_reviewed_folder_copy(&mut app, None);
    app.update(Message::Select(root.clone()));
    assert!(app.update(Message::Run(true)).is_empty());
    assert!(
        matches!(&app.view.confirm, Some(Confirmation::SyncCascade(review, _))
        if review.copies().eq([(&copy, true)]))
    );

    let mut other_view = app.runtime.new_view();
    app.runtime
        .update_view(&mut other_view, Message::Select(copy.clone()));
    assert!(
        app.runtime
            .update_view(&mut other_view, Message::Run(false))
            .is_empty()
    );
    let manual = app
        .runtime
        .update_view(&mut other_view, Message::Confirm)
        .pop()
        .unwrap();
    let context = app.runtime.sync_run_context(&manual).unwrap();
    let RemoteAction::Sync(action) = manual.action.remote_action().unwrap() else {
        unreachable!()
    };
    let receipt = context.load_receipt(action.approval.as_deref()).unwrap();
    context.store.claim(&copy, &receipt).unwrap();
    let finish = |id| {
        Message::Finished(Completion {
            id,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            swarm_report: None,
        })
    };
    assert!(app.update(finish(manual.id)).is_empty());

    assert!(app.update(Message::Confirm).is_empty());
    assert!(
        matches!(&app.view.confirm, Some(Confirmation::SyncCascade(review, _))
        if review.copies().eq([(&copy, false)]))
    );
    let start = app.update(Message::Confirm).pop().unwrap();
    assert_eq!(start.job, root);
    assert!(!app.runtime.reviewed_cascade_root_blocked(&start));

    app.runtime
        .update_view(&mut other_view, Message::Select(prerequisite));
    let failed = app
        .runtime
        .update_view(&mut other_view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(
        app.update(Message::Finished(Completion {
            id: failed.id,
            outcome: RunOutcome::Failed { exit_code: Some(1) },
            detail: None,
            recording: None,
            swarm_report: None,
        }))
        .is_empty()
    );
    assert!(app.runtime.reviewed_cascade_root_blocked(&start));
    let recovered = app
        .runtime
        .update_view(&mut other_view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(app.update(finish(recovered.id)).is_empty());
    assert!(!app.runtime.reviewed_cascade_root_blocked(&start));

    let starts = app.update(finish(start.id));
    assert_eq!(starts.len(), 2);
    assert!(starts.iter().any(|request| request.job == sibling));
    let denied = starts.iter().find(|request| request.job == copy).unwrap();
    assert!(app.runtime.sync_run_context(denied).is_none());
}

#[test]
fn privileged_tree_upload_uses_editor_review_and_a_distinct_one_use_receipt() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("reviewed-root-tree").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "sudo": "passwordless",
        "remote": {"kind": "privileged_tree_upload", "direction": "upload",
                   "local": root.join("local"), "remote": "/protected/new", "excludes": "*.tmp"}
    })).unwrap();
    let mut forged = action.clone();
    forged.sudo = SudoMode::Disabled;
    assert!(forged.validated().is_err());
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Reviewed root tree", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        assert!(app.view.editing_privileged_tree());
        let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
        assert_eq!(pending.sudo, SudoMode::Passwordless);
        app.update(Message::SyncPreviewed(
            pending.ticket,
            Ok(Arc::new(sync::Review {
                preview: crate::sftp_window::scheduled::Preview {
                    roots: ["Local reviewed".into(), "Server reviewed".into()],
                    replacements: Vec::new(),
                    paths: vec![(true, "empty".into()), (false, "new.txt".into())],
                    proof: [8; 32],
                    bytes: 4,
                },
                pins: vec!["server.example SHA256:fixture".into()],
                standing_roots: None,
            })),
        ));
        app.update(Message::ApproveSync);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let RemoteAction::PrivilegedTreeUpload(copy) = request.action.remote_action().unwrap() else {
        panic!("expected privileged tree upload");
    };
    let reference = request.action.connection.as_ref().unwrap();
    let schedule = app.runtime.scheduler.graph().job(&id).unwrap().schedule();
    let normal = copy.scope(reference, schedule).unwrap();
    assert_ne!(context.scope, normal);
    assert_ne!(
        context.scope,
        copy.privileged_scope(reference, schedule, SudoMode::Vault)
            .unwrap()
    );
    let approval = copy.approval.as_deref();
    let receipt = context.store.load(&id, approval, context.scope).unwrap();
    assert_eq!(receipt.proof, [8; 32]);
    assert!(context.store.require_claimed(&id, &receipt).is_err());
    context.store.claim(&id, &receipt).unwrap();
    context.store.require_claimed(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
}

#[test]
fn privileged_tree_download_requires_complete_source_and_saved_elevation() {
    let profile = Profile::temporary();
    let path = profile.0.canonicalize().unwrap().join("new-folder");
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "sudo": "passwordless",
        "remote": {"kind": "privileged_tree_download", "direction": "download",
                   "local": path, "remote": "/protected/source"}
    })).unwrap();
    assert!(action.clone().validated().is_ok());
    assert!(action.remote_action().unwrap().writes_local_file());
    let mut missing_sudo = action.clone();
    missing_sudo.sudo = SudoMode::Disabled;
    assert!(missing_sudo.validated().is_err());
    let mut excluded = action.clone();
    let Some(RemoteAction::PrivilegedTreeDownload(copy)) = &mut excluded.remote else {
        unreachable!();
    };
    copy.excludes = "*.tmp".into();
    assert!(excluded.validated().is_err());
    let mut wrong_direction = action;
    let Some(RemoteAction::PrivilegedTreeDownload(copy)) = &mut wrong_direction.remote else {
        unreachable!();
    };
    copy.direction = sync::Direction::Upload;
    assert!(wrong_direction.validated().is_err());
}

#[test]
fn privileged_staging_is_explicit_and_saved_review_binds_recurring_transfers() {
    use kortty_ssh::HostKey;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("privileged-sftp").unwrap();
    let ordinary: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {
            "id": 7,
            "target": {"host": "server.example", "port": 22, "username": "operator"},
            "jump": {"host": "jump.example", "port": 2222, "username": "relay"}
        },
        "remote": {"kind":"upload", "local":root.join("config.txt"), "remote":"/approved/config.txt"}
    })).unwrap();
    assert!(ordinary.clone().validated().is_ok());
    let mut forged = ordinary.clone();
    forged.sudo = SudoMode::Passwordless;
    assert!(forged.validated().is_err());
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [
        (Endpoint::new("jump.example", 2222).unwrap(), key.clone()),
        (Endpoint::new("server.example", 22).unwrap(), key),
    ];
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Privileged fixture", ordinary).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::Sudo(SudoMode::Vault));
        app.update(Message::Save);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("Review the exact"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::Edit(Field::RemotePath, "/other/config.txt".into()));
        app.update(Message::ApprovePrivileged);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("changed"))
        );
        app.update(Message::Edit(
            Field::RemotePath,
            "/approved/config.txt".into(),
        ));
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));

        app.update(Message::Edit(Field::RemotePath, "/new/config.txt".into()));
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        let backup = root.join("previous.json");
        fs::rename(&path, &backup).unwrap();
        fs::create_dir(&path).unwrap();
        app.update(Message::Save);
        assert_ne!(app.status(), Some("Job saved"));
        assert!(app.view.dirty());
        fs::remove_dir(&path).unwrap();
        fs::rename(backup, &path).unwrap();
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::Privileged(transfer) = request.action.remote_action().unwrap() else {
        panic!("expected explicitly approved staging");
    };
    let route = request.action.connection.as_ref().unwrap();
    assert!(transfer.approval.is_some());
    for _ in 0..2 {
        assert!(
            context
                .authorize(transfer, route, SudoMode::Vault, &pins)
                .is_ok()
        );
    }
    assert!(
        context
            .authorize(transfer, route, SudoMode::Vault, &pins[1..])
            .is_err()
    );
    assert!(
        context
            .authorize(transfer, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    let mut wrong_route = route.clone();
    wrong_route.target.username = "other".into();
    assert!(
        context
            .authorize(transfer, &wrong_route, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut changed = transfer.clone();
    changed.local = root.join("other.txt");
    assert!(
        context
            .authorize(&changed, route, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut edited = Draft::from_job(app.selected_job().unwrap());
    edited.select_source(Source::Command);
    assert_eq!(edited.sudo, SudoMode::Disabled);
}

#[test]
fn privileged_file_action_review_persists_and_binds_the_typed_operation() {
    use crate::sftp_window::privileged_file::ProtectedOperation;
    use kortty_ssh::HostKey;
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.0.join("scheduler.json");
    let id = JobId::new("privileged-metadata").unwrap();
    let connection = json!({
        "id": 7,
        "target": {"host": "server.example", "port": 22, "username": "operator"}
    });
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [(Endpoint::new("server.example", 22).unwrap(), key)];
    for operation in [
        json!({"kind": "mkdir"}),
        json!({"kind": "chmod", "mode": 416}),
        json!({"kind": "chown", "uid": null, "gid": 42}),
        json!({"kind": "copy", "to": "/approved/copied"}),
        json!({"kind": "copy_tree", "to": "/approved/copied-tree"}),
        json!({"kind": "rename", "to": "/approved/renamed"}),
        json!({"kind": "delete", "recursive": false}),
        json!({"kind": "delete", "recursive": true}),
    ] {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": connection,
            "sudo": "passwordless",
            "remote": {
                "kind": "privileged_file_action",
                "path": "/approved/target",
                "operation": operation
            }
        }))
        .unwrap();
        let action = action.validated().unwrap();
        assert!(!action.remote_action().unwrap().transfers_files());
        let job = Job::new(id.clone(), "Privileged metadata fixture", action.clone()).unwrap();
        let mut draft = Draft::from_job(&job);
        assert_eq!(draft.build(id.clone()).unwrap().action(), &action);
        if operation["kind"] == "copy_tree" {
            draft.set_privileged_copy_tree(false);
            let changed = draft.build(id.clone()).unwrap();
            assert!(changed.action().remote_action() != action.remote_action());
            draft.set_privileged_copy_tree(true);
            draft.set(Field::RemoteDestination, "/approved/target/child".into());
            assert!(draft.build(id.clone()).is_err());
        }
        if operation["kind"] == "delete" {
            draft.set(Field::RemotePath, "/approved/changed".into());
            assert!(draft.build(id.clone()).is_err());
            draft.set(Field::RemoteDeleteConfirmation, "/approved/changed".into());
            assert!(draft.build(id.clone()).is_ok());
        }
    }
    let ordinary: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "remote": {"kind": "chmod", "path": "/approved/target", "mode": 416}
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Metadata fixture", ordinary).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::Sudo(SudoMode::Passwordless));
        app.update(Message::Save);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("Review the exact"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::Edit(Field::RemoteMode, "0600".into()));
        app.update(Message::ApprovePrivileged);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("changed"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::PrivilegedFileAction(metadata) = request.action.remote_action().unwrap()
    else {
        panic!("expected the reviewed privileged file action");
    };
    assert_eq!(
        metadata.operation,
        ProtectedOperation::Chmod { mode: 0o600 }
    );
    assert!(metadata.approval.is_some());
    let route = request.action.connection.as_ref().unwrap();
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &[])
            .is_err()
    );
    let mut changed = metadata.clone();
    changed.operation = ProtectedOperation::Chmod { mode: 0o640 };
    assert!(
        context
            .authorize_file_action(&changed, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    changed.operation = ProtectedOperation::Chmod { mode: 0o4600 };
    assert!(changed.validate().is_err());
    let mut edited = Draft::from_job(app.selected_job().unwrap());
    edited.select_source(Source::Command);
    assert_eq!(edited.sudo, SudoMode::Disabled);
}

#[test]
fn privileged_archive_review_binds_saved_spec_route_and_pins() {
    use kortty_ssh::HostKey;
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("privileged-archive").unwrap();
    let command: SshCommand = serde_json::from_value(json!({
        "connection": {
            "id": 7,
            "target": {"host": "server.example", "port": 22, "username": "operator"}
        },
        "remote": {
            "kind": "server_archive",
            "sources": ["/protected/logs", "/protected/config"],
            "target": "/backups/snapshot.tar.bz2",
            "format": "tar_bz2",
            "level": 6,
            "excludes": ["*.tmp"]
        }
    }))
    .unwrap();
    let job = Job::new(id.clone(), "Protected archive", command).unwrap();
    let graph = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [(Endpoint::new("server.example", 22).unwrap(), key)];
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    assert!(app.view.editing_privileged_file_action());
    app.update(Message::Sudo(SudoMode::Vault));
    assert_eq!(app.view.draft.sudo, SudoMode::Disabled);
    app.update(Message::Sudo(SudoMode::Passwordless));
    app.update(Message::Save);
    assert!(
        app.status()
            .is_some_and(|status| status.contains("Review the exact"))
    );
    app.runtime
        .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    app.update(Message::Edit(Field::ArchiveLevel, "7".into()));
    app.update(Message::ApprovePrivileged);
    assert!(
        app.status()
            .is_some_and(|status| status.contains("changed"))
    );
    app.runtime
        .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    app.update(Message::ApprovePrivileged);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut reopened = TestWindow::open(path).unwrap();
    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Select(id.clone()));
    let request = reopened.update(Message::Run(false)).pop().unwrap();
    let context = reopened.runtime.privileged_run_context(&request).unwrap();
    let route = request.action.connection.as_ref().unwrap();
    let RemoteAction::PrivilegedArchive(archive) = request.action.remote_action().unwrap() else {
        panic!("expected a protected archive");
    };
    assert_eq!(archive.archive.level, 7);
    assert!(!request.action.remote_action().unwrap().transfers_files());
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Passwordless, &[])
            .is_err()
    );
    let mut other_route = route.clone();
    other_route.id += 1;
    assert!(
        context
            .authorize_archive(archive, &other_route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    let mut changed = archive.clone();
    changed.archive.sources.swap(0, 1);
    assert!(
        context
            .authorize_archive(&changed, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    changed.archive.target = "/protected/logs/snapshot.tar.bz2"
        .to_owned()
        .try_into()
        .unwrap();
    assert!(changed.validate().is_err());

    let mut draft = Draft::from_job(reopened.selected_job().unwrap());
    assert_eq!(
        draft.build(id.clone()).unwrap().action(),
        request.action.as_ref()
    );
    assert!(!draft.select_archive_format(ArchiveFormat::ZipPassword));
    assert!(draft.set_archive_server_only(false));
    assert_eq!(draft.sudo, SudoMode::Disabled);
    assert!(matches!(
        draft.build(id).unwrap().action().remote_action(),
        Ok(RemoteAction::Archive(_))
    ));
}

#[test]
fn scheduled_agent_editor_saves_exact_probe_approval_and_invalidates_edits() {
    check_scheduled_agent_editor_saves_exact_probe_approval(ProfileRef::Numeric(7));
}

#[test]
fn scheduled_agent_editor_preserves_exact_named_approval_on_reopen() {
    check_scheduled_agent_editor_saves_exact_probe_approval(ProfileRef::Policy(
        "policy-Diagnostics".into(),
    ));
}

fn check_scheduled_agent_editor_saves_exact_probe_approval(profile_ref: ProfileRef) {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;
    use kortty_ssh::{HostKey, TrustStore};

    let scratch = Profile::temporary();
    let path = scratch.file();
    let id = JobId::new("diagnostics").unwrap();
    let connection = Connection {
        id: 1,
        name: "Diagnostic target".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "example.test".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::VaultPassword("fixture-vault-reference".into()),
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
        window_geometry: None,
        terminal: Default::default(),
    };
    let reference = ConnectionRef::from(&connection);
    let host = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let trust =
        TrustStore::from_pins(vec![(Endpoint::new("example.test", 22).unwrap(), host)]).unwrap();
    let target = agent::Target::capture(&reference, &connection, &trust).unwrap();
    let mut changed = connection.clone();
    changed.terminal.ai_agent_disabled = true;
    assert!(agent::Target::capture(&reference, &changed, &trust).is_err());
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": reference,
        "working_directory": "/srv",
        "agent": {"kind":"agent","probe":"system","profile":profile_ref,"goal":"Summarize the system"}
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph =
            JobGraph::new(vec![Job::new(id.clone(), "Diagnostics", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::AutomationJournal(
        automation_journal::Event::Enabled(true),
    ));
    app.update(Message::AutomationJournal(
        automation_journal::Event::Input(true),
    ));
    let review = app.runtime.begin_agent_review(&app.view).unwrap();
    let profile = agent::ProfileChoice {
        id: profile_ref.clone(),
        name: "Local".into(),
        model: "fixture".into(),
        destination: "127.0.0.1:11434".into(),
        policy_model_fingerprint: matches!(profile_ref, ProfileRef::Policy(_)).then_some([7; 32]),
        reviewed_https: false,
        reviewed_cli: None,
    };
    app.runtime
        .finish_agent_review(&mut app.view, review, profile, vec![target])
        .unwrap();
    app.update(Message::ApproveAgent);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let action = request.action.agent.as_ref().unwrap();
    assert_eq!(action.profile, profile_ref);
    assert_eq!(action.approval.as_ref().unwrap().profile.id, profile_ref);
    let journal = request.action.automation_journal.as_ref().unwrap();
    assert!(journal.enabled && journal.capture_input);
    assert!(app.runtime.agent_run_approved(&request));
    assert!(app.runtime.active_agent_run_approved(&request));
    assert!(app.runtime.cancel_menu_run(request.id.value()));
    assert!(!app.runtime.active_agent_run_approved(&request));
    drop(app);
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::AgentProbe(agent::Probe::Filesystem));
    app.update(Message::Save);
    let request = app.update(Message::Run(false)).pop().unwrap();
    assert!(!app.runtime.agent_run_approved(&request));
}

#[test]
fn rsync_program_approval_is_bound_to_the_saved_editor_executable_and_route() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("approved-rsync").unwrap();
    let program = root.join("rsync");
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "rsync_download", "program": program, "local": root.join("new-file"), "remote": "/reviewed"}
    })).unwrap();
    let approval = rsync::Approval {
        program,
        digest: [8; 32],
        reference: action.connection.clone().unwrap(),
        pins: vec!["server.example SHA256:fixture".into()],
        upload: None,
        directory: None,
        directory_upload: None,
    };
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Rsync download", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.view.rsync_review.begin("first".into());
    app.update(Message::RsyncProgramReviewed("first".into(), Ok(approval)));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    let action = app.selected_job().unwrap().action();
    let RemoteAction::RsyncDownload(download) = action.remote_action().unwrap() else {
        panic!("expected rsync download");
    };
    assert_eq!(download.approval.as_ref().unwrap().digest, [8; 32]);
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::Edit(
        Field::RsyncProgram,
        root.join("different").to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    let request = app.update(Message::Run(false)).pop().unwrap();
    let RemoteAction::RsyncDownload(download) = request.action.remote_action().unwrap() else {
        panic!("expected rsync download");
    };
    assert!(download.approval.is_none());
    assert!(request.action.remote_action().unwrap().transfers_files());
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
    assert!(
        runtime
            .update_view(&mut second, Message::Run(false))
            .is_empty()
    );
    assert!(
        second
            .status
            .as_deref()
            .unwrap()
            .contains("changed in another view")
    );
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
                swarm_report: None,
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
fn scheduler_run_saves_a_new_draft_before_dispatch() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Maintenance".to_owned()),
        (Field::Host, "server.example".to_owned()),
        (Field::Username, "operator".to_owned()),
        (
            Field::KeyFile,
            profile.0.join("fixture-key").display().to_string(),
        ),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(std::sync::Arc::new("printf maintenance".into())),
    )));

    assert!(window.update(Message::Run(false)).is_empty());
    assert!(window.runtime.store.definitions().is_empty());
    assert!(window.view.dirty());

    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.trigger, RunTrigger::Manual);
    assert_eq!(request.action.command, "printf maintenance");
    assert_eq!(window.view.draft.id.as_ref(), Some(&request.job));
    assert!(!window.view.dirty());
    drop(window);

    let restored = TestWindow::open(path).unwrap();
    let job = restored.selected_job().unwrap();
    assert_eq!(job.id(), &request.job);
    assert_eq!(job.name(), "Maintenance");
    assert_eq!(job.action(), request.action.as_ref());
}

#[test]
fn scheduler_run_saves_the_draft_before_checking_cascade_dependencies() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("first").unwrap();
    let predecessor = JobId::new("external-predecessor").unwrap();
    let successor = JobId::new("successor").unwrap();
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
            Job::new(predecessor.clone(), "External predecessor", action.clone()).unwrap(),
            Job::new(successor.clone(), "Successor", action)
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![first.clone()], None),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path).unwrap();
    window.runtime.set_execution_allowed(true);
    window.update(Message::Select(first.clone()));
    window.update(Message::Dependency(predecessor.clone(), true));
    window.update(Message::Edit(Field::WorkingDirectory, "/updated".into()));
    assert!(window.update(Message::Run(true)).is_empty());
    assert!(!window.runtime.has_active_runs());
    assert!(!window.view.dirty());
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::Run(job, true, blockers))
            if job == &first && blockers.iter().any(|blocker| blocker.predecessor == predecessor)
    ));
    let saved = window
        .runtime
        .store
        .definitions()
        .iter()
        .find(|definition| definition.id == first)
        .unwrap();
    assert_eq!(
        saved.dependencies.as_slice(),
        std::slice::from_ref(&predecessor)
    );

    let mut predecessor_view = window.runtime.new_view();
    window
        .runtime
        .update_view(&mut predecessor_view, Message::Select(predecessor.clone()));
    let predecessor_run = window
        .runtime
        .update_view(&mut predecessor_view, Message::Run(false))
        .pop()
        .unwrap();
    let failed = RunOutcome::Failed { exit_code: Some(1) };
    assert!(
        window
            .update(Message::Finished(Completion {
                id: predecessor_run.id,
                outcome: failed,
                detail: None,
                recording: None,
                swarm_report: None,
            }))
            .is_empty()
    );
    assert!(window.update(Message::Confirm).is_empty());
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::Run(job, true, blockers))
            if job == &first && blockers.iter().any(|blocker| blocker.predecessor == predecessor
                && blocker.reason == kortty_jobs::BlockReason::NotSuccessful(failed))
    ));

    let request = window.update(Message::Confirm).pop().unwrap();
    assert_eq!(request.job, first);
    assert_eq!(request.trigger, RunTrigger::Cascade);
    assert_eq!(
        request.action.working_directory.as_deref(),
        Some("/updated")
    );
    let next = window
        .update(Message::Finished(Completion {
            id: request.id,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            swarm_report: None,
        }))
        .pop()
        .unwrap();
    assert_eq!(next.job, successor);
}

#[test]
fn scheduler_run_saves_nothing_and_dispatches_nothing_after_a_failed_save() {
    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("original").unwrap();
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
        let graph = JobGraph::new(vec![Job::new(id, "Original", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    window.update(Message::Edit(Field::Name, "Edited".into()));
    window.update(Message::Edit(Field::Port, "invalid".into()));
    assert!(window.update(Message::Run(false)).is_empty());
    assert_eq!(window.status(), Some("Enter a valid SSH port"));

    window.update(Message::Edit(Field::Port, "22".into()));
    let backup = profile.0.join("previous.json");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(window.update(Message::Run(true)).is_empty());
    assert!(window.view.dirty());
    assert!(!window.runtime.has_active_runs());
    assert_eq!(window.selected_job().unwrap().name(), "Original");
    fs::remove_dir(&path).unwrap();
    fs::rename(backup, path).unwrap();

    let request = window.update(Message::Run(true)).pop().unwrap();
    assert_eq!(request.trigger, RunTrigger::Cascade);
    assert_eq!(window.selected_job().unwrap().name(), "Edited");
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
        swarm_report: None,
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
        target_groups: None,
        command: "true".into(),
        working_directory: None,
        script: None,
        snippet: None,
        remote: None,
        agent: None,
        sudo: SudoMode::Disabled,
        virtual_terminal: Some(options),
        automation_journal: None,
        desktop_notices: Default::default(),
        webhook_targets: Vec::new(),
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
        swarm_report: None,
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
        swarm_report: None,
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
fn scheduler_journal_sorts_runs_and_deletes_only_the_reviewed_visible_selection() {
    let profile = Profile::temporary();
    let path = profile.file();
    let zulu = JobId::new("zulu").unwrap();
    let amber = JobId::new("amber").unwrap();
    let active = JobId::new("active").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(zulu.clone(), "Zulu backup", action.clone()).unwrap(),
        Job::new(amber.clone(), "amber backup", action.clone()).unwrap(),
        Job::new(active.clone(), "Bravo backup", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    let now = SystemTime::now();
    let start = |window: &mut TestWindow, job: &JobId, seconds| {
        let at = now + Duration::from_secs(seconds);
        let batch = window.runtime.start(job, false, at).unwrap();
        window.runtime.receive(batch, None, at).pop().unwrap().id
    };
    let finish = |window: &mut TestWindow, id, outcome, detail: &str, seconds| {
        window.runtime.update_runtime_at(
            Message::Finished(Completion {
                id,
                outcome,
                detail: Some(detail.into()),
                recording: None,
                swarm_report: None,
            }),
            now + Duration::from_secs(seconds),
        );
    };
    let first = start(&mut window, &zulu, 0);
    finish(&mut window, first, RunOutcome::Success, "First run", 1);
    let second = start(&mut window, &amber, 2);
    finish(&mut window, second, RunOutcome::Cancelled, "Second run", 3);
    let running = start(&mut window, &active, 4);

    let sorted = |window: &TestWindow| {
        let records = window.runtime.store.journal();
        window
            .view
            .journal_rows(&window.runtime, &records)
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>()
    };
    window.update(Message::Page(Page::Journal));
    assert_eq!(sorted(&window), [running, second, first]);
    window.update(Message::JournalSort(JournalSortColumn::Job));
    assert_eq!(sorted(&window), [second, running, first]);
    window.update(Message::JournalSort(JournalSortColumn::Job));
    assert_eq!(sorted(&window), [first, running, second]);
    window.update(Message::JournalSort(JournalSortColumn::Summary));
    assert_eq!(sorted(&window), [running, first, second]);

    window.update(Message::SelectRecord(first));
    window.update(Message::MarkRecord(second, true));
    window.update(Message::MarkRecord(running, true));
    assert_eq!(window.view.selected_run, Some(second));
    assert_eq!(window.view.marked_records, BTreeSet::from([first, second]));
    window.update(Message::DeleteRecord);
    assert!(window.view.confirm.is_none());

    finish(&mut window, running, RunOutcome::Success, "Third run", 5);
    window.update(Message::DeleteRecord);
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::DeleteRecord(ids))
            if ids.len() == 2 && ids.iter().copied().collect::<BTreeSet<_>>() == BTreeSet::from([first, second])
    ));

    window.update(Message::JournalSearch("amber".into()));
    window.update(Message::Confirm);
    assert_eq!(window.runtime.store.journal().len(), 3);
    assert_eq!(window.view.marked_records, BTreeSet::from([second]));
    window.update(Message::JournalSearch(String::new()));
    window.update(Message::MarkRecord(first, true));
    window.update(Message::DeleteRecord);
    window.update(Message::Confirm);
    assert_eq!(window.status(), Some("2 journal entries deleted"));
    assert_eq!(window.view.selected_run, None);
    assert!(window.view.marked_records.is_empty());
    drop(window);
    let store = SchedulerStore::open(path).unwrap();
    let remaining = store.journal();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, running);
}

#[test]
fn scheduler_run_journal_retention_follows_local_calendar_days() {
    let zone = TimeZone::get("Europe/Berlin").unwrap();
    for (now, expected) in [
        ("2025-03-30T12:00:00+02:00", "2025-03-29T12:00:00+01:00"),
        ("2025-03-31T02:30:00+02:00", "2025-03-30T03:30:00+02:00"),
        ("2025-10-27T02:30:00+01:00", "2025-10-26T02:30:00+01:00"),
    ] {
        let now = SystemTime::from(now.parse::<Timestamp>().unwrap());
        let expected = SystemTime::from(expected.parse::<Timestamp>().unwrap());
        assert_eq!(journal_cutoff(now, &zone, 1).unwrap(), expected);
    }
}

#[test]
fn scheduler_run_journal_retention_prunes_after_apply_commit_and_reopen() {
    let profile = Profile::temporary();
    let path = profile.file();
    let old = JobId::new("old").unwrap();
    let recent = JobId::new("recent").unwrap();
    let dependent = JobId::new("dependent").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(old.clone(), "Old", action.clone()).unwrap(),
        Job::new(recent.clone(), "Recent", action.clone()).unwrap(),
        Job::new(dependent.clone(), "Dependent", action)
            .unwrap()
            .with_dependencies(DependencyMode::Gate, vec![old.clone()], None),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut window = TestWindow::open(path.clone()).unwrap();
    let directory = window.runtime.recording_directory.clone();
    fs::create_dir(&directory).unwrap();
    window.runtime.zone = TimeZone::UTC;
    window.runtime.set_execution_allowed(true);
    let now = SystemTime::from("2025-01-15T12:00:00Z".parse::<Timestamp>().unwrap());
    let day = Duration::from_secs(86_400);
    let start = |window: &mut TestWindow, job: &JobId, at| {
        let batch = window.runtime.start(job, false, at).unwrap();
        window.runtime.receive(batch, None, at).pop().unwrap().id
    };
    let finish = |window: &mut TestWindow, id, at, token| {
        window.runtime.update_runtime_at(
            Message::Finished(Completion {
                id,
                outcome: RunOutcome::Success,
                detail: None,
                recording: token,
                swarm_report: None,
            }),
            at,
        );
    };
    let expired = start(&mut window, &old, now - day - Duration::from_secs(1));
    let boundary = start(&mut window, &recent, now - day);
    let expired_file = virtual_terminal::path_for(&directory, expired, [1; 16]);
    let boundary_file = virtual_terminal::path_for(&directory, boundary, [2; 16]);
    let screenshots = crate::recording_window::screenshots::attachment_dir(&expired_file);
    fs::create_dir(&screenshots).unwrap();
    fs::write(&expired_file, b"old recording").unwrap();
    fs::write(&boundary_file, b"cutoff recording").unwrap();
    finish(
        &mut window,
        expired,
        now - day - Duration::from_secs(1),
        Some([1; 16]),
    );
    finish(&mut window, boundary, now - day, Some([2; 16]));

    window.update(Message::Page(Page::Journal));
    window.update(Message::MarkRecord(expired, true));
    window.update(Message::SelectRecord(expired));
    window.update(Message::EditJournalRetention("3651".into()));
    assert_eq!(window.view.journal_retention_change(&window.runtime), None);
    window.update(Message::EditJournalRetention("1".into()));
    assert_eq!(
        window.view.journal_retention_change(&window.runtime),
        Some(1)
    );
    assert_eq!(window.runtime.store.journal().len(), 2);
    assert_eq!(window.runtime.set_journal_retention(1, now), Some(1));
    window.runtime.sync_view(&mut window.view);
    assert!(window.view.journal_retention_draft.is_none());
    assert!(window.view.marked_records.is_empty());
    assert!(window.view.selected_run.is_none());
    assert!(window.runtime.store.journal_record(expired).is_none());
    assert!(!expired_file.exists() && !screenshots.exists());
    assert!(boundary_file.is_file());
    assert!(window.runtime.store.journal_record(boundary).is_some());
    assert!(window.runtime.scheduler.last_finished(&old).is_none());
    assert!(
        window
            .runtime
            .scheduler
            .blockers(&dependent, now)
            .unwrap()
            .is_empty()
    );

    let run = start(&mut window, &recent, now);
    let previous = profile.0.join("previous.json");
    fs::rename(&path, &previous).unwrap();
    fs::create_dir(&path).unwrap();
    let later = now + Duration::from_secs(1);
    finish(&mut window, run, later, None);
    assert!(window.runtime.store.journal_record(run).is_none());
    assert!(boundary_file.is_file());
    assert!(window.runtime.store.journal_record(boundary).is_some());
    fs::remove_dir(&path).unwrap();
    fs::rename(previous, &path).unwrap();
    window.runtime.update_runtime_at(Message::Tick, later);
    assert!(window.runtime.store.journal_record(run).is_some());
    assert!(!boundary_file.exists());
    assert!(window.runtime.store.journal_record(boundary).is_none());
    drop(window);

    let reopened = SchedulerWindow::open_with_journal_retention(path, 1).unwrap();
    assert!(reopened.store.journal().is_empty());
    assert!(
        reopened
            .scheduler
            .blockers(&dependent, now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_chain_waits_for_the_predecessor_journal_commit_with_session_journals() {
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
        target_groups: None,
        command: "true".into(),
        working_directory: None,
        script: None,
        snippet: None,
        remote: None,
        agent: None,
        sudo: SudoMode::Disabled,
        virtual_terminal: None,
        automation_journal: None,
        desktop_notices: Default::default(),
        webhook_targets: Vec::new(),
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
    let links = vec![
        "a0000000-0000-4000-8000-000000000001".to_owned(),
        "b0000000-0000-4000-8000-000000000002".to_owned(),
    ];
    let completion = Message::FinishedWithSessionJournals(
        Completion {
            id: run.id,
            outcome: RunOutcome::Success,
            detail: Some("Exit status 0".into()),
            recording: Some([3; 16]),
            swarm_report: None,
        },
        links.clone(),
    );
    assert!(window.update(completion).is_empty());
    assert!(window.runtime.has_active_runs());
    assert!(window.runtime.next_due().is_some());
    assert!(window.runtime.store.journal().is_empty());
    assert_eq!(window.runtime.store.session_journals(run.id), None);

    fs::remove_dir(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    let leaf_run = window.update(Message::Tick).pop().unwrap();
    assert_eq!(leaf_run.job, leaf);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
    assert_eq!(
        window.runtime.store.session_journals(run.id),
        Some(links.as_slice())
    );
    assert!(window.update(Message::Tick).is_empty());
    let failed = RunOutcome::Failed { exit_code: Some(5) };
    assert!(
        window
            .update(Message::Finished(Completion {
                id: leaf_run.id,
                outcome: failed,
                detail: None,
                recording: None,
                swarm_report: None,
            }))
            .is_empty()
    );
    assert!(!window.runtime.has_active_runs());
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.runtime.store.journal().len(), 2);
    assert_eq!(window.runtime.store.journal()[0].outcome, failed);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
    );
    window.update(Message::Page(Page::Journal));
    window.update(Message::SelectRecord(run.id));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        Some((&root, links.as_slice()))
    );
    assert!(
        window
            .runtime
            .selected_recording_path(&window.view, run.id)
            .is_some()
    );
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, leaf_run.id),
        None
    );
    window.update(Message::SelectRecord(leaf_run.id));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
    );
    window.update(Message::SelectRecord(run.id));
    window.update(Message::JournalSearch("not-in-any-run-field".into()));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
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
        (Field::WorkingDirectory, "/jobs/it's literal $HOME".into()),
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
    assert_eq!(
        original_action.command_for_exec(None).unwrap(),
        format!("CDPATH= cd '/jobs/it'\\''s literal $HOME' || exit\n{command}")
    );
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
fn working_directory_wraps_shell_actions_and_validates_stored_paths() {
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "command": "# first line\nfalse\ntrue",
    }))
    .unwrap();
    assert!(matches!(
        action.command_for_exec(None),
        Ok(Cow::Borrowed(_))
    ));
    assert!(
        serde_json::to_value(&action)
            .unwrap()
            .get("working_directory")
            .is_none()
    );
    action.working_directory = Some("-job".into());
    assert_eq!(
        action.command_for_exec(None).unwrap(),
        "CDPATH= cd './-job' || exit\n# first line\nfalse\ntrue"
    );
    for path in ["/invalid\npath".to_owned(), "x".repeat(4097)] {
        action.working_directory = Some(path);
        assert!(action.command_for_exec(None).is_err());
    }
    action.working_directory = Some("/run".into());
    action.command = "x".repeat(MAX_COMMAND_BYTES);
    assert!(action.command_for_exec(None).is_err());

    action.command.clear();
    action.remote = Some(RemoteAction::Mkdir {
        path: "/run/test".to_owned().try_into().unwrap(),
    });
    assert!(action.remote_action().is_err());
}

#[test]
fn native_form_reopens_each_sftp_action_and_honors_the_scheduler_policy() {
    use crate::session_journal::automation::{Closing, Keep};
    use automation_journal::Event;

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
        for event in [
            Event::Enabled(true),
            Event::Input(true),
            Event::Keep(Keep::Failures),
            Event::Closing(Closing::Failures),
        ] {
            window.update(Message::AutomationJournal(event));
        }
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
        let options = action.automation_journal.as_ref().unwrap();
        assert!(options.enabled && options.capture_input);
        assert_eq!(options.keep, Keep::Failures);
        assert_eq!(options.closing, Closing::Failures);
        drop(window);

        let mut window = TestWindow::open(path).unwrap();
        assert_eq!(window.view.draft.source, source);
        let estimate = window.view.draft.automation_estimate_subject().unwrap();
        assert_eq!(estimate.targets, 1);
        assert_eq!(estimate.mode, (Closing::Failures, Keep::Failures));
        window.update(Message::Save);
        assert_eq!(window.selected_job().unwrap().action(), &action);
        assert!(window.update(Message::Run(false)).is_empty());
        window.runtime.set_execution_allowed(true);
        let ticket = window.update(Message::Run(false)).pop().unwrap();
        assert_eq!(ticket.action.as_ref(), &action);
    }
}

#[test]
fn automation_policy_preserves_scheduler_options_and_invalidates_estimates() {
    use crate::session_journal::automation::Closing;
    use automation_journal::Event;

    let profile = Profile::temporary();
    let mut window = TestWindow::open(profile.file()).unwrap();
    window.update(Message::Page(Page::Action));
    window.update(Message::AutomationJournal(Event::Enabled(true)));
    window.update(Message::AutomationJournal(Event::Closing(Closing::Always)));
    let options = window.view.draft.automation_journal.build().unwrap();
    let old = window
        .runtime
        .begin_automation_estimate(&mut window.view)
        .unwrap();

    window.view.set_automation_journal_allowed(false);
    window.update(Message::AutomationJournal(Event::Enabled(false)));
    assert_eq!(
        window.view.draft.automation_journal.build().unwrap(),
        options
    );
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_none()
    );

    window.view.set_automation_journal_allowed(true);
    let current = window
        .runtime
        .begin_automation_estimate(&mut window.view)
        .unwrap();
    window
        .view
        .finish_automation_estimate(old.ticket, AutomationEstimate::unavailable());
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_none()
    );
    window
        .view
        .finish_automation_estimate(current.ticket, AutomationEstimate::unavailable());
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_some()
    );
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
        window_geometry: None,
        terminal: Default::default(),
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
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, &crate::clipboard_policy::ServerAccess::default(), |_, _, _| panic!("a stale selection must block before vault lookup")), Err(error) if error.contains("route changed"))
    );
    store.delete(second).unwrap();
    assert!(
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, &crate::clipboard_policy::ServerAccess::default(), |_, _, _| panic!("a missing selection must block before vault lookup")), Err(error) if error.contains("missing"))
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
