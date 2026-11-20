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
fn agent_text_zoom_keeps_independent_sizes_and_remembered_defaults() {
    use crate::workspace::terminal_agent::font::View;
    use iced::keyboard::{Key, Modifiers};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.window_settings.agent_activity_font_size = Some(19.5);
    fixture.workspace.window_settings.agent_plan_font_size = Some(21.5);
    let plan = fixture.open(Mode::Plan);
    assert_eq!(fixture.run(plan).fonts.activity, 13.0);
    assert_eq!(fixture.run(plan).fonts.plan, 21.5);
    no_task(fixture.send(plan, Message::Zoom(View::Activity, true)));
    assert_eq!(fixture.run(plan).fonts.activity, 14.0);
    assert_eq!(
        fixture.workspace.window_settings.agent_activity_font_size,
        Some(19.5)
    );

    no_task(fixture.send(plan, Message::RememberActivityFont(true)));
    no_task(fixture.send(plan, Message::Zoom(View::Activity, false)));
    assert_eq!(
        fixture.workspace.window_settings.agent_activity_font_size,
        Some(13.0)
    );
    no_task(
        fixture
            .workspace
            .agent_plan_zoom_shortcut(plan, &Key::Character("=".into()), Modifiers::COMMAND)
            .unwrap(),
    );
    assert_eq!(fixture.run(plan).fonts.plan, 22.0);
    assert_eq!(fixture.run(plan).fonts.activity, 13.0);
    assert_eq!(
        fixture.workspace.window_settings.agent_plan_font_size,
        Some(22.0)
    );

    let mut private = Fixture::new(false, true);
    let plan = private.open(Mode::Plan);
    no_task(private.send(plan, Message::Zoom(View::Plan, true)));
    no_task(private.send(plan, Message::RememberActivityFont(true)));
    assert_eq!(private.run(plan).fonts.plan, 14.0);
    assert_eq!(private.workspace.window_settings.agent_plan_font_size, None);
    assert_eq!(
        private
            .workspace
            .window_settings
            .agent_remember_activity_font,
        None
    );
}

#[test]
fn root_bypass_requires_probe_overrides_ask_and_revokes_with_saved_confirmation() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    no_task(fixture.send(id, Message::AutoApproveRoot(true)));
    no_task(fixture.send(id, Message::ConfirmEveryCommand(true)));
    let root = fixture.profile.0.canonicalize().unwrap();
    let checking = fixture.run_mut(id).next(Phase::Directory, "Checking");
    drop(fixture.send(
        id,
        Message::Directory(checking, Ok(LocalDirectory::capture(root.clone()).unwrap())),
    ));
    fixture.run_mut(id).manual_directory = Some(root.to_str().unwrap().into());
    let decide = |fixture: &mut Fixture| {
        fixture.run_mut(id).commands.clear();
        let epoch = fixture.run_mut(id).next(Phase::Thinking, "Deciding");
        let decision = Decision {
            summary: "Write".into(),
            message: "Write fixture".into(),
            action: Action::Commands(vec![Command {
                command: "touch root-fixture".into(),
                purpose: "Write".into(),
                risk: engine::Risk::RequiresConfirmation,
                elevation: Elevation::None,
            }]),
        };
        fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into())))
    };
    drop(decide(&mut fixture));
    assert!(fixture.run(id).phase == Phase::Approval);
    fixture.run_mut(id).probe_reviewed = true;
    // The automatic path schedules directory revalidation; never poll its command task.
    drop(decide(&mut fixture));
    assert!(matches!(
        fixture.run(id).phase,
        Phase::ReviewedDirectory { probe: false }
    ));
    let dispatch = fixture
        .workspace
        .agent_dispatch_policy(fixture.run(id))
        .unwrap();
    assert!(dispatch.check().is_ok());
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut json = serde_json::to_value(&baseline).unwrap();
    json["general"]["agent_confirm_mutating"] = true.into();
    let changed: Configuration = serde_json::from_value(json).unwrap();
    changed.save_fixture_at(&root, &baseline).unwrap();
    assert!(dispatch.check().is_err());
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    run.next(Phase::Approval, "Review again");
    assert!(!fixture.workspace.automatic_agent_command(&mut run));
    assert!(!run.auto_approve_root && run.run_approval.is_none());
    assert!(!root.join("root-fixture").exists());
}

#[test]
fn root_bypass_pause_revokes_captured_dispatch_and_pending_directory_epoch() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let root = fixture.profile.0.canonicalize().unwrap();
    let checking = fixture.run_mut(id).next(Phase::Directory, "Checking");
    drop(fixture.send(
        id,
        Message::Directory(checking, Ok(LocalDirectory::capture(root.clone()).unwrap())),
    ));
    let run = fixture.run_mut(id);
    run.auto_approve_root = true;
    run.probe_reviewed = true;
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "touch stale-root-fixture".into(),
            purpose: "Write".into(),
            risk: engine::Risk::RequiresConfirmation,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    run.next(Phase::Approval, "Review");
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    assert!(fixture.workspace.automatic_agent_command(&mut run));
    let dispatch = fixture.workspace.agent_dispatch_policy(&run).unwrap();
    assert!(dispatch.check().is_ok());
    let pending = run.next(
        Phase::ReviewedDirectory { probe: false },
        "Checking approved directory",
    );
    fixture.workspace.terminal_agents.runs.insert(id, run);
    drop(fixture.send(id, Message::Pause));
    assert!(dispatch.check().is_err());
    assert!(fixture.run(id).phase == Phase::Approval);
    assert!(fixture.run(id).epoch > pending);
    drop(fixture.send(id, Message::Pause));
    no_task(fixture.send(
        id,
        Message::Directory(pending, Ok(LocalDirectory::capture(root.clone()).unwrap())),
    ));
    assert!(fixture.run(id).phase == Phase::Approval);
    assert!(!fixture.run(id).auto_approve_root);
    assert!(!root.join("stale-root-fixture").exists());
}

#[test]
fn root_bypass_obeys_managed_confirm_and_pause_discards_the_request() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    fixture.run_mut(id).auto_approve_root = true;
    fixture.run_mut(id).confirm_every_command = true;
    fixture.run_mut(id).probe_reviewed = true;
    fixture.run_mut(id).commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "sudo -n id -u".into(),
            purpose: "Inspect".into(),
            risk: engine::Risk::RequiresConfirmation,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    fixture.run_mut(id).next(Phase::Approval, "Review");
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Confirm;
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    assert!(!fixture.workspace.automatic_agent_command(&mut run));
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Allow;
    assert!(fixture.workspace.automatic_agent_command(&mut run));
    fixture.workspace.terminal_agents.runs.insert(id, run);
    drop(fixture.send(id, Message::Pause));
    assert!(!fixture.run(id).auto_approve_root && fixture.run(id).run_approval.is_none());
}

#[test]
fn automatic_read_only_commands_require_probe_review_and_obey_per_run_confirmation() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let root = fixture.profile.0.canonicalize().unwrap();
    let checking = fixture
        .run_mut(id)
        .next(Phase::Directory, "Checking directory");
    let task = fixture.send(
        id,
        Message::Directory(checking, Ok(LocalDirectory::capture(root.clone()).unwrap())),
    );
    drop(task);
    fixture.run_mut(id).manual_directory = Some(root.to_str().unwrap().into());
    let decide = |fixture: &mut Fixture, command: &str| {
        fixture.run_mut(id).commands.clear();
        let epoch = fixture.run_mut(id).next(Phase::Thinking, "Deciding");
        let decision = Decision {
            summary: "Inspect".into(),
            message: "Check".into(),
            action: Action::Commands(vec![Command {
                command: command.into(),
                purpose: "Inspect".into(),
                risk: engine::Risk::ReadOnly,
                elevation: Elevation::None,
            }]),
        };
        fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into())))
    };
    drop(decide(&mut fixture, "pwd"));
    assert!(fixture.run(id).phase == Phase::Approval);
    fixture.run_mut(id).probe_reviewed = true;
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Confirm;
    drop(decide(&mut fixture, "pwd"));
    assert!(matches!(
        fixture.run(id).phase,
        Phase::ReviewedDirectory { probe: false }
    ));
    assert!(fixture.run(id).run_approval.is_some());
    fixture.run_mut(id).confirm_every_command = true;
    drop(decide(&mut fixture, "pwd"));
    assert!(fixture.run(id).phase == Phase::Approval);
    fixture.run_mut(id).confirm_every_command = false;
    drop(decide(&mut fixture, "touch file"));
    assert!(fixture.run(id).phase == Phase::Approval);
    assert!(fixture.run(id).run_approval.is_none());
}

#[test]
fn run_wide_approval_continues_ordinary_decisions_and_revokes_before_dispatch() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let root = fixture.profile.0.canonicalize().unwrap();
    let checking = fixture
        .run_mut(id)
        .next(Phase::Directory, "Checking directory");
    let task = fixture.send(
        id,
        Message::Directory(checking, Ok(LocalDirectory::capture(root.clone()).unwrap())),
    );
    drop(task);
    let command = Command {
        command: "printf fixture".into(),
        purpose: "Fixture".into(),
        risk: engine::Risk::RequiresConfirmation,
        elevation: Elevation::None,
    };
    fixture.run_mut(id).commands.push_back(ReviewedCommand {
        proposal: command.clone(),
        sudo: None,
    });
    fixture.run_mut(id).next(Phase::Approval, "Review");
    let approval = fixture
        .workspace
        .approve_remaining_agent_commands(fixture.run(id))
        .unwrap();
    fixture.run_mut(id).run_approval = Some(approval);
    fixture.run_mut(id).manual_directory = Some(root.to_str().unwrap().into());
    let epoch = fixture.run_mut(id).next(Phase::Thinking, "Deciding");
    let decision = Decision {
        summary: "Fixture".into(),
        message: "Continue".into(),
        action: Action::Commands(vec![command]),
    };
    let task = fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into())));
    assert!(matches!(
        fixture.run(id).phase,
        Phase::ReviewedDirectory { probe: false }
    ));
    assert!(fixture.run(id).run_approval.is_some());
    drop(task);
    let dispatch = fixture
        .workspace
        .agent_dispatch_policy(fixture.run(id))
        .unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut json = serde_json::to_value(&baseline).unwrap();
    json["general"]["agent_confirm_mutating"] = true.into();
    let changed: Configuration = serde_json::from_value(json).unwrap();
    changed.save_fixture_at(&root, &baseline).unwrap();
    assert!(dispatch.check().is_err());
    let run = fixture.run_mut(id);
    run.next(Phase::Approval, "Review again");
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    assert!(!fixture.workspace.automatic_agent_command(&mut run));
    assert!(run.run_approval.is_none());
}

#[test]
fn run_wide_approval_is_blocked_by_policy_and_expires_on_pause_or_elevation() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    fixture.run_mut(id).commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "printf fixture".into(),
            purpose: "Fixture".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    fixture.run_mut(id).next(Phase::Approval, "Review");
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Confirm;
    assert!(
        fixture
            .workspace
            .approve_remaining_agent_commands(fixture.run(id))
            .is_err()
    );
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Allow;
    let approval = fixture
        .workspace
        .approve_remaining_agent_commands(fixture.run(id))
        .unwrap();
    fixture.run_mut(id).run_approval = Some(approval);
    let task = fixture.send(id, Message::Pause);
    drop(task);
    assert!(fixture.run(id).run_approval.is_none());
    fixture.run_mut(id).paused = false;
    fixture
        .run_mut(id)
        .commands
        .front_mut()
        .unwrap()
        .proposal
        .command = "sudo id".into();
    assert!(
        fixture
            .workspace
            .approve_remaining_agent_commands(fixture.run(id))
            .is_err()
    );
}

#[test]
fn execution_preference_blocks_run_and_reconciles_existing_runs_without_blocking_ask_or_plan() {
    let mut fixture = Fixture::new(false, true);
    let run = fixture.open(Mode::Run);
    let disabled: Configuration = serde_json::from_value(json!({
        "general": {"agent_execution_enabled": false},
        "profiles": {"profiles": [{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"fixture"}], "default":1,"last_id":1}
    })).unwrap();
    fixture.workspace.set_test_ai_configuration(disabled);
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
    assert!(fixture.workspace.swarm_ai_policy(Mode::Run).is_err());
    assert!(fixture.workspace.swarm_ai_policy(Mode::Ask).is_ok());
    let _ = fixture.workspace.reconcile_terminal_agents();
    assert!(fixture.run(run).cancel.requested());
    assert!(fixture.run(run).phase == Phase::Cancelled);
}

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
fn terminal_agent_can_export_frozen_archive_while_another_run_is_active() {
    use std::path::Path;

    use crate::terminal_agent::activity_report::Format;
    use activity_export::{Message as Export, Scope};

    let mut fixture = Fixture::new(false, false);
    complete_export_run(&mut fixture, "First retained task");
    complete_export_run(&mut fixture, "Second retained task");
    let id = fixture.open(Mode::Ask);
    let run = fixture.run_mut(id);
    run.goal = "Currently active task".into();
    run.activity_export.start();
    let epoch = run.next(Phase::Thinking, "Waiting for a fixture decision");
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let actions = fixture
        .workspace
        .terminal_agent_design_actions(fixture.session, stream);
    let [(current, export)] = actions.as_slice() else {
        panic!("an active run should offer only the archive export in this fixture");
    };
    assert_eq!(*current, id);
    assert_eq!(export.label(), "Export all finished runs…");
    let export = *export;

    let root = fixture.profile.0.canonicalize().unwrap();
    let review = |fixture: &mut Fixture, path: &Path| {
        drop(fixture.send(id, Message::DesignAction(export)));
        for action in [
            Export::Scope(Scope::This),
            Export::Format(Format::Json),
            Export::Path(path.to_str().unwrap().into()),
        ] {
            no_task(fixture.send(id, Message::ActivityExport(action)));
        }
        assert!(!path.exists());
    };
    let publish = |fixture: &mut Fixture, path: &Path| {
        let worker = fixture.send(id, Message::ActivityExport(Export::Confirm));
        let write = fixture.send(id, activity_result(worker, id));
        no_task(fixture.send(id, activity_result(write, id)));
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("fixture-plain-secret"));
        let report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let runs = report["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["prompt"], "First retained task");
        assert_eq!(runs[1]["prompt"], "Second retained task");
    };

    let path = root.join("archive-during-run.json");
    review(&mut fixture, &path);
    publish(&mut fixture, &path);
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(fixture.run(id).phase == Phase::Thinking);
    assert!(!fixture.run(id).cancel.requested());

    let path = root.join("archive-after-settlement.json");
    review(&mut fixture, &path);
    let decision = Decision {
        summary: "Finished".into(),
        message: "Recently completed answer".into(),
        action: Action::Done,
    };
    drop(fixture.send(id, Message::Decision(epoch, Arc::new(Ok(decision).into()))));
    assert!(fixture.run(id).phase == Phase::Finished);
    publish(&mut fixture, &path);
}

#[test]
fn terminal_agent_private_activity_never_offers_or_starts_export() {
    use activity_export::Message as Export;

    let mut fixture = Fixture::new(false, true);
    let id = complete_export_run(&mut fixture, "Private task");
    assert!(fixture.run(id).activity_export.view(None).is_none());
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
                terminal_effect: None,
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
fn agent_memory_masks_learned_credentials_and_blocks_changed_or_new_memory() {
    use crate::local_ai::test_support::{exclusive_run, listener};
    use crate::private_files::Directory;
    use crate::terminal_logging::RedactionSecrets;

    const SECRET: &str = r#"private-agent-"credential\value"#;
    let _exclusive = exclusive_run();
    let (model, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &model.local_addr().unwrap().to_string());
    let root = fixture.profile.0.canonicalize().unwrap();
    let memory = Directory::open(&root.join("memory"), true).unwrap();
    let start_review = |fixture: &mut Fixture| {
        let id = fixture.open(Mode::Ask);
        let _ = fixture.send(id, Message::Goal("Inspect disk usage".into()));
        let run = fixture.run_mut(id);
        let learned = RedactionSecrets::with_secret(None, SECRET).unwrap();
        run.redactor = run.redactor.with_overlay(&learned).unwrap();
        run.learned_sudo = Some(learned);
        let task = fixture.send(id, Message::Start);
        finish_agent_task(fixture, task);
        assert!(fixture.run(id).phase == Phase::ModelReview);
        id
    };

    let absent = start_review(&mut fixture);
    memory
        .publish_copy(
            "MEMORY.md",
            format!("Wait for {SECRET} before checking disk usage.").as_bytes(),
        )
        .unwrap();
    let stale_epoch = fixture.run(absent).epoch;
    let stale = fixture.send(
        absent,
        Message::ReviewModel(request_review::Message::Send(stale_epoch)),
    );
    finish_agent_task(&mut fixture, stale);
    assert!(fixture.run(absent).phase == Phase::Blocked);

    let id = start_review(&mut fixture);
    let run = fixture.run(id);
    let review = run.model_review.as_ref().unwrap();
    assert!(
        review
            .system
            .contains("Wait for *** before checking disk usage.")
    );
    assert!(!review.system.contains("private-agent-"));
    let epoch = run.epoch;
    memory
        .replace_file("MEMORY.md", b"Do not check usage until Friday.")
        .unwrap();
    let stale = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, stale);
    assert!(fixture.run(id).phase == Phase::Blocked);
    assert!(fixture.run(id).notice.contains("AI memory changed"));
    assert_eq!(
        model.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
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
        run.decision_input(false, 4096, None).unwrap(),
        original.to_string()
    );

    let learned = RedactionSecrets::with_secret(None, SECRET).unwrap();
    run.redactor = run.redactor.with_overlay(&learned).unwrap();
    run.learned_sudo = Some(learned);
    let reviewed = run.decision_input(false, 4096, None).unwrap();
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
fn saved_connection_preselects_the_server_model_for_foreground_agent_only() {
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&baseline).unwrap();
    draft["profiles"]["profiles"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"id": 2, "name": "Connection model", "endpoint": "127.0.0.1:12346",
            "model_mode": "server-default", "model": ""}),
            json!({"id": 3, "name": "Chat only", "connection_mode": "remote-open-ai",
            "endpoint": "https://example.invalid/v1", "trusted_remote_endpoint": true,
            "model_mode": "server-default", "model": ""}),
        ]);
    draft["profiles"]["last_id"] = json!(3);
    let config: Configuration = serde_json::from_value(draft).unwrap();
    config.save_fixture_at(&root, &baseline).unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    let mut store = Store::open(root.join("connections.json")).unwrap();
    let mut connection = fixture.workspace.sessions[&fixture.session]
        .saved_local
        .clone()
        .unwrap();
    connection.terminal.ai_profile = Some(ProfileRef::Numeric(2));
    store.save_local(connection.clone()).unwrap();
    fixture.workspace.refresh_saved_terminal_preferences(&store);

    let ask = fixture.open(Mode::Ask);
    assert_eq!(fixture.run(ask).profile().unwrap().id, 2);
    let run = fixture.open(Mode::Run);
    assert_eq!(fixture.run(run).profile().unwrap().id, 2);
    let plan = fixture.open(Mode::Plan);
    assert_eq!(fixture.run(plan).profile().unwrap().id, 2);

    connection.terminal.ai_profile = Some(ProfileRef::Numeric(3));
    store.save_local(connection).unwrap();
    fixture.workspace.refresh_saved_terminal_preferences(&store);
    let ask = fixture.open(Mode::Ask);
    assert_eq!(fixture.run(ask).profile().unwrap().id, 1);
}

#[test]
fn standalone_agent_reviews_server_default_before_sending_without_a_model() {
    use std::io::Write as _;

    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_response, listener, request,
    };

    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&baseline).unwrap();
    draft["profiles"] = json!({
        "profiles": [{"id": 2, "name": "Loopback fixture", "endpoint": endpoint,
                      "model": "", "model_mode": "server-default"}],
        "last_id": 2
    });
    let saved: Configuration = serde_json::from_value(draft.clone()).unwrap();
    saved.save_fixture_at(&root, &baseline).unwrap();
    let saved = Configuration::load_fixture_from(&root).unwrap();
    assert!(saved.agent_profiles().next().is_none());
    assert!(saved.agent_transport(2).is_err());
    fixture.workspace.set_test_ai_configuration(saved.clone());

    let run = fixture.open(Mode::Run);
    assert!(fixture.run(run).profile().is_none());
    assert!(fixture.run(run).profiles[0].server_default.is_some());
    let plan = fixture.open(Mode::Plan);
    assert!(fixture.run(plan).profile().is_none());
    assert!(fixture.run(plan).profiles[0].server_default.is_some());

    draft["profiles"]["profiles"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": 1, "name": "Manual fallback", "endpoint": endpoint, "model": "fixture",
        }));
    draft["profiles"]["default"] = json!(1);
    draft["local"]["roles"] = json!([null, 2, 1, null]);
    let named_agent: Configuration = serde_json::from_value(draft.clone()).unwrap();
    assert!(named_agent.foreground_agent_default_role_id().is_none());
    draft["local"]["roles"] = json!([null, 2, null, null]);
    let assigned: Configuration = serde_json::from_value(draft).unwrap();
    assigned.save_fixture_at(&root, &saved).unwrap();
    let assigned = Configuration::load_fixture_from(&root).unwrap();
    assert_eq!(assigned.agent_profile_id(), Some(1));
    fixture.workspace.set_test_ai_configuration(assigned);

    let id = fixture.open(Mode::Ask);
    assert_eq!(fixture.run(id).profile().unwrap().id, 2);
    let _ = fixture.send(id, Message::Goal("Inspect disk usage".into()));
    let task = fixture.send(id, Message::Start);
    finish_agent_task(&mut fixture, task);
    let run = fixture.run(id);
    assert!(run.phase == Phase::ModelReview);
    let review = run.model_review.as_ref().unwrap();
    assert_eq!(
        review.reviewed_destination(),
        Some((endpoint.as_str(), "Server selects the model"))
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let epoch = run.epoch;
    let worker = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let payload = request(&mut stream);
        assert!(payload.get("model").is_none());
        stream
            .get_mut()
            .write_all(
                fixture_response(
                    "200 OK",
                    "",
                    r#"{"status":"done","summary":"Inspected","userMessage":"Reviewed answer"}"#,
                )
                .as_bytes(),
            )
            .unwrap();
    });
    let send = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    finish_agent_task(&mut fixture, send);
    worker.join().unwrap();
    assert!(
        fixture.run(id).phase == Phase::Finished,
        "{}",
        fixture.run(id).notice
    );
}

#[test]
fn standalone_https_agent_requires_explicit_selection_and_rechecks_saved_opt_in() {
    check_standalone_https_agent_review(false);
}

#[test]
fn server_selected_https_agent_requires_a_saved_foreground_opt_in() {
    check_standalone_https_agent_review(true);
}

fn check_standalone_https_agent_review(server_default: bool) {
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&baseline).unwrap();
    let destination = if server_default {
        "https://example.invalid/gateway/complete/"
    } else {
        "https://example.invalid/v1/chat/completions"
    };
    draft["profiles"] = json!({
        "profiles": [{
            "id": 2,
            "name": "Remote fixture",
            "connection_mode": if server_default { "remote-open-ai-exact" } else { "remote-open-ai" },
            "endpoint": destination,
            "model": if server_default { "" } else { "fixture" },
            "model_mode": if server_default { "server-default" } else { "manual" },
            "trusted_remote_endpoint": true,
            "reviewed_agent_https": true
        }],
        "default": if server_default { None } else { Some(2) },
        "last_id": 2
    });
    let saved: Configuration = serde_json::from_value(draft.clone()).unwrap();
    saved.save_fixture_at(&root, &baseline).unwrap();
    let saved = Configuration::load_fixture_from(&root).unwrap();
    assert!(saved.agent_profile_id().is_none());
    assert!(saved.agent_profiles().next().is_none());
    assert!(saved.agent_transport(2).is_err());
    assert_eq!(
        saved.reviewed_agent_https_profiles().count(),
        usize::from(!server_default)
    );
    assert_eq!(
        saved.foreground_default_profiles(false).count(),
        usize::from(server_default)
    );
    fixture.workspace.set_test_ai_configuration(saved.clone());

    let id = fixture.open(Mode::Ask);
    assert!(fixture.run(id).profile().is_none());
    let selected = fixture.run(id).profiles[0].clone();
    assert_eq!(selected.reviewed_https.as_deref(), Some(destination));
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
    assert_eq!(
        review.transport().0.uses_server_default_model(),
        server_default
    );
    if server_default {
        assert_eq!(
            review.reviewed_destination(),
            Some((destination, "Server selects the model"))
        );
    }
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
fn managed_https_agent_role_requires_selection_and_rechecks_installed_policy() {
    use crate::ai_manager::ProfileRef;

    let (listener, _) = crate::local_ai::test_support::listener();
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&baseline).unwrap();
    draft["local"]["roles"] = json!([null, null, "policy-agent-fixture", null]);
    // The test projects an in-memory policy without changing the installed one.
    std::fs::write(
        root.join(crate::ai_manager::FILE_NAME),
        serde_json::to_vec(&draft).unwrap(),
    )
    .unwrap();
    let policy = crate::clipboard_policy::parse(&format!(
        "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-agent-fixture'\n\
         name = 'Managed agent fixture'\nprovider = 'openai-compatible'\n\
         endpoint = 'https://{}/v1'\nmodel = 'fixture'\n",
        listener.local_addr().unwrap(),
    ))
    .unwrap();
    fixture.workspace.set_test_ai_configuration(
        Configuration::load_fixture_from(&root)
            .unwrap()
            .with_policy_profiles(&policy)
            .unwrap(),
    );

    let id = fixture.open(Mode::Ask);
    assert!(fixture.run(id).profile().is_none());
    let selected = fixture
        .run(id)
        .profiles
        .iter()
        .find(|choice| choice.reference == ProfileRef::Policy("policy-agent-fixture".into()))
        .unwrap()
        .clone();
    assert!(selected.reviewed_https.is_some());
    let _ = fixture.send(id, Message::Goal("Inspect password=fixture-secret".into()));
    no_task(fixture.send(id, Message::Start));
    assert!(fixture.run(id).phase == Phase::Setup);
    let (transport, _) = selected
        .transport(&fixture.workspace.ai_configuration().unwrap())
        .unwrap();
    assert!(transport.is_remote());
    assert_eq!(transport.policy_profile_id(), Some("policy-agent-fixture"));
    let _ = fixture.send(id, Message::Profile(selected));

    // Only the fixture projection contains this policy profile.
    no_task(fixture.send(id, Message::Start));
    let run = fixture.run(id);
    assert!(run.phase == Phase::Blocked);
    assert!(run.model_review.is_none());
    assert!(
        run.notice.contains("policy-provided AI profile changed"),
        "{}",
        run.notice
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
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

#[test]
fn terminal_agent_cursor_keys_bind_the_review_to_the_visible_stream_and_epoch() {
    use crate::terminal_agent::design::Approval;
    use crate::terminal_view::agent_cursor::{Key, Message as Cursor};

    let mut fixture = Fixture::new(false, false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    #[cfg(unix)]
    let session = {
        let session =
            Session::start(GridSize::default(), kortty_terminal::TerminalProfile::Xterm).unwrap();
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
        session
    };
    #[cfg(windows)]
    let session = {
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = r#"
            $esc = [char]27
            $bell = [char]7
            [Console]::Write("${esc}]133;A${bell}cursor-fixture-ready${esc}]133;B${bell}")
            [void][Console]::ReadLine()
        "#;
        Session::start_local(
            GridSize::default(),
            kortty_terminal::TerminalProfile::Xterm,
            kortty_pty::LaunchOptions {
                command: Some(vec![
                    powershell.to_str().unwrap().into(),
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script.into(),
                ]),
                ..Default::default()
            },
            true,
            None,
        )
        .unwrap()
    };
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
    fixture.run_mut(id).auto_approve_root = true;
    fixture.run_mut(id).probe_reviewed = true;
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
    fixture.run_mut(id).auto_approve_root = true;
    fixture.run_mut(id).probe_reviewed = true;
    let epoch = fixture
        .run_mut(id)
        .next(Phase::Thinking, "Waiting for a decision");
    no_task(fixture.send(id, Message::Password(password::Message::Challenge(epoch))));
    no_task(fixture.send(id, Message::Password(password::Message::Use(epoch))));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Manual)),
    ));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::ManualReady(epoch, true)),
    ));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Approve(epoch, password::Source::Ticket)),
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

#[test]
fn agent_password_sudo_completion_cannot_restore_a_cancelled_run() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let epoch = fixture.run_mut(id).next(Phase::SudoRunning, "Running sudo");
    let completed = password::Completed::new(engine::Status::Exited(0), None);
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Finished(
            epoch - 1,
            Ok(completed.clone()),
        )),
    ));
    assert!(fixture.run(id).phase == Phase::SudoRunning);
    no_task(fixture.send(id, Message::Cancel));
    no_task(fixture.send(
        id,
        Message::Password(password::Message::Finished(epoch, Ok(completed))),
    ));
    assert!(fixture.run(id).phase == Phase::Cancelled);
    assert!(fixture.run(id).sudo_terminal.is_none());
}
