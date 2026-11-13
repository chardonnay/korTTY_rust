use std::io::{ErrorKind, Write};

use serde_json::{Value, json};

use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::terminal_agent::{Outcome, Status as CommandStatus};
use request_review::Message as Review;

fn agent_message(task: Task<Desktop>, member: window::Id) -> RunMessage {
    let mut stream = iced_runtime::task::into_stream(task).expect("agent update task");
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
            assert_eq!(id, member);
            return message;
        }
    }
    panic!("missing agent update");
}

#[test]
fn live_swarm_agent_reviews_a_new_model_request_after_each_approved_command() {
    let _exclusive = exclusive_run();
    let (model, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":model.local_addr().unwrap().to_string(),"model":"test"}],
        "default":1,"last_id":1
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    let _ = workspace.open_agent_swarm(owner);
    let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let key = review_source(&mut workspace, group, &root)[0];
    let _ = workspace.swarm_action(group, Message::Mode(SwarmMode::Agent));
    let _ = workspace.swarm_action(group, Message::Toggle(key, true));
    let _ = workspace.swarm_action(group, Message::Goal("Inspect this directory".into()));
    let task = workspace.swarm_action(group, Message::Start);
    drain(&mut workspace, task);
    let member = *workspace.terminal_agents.swarms[&group]
        .members
        .keys()
        .next()
        .unwrap();
    let action = |workspace: &mut Workspace, action| {
        workspace.swarm_action(group, Message::Member(member, action))
    };
    let no_request = || {
        assert!(matches!(model.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    };

    let mut stale = None;
    for (turn, approval, working, response) in [
        (
            1,
            Phase::ProbeApproval,
            Phase::Probe,
            r#"{"status":"run_commands","summary":"Inspect","userMessage":"Review the next command.","commands":[{"command":"echo reviewed-swarm","purpose":"Check the command lane","risk":"read_only"}]}"#,
        ),
        (
            2,
            Phase::Approval,
            Phase::Running,
            r#"{"status":"done","summary":"Finished","userMessage":"Reviewed command completed."}"#,
        ),
    ] {
        let run = &workspace.terminal_agents.runs[&member];
        assert!(run.phase == approval);
        assert!(run.model_review.is_none());
        let epoch = run.epoch;
        no_request();
        let directory = agent_message(action(&mut workspace, MemberAction::Approve(epoch)), member);
        assert!(matches!(directory, RunMessage::Directory(..)));
        // Exercise the real review transition without launching a fixture shell.
        drop(workspace.update_terminal_agent(member, directory));
        let run = &workspace.terminal_agents.runs[&member];
        assert!(run.phase == working);
        let result = RunMessage::Command(
            run.epoch,
            Arc::new(Ok(Outcome {
                status: CommandStatus::Exited(0),
                stdout: Default::default(),
                stderr: Default::default(),
            })),
        );
        let task = workspace.update_terminal_agent(member, result);
        drain(&mut workspace, task);
        let run = &workspace.terminal_agents.runs[&member];
        assert!(run.phase == Phase::ModelReview);
        assert_eq!(run.turns, turn - 1);
        let review = run.model_review.as_ref().unwrap();
        let system = review.system.clone();
        let input = review.input.clone();
        let epoch = run.epoch;
        let data: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(data["turn"], turn);
        assert_eq!(data["target"]["passwordSudo"], false);
        assert_eq!(data["target"]["passwordlessSudo"], false);
        if let Some(stale) = stale {
            assert_eq!(
                action(
                    &mut workspace,
                    MemberAction::ReviewModel(Review::Send(stale))
                )
                .units(),
                0
            );
            assert!(input.contains("echo reviewed-swarm"));
        }
        no_request();
        let pending = action(
            &mut workspace,
            MemberAction::ReviewModel(Review::Send(epoch)),
        );
        let mut socket = accept(&model);
        let wire = request(&mut socket);
        assert_eq!(wire["messages"][0]["content"], system);
        assert_eq!(wire["messages"][1]["content"], input);
        let response = json!({"choices":[{"message":{"content":response}}]}).to_string();
        socket
            .get_mut()
            .write_all(fixture_http_response("200 OK", "", &response).as_bytes())
            .unwrap();
        drop(socket);
        let completion = agent_message(pending, member);
        assert!(matches!(completion, RunMessage::Decision(..)));
        let task = workspace.update_terminal_agent(member, completion);
        drain(&mut workspace, task);
        stale = Some(epoch);
    }
    assert!(
        workspace.terminal_agents.swarms[&group]
            .queue
            .as_ref()
            .unwrap()
            .complete()
    );
    assert!(!workspace.terminal_agents.runs.contains_key(&member));
    no_request();
}
