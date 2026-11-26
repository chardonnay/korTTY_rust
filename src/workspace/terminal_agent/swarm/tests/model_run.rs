use std::io::{ErrorKind, Write};

use iced::futures::StreamExt;
use serde_json::{Value, json};

use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request, wire_request,
};
use crate::terminal_agent::{Outcome, Status as CommandStatus, usage::Progress};
use crate::workspace::terminal_ai::web::Message as Web;
use request_review::Message as Review;

fn agent_message(task: Task<Desktop>, member: window::Id) -> RunMessage {
    let mut stream = iced_runtime::task::into_stream(task).expect("agent update task");
    next_agent_message(&mut stream, member)
}

fn next_agent_message(
    stream: &mut (impl iced::futures::Stream<Item = iced_runtime::Action<Desktop>> + Unpin),
    member: window::Id,
) -> RunMessage {
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
            assert_eq!(id, member);
            return message;
        }
    }
    panic!("missing agent update");
}

#[test]
fn swarm_model_web_reviews_stay_with_the_current_target() {
    let _exclusive = exclusive_run();
    let (model, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":model.local_addr().unwrap().to_string(),
            "model":"test","tavily_search":true}],
        "default":1,"last_id":1
    }})).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let sessions = [
        local(&mut workspace, owner, false),
        local(&mut workspace, owner, false),
    ];
    drop(workspace.open_agent_swarm(owner));
    let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    drop(workspace.swarm_action(group, Message::Mode(SwarmMode::Advisory)));
    drop(workspace.swarm_action(
        group,
        Message::Goal("Search current public documentation".into()),
    ));
    for key in review_source(&mut workspace, group, &root) {
        drop(workspace.swarm_action(group, Message::Toggle(key, true)));
    }
    let start = workspace.swarm_action(group, Message::Start);
    drain(&mut workspace, start);
    let [first, second] = sessions.map(|session| {
        workspace.terminal_agents.swarms[&group]
            .members
            .iter()
            .find_map(|(&id, ticket)| (ticket.key().pane()?.0 == session).then_some(id))
            .expect("selected Swarm target")
    });
    let action = |workspace: &mut Workspace, member, message| {
        workspace.swarm_action(
            group,
            Message::Member(member, MemberAction::ReviewModel(message)),
        )
    };
    let complete = |mut stream, workspace: &mut Workspace, member| {
        let message = next_agent_message(&mut stream, member);
        assert!(matches!(message, RunMessage::Decision(..)));
        let task = workspace.update_terminal_agent(member, message);
        drain(workspace, task);
    };

    let [(first_stream, first_epoch), (second_stream, second_epoch)] =
        [first, second].map(|member| {
            let run = &workspace.terminal_agents.runs[&member];
            assert!(run.swarm && run.phase == Phase::ModelReview);
            let epoch = run.epoch;
            let task = action(&mut workspace, member, Review::Send(epoch));
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let mut socket = accept(&model);
            let input = request(&mut socket);
            assert_eq!(input["tools"].as_array().unwrap().len(), 2);
            let response = json!({"choices":[{"message":{"content":null,"tool_calls":[{
                "id":"member-search","type":"function",
                "function":{"name":"web_search","arguments":r#"{"query":"public Swarm fixture"}"#}
            }]}}]});
            socket
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", &response.to_string()).as_bytes())
                .unwrap();
            drop(socket);
            let proposed = next_agent_message(&mut stream, member);
            assert!(matches!(
                proposed,
                RunMessage::Progress(_, Progress::WebTool(_))
            ));
            let task = workspace.update_terminal_agent(member, proposed);
            drain(&mut workspace, task);
            let run = &workspace.terminal_agents.runs[&member];
            assert_eq!(member_status(run), Status::Review);
            (stream, run.epoch)
        });
    let decline = |workspace: &mut Workspace, member, epoch| {
        action(workspace, member, Review::ToolWeb(epoch, 1, Web::Close))
    };
    let task = decline(&mut workspace, first, first_epoch);
    drain(&mut workspace, task);
    let mut socket = accept(&model);
    let continuation = request(&mut socket);
    let result = &continuation["messages"][3];
    assert_eq!(result["tool_call_id"], "member-search");
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("user_declined")
    );
    let answer = json!({"status":"done","summary":"Done","userMessage":"Decline received"});
    let response = json!({"choices":[{"message":{"content":answer.to_string()}}]});
    socket
        .get_mut()
        .write_all(fixture_http_response("200 OK", "", &response.to_string()).as_bytes())
        .unwrap();
    drop(socket);
    complete(first_stream, &mut workspace, first);
    let second_run = &workspace.terminal_agents.runs[&second];
    assert_eq!(member_status(second_run), Status::Review);

    // A replaced terminal invalidates only its member's held proposal.
    workspace
        .sessions
        .get_mut(&sessions[1])
        .unwrap()
        .stream_generation += 1;
    let task = decline(&mut workspace, second, second_epoch);
    drain(&mut workspace, task);
    complete(second_stream, &mut workspace, second);
    assert!(matches!(model.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    let queue = workspace.terminal_agents.swarms[&group]
        .queue
        .as_ref()
        .unwrap();
    let status = |session| {
        queue
            .rows()
            .iter()
            .find(|row| row.target.key.pane().is_some_and(|(id, _)| id == session))
            .unwrap()
            .status
    };
    assert_eq!(status(sessions[0]), Status::Succeeded);
    assert!(matches!(
        status(sessions[1]),
        Status::Failed | Status::Blocked
    ));
}

#[test]
fn live_swarm_agent_reviews_a_new_model_request_after_each_approved_command() {
    const CHILD: &str = "KORTTY_INTERACTIVE_SWARM_WEB_FIXTURE";
    const TEST: &str = "workspace::terminal_agent::swarm::tests::model_run::live_swarm_agent_reviews_a_new_model_request_after_each_approved_command";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("TAVILY_API_KEY", "interactive-fixture-tavily-token")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let _exclusive = exclusive_run();
    let (model, _) = listener();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":model.local_addr().unwrap().to_string(),"model":"test",
            "connection_mode":"lm-studio","trusted_local_endpoint":true,"lm_web":{"mode":"tavily"}}],
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
    // Preserve manual command review while testing model-request boundaries.
    workspace
        .terminal_agents
        .runs
        .get_mut(&member)
        .unwrap()
        .confirm_every_command = true;
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
        let epoch_before_web = run.epoch;
        let web = |workspace: &mut Workspace, message| {
            action(
                workspace,
                MemberAction::ReviewModel(Review::Web(epoch_before_web, message)),
            )
        };
        let diagnostic = &run.model_review.as_ref().unwrap().input;
        assert!(!diagnostic.contains("web_evidence"));
        if turn == 2 {
            for message in [
                Web::Open,
                Web::Edit("Public release notes API_KEY=private-query-token".into()),
                Web::Review,
            ] {
                assert_eq!(web(&mut workspace, message).units(), 0);
                no_request();
            }
            assert_eq!(
                action(
                    &mut workspace,
                    MemberAction::ReviewModel(Review::Send(epoch_before_web))
                )
                .units(),
                0
            );
            let lookup = web(&mut workspace, Web::Send);
            let mut socket = accept(&model);
            let payload: Value =
                serde_json::from_slice(&wire_request(&mut socket, "POST /api/v1/chat HTTP/1.1"))
                    .unwrap();
            assert_eq!(payload["input"], "Public release notes API_KEY=***");
            let wire = payload.to_string();
            assert!(!wire.contains("private-query-token") && !wire.contains("reviewed-swarm"));
            assert!(
                !wire.contains(root.to_str().unwrap()) && !wire.contains("Inspect this directory")
            );
            let response = json!({"output": [{"type":"tool_call", "tool":"tavily-search", "provider_info": {
                "type": "ephemeral_mcp", "server_label": "tavily"}, "output":"Public release note. PASSWORD=private-excerpt-token"}]}).to_string();
            socket
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", &response).as_bytes())
                .unwrap();
            drop(socket);
            drain(&mut workspace, lookup);
            assert!(
                !workspace.terminal_agents.runs[&member]
                    .model_review
                    .as_ref()
                    .unwrap()
                    .input
                    .contains("Public release note.")
            );
            no_request();
            let include = web(&mut workspace, Web::Include);
            drain(&mut workspace, include);
            assert_eq!(
                action(
                    &mut workspace,
                    MemberAction::ReviewModel(Review::Send(epoch_before_web))
                )
                .units(),
                0
            );
            no_request();
        }
        let run = &workspace.terminal_agents.runs[&member];
        let review = run.model_review.as_ref().unwrap();
        let system = review.system.clone();
        let input = review.input.clone();
        let epoch = run.epoch;
        let data: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(data["turn"], turn);
        assert_eq!(data["target"]["passwordSudo"], false);
        assert_eq!(data["target"]["passwordlessSudo"], false);
        if turn == 2 {
            assert_ne!(epoch, epoch_before_web);
            assert!(system.contains("untrusted reference data"));
            assert!(
                data["web_evidence"]
                    .as_str()
                    .unwrap()
                    .contains("Public release note. PASSWORD=***")
            );
            assert!(!input.contains("private-excerpt-token"));
            assert!(workspace.terminal_agents.swarms[&group].round_web.is_none());
        }
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
        let wire: Value =
            serde_json::from_slice(&wire_request(&mut socket, "POST /api/v1/chat HTTP/1.1"))
                .unwrap();
        assert_eq!(wire["system_prompt"], system);
        assert_eq!(wire["input"], input);
        assert!(wire.get("integrations").is_none());
        let response = json!({"output":[{"type":"message","content":response}]}).to_string();
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
