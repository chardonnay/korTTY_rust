use std::sync::Arc;

use iced::futures::{StreamExt as _, executor::block_on};

use super::*;
use crate::ai_manager::Configuration;
use crate::session::{GridSize, IbmKind, Session, SessionId, Status as SessionStatus};
use crate::session_security::LocalSettings;
use crate::snippets::tests::Profile;
use crate::terminal_view::selection_test_frame;
use crate::workspace::terminal_notifications::AiRunNotice;

mod automation_tests;
mod batch_approval;
mod comparison;
mod model_review;
mod model_run;
mod restarts;

fn local(workspace: &mut Workspace, owner: window::Id, private: bool) -> SessionId {
    let source = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    workspace.add_tab(owner, source);
    workspace.active_tab_mut(owner).unwrap().incognito = private;
    let pane = workspace.sessions.get_mut(&source).unwrap();
    pane.local_security = Some(LocalSettings::default());
    pane.frame = selection_test_frame(pane.frame.screen.clone());
    Arc::get_mut(&mut pane.frame).unwrap().status = SessionStatus::Running;
    source
}

fn drain(workspace: &mut Workspace, task: Task<Desktop>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
                    assert!(
                        !matches!(
                            message,
                            RunMessage::Command(_, _) | RunMessage::Decision(_, _)
                        ),
                        "A fixture must never launch a command or contact a model"
                    );
                    pending.push(workspace.update_terminal_agent(id, message));
                }
            }
        }
    }
}

fn review_source(workspace: &mut Workspace, id: window::Id, path: &std::path::Path) -> Vec<Key> {
    let group = workspace.terminal_agents.swarms.get_mut(&id).unwrap();
    group
        .choices
        .iter_mut()
        .map(|choice| {
            choice.directory = Some(path.to_str().unwrap().into());
            choice.target.key
        })
        .collect()
}

#[test]
fn swarm_scheduler_handoff_preserves_saved_targets_and_blocks_route_changes() {
    use crate::connection_store::{Auth, Connection, SavedLogin, Store};
    use crate::ssh_dialog::LoginTarget;

    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
        "default":1,"last_id":1
    }}))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    let _ = workspace.open_agent_swarm(owner);
    let group_id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let store_path = root.join("connections.json");
    let mut store = Store::open(store_path.clone()).unwrap();
    let mut sources = Vec::new();
    for host in ["one.example.test", "two.example.test"] {
        let id = store
            .save(Connection {
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
            })
            .unwrap();
        sources.push(headless::Source::new(store.get(id).unwrap(), &store_path));
    }
    let targets: Vec<_> = sources
        .iter()
        .map(|source| source.reference.clone())
        .collect();
    let _ = workspace.swarm_action(group_id, Message::SavedTargets(Ok(sources)));
    let keys: Vec<_> = workspace.terminal_agents.swarms[&group_id]
        .choices
        .iter()
        .map(|choice| choice.target.key)
        .collect();
    assert_eq!(keys.len(), 3);
    for key in keys {
        let _ = workspace.swarm_action(group_id, Message::Toggle(key, true));
    }
    let _ = workspace.swarm_action(
        group_id,
        Message::Goal("  Inspect available storage  ".into()),
    );
    let group = &workspace.terminal_agents.swarms[&group_id];
    let (actual_owner, handoff) = workspace.swarm_scheduler_draft(group).unwrap();
    assert_eq!(actual_owner, owner);
    assert_eq!(handoff.goal, "Inspect available storage");
    assert_eq!(handoff.targets, targets);
    assert!(handoff.read_only);
    let _ = workspace.swarm_action(group_id, Message::Toggle(Key::Saved(targets[1].id), false));
    let group = &workspace.terminal_agents.swarms[&group_id];
    assert!(group.can_schedule());
    let (_, singleton) = workspace.swarm_scheduler_draft(group).unwrap();
    assert_eq!(singleton.targets, [targets[0].clone()]);
    let mut changed = store.get(targets[0].id).unwrap().clone();
    changed.login.endpoint.port = 2222;
    store.save(changed).unwrap();
    assert!(workspace.swarm_scheduler_draft(group).is_err());
}

#[test]
fn swarm_revocation_blocks_queued_ai_actions_and_leaves_saved_script_mode_available() {
    let (listener, _) = crate::local_ai::test_support::listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":listener.local_addr().unwrap().to_string(),"model":"test"}],
        "default":1,"last_id":1
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let key = review_source(&mut workspace, id, &root)[0];
    let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    let _ = workspace.swarm_action(id, Message::Goal("Inspect a fixture".into()));
    assert!(
        workspace.terminal_agents.swarms[&id]
            .modes
            .contains(&SwarmMode::Advisory)
    );

    workspace.clipboard.policy.ai_swarm_feature_allowed = false;
    let task = workspace.swarm_action(id, Message::Start);
    assert_eq!(task.units(), 0);
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(group.notice.contains("blocks AI Swarm"));
    assert!(group.queue.is_none() && group.members.is_empty());
    assert!(group.modes.as_slice() == [SwarmMode::Snippet]);
    assert!(workspace.terminal_agents.runs.is_empty());
    assert!(workspace.agent_swarm_available());
    let _ = workspace.swarm_action(id, Message::ShowWorkflow);
    assert!(workspace.terminal_agents.swarms[&id].workflow.is_none());
    let _ = workspace.swarm_action(id, Message::Mode(SwarmMode::Snippet));
    assert!(workspace.terminal_agents.swarms[&id].mode == SwarmMode::Snippet);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    workspace.clipboard.policy.ai_swarm_feature_allowed = true;
    workspace.clipboard.policy.ai_agent_feature_allowed = false;
    assert!(workspace.swarm_ai_policy(Mode::Ask).is_err());
    assert!(workspace.swarm_policy().is_ok());

    workspace.clipboard.policy.ai_agent_feature_allowed = true;
    workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::ReadOnly;
    assert!(workspace.swarm_ai_policy(Mode::Run).is_err());
    assert!(workspace.swarm_ai_policy(Mode::Ask).is_ok());
}

#[test]
fn swarm_targets_own_independent_exact_reviews_and_use_only_the_real_window() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
        "default":1,"last_id":1
    }}))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, true);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let keys = review_source(&mut workspace, id, &root);
    assert_eq!(keys.len(), 2);
    let _ = workspace.swarm_action(id, Message::Mode(SwarmMode::Agent));
    let _ = workspace.swarm_action(id, Message::Goal("Inspect these two fixtures".into()));
    for &key in &keys {
        let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    }
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    let mut group = workspace.terminal_agents.swarms.remove(&id).unwrap();
    assert!(group.take_notifications(&workspace).is_empty());
    let (&member, ticket) = group.members.iter().next().unwrap();
    let key = ticket.key();
    workspace
        .terminal_agents
        .runs
        .get_mut(&member)
        .unwrap()
        .next(Phase::ProbeApproval, "Review the next exact command.");
    assert!(matches!(
        group.take_notifications(&workspace).as_slice(),
        [(target, AiRunNotice::Approval)] if *target == key
    ));
    assert!(group.take_notifications(&workspace).is_empty());
    workspace.terminal_agents.swarms.insert(id, group);
    let group = &workspace.terminal_agents.swarms[&id];
    assert_eq!(workspace.terminal_agents.running_window(), Some(id));
    for &member in group.members.keys() {
        assert!(workspace.terminal_agents.title(member).is_none());
        let run = &workspace.terminal_agents.runs[&member];
        assert_eq!(run.reviewed_command().unwrap().0, "Fixed environment probe");
        assert!(run.activity.is_empty());
    }

    let member = group
        .members
        .iter()
        .find_map(|(&member, ticket)| (ticket.key() == keys[0]).then_some(member))
        .unwrap();
    {
        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let run = workspace.terminal_agents.runs.get_mut(&member).unwrap();
        let pending =
            crate::local_ai::start(run.transport.as_ref().unwrap().0.clone(), |_| Ok(())).unwrap();
        pending.reply.recv_blocking().unwrap().unwrap();
        run.ai_cancel = Some(pending.cancel);
        let epoch = run.next(Phase::Thinking, "Waiting for the AI decision…");
        let initial = crate::terminal_agent::usage::Attempt {
            kind: crate::terminal_agent::usage::AttemptKind::Initial,
            outcome: crate::terminal_agent::usage::AttemptOutcome::InvalidReply,
            elapsed: std::time::Duration::ZERO,
            input_tokens: Some(0),
            output_tokens: None,
            total_tokens: Some(4),
        };
        let (permit, received) = async_channel::bounded(1);
        let _ = workspace.update_terminal_agent(
            member,
            RunMessage::Progress(
                epoch,
                crate::terminal_agent::usage::Progress::Repairing(initial, permit),
            ),
        );
        assert_eq!(received.try_recv(), Ok(()));
        let rows = workspace.terminal_agents.swarms[&id]
            .queue
            .as_ref()
            .unwrap()
            .rows();
        assert_eq!(rows[0].status, Status::Running);
        assert_eq!(rows[0].detail, "Repairing the AI response format…");
        assert_eq!(rows[1].status, Status::Review);
        assert!(!rows[1].detail.contains("Repairing"));
        let run = workspace.terminal_agents.runs.get_mut(&member).unwrap();
        run.finish_model_repair(epoch, true);
        run.ai_cancel = None;
        run.next(Phase::ProbeApproval, "Review the next exact command.");
    }

    let _ = workspace.swarm_action(id, Message::Cancel(keys[0]));
    let rows = workspace.terminal_agents.swarms[&id]
        .queue
        .as_ref()
        .unwrap()
        .rows();
    assert_eq!(rows[0].status, Status::Cancelled);
    assert_eq!(rows[1].status, Status::Review);
    workspace
        .sessions
        .get_mut(&keys[1].pane().unwrap().0)
        .unwrap()
        .stream_generation += 1;
    let reconciled = workspace.reconcile_terminal_agents();
    drain(&mut workspace, reconciled);
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(group.queue.as_ref().unwrap().complete());
    assert!(group.members.is_empty());
    assert!(workspace.terminal_agents.running_window().is_none());

    let path = root.join("swarm.md");
    let _ = workspace.swarm_action(id, Message::ReportPath(path.to_str().unwrap().into()));
    let save = workspace.swarm_action(id, Message::SaveReport);
    assert!(workspace.terminal_agents.swarms[&id].report_writing);
    drain(&mut workspace, save);
    let report = std::fs::read_to_string(&path).unwrap();
    assert!(report.contains("| Cancelled |"));
    assert_eq!(
        workspace.terminal_agents.swarms[&id].notice,
        "Swarm report saved."
    );
    let duplicate = workspace.swarm_action(id, Message::SaveReport);
    drain(&mut workspace, duplicate);
    assert_eq!(std::fs::read_to_string(path).unwrap(), report);
}

#[test]
fn swarm_completion_notices_are_bound_to_the_current_round_and_live_stream() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let session = local(&mut workspace, owner, false);
    let stream = workspace.sessions[&session].stream_generation;
    let live = Key::Live { session, stream };
    let saved = Key::Saved(1);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let mut group = workspace.terminal_agents.swarms.remove(&id).unwrap();
    let complete = |group: &mut Group, round, results: &[(Key, Status)]| {
        let targets = results
            .iter()
            .map(|&(key, _)| Target {
                key,
                label: "Swarm fixture".into(),
                private: false,
            })
            .collect();
        let mut queue = Queue::new(round, targets, engine::MAX_PARALLEL).unwrap();
        for (ticket, &(_, status)) in queue.reserve().into_iter().zip(results) {
            assert!(queue.change(ticket, status, "", ""));
        }
        group.round = round;
        group.queue = Some(queue);
        group.notifications = notice::State::new(round);
    };

    complete(
        &mut group,
        1,
        &[(saved, Status::Failed), (live, Status::Succeeded)],
    );
    group.focused = Some(saved);
    assert!(matches!(
        group.take_notifications(&workspace).as_slice(),
        [(target, AiRunNotice::Finished)] if *target == live
    ));
    assert!(group.take_notifications(&workspace).is_empty());

    complete(&mut group, 2, &[(saved, Status::Blocked)]);
    assert!(matches!(
        group.take_notifications(&workspace).as_slice(),
        [(target, AiRunNotice::Stopped)] if *target == saved
    ));

    complete(&mut group, 3, &[(live, Status::Failed)]);
    workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .stream_generation += 1;
    assert!(group.take_notifications(&workspace).is_empty());

    complete(&mut group, 4, &[(saved, Status::Succeeded)]);
    workspace.terminal_agents.swarms.insert(id, group);
    assert!(workspace.swarm_notice_current(id, 4));
    assert!(!workspace.swarm_notice_current(id, 3));
    let _ = workspace.swarm_action(id, Message::CancelAll);
    assert!(!workspace.swarm_notice_current(id, 4));
    let _ = workspace.swarm_action(id, Message::Close);
    assert!(!workspace.swarm_notice_current(id, 4));
}

#[test]
fn swarm_saved_script_checks_revision_after_review_without_an_ai_profile() {
    use crate::snippets::{Input, Library};
    use crate::terminal_agent::swarm::script::Prepared;

    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let mut library = Library::open(profile.path()).unwrap();
    let (language, prefix) = if cfg!(windows) {
        ("powershell", "Write-Output")
    } else {
        ("bash", "printf")
    };
    let input = |content: &str| Input {
        name: "Review fixture".into(),
        language: language.into(),
        category: String::new(),
        content: format!("{prefix} {content}").into(),
    };
    let script = library.save(None, input("original")).unwrap();
    let review = snippet::Review {
        path: profile.path(),
        prepared: Arc::new(Prepared::capture(&library, script, "").unwrap()),
    };
    let mut workspace = Workspace::default();
    workspace.clipboard.policy.ai_swarm_feature_allowed = false;
    workspace.clipboard.policy.ai_agent_feature_allowed = false;
    assert!(workspace.ai_configuration().is_none_or(|config| {
        config.agent_profile_id().is_none() && config.agent_profiles().next().is_none()
    }));
    let (owner, _) = workspace.open_window(false);
    let session = local(&mut workspace, owner, false);
    #[cfg(windows)]
    {
        workspace
            .sessions
            .get_mut(&session)
            .unwrap()
            .local_security
            .as_mut()
            .unwrap()
            .shell = kortty_pty::LocalShell::PowerShell;
    }
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let key = review_source(&mut workspace, id, &root)[0];
    assert_eq!(key.pane().unwrap().0, session);
    let group = workspace.terminal_agents.swarms.get_mut(&id).unwrap();
    assert!(group.mode == SwarmMode::Snippet);
    group.selected.insert(key);
    group.round = 1;
    group.preparing = true;
    let task = workspace.swarm_action(id, Message::SnippetPrepared(1, Ok(review)));
    drain(&mut workspace, task);
    let group = &workspace.terminal_agents.swarms[&id];
    let member = *group.members.keys().next().unwrap();
    let run = &workspace.terminal_agents.runs[&member];
    let epoch = run.epoch;
    assert!(
        run.reviewed_command()
            .unwrap()
            .1
            .contains(&format!("{prefix} original"))
    );
    assert!(run.activity.is_empty());
    library.save(Some(script), input("changed")).unwrap();
    let task = workspace.swarm_action(id, Message::Member(member, MemberAction::Approve(epoch)));
    drain(&mut workspace, task);
    let group = &workspace.terminal_agents.swarms[&id];
    let row = &group.queue.as_ref().unwrap().rows()[0];
    assert_eq!(row.status, Status::Blocked);
    assert!(row.detail.contains("changed"));
    assert!(row.transcript.is_empty());

    #[cfg(windows)]
    {
        // The wire encoding hides these bytes. Review still has to examine
        // the original argument, before either display or process launch.
        let review = snippet::Review {
            path: profile.path(),
            prepared: Arc::new(
                Prepared::capture(&library, script, "password=fixture-value").unwrap(),
            ),
        };
        let group = workspace.terminal_agents.swarms.get_mut(&id).unwrap();
        group.round = 2;
        group.preparing = true;
        let task = workspace.swarm_action(id, Message::SnippetPrepared(2, Ok(review)));
        drain(&mut workspace, task);
        let group = &workspace.terminal_agents.swarms[&id];
        let row = &group.queue.as_ref().unwrap().rows()[0];
        assert_eq!(row.status, Status::Blocked);
        assert!(row.detail.contains("requires masking"));
        assert!(row.transcript.is_empty());
    }
}

#[test]
fn swarm_workflow_sends_only_the_reviewed_input_and_discards_stale_panel_results() {
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, listener, request,
    };
    use std::io::Write as _;

    let _exclusive = exclusive_run();
    let (peer, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Workflow fixture","endpoint":peer.local_addr().unwrap().to_string(),"model":"test"}],
        "default":1,"last_id":1
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let _ = workspace.swarm_action(
        id,
        Message::Goal("Inspect services password=fixture-secret".into()),
    );
    let _ = workspace.swarm_action(id, Message::ShowWorkflow);
    let old = workspace.terminal_agents.swarms[&id].workflow_generation;
    let _ = workspace.swarm_action(id, Message::CloseWorkflow);
    let _ = workspace.swarm_action(id, Message::ShowWorkflow);
    let generation = workspace.terminal_agents.swarms[&id].workflow_generation;
    assert_ne!(old, generation);
    let send = |workspace: &mut Workspace, message| {
        workspace.swarm_action(id, Message::Workflow(generation, message))
    };
    let _ = send(
        &mut workspace,
        workflow::Message::HostSource(workflow::HostSource::Manual),
    );
    let _ = send(
        &mut workspace,
        workflow::Message::Hosts(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("operator@workflow.example.test:2222".into()),
        ))),
    );
    let _ = send(&mut workspace, workflow::Message::Review);
    let workflow = workspace.terminal_agents.swarms[&id]
        .workflow
        .as_ref()
        .unwrap();
    let (_, reviewed, header) = workflow.reviewed(1).unwrap();
    assert!(reviewed.contains("password=***"));
    assert!(!reviewed.contains("fixture-secret"));
    let reviewed = reviewed.to_owned();
    let header = header.text().unwrap().to_owned();
    assert!(header.contains("# Source:  Generated by kortty AI (Workflow fixture)"));
    assert!(header.contains("# Request: Inspect services password=***"));
    assert!(!header.contains("fixture-secret"));
    let stale = workspace.swarm_action(id, Message::Workflow(old, workflow::Message::Generate(1)));
    assert!(iced_runtime::task::into_stream(stale).is_none());
    assert!(
        workspace.terminal_agents.swarms[&id]
            .workflow
            .as_ref()
            .unwrap()
            .reviewed(1)
            .is_some()
    );

    let server = std::thread::spawn(move || {
        let mut socket = accept(&peer);
        let body = request(&mut socket);
        assert!(body.get("response_format").is_none());
        let input = body["messages"][1]["content"].as_str().unwrap();
        assert_eq!(input, reviewed);
        assert!(input.contains("operator") && input.contains("2222"));
        assert!(input.contains("password=***") && !input.contains("fixture-secret"));
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Execute nothing"));
        assert!(system.contains("Omit any metadata header"));
        assert!(system.contains("brief functional description"));
        let request = body.to_string();
        assert!(!request.contains("Workflow fixture"));
        assert!(!request.contains("# Author:") && !request.contains("# Created:"));
        let body =
            serde_json::json!({"choices":[{"message":{"content":"```bash\r\nprintf ready\r\n```"}}]}).to_string();
        socket
            .get_mut()
            .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
            .unwrap();
    });
    let generate = send(&mut workspace, workflow::Message::Generate(1));
    let stale = workspace.swarm_action(
        id,
        Message::Workflow(
            old,
            workflow::Message::Reply(1, Arc::new(Err("obsolete".into()))),
        ),
    );
    assert!(iced_runtime::task::into_stream(stale).is_none());
    assert!(
        workspace.terminal_agents.swarms[&id]
            .workflow
            .as_ref()
            .unwrap()
            .running()
    );
    drain(&mut workspace, generate);
    server.join().unwrap();
    let group = &workspace.terminal_agents.swarms[&id];
    let draft = group.workflow.as_ref().unwrap().draft(1).unwrap();
    assert_eq!(draft.name(), "inspect_services_password.sh");
    assert_eq!(
        draft.code(),
        format!("#!/usr/bin/env bash\n{header}\nprintf ready")
    );
    assert!(draft.goal().contains("password=***"));
    assert!(group.members.is_empty());
    assert!(group.queue.is_none());
}
