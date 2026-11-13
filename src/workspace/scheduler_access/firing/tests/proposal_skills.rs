use kortty_jobs::{RunId, SwarmReport, SwarmTargetReport};
use serde_json::Value;

use super::*;
use crate::ai_manager::recorded_skill_uses_for_tests;
use crate::job_executor::agent::advisory;

const PROFILE_ROOT: &str = "KORTTY_SCHEDULER_PROPOSAL_SKILLS_TEST_ROOT";
const TEST_NAME: &str = "workspace::scheduler_access::firing::tests::proposal_skills::scheduler_proposal_skills_require_separate_confirmations_and_current_pins";
const FIRING_PROFILE_ROOT: &str = "KORTTY_SCHEDULER_FIRING_SKILLS_TEST_ROOT";
const FIRING_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::proposal_skills::standing_agent_classifies_once_for_two_targets_and_reuses_the_reviewed_receipts";
const ADVISORY_PROFILE_ROOT: &str = "KORTTY_SCHEDULER_ADVISORY_SKILLS_TEST_ROOT";
const ADVISORY_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::proposal_skills::standing_read_only_swarm_classifies_and_saves_isolated_advisory_requests";
const LOCAL_GUIDANCE: &str = "Read the disk inventory before proposing a command.";
const CLASSIFIED_GUIDANCE: &str = "Use the reviewed recovery checklist.";
const PINNED_GUIDANCE: &str = "Keep this connection on the audited filesystem.";
const CHAT_GUIDANCE: &str = "Chat fixture body";
const SECRET: &str = "scheduler-proposal-fixture-token";
const COMPARISON_RESULT: &str = "| Target | Result | Errors |\n| --- | --- | --- |\n| First | Healthy | - |\n| Second | *** | - |";

fn quota_config(model: &TcpListener) -> Configuration {
    serde_json::from_value(json!({
        "profiles": {"profiles": [{
            "id": 1, "name": "Local fixture", "model": "fixture:local",
            "endpoint": model.local_addr().unwrap().to_string(), "max_output_tokens": 8192,
            "usage_budget": {"token_limit": 9000}
        }], "default": 1, "last_id": 1},
        "skills": {"last_id": 54, "skills": {
            "51": {"name": "Disk", "tags": ["disk"], "target": "AGENT", "enabled": true, "content": LOCAL_GUIDANCE},
            "52": {"name": "Recovery", "target": "BOTH", "enabled": true, "content": CLASSIFIED_GUIDANCE},
            "53": {"name": "Chat only", "target": "CHAT", "enabled": true, "content": CHAT_GUIDANCE},
            "54": {"name": "Audited connection", "target": "CONNECTION", "enabled": true, "content": PINNED_GUIDANCE}
        }}
    })).unwrap()
}

fn finish(workspace: &mut Workspace, task: Task<Message>) {
    if let Some(mut actions) = iced_runtime::task::into_stream(task) {
        while let Some(action) = block_on(actions.next()) {
            if let Action::Output(Message::Scheduler(id, event)) = action {
                let next = workspace.update_scheduler(id, event);
                finish(workspace, next);
            }
        }
    }
}

fn send(workspace: &mut Workspace, id: window::Id, event: SchedulerMessage) {
    let task = workspace.update_scheduler(id, event);
    finish(workspace, task);
}

fn preview(workspace: &Workspace, id: window::Id) -> String {
    workspace.scheduler_views[&id]
        .proposal_preview_ticket()
        .unwrap()
        .into()
}

fn model_reply(socket: &mut std::io::BufReader<std::net::TcpStream>, content: &str) {
    let body = json!({
        "choices": [{"message": {"content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 9, "completion_tokens": 4}
    })
    .to_string();
    socket
        .get_mut()
        .write_all(test_support::fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
}

fn approve_swarm_comparison(workspace: &mut Workspace, id: window::Id, main: &str) -> String {
    let task = workspace.update_scheduler(id, SchedulerMessage::ReviewSwarmComparison(main.into()));
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::Scheduler(
        window,
        SchedulerMessage::SwarmComparisonQuotaFinished(ticket, forecast),
    ))) = block_on(actions.next())
    else {
        panic!("the reviewed comparison must produce a quota forecast");
    };
    assert_eq!(window, id);
    assert!(
        forecast.contains("26 of 9000 tokens reported"),
        "{forecast}"
    );
    assert!(forecast.contains("up to 2048 output tokens"), "{forecast}");
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    send(
        workspace,
        id,
        SchedulerMessage::ApproveSwarmComparison(ticket.clone()),
    );
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    send(
        workspace,
        id,
        SchedulerMessage::SwarmComparisonQuotaFinished(ticket.clone(), forecast),
    );
    send(
        workspace,
        id,
        SchedulerMessage::ApproveSwarmComparison(ticket),
    );
    assert!(block_on(actions.next()).is_none());
    let approved = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .proposal_authority(&workspace.scheduler_views[&id])
        .unwrap()
        .unwrap();
    assert!(approved.comparison.is_some());
    let current = preview(workspace, id);
    assert_ne!(current, main);
    current
}

fn run_swarm_comparison(
    workspace: &mut Workspace,
    id: window::Id,
    model: &TcpListener,
    targets: [u64; 2],
    cancel: bool,
) -> RunId {
    let request = workspace
        .scheduler
        .as_mut()
        .unwrap()
        .update_view(
            workspace.scheduler_views.get_mut(&id).unwrap(),
            SchedulerMessage::Run(false),
        )
        .pop()
        .expect("the scheduler must issue a live run request");
    let run = request.id;
    workspace.scheduler_swarm_comparisons.remember(&request);
    let report = SwarmReport {
        question: "Inspect disk usage. token=***".into(),
        targets: targets
            .into_iter()
            .zip(["Healthy disk", "Masked token=***"])
            .map(|(target, detail)| SwarmTargetReport {
                target,
                label: format!("Profile {target}"),
                outcome: RunOutcome::Success,
                detail: detail.into(),
            })
            .collect(),
        comparison: None,
    };
    let journal_root = profile::root().unwrap().join("comparison-journals");
    workspace.window_settings.session_journal.directory = journal_root.to_str().unwrap().into();
    let journals = crate::workspace::journal_automation::route_tests::capture_job_journals(
        workspace,
        &request.job,
        "AI_SWARM",
        &["North", "South"],
    );
    let journal_ids = journals
        .iter()
        .map(|artifact| artifact.source.id().to_owned())
        .collect::<Vec<_>>();
    let task = workspace.complete_scheduler_run(
        crate::job_executor::Completion {
            id: run,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            swarm_report: Some(Box::new(report)),
        },
        journals,
    );
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledSwarmComparison(permit))) = block_on(actions.next())
    else {
        panic!("the completed Swarm must wait for the model dispatch permit");
    };
    assert!(
        workspace
            .scheduler
            .as_ref()
            .unwrap()
            .active_swarm_comparison_approved(&request)
    );
    assert_no_connection(model);
    if cancel {
        assert!(
            workspace
                .scheduler
                .as_ref()
                .unwrap()
                .cancel_menu_run(run.value())
        );
    }
    let released = workspace.update_scheduled_swarm_comparison(permit);
    finish(workspace, released);
    if !cancel {
        let mut socket = test_support::accept(model);
        let wire = test_support::request(&mut socket);
        assert_eq!(wire["max_tokens"], 2048);
        assert!(
            wire["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("last column header must be exactly Errors")
        );
        let input: Value =
            serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({
                "question":"Inspect disk usage. token=***",
                "targets":[
                    {"target":targets[0],"label":format!("Profile {}", targets[0]),"status":"Completed","detail":"Healthy disk"},
                    {"target":targets[1],"label":format!("Profile {}", targets[1]),"status":"Completed","detail":"Masked token=***"}
                ]
            })
        );
        assert!(!wire.to_string().contains(SECRET));
        assert!(!wire.to_string().contains(PASSWORD));
        model_reply(&mut socket, &COMPARISON_RESULT.replace("***", PASSWORD));
    }
    let Some(Action::Output(Message::ScheduledSwarmComparison(done))) = block_on(actions.next())
    else {
        panic!("the comparison worker must return its result");
    };
    let finished = workspace.update_scheduled_swarm_comparison(done);
    finish(workspace, finished);
    assert!(block_on(actions.next()).is_none());
    assert_no_connection(model);
    let scheduler = workspace.scheduler.as_mut().unwrap();
    let mut journal_view = scheduler.new_view();
    scheduler.update_view(
        &mut journal_view,
        SchedulerMessage::Page(crate::scheduler_window::Page::Journal),
    );
    scheduler.update_view(&mut journal_view, SchedulerMessage::SelectRecord(run));
    assert_eq!(
        scheduler.selected_session_journals(&journal_view, run),
        Some((&request.job, journal_ids.as_slice()))
    );
    run
}

fn two_saved_targets(ssh: &[TcpListener; 2]) -> ([ConnectionRef; 2], VaultWindow) {
    let (first, vault) = saved_target(ssh[0].local_addr().unwrap().port());
    let mut connections = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let mut saved = connections.get(first.id).unwrap().clone();
    saved.terminal.ai_skills = vec![54];
    connections.save(saved.clone()).unwrap();
    let second_port = ssh[1].local_addr().unwrap().port();
    saved.id = 0;
    saved.name = "Second scheduled fixture server".into();
    saved.login.endpoint.port = second_port;
    saved.terminal.ai_skills = vec![53];
    let second_id = connections.save(saved).unwrap();
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo",
    )
    .unwrap();
    crate::ssh_connection::trust_store()
        .unwrap()
        .remember(&Endpoint::new("127.0.0.1", second_port).unwrap(), &key)
        .unwrap();
    let second = ConnectionRef::from(connections.get(second_id).unwrap());
    ([first, second], vault)
}

#[test]
fn scheduler_proposal_skills_require_separate_confirmations_and_current_pins() {
    let Some(root) = std::env::var_os(PROFILE_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let root = scratch.0.canonicalize().unwrap().join("proposal-profile");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(PROFILE_ROOT, root)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    profile::set_runtime_root(root.clone());
    assert_eq!(profile::root().unwrap(), root);
    let (model, _) = test_support::listener();
    let ssh = TcpListener::bind("127.0.0.1:0").unwrap();
    ssh.set_nonblocking(true).unwrap();
    quota_config(&model)
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let (reference, vault) = saved_target(ssh.local_addr().unwrap().port());
    let mut connections = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let mut saved = connections.get(reference.id).unwrap().clone();
    saved.terminal.ai_skills = vec![54];
    connections.save(saved).unwrap();

    let jobs_path = profile::jobs_path().unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "working_directory": "/srv",
        "agent": {"kind":"agent", "probe":"proposed", "profile":1,
            "goal": format!("Inspect disk usage. token={SECRET}")}
    }))
    .unwrap();
    let job = Job::new(
        JobId::new("skill-proposal").unwrap(),
        "Review proposal skills",
        action,
    )
    .unwrap();
    SchedulerStore::open(&jobs_path)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs_path).unwrap();
    scheduler.set_execution_allowed(true);
    let view = scheduler.new_view();
    let id = window::Id::unique();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    send(&mut workspace, id, SchedulerMessage::Page(Page::Action));
    send(&mut workspace, id, SchedulerMessage::ReviewAgent);
    let initial = preview(&workspace, id);
    let authority = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .proposal_authority(&workspace.scheduler_views[&id])
        .unwrap()
        .unwrap();
    assert!(authority.prompt.messages()[0].contains(LOCAL_GUIDANCE));
    assert!(authority.prompt.messages()[0].contains(PINNED_GUIDANCE));
    assert!(!authority.prompt.messages()[1].contains(SECRET));
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    send(
        &mut workspace,
        id,
        SchedulerMessage::ReviewProposalSkills(initial.clone()),
    );
    send(
        &mut workspace,
        id,
        SchedulerMessage::RequestAgentProposal(initial.clone()),
    );
    assert_no_connection(&model);
    let classification = workspace.update_scheduler(
        id,
        SchedulerMessage::ClassifyProposalSkills(initial.clone()),
    );
    let mut socket = test_support::accept(&model);
    let request = test_support::request(&mut socket);
    assert_eq!(request["max_tokens"], 128);
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let payload: Value = serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(
        payload["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| skill["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["51", "52"]
    );
    assert!(payload["requestContext"].get("terminalSelection").is_none());
    let encoded = payload.to_string();
    for private in [SECRET, LOCAL_GUIDANCE, CLASSIFIED_GUIDANCE, PINNED_GUIDANCE] {
        assert!(!encoded.contains(private));
    }
    model_reply(&mut socket, r#"{"skillIds":["52","54"]}"#);
    drop(socket);
    finish(&mut workspace, classification);
    let current = preview(&workspace, id);
    assert_ne!(initial, current);
    let authority = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .proposal_authority(&workspace.scheduler_views[&id])
        .unwrap()
        .unwrap();
    let [system, input] = authority.prompt.messages().map(str::to_owned);
    assert!(system.contains(CLASSIFIED_GUIDANCE));
    assert!(system.contains(PINNED_GUIDANCE));
    assert!(!system.contains(LOCAL_GUIDANCE));
    assert_eq!(recorded_skill_uses_for_tests(&root), (0, 0));
    send(
        &mut workspace,
        id,
        SchedulerMessage::RequestAgentProposal(initial),
    );
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    let proposal = workspace.update_scheduler(id, SchedulerMessage::RequestAgentProposal(current));
    let mut socket = test_support::accept(&model);
    let request = test_support::request(&mut socket);
    assert_eq!(request["max_tokens"], 4096);
    assert_eq!(request["messages"][0]["content"], system);
    assert_eq!(request["messages"][1]["content"], input);
    model_reply(&mut socket, &json!({"commands": [{"target": reference.id, "command": "pwd -P", "purpose": "Check the working directory"}]}).to_string());
    drop(socket);
    finish(&mut workspace, proposal);
    assert_eq!(
        workspace.scheduler_views[&id]
            .reviewed_proposal_items()
            .unwrap()[0]
            .command,
        "pwd -P"
    );
    assert_eq!(recorded_skill_uses_for_tests(&root), (1, 1));
    let commands = workspace.scheduler_views[&id]
        .proposal_command_ticket()
        .unwrap()
        .to_owned();
    send(&mut workspace, id, SchedulerMessage::ApproveAgent);
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveProposalCommands("obsolete".into()),
    );
    assert_eq!(
        workspace.scheduler_views[&id].proposal_command_ticket(),
        Some(commands.as_str())
    );
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveProposalCommands(commands),
    );
    assert!(workspace.scheduler_views[&id].dirty());
    assert!(
        workspace.scheduler_views[&id]
            .proposal_command_ticket()
            .is_none()
    );
    assert_no_connection(&ssh);
    let usage = local_ai::UsageAccount::new(
        root.clone(),
        1,
        local_ai::UsageBudget {
            token_limit: Some(9000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(usage.snapshot().unwrap().counts.requests, 2);

    send(&mut workspace, id, SchedulerMessage::ReviewAgent);
    let pending = preview(&workspace, id);
    send(
        &mut workspace,
        id,
        SchedulerMessage::ReviewProposalSkills(pending.clone()),
    );
    let mut saved = connections.get(reference.id).unwrap().clone();
    saved.terminal.ai_skills.clear();
    connections.save(saved).unwrap();
    send(
        &mut workspace,
        id,
        SchedulerMessage::ClassifyProposalSkills(pending),
    );
    assert!(
        workspace.scheduler_views[&id]
            .proposal_preview_ticket()
            .is_none()
    );
    assert_eq!(usage.snapshot().unwrap().counts.requests, 2);
    assert_eq!(recorded_skill_uses_for_tests(&root), (1, 1));
    assert_no_connection(&model);
    assert_no_connection(&ssh);
}

#[test]
fn standing_agent_classifies_once_for_two_targets_and_reuses_the_reviewed_receipts() {
    let Some(root) = std::env::var_os(FIRING_PROFILE_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let root = scratch.0.canonicalize().unwrap().join("firing-profile");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", FIRING_TEST_NAME, "--nocapture"])
            .env(FIRING_PROFILE_ROOT, root)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    profile::set_runtime_root(root.clone());
    assert_eq!(profile::root().unwrap(), root);
    let (model, _) = test_support::listener();
    let ssh = [(); 2].map(|()| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    });
    quota_config(&model)
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let ([first, second], vault) = two_saved_targets(&ssh);
    let targets = [first.id, second.id];
    let goal = format!("Inspect disk usage. token={SECRET}");
    let action: SshCommand = serde_json::from_value(json!({
        "connection": first,
        "additional_connections": [second],
        "working_directory": "/srv",
        "agent": {"kind":"agent", "probe":"per_firing", "profile":1, "goal":goal}
    }))
    .unwrap();
    let job_id = JobId::new("standing-agent-skills").unwrap();
    let job = Job::new(job_id.clone(), "Review two Agent targets", action).unwrap();
    let jobs_path = profile::jobs_path().unwrap();
    SchedulerStore::open(&jobs_path)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs_path.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let view = scheduler.new_view();
    let id = window::Id::unique();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    send(&mut workspace, id, SchedulerMessage::Page(Page::Action));
    send(&mut workspace, id, SchedulerMessage::ReviewAgent);
    let initial = preview(&workspace, id);
    let view = &workspace.scheduler_views[&id];
    assert_eq!(view.firing_review_ticket(), Some(initial.as_str()));
    let initial_forecast = view.proposal_quota_label().unwrap().to_owned();
    assert!(initial_forecast.contains("up to 8192 output tokens"));
    assert!(initial_forecast.contains("0 of 9000 tokens reported"));
    send(
        &mut workspace,
        id,
        SchedulerMessage::ReviewProposalSkills(initial.clone()),
    );
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveFiringRequest(initial.clone()),
    );
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    assert_no_connection(&model);

    let classification = workspace.update_scheduler(
        id,
        SchedulerMessage::ClassifyProposalSkills(initial.clone()),
    );
    let mut socket = test_support::accept(&model);
    let request = test_support::request(&mut socket);
    assert_eq!(request["max_tokens"], 128);
    let payload: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["target"], "AGENT");
    assert_eq!(
        payload["requestContext"],
        json!({"question":"Inspect disk usage. token=***"})
    );
    let candidate_ids: Vec<_> = payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["id"].as_str().unwrap())
        .collect();
    assert_eq!(candidate_ids, ["51", "52"]);
    let encoded = request.to_string();
    for private in [
        SECRET,
        LOCAL_GUIDANCE,
        CLASSIFIED_GUIDANCE,
        PINNED_GUIDANCE,
        CHAT_GUIDANCE,
    ] {
        assert!(!encoded.contains(private));
    }
    model_reply(&mut socket, r#"{"skillIds":["52","54","53"]}"#);
    drop(socket);
    finish(&mut workspace, classification);
    let current = preview(&workspace, id);
    assert_ne!(current, initial);
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveFiringRequest(initial),
    );
    let view = &workspace.scheduler_views[&id];
    assert_eq!(view.firing_review_ticket(), Some(current.as_str()));
    let forecast = view.proposal_quota_label().unwrap().to_owned();
    assert!(forecast.contains("13 of 9000 tokens reported"));
    let authority = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .proposal_authority(view)
        .unwrap()
        .unwrap();
    assert!(authority.standing && !authority.advisory);
    assert!(
        authority
            .targets
            .iter()
            .map(|target| target.reference.id)
            .eq(targets)
    );
    let prompts = authority.prompts.to_vec();
    assert_eq!(prompts.len(), 2);
    for (prompt, (pin, other_pin)) in prompts.iter().zip([
        (PINNED_GUIDANCE, CHAT_GUIDANCE),
        (CHAT_GUIDANCE, PINNED_GUIDANCE),
    ]) {
        assert_eq!(prompt.firing_skill_ids(), [52]);
        let guidance = prompt.firing_skill_guidance().unwrap();
        assert!(guidance.contains(CLASSIFIED_GUIDANCE) && guidance.contains(pin));
        assert!(!guidance.contains(other_pin) && !guidance.contains(LOCAL_GUIDANCE));
    }
    assert_no_connection(&model);
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveFiringRequest(current),
    );
    send(&mut workspace, id, SchedulerMessage::Save);
    assert!(!workspace.scheduler_views[&id].dirty());
    assert_eq!(recorded_skill_uses_for_tests(&root), (0, 0));
    drop(workspace.scheduler.take());
    {
        let store = SchedulerStore::open(&jobs_path).unwrap();
        let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
        let approval = scheduler
            .graph()
            .job(&job_id)
            .unwrap()
            .action()
            .agent
            .as_ref()
            .unwrap()
            .approval
            .as_ref()
            .unwrap();
        assert_eq!(approval.firing_skill_ids(), [52]);
        assert_eq!(
            serde_json::to_value(approval).unwrap()["generation"]["version"],
            "firing-skills-v2"
        );
        for (index, prompt) in prompts.iter().enumerate() {
            assert!(approval.generation_matches_for(index, prompt));
            assert_eq!(
                approval.firing_skill_guidance_for(index),
                prompt.firing_skill_guidance()
            );
        }
    }

    let mut scheduler = SchedulerWindow::open(jobs_path).unwrap();
    scheduler.set_execution_allowed(true);
    workspace.scheduler_views.insert(id, scheduler.new_view());
    workspace.scheduler = Some(scheduler);
    send(&mut workspace, id, SchedulerMessage::Page(Page::Journal));
    let mut next = workspace.update_scheduler(id, SchedulerMessage::Run(false));
    let mut estimated_input = 0;
    for (index, (target, prompt)) in targets.into_iter().zip(prompts).enumerate() {
        let mut actions = iced_runtime::task::into_stream(next).unwrap();
        let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
            ticket,
            permit,
        ))))) = block_on(actions.next())
        else {
            panic!("the model must request dispatch permission for target {target}");
        };
        assert_eq!(ticket.target_index(), index);
        assert_no_connection(&model);
        let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
        let mut socket = test_support::accept(&model);
        let request = test_support::request(&mut socket);
        assert_eq!(request["max_tokens"], 4096);
        let system = request["messages"][0]["content"].as_str().unwrap();
        let input = request["messages"][1]["content"].as_str().unwrap();
        assert!(system.ends_with(prompt.firing_skill_guidance().unwrap()));
        assert_eq!(
            serde_json::from_str::<Value>(input).unwrap(),
            json!({
                "question":"Inspect disk usage. token=***", "working_directory":"/srv", "targets":[target]
            })
        );
        assert!(!request.to_string().contains(SECRET));
        estimated_input += crate::workspace::ai_quota::estimate_input([system, input]);
        model_reply(&mut socket, r#"{"state":"done"}"#);
        drop(socket);
        let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
            completed,
            result,
        ))))) = block_on(actions.next())
        else {
            panic!("the model must return a result for target {target}");
        };
        assert_eq!(completed, ticket);
        assert!(matches!(&result, Ok(Decision::Done {})));
        next = workspace.update_firing(Event(Progress::Generated(completed, result)));
        assert!(block_on(actions.next()).is_none());
    }
    finish(&mut workspace, next);
    assert_eq!(recorded_skill_uses_for_tests(&root), (2, 2));
    assert!(forecast.contains(&format!(
        "about {estimated_input} input tokens and up to 8192 output tokens"
    )));
    assert!(workspace.scheduler_agent_sequences.is_empty());
    assert_no_connection(&model);
    for listener in &ssh {
        assert_no_connection(listener);
    }
}

#[test]
fn standing_read_only_swarm_classifies_and_saves_isolated_advisory_requests() {
    let Some(root) = std::env::var_os(ADVISORY_PROFILE_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let root = scratch.0.canonicalize().unwrap().join("advisory-profile");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ADVISORY_TEST_NAME, "--nocapture"])
            .env(ADVISORY_PROFILE_ROOT, root)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    profile::set_runtime_root(root.clone());
    let (model, _) = test_support::listener();
    let ssh = [(); 2].map(|()| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    });
    quota_config(&model)
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let ([first, second], vault) = two_saved_targets(&ssh);
    let targets = [first.id, second.id];
    let action: SshCommand = serde_json::from_value(json!({
        "connection": first,
        "additional_connections": [second],
        "working_directory": "/srv",
        "agent": {"kind":"swarm", "probe":"swarm_read_only", "profile":1,
            "goal": format!("Inspect disk usage. token={SECRET}")}
    }))
    .unwrap();
    let job_id = JobId::new("standing-advisory-skills").unwrap();
    let job = Job::new(job_id.clone(), "Review two Advisory targets", action).unwrap();
    let jobs = profile::jobs_path().unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let view = scheduler.new_view();
    let id = window::Id::unique();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    send(&mut workspace, id, SchedulerMessage::Page(Page::Action));
    send(&mut workspace, id, SchedulerMessage::ReviewAgent);

    let stale = preview(&workspace, id);
    send(
        &mut workspace,
        id,
        SchedulerMessage::ReviewProposalSkills(stale.clone()),
    );
    let mut connections = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let original = connections.get(targets[1]).unwrap().clone();
    let mut changed = original.clone();
    changed.terminal.ai_skills.clear();
    connections.save(changed).unwrap();
    send(
        &mut workspace,
        id,
        SchedulerMessage::ClassifyProposalSkills(stale),
    );
    assert!(
        workspace.scheduler_views[&id]
            .proposal_preview_ticket()
            .is_none()
    );
    assert_no_connection(&model);
    connections.save(original).unwrap();
    send(&mut workspace, id, SchedulerMessage::ReviewAgent);

    for (reply, valid) in [
        (r#"{"skillIds":["54","53"]}"#, false),
        (r#"{"skillIds":["52","54","53"]}"#, true),
    ] {
        let ticket = preview(&workspace, id);
        send(
            &mut workspace,
            id,
            SchedulerMessage::ReviewProposalSkills(ticket.clone()),
        );
        send(
            &mut workspace,
            id,
            SchedulerMessage::ApproveFiringRequest(ticket.clone()),
        );
        assert!(
            workspace.scheduler_views[&id]
                .firing_review_ticket()
                .is_none()
        );
        assert_no_connection(&model);
        let task = workspace
            .update_scheduler(id, SchedulerMessage::ClassifyProposalSkills(ticket.clone()));
        let mut socket = test_support::accept(&model);
        let wire = test_support::request(&mut socket);
        assert_eq!(wire["max_tokens"], 128);
        let input = wire["messages"][1]["content"].as_str().unwrap();
        let classifier: Value = serde_json::from_str(input).unwrap();
        assert_eq!(classifier["target"], "AGENT");
        assert_eq!(
            classifier["requestContext"],
            json!({"question":"Inspect disk usage. token=***"})
        );
        let candidates: Vec<_> = classifier["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| skill["id"].as_str().unwrap())
            .collect();
        assert_eq!(candidates, ["51", "52"]);
        for private in [
            SECRET,
            PINNED_GUIDANCE,
            CHAT_GUIDANCE,
            LOCAL_GUIDANCE,
            CLASSIFIED_GUIDANCE,
        ] {
            assert!(!input.contains(private));
        }
        model_reply(&mut socket, reply);
        drop(socket);
        finish(&mut workspace, task);
        assert_ne!(preview(&workspace, id), ticket);
        send(
            &mut workspace,
            id,
            SchedulerMessage::ApproveFiringRequest(ticket),
        );
        if !valid {
            let authority = workspace
                .scheduler
                .as_ref()
                .unwrap()
                .proposal_authority(&workspace.scheduler_views[&id])
                .unwrap()
                .unwrap();
            assert_eq!(authority.prompts.len(), 2);
            for prompt in authority.prompts {
                assert!(prompt.firing_skill_ids().is_empty());
                assert!(prompt.firing_system().contains(LOCAL_GUIDANCE));
                assert!(!prompt.firing_system().contains(CLASSIFIED_GUIDANCE));
            }
            send(&mut workspace, id, SchedulerMessage::ReviewAgent);
        }
    }

    let current = preview(&workspace, id);
    let view = &workspace.scheduler_views[&id];
    assert_eq!(view.firing_review_ticket(), Some(current.as_str()));
    let authority = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .proposal_authority(view)
        .unwrap()
        .unwrap();
    assert!(authority.standing && authority.advisory);
    let prompts = authority.prompts.to_vec();
    assert_eq!(prompts.len(), 2);
    let mut estimated_input = 0;
    for (index, (pin, other)) in [
        (PINNED_GUIDANCE, CHAT_GUIDANCE),
        (CHAT_GUIDANCE, PINNED_GUIDANCE),
    ]
    .into_iter()
    .enumerate()
    {
        let prompt = &prompts[index];
        assert_eq!(prompt.firing_skill_ids(), [52]);
        let system = prompt.firing_system();
        assert!(system.contains("no execution capability"));
        assert!(system.contains(pin) && system.contains(CLASSIFIED_GUIDANCE));
        assert!(!system.contains(other) && !system.contains(LOCAL_GUIDANCE));
        let input = prompt.advisory_input(None).unwrap();
        assert!(!input.contains(SECRET));
        let parsed: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(parsed["question"], "Inspect disk usage. token=***");
        assert_eq!(parsed["working_directory"], "/srv");
        assert_eq!(
            parsed["diagnostics"],
            json!([{"target":targets[index], "output":null}])
        );
        estimated_input += crate::workspace::ai_quota::estimate_input([system, input.as_str()]);
    }
    let forecast = view.proposal_quota_label().unwrap();
    assert!(
        forecast.contains("26 of 9000 tokens reported"),
        "{forecast}"
    );
    assert!(
        forecast.contains(&format!(
            "about {estimated_input} input tokens and up to 4096 output tokens"
        )),
        "{forecast}"
    );
    let comparison_approved = approve_swarm_comparison(&mut workspace, id, &current);
    assert_no_connection(&model);
    for listener in &ssh {
        assert_no_connection(listener);
    }
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveFiringRequest(current),
    );
    assert_eq!(
        workspace.scheduler_views[&id].firing_review_ticket(),
        Some(comparison_approved.as_str())
    );
    send(
        &mut workspace,
        id,
        SchedulerMessage::ApproveFiringRequest(comparison_approved),
    );
    send(&mut workspace, id, SchedulerMessage::Save);
    assert!(!workspace.scheduler_views[&id].dirty());
    drop(workspace.scheduler.take());

    let store = SchedulerStore::open(&jobs).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let approval = scheduler
        .graph()
        .job(&job_id)
        .unwrap()
        .action()
        .agent
        .as_ref()
        .unwrap()
        .approval
        .as_ref()
        .unwrap();
    let grant = approval.advisory.as_ref().unwrap();
    assert!(grant.is_guided());
    assert_eq!(grant.skill_ids(), [52]);
    assert_eq!(
        grant.reviewed_pins().unwrap(),
        &[(targets[0], vec![54]), (targets[1], vec![53])]
    );
    for (index, prompt) in prompts.iter().enumerate() {
        assert!(grant.matches_prompt(index, prompt));
        assert!(!grant.matches_prompt(1 - index, prompt));
        assert_eq!(
            grant.skill_guidance_for(index),
            prompt.firing_skill_guidance()
        );
    }
    let receipt = serde_json::to_value(grant).unwrap();
    assert_eq!(receipt["guided"]["version"], "advisory-skills-v1");
    assert!(!receipt.to_string().contains(SECRET));
    assert_eq!(
        serde_json::to_value(approval.comparison.as_ref().unwrap()).unwrap()["version"],
        "swarm-comparison-v1"
    );
    drop(store);

    let mut reopened = SchedulerWindow::open(jobs.clone()).unwrap();
    reopened.set_execution_allowed(true);
    workspace.scheduler_views.insert(id, reopened.new_view());
    workspace.scheduler = Some(reopened);
    send(&mut workspace, id, SchedulerMessage::Page(Page::Journal));
    let complete = run_swarm_comparison(&mut workspace, id, &model, targets, false);
    let cancelled = run_swarm_comparison(&mut workspace, id, &model, targets, true);
    for listener in &ssh {
        assert_no_connection(listener);
    }
    drop(workspace.scheduler.take());
    let store = SchedulerStore::open(&jobs).unwrap();
    let report = store.journal_swarm_report(complete).unwrap();
    assert_eq!(report.comparison.as_deref(), Some(COMPARISON_RESULT));
    assert!(report.targets.iter().map(|row| row.target).eq(targets));
    assert_eq!(
        store.journal_record(complete).unwrap().outcome,
        RunOutcome::Success
    );
    assert_eq!(
        store.journal_record(cancelled).unwrap().outcome,
        RunOutcome::Cancelled
    );
    assert!(store.journal_swarm_report(cancelled).is_none());

    let config = Configuration::load_fixture_from(&root).unwrap();
    for (index, prompt) in prompts.iter().enumerate() {
        advisory::require_current_guidance(&config, &connections, grant, index, prompt).unwrap();
    }
    let mut stale = connections.get(targets[1]).unwrap().clone();
    stale.terminal.ai_skills.clear();
    connections.save(stale).unwrap();
    advisory::require_current_guidance(&config, &connections, grant, 0, &prompts[0]).unwrap();
    assert!(
        advisory::require_current_guidance(&config, &connections, grant, 1, &prompts[1]).is_err()
    );
}
