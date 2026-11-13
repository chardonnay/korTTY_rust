use std::sync::Arc;

use super::*;
use crate::ai_manager::Configuration;
use crate::connection_store::{LocalProfile, Store, TerminalOverrides};
use crate::session::{GridSize, IbmKind, Session};
use crate::session_security::LocalSettings;
use crate::snippets::tests::Profile;
use crate::terminal_view::selection_test_frame;

mod ask_hybrid;
mod designs;
mod final_advice;
mod handoff;
mod history;
mod knowledge;
mod live_repair;
mod pip;
mod shortcut;

#[test]
fn terminal_agent_activity_export_keeps_completed_runs_scoped_and_masked() {
    use crate::terminal_agent::activity_report::Format;
    use activity_export::{Message as Export, Scope};

    let mut fixture = Fixture::new(false, false);
    let first = complete_export_run(&mut fixture, "First retained run");
    let _ = fixture.send(first, Message::Close);
    let second = complete_export_run(&mut fixture, "Second retained run");
    let path = fixture
        .profile
        .0
        .canonicalize()
        .unwrap()
        .join("activity.json");
    for action in [
        Export::Open,
        Export::Scope(Scope::All),
        Export::Format(Format::Json),
        Export::Path(path.to_str().unwrap().into()),
    ] {
        no_task(fixture.send(second, Message::ActivityExport(action)));
    }
    let render = fixture.send(second, Message::ActivityExport(Export::Confirm));
    let rendered = activity_result(render, second);
    let publish = fixture.send(second, rendered);
    let published = activity_result(publish, second);
    no_task(fixture.send(second, published));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("fixture-plain-secret"));
    let export: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let runs = export["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["prompt"], "First retained run");
    assert_eq!(runs[1]["prompt"], "Second retained run");
    assert_eq!(runs[1]["activity"][0], "Done\nAPI_KEY=***");

    let other = fixture.profile.0.canonicalize().unwrap().join("stale.json");
    for action in [
        Export::Open,
        Export::Format(Format::Json),
        Export::Path(other.to_str().unwrap().into()),
    ] {
        no_task(fixture.send(second, Message::ActivityExport(action)));
    }
    let render = fixture.send(second, Message::ActivityExport(Export::Confirm));
    let rendered = activity_result(render, second);
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    no_task(fixture.send(second, rendered));
    assert!(!other.exists());
    assert!(!fixture.run(second).activity_export.writing());

    let fresh = complete_export_run(&mut fixture, "Fresh stream");
    for action in [
        Export::Open,
        Export::Scope(Scope::All),
        Export::Format(Format::Json),
        Export::Path(other.to_str().unwrap().into()),
    ] {
        no_task(fixture.send(fresh, Message::ActivityExport(action)));
    }
    let render = fixture.send(fresh, Message::ActivityExport(Export::Confirm));
    let publish = fixture.send(fresh, activity_result(render, fresh));
    no_task(fixture.send(fresh, activity_result(publish, fresh)));
    let export: serde_json::Value = serde_json::from_slice(&std::fs::read(other).unwrap()).unwrap();
    assert_eq!(export["runs"].as_array().unwrap().len(), 1);
    assert_eq!(export["runs"][0]["prompt"], "Fresh stream");
}

#[test]
fn terminal_agent_private_activity_never_offers_or_starts_export() {
    use activity_export::Message as Export;

    let mut fixture = Fixture::new(false, true);
    let id = complete_export_run(&mut fixture, "Private task");
    assert!(fixture.run(id).activity_export.view().is_none());
    no_task(fixture.send(id, Message::ActivityExport(Export::Open)));
    no_task(fixture.send(id, Message::ActivityExport(Export::Confirm)));
    assert!(!fixture.run(id).activity_export.writing());
}

fn complete_export_run(fixture: &mut Fixture, goal: &str) -> window::Id {
    let id = fixture.open(Mode::Ask);
    let run = fixture.run_mut(id);
    run.goal = goal.into();
    run.activity_export.start();
    let epoch = run.next(Phase::Thinking, "Waiting for a fixture decision");
    let decision = Decision {
        summary: "Done".into(),
        message: "API_KEY=fixture-plain-secret".into(),
        action: Action::Done,
    };
    assert_only_agent_notice_task(
        fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into()))),
        fixture.owner,
    );
    assert!(fixture.run(id).phase == Phase::Finished);
    id
}

fn activity_result(task: Task<Desktop>, id: window::Id) -> Message {
    use iced::futures::{StreamExt as _, executor::block_on};

    let mut stream = iced_runtime::task::into_stream(task).expect("agent export worker");
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(
            actual,
            message @ Message::ActivityExport(_),
        )) = action
        {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("missing agent export result");
}

#[test]
fn agent_design_keeps_status_and_focus_bound_to_the_saved_connection_stream() {
    use iced::futures::{StreamExt, executor::block_on};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.window_settings.ai_agent_design = AgentDesign::Classic;
    let classic = fixture.open(Mode::Plan);
    let session = fixture.session;
    let stream = fixture.workspace.sessions[&session].stream_generation;
    assert_eq!(fixture.run(classic).design, AgentDesign::Classic.layout());
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(session, stream)
            .is_none()
    );

    let path = fixture
        .profile
        .0
        .canonicalize()
        .unwrap()
        .join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let mut local = fixture.workspace.sessions[&session]
        .saved_local
        .clone()
        .unwrap();
    local.terminal.ai_agent_design = Some(AgentDesign::Floating.id().into());
    let local_id = store.save_local(local).unwrap();
    let reopened = Store::open(path).unwrap();
    assert_eq!(
        reopened
            .local(local_id)
            .unwrap()
            .terminal
            .ai_agent_design
            .as_deref(),
        Some("floating")
    );
    fixture
        .workspace
        .refresh_saved_terminal_preferences(&reopened);
    let floating = fixture.open(Mode::Plan);
    assert_eq!(fixture.run(floating).design, AgentDesign::Floating.layout());
    let (_, settings) = design::window(fixture.run(floating).design);
    assert!(matches!(settings.level, window::Level::AlwaysOnTop));
    assert!(fixture.run(floating).phase == Phase::Setup);
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(session, stream)
            .is_some()
    );

    let focus = fixture.send(floating, Message::Focus);
    let mut task = iced_runtime::task::into_stream(focus).unwrap();
    assert!(matches!(
        block_on(task.next()),
        Some(iced_runtime::Action::Window(iced_runtime::window::Action::Minimize(id, false)))
            if id == floating
    ));
    assert!(matches!(
        block_on(task.next()),
        Some(iced_runtime::Action::Window(iced_runtime::window::Action::GainFocus(id)))
            if id == floating
    ));
    assert!(block_on(task.next()).is_none());
    fixture.run_mut(floating).closing = true;
    no_task(fixture.send(floating, Message::Focus));
    fixture.run_mut(floating).closing = false;
    fixture
        .workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .stream_generation += 1;
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(session, stream + 1)
            .is_none()
    );
    assert!(iced_runtime::task::into_stream(fixture.send(floating, Message::Focus)).is_none());
}

#[test]
fn custom_agent_design_is_snapshotted_from_the_saved_profile_for_each_run() {
    use crate::terminal_agent::design::{Arrangement, History, Status};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.window_settings.ai_agent_design = AgentDesign::Classic;
    let catalog = &mut fixture.workspace.window_settings.ai_agent_designs;
    let design_id = catalog.duplicate("floating").unwrap().id;
    let custom = AgentLayout {
        history: History::StepCards,
        status: Status::Pill,
        ..AgentDesign::Floating.layout()
    };
    catalog.set_layout(&design_id, custom);
    let path = fixture
        .profile
        .0
        .canonicalize()
        .unwrap()
        .join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let mut local = fixture.workspace.sessions[&fixture.session]
        .saved_local
        .clone()
        .unwrap();
    local.terminal.ai_agent_design = Some(design_id.clone());
    let local_id = store.save_local(local).unwrap();
    fixture
        .workspace
        .refresh_saved_terminal_preferences(&Store::open(path.clone()).unwrap());

    let id = fixture.open(Mode::Ask);
    assert_eq!(fixture.run(id).design, custom);
    assert_eq!(fixture.run(id).design.arrangement, Arrangement::Floating);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(fixture.session, stream)
            .is_some()
    );

    fixture
        .workspace
        .window_settings
        .ai_agent_designs
        .remove(&design_id);
    let fresh = fixture.open(Mode::Ask);
    assert_eq!(fixture.run(fresh).design, AgentDesign::Classic.layout());
    assert_eq!(fixture.run(id).design, custom);
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(fixture.session, stream)
            .is_none()
    );
    let pane = &fixture.workspace.sessions[&fixture.session];
    // The pane keeps its original launch profile. Live terminal preferences
    // are refreshed separately from the connection store.
    assert_eq!(
        fixture
            .workspace
            .terminal_overrides(pane)
            .and_then(|settings| settings.ai_agent_design.as_deref()),
        Some(design_id.as_str())
    );
    let reopened = Store::open(path).unwrap();
    assert_eq!(
        reopened
            .local(local_id)
            .unwrap()
            .terminal
            .ai_agent_design
            .as_deref(),
        Some(design_id.as_str())
    );
}

#[test]
fn agent_window_palette_uses_the_bound_pane_theme_until_the_stream_changes() {
    use crate::connection_store::ThemeOverride;
    use crate::terminal_theme::{Rgb, Scheme};

    let mut fixture = Fixture::new(false, false);
    let run = fixture.open(Mode::Run);
    let themes = &mut fixture.workspace.window_settings.themes;
    let global = themes
        .store(None, "Global agent colors", Scheme::default())
        .unwrap();
    let foreground = Rgb::from_hex(0xd6d6d6);
    let own = themes
        .store(
            None,
            "Pane agent colors",
            Scheme {
                foreground,
                ..Scheme::default()
            },
        )
        .unwrap();
    let global_accent = Rgb::from_hex(0x6688cc);
    let pane_accent = Rgb::from_hex(0xcc8833);
    themes.set_agent_panel_colors(global, [None, None, None, None, Some(global_accent), None]);
    themes.set_agent_panel_colors(own, [None, None, None, None, Some(pane_accent), None]);
    themes.select(Some(global));

    let mut store = Store::open(fixture.profile.0.join("connections.json")).unwrap();
    let mut profile = fixture.workspace.sessions[&fixture.session]
        .saved_local
        .clone()
        .unwrap();
    profile.terminal.theme = Some(ThemeOverride::from(own));
    store.save_local(profile).unwrap();
    fixture.workspace.refresh_saved_terminal_preferences(&store);
    assert_eq!(
        fixture.workspace.agent_window_palette(run).unwrap().accent,
        pane_accent
    );
    assert_eq!(
        fixture
            .workspace
            .pane_appearance(fixture.session)
            .foreground,
        foreground
    );

    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    assert_eq!(
        fixture.workspace.agent_window_palette(run).unwrap().accent,
        global_accent
    );
}

pub(in crate::workspace::terminal_agent) struct Fixture {
    profile: Profile,
    pub(in crate::workspace::terminal_agent) workspace: Workspace,
    owner: window::Id,
    pub(in crate::workspace::terminal_agent) session: SessionId,
}

impl Fixture {
    fn new(disabled: bool, private: bool) -> Self {
        Self::with_endpoint(disabled, private, "127.0.0.1:12345")
    }

    pub(in crate::workspace::terminal_agent) fn with_endpoint(
        disabled: bool,
        private: bool,
        endpoint: &str,
    ) -> Self {
        Self::with_endpoint_and_preset(disabled, private, endpoint, "generic")
    }

    pub(in crate::workspace::terminal_agent) fn with_endpoint_and_preset(
        disabled: bool,
        private: bool,
        endpoint: &str,
        prompt_preset: &str,
    ) -> Self {
        let profile = Profile::new();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{"id":1,"name":"Fixture","endpoint":endpoint,"model":"fixture","prompt_preset":prompt_preset,"usage_budget":{"token_limit":50000}}],
                "default":1,"last_id":1
            }
        })).unwrap();
        let root = profile.0.canonicalize().unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let mut store = Store::open(root.join("connections.json")).unwrap();
        let id = store
            .save_local(LocalProfile {
                id: 0,
                name: "Local agent fixture".into(),
                shell: Default::default(),
                command: None,
                working_directory: None,
                shell_integration: false,
                emulation: "xterm-256color".into(),
                terminal: TerminalOverrides {
                    ai_agent_disabled: disabled,
                    ..Default::default()
                },
            })
            .unwrap();
        let (owner, _) = workspace.open_window(false);
        // Detached transport supplies pane identity without launching a real shell.
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, session);
        workspace.active_tab_mut(owner).unwrap().incognito = private;
        let pane = workspace.sessions.get_mut(&session).unwrap();
        pane.saved_local = store.local(id).cloned();
        pane.local_security = Some(LocalSettings::default());
        workspace.refresh_saved_terminal_preferences(&store);
        let pane = workspace.sessions.get_mut(&session).unwrap();
        pane.frame = selection_test_frame(pane.frame.screen.clone());
        Arc::get_mut(&mut pane.frame).unwrap().status = SessionStatus::Running;
        Self {
            profile,
            workspace,
            owner,
            session,
        }
    }

    pub(in crate::workspace::terminal_agent) fn open(&mut self, mode: Mode) -> window::Id {
        let old: Vec<_> = self
            .workspace
            .terminal_agents
            .runs
            .keys()
            .copied()
            .collect();
        let _ = self
            .workspace
            .open_terminal_agent(self.owner, self.session, mode);
        *self
            .workspace
            .terminal_agents
            .runs
            .keys()
            .find(|id| !old.contains(id))
            .unwrap()
    }

    pub(in crate::workspace::terminal_agent) fn send(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        self.workspace.update_terminal_agent(id, message)
    }

    fn run(&self, id: window::Id) -> &Run {
        &self.workspace.terminal_agents.runs[&id]
    }

    pub(in crate::workspace::terminal_agent) fn run_mut(&mut self, id: window::Id) -> &mut Run {
        self.workspace.terminal_agents.runs.get_mut(&id).unwrap()
    }
}

fn no_task(task: Task<Desktop>) {
    assert!(iced_runtime::task::into_stream(task).is_none());
}

fn finish_agent_task(fixture: &mut Fixture, first: Task<Desktop>) {
    use iced::futures::{StreamExt as _, executor::block_on};

    let mut tasks = vec![first];
    while let Some(task) = tasks.pop() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
                    tasks.push(fixture.send(id, message));
                }
            }
        }
    }
}

#[test]
fn learned_sudo_password_is_masked_before_decision_json_escapes_it() {
    use crate::terminal_logging::RedactionSecrets;

    const SECRET: &str = r#"newly-"learned\credential"#;
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.goal = format!("Goal {SECRET}");
    run.context = format!("Context {SECRET}");
    run.plan = Some(format!("Plan {SECRET}"));
    run.record(format!("Earlier evidence\n{SECRET}"));
    let original = json!({
        "goal": run.goal,
        "target": null,
        "context": run.context,
        "acceptedPlan": run.plan,
        "steps": run.activity,
        "turn": 1,
        "turnLimit": engine::MAX_TURNS,
    });
    assert_eq!(
        run.decision_input(false, 4096).unwrap(),
        original.to_string()
    );

    let learned = RedactionSecrets::with_secret(None, SECRET).unwrap();
    run.redactor = run.redactor.with_overlay(&learned).unwrap();
    run.learned_sudo = Some(learned);
    let reviewed = run.decision_input(false, 4096).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reviewed).unwrap(),
        json!({
            "goal": "Goal ***",
            "target": null,
            "context": "Context ***",
            "acceptedPlan": "Plan ***",
            "steps": ["Earlier evidence\n***"],
            "turn": 1,
            "turnLimit": engine::MAX_TURNS,
        }),
    );
}

#[test]
fn standalone_https_agent_requires_explicit_selection_and_rechecks_saved_opt_in() {
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&baseline).unwrap();
    draft["profiles"] = json!({
        "profiles": [{
            "id": 2,
            "name": "Remote fixture",
            "connection_mode": "remote-open-ai",
            "endpoint": "https://example.invalid/v1",
            "model": "fixture",
            "trusted_remote_endpoint": true,
            "reviewed_agent_https": true
        }],
        "default": 2,
        "last_id": 2
    });
    let saved: Configuration = serde_json::from_value(draft.clone()).unwrap();
    saved.save_fixture_at(&root, &baseline).unwrap();
    let saved = Configuration::load_fixture_from(&root).unwrap();
    assert!(saved.agent_profile_id().is_none());
    assert!(saved.agent_profiles().next().is_none());
    assert!(saved.agent_transport(2).is_err());
    fixture.workspace.set_test_ai_configuration(saved.clone());

    let id = fixture.open(Mode::Ask);
    assert!(fixture.run(id).profile().is_none());
    let selected = fixture.run(id).profiles[0].clone();
    assert_eq!(
        selected.reviewed_https.as_deref(),
        Some("https://example.invalid/v1/chat/completions")
    );
    let _ = fixture.send(id, Message::Goal("Inspect password=fixture-secret".into()));
    no_task(fixture.send(id, Message::Start));
    assert!(fixture.run(id).phase == Phase::Setup);
    let _ = fixture.send(id, Message::Profile(selected));
    let _ = fixture.send(id, Message::Start);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview);
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(review.send_label(), "Send reviewed HTTPS request");
    assert!(review.transport().0.is_remote());
    assert!(!review.input.contains("fixture-secret"));
    assert!(review.classifier.is_none());
    let epoch = run.epoch;

    draft["profiles"]["profiles"][0]["reviewed_agent_https"] = json!(false);
    let revoked: Configuration = serde_json::from_value(draft).unwrap();
    revoked.save_fixture_at(&root, &saved).unwrap();
    let _ = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).notice.contains("changed"));
}

#[test]
fn standalone_agent_preselects_the_cli_role_but_requires_request_approval() {
    use crate::local_ai::cli::{Program, fixture_executable};

    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let (path, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
    let mut value = serde_json::to_value(&baseline).unwrap();
    value["profiles"]["last_id"] = json!(2);
    value["profiles"]["profiles"].as_array_mut().unwrap().push(json!({
        "id": 2, "name": "CLI fixture", "connection_mode": "cli", "endpoint": "",
        "vision": "disabled", "cli": {"path": path, "arguments": ["{stdinPrompt}"], "fingerprint": fingerprint}
    }));
    value["local"]["roles"] = json!([null, null, 2, null]);
    let draft: Configuration = serde_json::from_value(value).unwrap();
    draft.save_fixture_at(&root, &baseline).unwrap();
    let config = Configuration::load_fixture_from(&root).unwrap();
    fixture.workspace.set_test_ai_configuration(config);

    let id = fixture.open(Mode::Ask);
    assert!(fixture.run(id).profile().unwrap().reviewed_cli.is_some());
    no_task(fixture.send(id, Message::Goal("Check the current output".into())));
    let start = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, start);
    let review = fixture.run(id).model_review.as_ref().unwrap();
    assert_eq!(review.send_label(), "Run reviewed CLI request");
    assert!(review.transport().0.is_cli());
    assert!(!root.join("fixture-stdin").exists());

    fixture.workspace.clipboard.policy.minimum_isolation = IsolationLevel::Process;
    let isolated = fixture.open(Mode::Ask);
    let run = fixture.run(isolated);
    assert_eq!(run.profile().unwrap().id, 1);
    assert!(
        run.profiles
            .iter()
            .all(|profile| profile.reviewed_cli.is_none())
    );
}

#[test]
fn terminal_agent_setup_and_planning_require_separate_execution_approval() {
    let mut fixture = Fixture::new(false, false);
    let run = fixture.open(Mode::Run);
    let ask = fixture.open(Mode::Ask);
    let plan = fixture.open(Mode::Plan);
    for id in [run, ask, plan] {
        assert!(fixture.run(id).phase == Phase::Setup);
        assert!(fixture.run(id).target.is_none());
        assert!(!fixture.run(id).phase.working());
        no_task(fixture.send(id, Message::Approve(0)));
    }
    no_task(fixture.send(plan, Message::Implement(0)));
    assert!(fixture.run(plan).phase == Phase::Setup);
    no_task(fixture.send(plan, Message::Cancel));
    assert!(fixture.run(plan).phase == Phase::Cancelled);
    assert_eq!(fixture.run(plan).notice, "Agent run cancelled.");
}

#[test]
fn terminal_agent_usage_accepts_failed_and_cancelled_attempts_for_the_owned_epoch() {
    use engine::usage::{Attempt, AttemptKind, AttemptOutcome, Report};

    let mut fixture = Fixture::new(false, true);
    let failed = fixture.open(Mode::Ask);
    let epoch = fixture.run_mut(failed).next(Phase::Thinking, "Thinking");
    let failure = Arc::new(Report {
        result: Err("Provider fixture failed".into()),
        attempts: vec![
            Attempt {
                kind: AttemptKind::Initial,
                outcome: AttemptOutcome::InvalidReply,
                elapsed: Duration::from_millis(8),
                input_tokens: Some(6),
                output_tokens: Some(0),
                total_tokens: Some(6),
            },
            Attempt {
                kind: AttemptKind::Repair,
                outcome: AttemptOutcome::Failed,
                elapsed: Duration::from_millis(12),
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            },
        ],
    });
    let _ = fixture.send(failed, Message::Decision(epoch, Arc::clone(&failure)));
    let _ = fixture.send(failed, Message::Decision(epoch, failure));
    let run = fixture.run(failed);
    assert!(run.phase == Phase::Blocked);
    assert_eq!(run.presentation.model_usage.attempts().count(), 2);
    assert!(run.presentation.model_usage.total().is_none());
    assert!(run.activity.is_empty());

    let cancelled = fixture.open(Mode::Ask);
    let epoch = fixture.run_mut(cancelled).next(Phase::Thinking, "Thinking");
    let _ = fixture.send(cancelled, Message::Cancel);
    let cancellation = Arc::new(Report {
        result: Err("Provider fixture cancelled".into()),
        attempts: vec![Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Cancelled,
            elapsed: Duration::from_millis(9),
            input_tokens: None,
            output_tokens: Some(0),
            total_tokens: None,
        }],
    });
    let _ = fixture.send(
        cancelled,
        Message::Decision(epoch, Arc::clone(&cancellation)),
    );
    let run = fixture.run(cancelled);
    assert!(run.phase == Phase::Cancelled);
    let attempt = run.presentation.model_usage.attempts().next().unwrap();
    assert_eq!(attempt.outcome, AttemptOutcome::Cancelled);
    assert_eq!(attempt.output_tokens, Some(0));

    let stale = fixture.open(Mode::Ask);
    let epoch = fixture.run_mut(stale).next(Phase::Thinking, "Thinking");
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    let _ = fixture.send(stale, Message::Decision(epoch, cancellation));
    assert_eq!(
        fixture
            .run(stale)
            .presentation
            .model_usage
            .attempts()
            .count(),
        0
    );
}

#[test]
fn agent_completion_marks_the_original_nonprivate_tab_with_fixed_text() {
    for (private, notifications) in [(false, false), (false, true), (true, true)] {
        let mut fixture = Fixture::new(false, private);
        let id = fixture.open(Mode::Ask);
        fixture
            .workspace
            .window_settings
            .terminal_notifications
            .ai_runs = notifications;
        let epoch = fixture.run(id).epoch;
        fixture.run_mut(id).phase = Phase::Thinking;
        let decision = Decision {
            summary: "Private model result".into(),
            message: "Private explanation".into(),
            action: Action::Done,
        };
        let task = fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into())));
        if notifications && !private {
            assert_eq!(
                task.units(),
                usize::from(cfg!(any(windows, target_os = "macos")))
            );
            assert_only_agent_notice_task(task, fixture.owner);
        } else {
            no_task(task);
        }
        assert!(fixture.run(id).phase == Phase::Finished);
        let mark = fixture
            .workspace
            .active_tab_mut(fixture.owner)
            .unwrap()
            .watch
            .attention_text();
        assert_eq!(mark.as_deref(), (!private).then_some("An AI run finished."));
    }
}

#[test]
fn terminal_agent_connection_denial_preserves_advisory_actions_and_blocks_implement() {
    let mut fixture = Fixture::new(true, false);
    assert!(
        !fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Run)
    );
    assert!(
        fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Ask)
    );
    assert!(
        fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Plan)
    );
    let plan = fixture.open(Mode::Plan);
    let epoch = fixture.run_mut(plan).next(Phase::PlanReady, "Plan ready");
    assert_only_agent_notice_task(fixture.send(plan, Message::Implement(epoch)), fixture.owner);
    assert!(fixture.run(plan).phase == Phase::Blocked);
    assert!(fixture.run(plan).target.is_none());
}

#[test]
fn managed_agent_and_planning_controls_keep_independent_authority() {
    let (listener, _) = crate::local_ai::test_support::listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &listener.local_addr().unwrap().to_string());
    let agent = fixture.open(Mode::Ask);
    let parse = |features: &str| {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version=1\n[[rule]]\n[rule.features]\n{features}"
        ))
        .unwrap()
    };
    fixture.workspace.clipboard.policy = parse("ai-agent = 'deny'");
    assert!(
        !fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Run)
    );
    assert!(
        !fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Ask)
    );
    assert!(
        fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Plan)
    );

    no_task(fixture.send(agent, Message::Goal("Inspect the reviewed system".into())));
    assert_only_agent_notice_task(fixture.send(agent, Message::Start), fixture.owner);
    assert!(fixture.run(agent).phase == Phase::Blocked);
    assert!(fixture.run(agent).notice.contains("blocks AI Agent"));

    let plan = fixture.open(Mode::Plan);
    let epoch = fixture.run_mut(plan).next(Phase::PlanReady, "Plan ready");
    assert_only_agent_notice_task(fixture.send(plan, Message::Implement(epoch)), fixture.owner);
    assert!(fixture.run(plan).phase == Phase::Blocked);
    assert!(fixture.run(plan).target.is_none());
    assert_eq!(fixture.run(plan).mode, Mode::Plan);

    fixture.workspace.clipboard.policy = parse("ai-planning = 'deny'");
    assert!(
        fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Run)
    );
    assert!(
        fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Ask)
    );
    assert!(
        !fixture
            .workspace
            .terminal_agent_available(fixture.session, Mode::Plan)
    );
    let aliases = engine::shortcut::Aliases::new("agent", false).unwrap();
    let before = fixture.workspace.terminal_agents.runs.len();
    no_task(fixture.workspace.open_agent_shortcut(
        fixture.owner,
        fixture.session,
        &aliases,
        "agent-plan Inspect the system",
    ));
    assert_eq!(fixture.workspace.terminal_agents.runs.len(), before);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn terminal_agent_approvals_are_epoch_bound_and_stream_replacement_cancels() {
    use iced::advanced::subscription::into_recipes;

    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Run);
    let second = fixture.open(Mode::Run);
    assert!(into_recipes(fixture.workspace.scheduler_subscription()).is_empty());
    let epoch = fixture
        .run_mut(first)
        .next(Phase::Directory, "Reading directory");
    let directory = LocalDirectory::capture(fixture.profile.0.canonicalize().unwrap()).unwrap();
    assert_only_agent_notice_task(
        fixture.send(first, Message::Directory(epoch, Ok(directory))),
        fixture.owner,
    );
    assert!(fixture.run(first).phase == Phase::ProbeApproval);
    assert_eq!(
        into_recipes(fixture.workspace.scheduler_subscription()).len(),
        1
    );
    assert!(fixture.run(first).activity.is_empty());
    no_task(fixture.send(first, Message::Approve(epoch)));
    assert!(fixture.run(first).phase == Phase::ProbeApproval);

    no_task(fixture.send(second, Message::Goal("Second run".into())));
    no_task(fixture.send(second, Message::Start));
    assert!(fixture.run(second).notice.contains("already owns"));
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    let _ = fixture.workspace.reconcile_terminal_agents();
    assert!(fixture.run(first).phase == Phase::Cancelled);
    assert!(fixture.run(first).activity.is_empty());
    assert!(into_recipes(fixture.workspace.scheduler_subscription()).is_empty());
}

#[test]
fn inline_review_masks_the_destination_and_withholds_keyboard_approval() {
    use crate::terminal_agent::design::{History, inline::Prompt};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let _ = fixture.send(id, Message::Goal("Check the test directory".into()));
    fixture.run_mut(id).design.history = History::InlineBlocks;
    for (name, exact) in [("ordinary", true), ("password=fixture-secret", false)] {
        let path = fixture.profile.0.canonicalize().unwrap().join(name);
        std::fs::create_dir(&path).unwrap();
        let directory = LocalDirectory::capture(path).unwrap();
        let epoch = fixture
            .run_mut(id)
            .next(Phase::Directory, "Reading directory");
        let _ = fixture.send(id, Message::Directory(epoch, Ok(directory)));
        fixture.run_mut(id).initialize_inline();
        let Some(Prompt::Approval {
            message, keyboard, ..
        }) = fixture
            .run(id)
            .inline_update(false)
            .unwrap()
            .transcript
            .prompt
        else {
            panic!("expected an inline review");
        };
        assert_eq!(keyboard, exact);
        assert!(message.contains(if exact { name } else { "password=***" }));
        assert!(!message.contains("fixture-secret"));
    }
}

#[cfg(unix)]
#[test]
fn terminal_agent_cursor_keys_bind_the_review_to_the_visible_stream_and_epoch() {
    use crate::terminal_agent::design::Approval;
    use crate::terminal_view::agent_cursor::{Key, Message as Cursor};

    let mut fixture = Fixture::new(false, false);
    let session =
        Session::start(GridSize::default(), kortty_terminal::TerminalProfile::Xterm).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    session
        .send(
            concat!(
                r"stty -echo; printf '\033]133;A\007cursor-fixture-ready\033]133;B\007'; ",
                r"IFS= read -r unused",
                "\r"
            )
            .as_bytes()
            .to_vec(),
        )
        .unwrap();
    let frame = loop {
        let frame = session.frame();
        if session.agent_shortcut_prompt(&frame).is_some() {
            break frame;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    let pane = fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap();
    pane.session = session;
    pane.frame = Arc::clone(&frame);
    fixture
        .workspace
        .windows
        .get_mut(&fixture.owner)
        .unwrap()
        .focused = true;
    let id = fixture.open(Mode::Run);
    let epoch = fixture
        .run_mut(id)
        .next(Phase::Directory, "Reading directory");
    let directory = LocalDirectory::capture(fixture.profile.0.canonicalize().unwrap()).unwrap();
    let _ = fixture.send(id, Message::Directory(epoch, Ok(directory)));
    fixture.run_mut(id).design.approval = Approval::Cursor;
    let binding = fixture
        .workspace
        .agent_cursor_review(fixture.owner, fixture.session, &frame)
        .unwrap()
        .binding;
    assert!(fixture.run(id).phase == Phase::ProbeApproval);

    let mut stale = binding;
    stale.epoch -= 1;
    no_task(
        fixture
            .workspace
            .update_agent_cursor(Cursor::Key(stale, Key::Escape)),
    );
    assert!(fixture.run(id).phase == Phase::ProbeApproval);
    stale = binding;
    stale.stream += 1;
    no_task(
        fixture
            .workspace
            .update_agent_cursor(Cursor::Key(stale, Key::Enter)),
    );
    assert!(fixture.run(id).phase == Phase::ProbeApproval);
    let _ = fixture
        .workspace
        .update_agent_cursor(Cursor::Key(binding, Key::Escape));
    assert!(fixture.run(id).phase == Phase::Cancelled);
    assert!(
        fixture
            .workspace
            .agent_cursor_repeated(&iced::keyboard::Key::Named(
                iced::keyboard::key::Named::Escape
            ))
    );
    assert!(
        fixture
            .workspace
            .agent_cursor_review(fixture.owner, fixture.session, &frame)
            .is_none()
    );
    let _ = fixture.workspace.sessions[&fixture.session]
        .session
        .stop()
        .recv_blocking();
}

#[test]
fn terminal_agent_private_transport_omits_durable_usage_account() {
    let mut fixture = Fixture::new(false, true);
    let id = fixture.open(Mode::Ask);
    let config = fixture.workspace.ai_configuration().unwrap();
    assert!(config.agent_transport(1).unwrap().0.has_quota());
    assert!(
        !fixture
            .workspace
            .agent_transport(fixture.run(id))
            .unwrap()
            .0
            .has_quota()
    );
}

#[test]
fn agent_passwordless_sudo_is_refused_on_local_targets_before_any_approval() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let directory = LocalDirectory::capture(fixture.profile.0.canonicalize().unwrap()).unwrap();
    let checking = fixture
        .run_mut(id)
        .next(Phase::Directory, "Checking directory");
    assert_only_agent_notice_task(
        fixture.send(id, Message::Directory(checking, Ok(directory))),
        fixture.owner,
    );
    assert!(fixture.run(id).phase == Phase::ProbeApproval);
    let epoch = fixture
        .run_mut(id)
        .next(Phase::Thinking, "Waiting for a decision");
    let decision = Decision {
        summary: "Inspect".into(),
        message: "Check the effective user".into(),
        action: Action::Commands(vec![Command {
            command: "id -u".into(),
            purpose: "Check effective user".into(),
            risk: engine::Risk::RequiresConfirmation,
            elevation: Elevation::PasswordlessSudo,
        }]),
    };
    assert_only_agent_notice_task(
        fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into()))),
        fixture.owner,
    );
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).commands.is_empty());
    assert!(fixture.run(id).reviewed_command().is_none());
    assert!(fixture.run(id).notice.contains("attached SSH"));
}

#[test]
fn agent_passwordless_sudo_failure_stops_queued_commands_without_a_retry() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "id -u".into(),
            purpose: "Inspect user".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    let epoch = run.next(Phase::Elevated, "Running an approved sudo command");
    let outcome = Outcome {
        status: engine::Status::Exited(1),
        stdout: Default::default(),
        stderr: Default::default(),
    };
    assert_only_agent_notice_task(
        fixture.send(id, Message::Command(epoch, Arc::new(Ok(outcome)))),
        fixture.owner,
    );
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).commands.is_empty());
    assert!(fixture.run(id).notice.contains("passwordless sudo"));
    assert!(
        fixture
            .run(id)
            .activity
            .iter()
            .any(|entry| entry.contains("Exit status 1"))
    );
}

#[test]
fn agent_password_sudo_blocks_local_targets_and_unowned_password_messages() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let epoch = fixture
        .run_mut(id)
        .next(Phase::Thinking, "Waiting for a decision");
    no_task(fixture.send(id, Message::Password(password::Message::Challenge(epoch))));
    no_task(fixture.send(id, Message::Password(password::Message::Use(epoch))));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Manual)),
    ));
    #[cfg(target_os = "windows")]
    no_task(fixture.send(
        id,
        Message::Password(password::Message::ManualReady(epoch, true)),
    ));
    assert!(fixture.run(id).phase == Phase::Thinking);
    let result = Decision {
        summary: "Inspect".into(),
        message: "Check the effective user".into(),
        action: Action::Commands(vec![Command {
            command: "id -u".into(),
            purpose: "Check the effective user".into(),
            risk: engine::Risk::RequiresConfirmation,
            elevation: Elevation::PasswordSudo,
        }]),
    };
    assert_only_agent_notice_task(
        fixture.send(id, Message::Decision(epoch, Arc::new(Ok(result).into()))),
        fixture.owner,
    );
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).password.is_none());
    assert!(fixture.run(id).commands.is_empty());
    assert!(fixture.run(id).notice.contains("attached SSH"));
}
