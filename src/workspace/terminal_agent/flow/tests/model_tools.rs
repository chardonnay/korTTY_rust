use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::workspace::terminal_agent::planning::Message as PlanMessage;
use crate::workspace::terminal_ai::web;
use iced::futures::{StreamExt as _, executor::block_on};
use std::{io::Write as _, net::TcpListener, thread};

fn next(
    stream: &mut (impl iced::futures::Stream<Item = iced_runtime::Action<Desktop>> + Unpin),
    id: window::Id,
) -> Message {
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = action {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("The model worker did not emit its expected update");
}

fn response(message: serde_json::Value) -> String {
    fixture_http_response(
        "200 OK",
        "",
        &json!({"choices":[{"message":message}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})
        .to_string(),
    )
}

#[test]
fn ask_and_run_decline_each_model_tool_and_reject_stale_review_actions() {
    let _lock = exclusive_run();
    for (mode, always) in [(Mode::Ask, false), (Mode::Ask, true), (Mode::Run, true)] {
        assert_declined_model_tools(mode, always);
    }
}

fn web_fixture(listener: &TcpListener) -> Fixture {
    web_fixture_with_offer(listener, false)
}

fn web_fixture_with_offer(listener: &TcpListener, always: bool) -> Fixture {
    let mut fixture =
        Fixture::with_endpoint(false, false, &listener.local_addr().unwrap().to_string());
    let root = fixture.profile.0.canonicalize().unwrap();
    let config = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&config).unwrap();
    draft["profiles"]["profiles"][0]["tavily_search"] = true.into();
    if always {
        draft["general"]["agent_always_offer_web_tools"] = true.into();
    }
    let config: Configuration = serde_json::from_value(draft).unwrap();
    config
        .save_fixture_at(&root, &Configuration::load_fixture_from(&root).unwrap())
        .unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    fixture
}

fn assert_declined_model_tools(mode: Mode, always: bool) {
    let (listener, _) = listener();
    let mut fixture = web_fixture_with_offer(&listener, always);
    let root = fixture.profile.0.canonicalize().unwrap();
    let server = thread::spawn(move || {
        let mut first = accept(&listener);
        let initial = request(&mut first);
        assert_eq!(initial["tools"].as_array().unwrap().len(), 2);
        let calls: Vec<_> = (1..=2)
            .map(|id| {
                json!({"id":format!("actor-{id}"),"type":"function",
            "function":{"name":"web_search","arguments":r#"{"query":"public fixture query"}"#}})
            })
            .collect();
        first
            .get_mut()
            .write_all(response(json!({"content":null,"tool_calls":calls})).as_bytes())
            .unwrap();
        let mut final_request = accept(&listener);
        let wire = request(&mut final_request);
        for (index, id) in [(3, "actor-1"), (4, "actor-2")] {
            assert_eq!(wire["messages"][index]["tool_call_id"], id);
            assert!(
                wire["messages"][index]["content"]
                    .as_str()
                    .unwrap()
                    .contains("user_declined")
            );
        }
        let answer =
            json!({"status":"done","summary":"Done","userMessage":"Reviewed declines received"})
                .to_string();
        final_request
            .get_mut()
            .write_all(response(json!({"content":answer})).as_bytes())
            .unwrap();
    });
    let id = fixture.open(mode);
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    run.goal = if always {
        "Inspect the local build settings"
    } else {
        "Search current public documentation"
    }
    .into();
    let draft = if mode == Mode::Ask {
        request_review::Draft::Ask
    } else {
        let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
        run.target = Some(RunTarget::Attached(Target::Local {
            shell,
            directory: LocalDirectory::capture(root).unwrap(),
        }));
        request_review::Draft::Run(engine::RunModel::Decision)
    };
    let quota = fixture
        .workspace
        .prepare_model_review(id, &mut run, draft)
        .unwrap();
    fixture.workspace.terminal_agents.runs.insert(id, run);
    finish_agent_task(&mut fixture, quota);
    let epoch = fixture.run(id).epoch;
    let task = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    assert!(
        fixture.run(id).phase == Phase::Thinking,
        "{}",
        fixture.run(id).notice
    );
    let epoch = fixture.run(id).epoch;
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    for serial in 1..=2 {
        let message = next(&mut stream, id);
        assert!(matches!(
            message,
            Message::Progress(_, engine::usage::Progress::WebTool(_))
        ));
        no_task(fixture.send(id, message));
        assert!(
            fixture
                .run(id)
                .model_review
                .as_ref()
                .unwrap()
                .model_tool_active()
        );
        no_task(fixture.send(
            id,
            Message::ReviewModel(request_review::Message::ToolWeb(
                epoch,
                serial - 1,
                web::Message::Close,
            )),
        ));
        assert!(
            fixture
                .run(id)
                .model_review
                .as_ref()
                .unwrap()
                .model_tool_active()
        );
        if mode == Mode::Run && serial == 2 {
            no_task(fixture.send(id, Message::Pause));
            assert!(fixture.run(id).paused);
            no_task(fixture.send(
                id,
                Message::ReviewModel(request_review::Message::ToolWeb(
                    epoch,
                    serial,
                    web::Message::Close,
                )),
            ));
            assert!(
                fixture
                    .run(id)
                    .model_review
                    .as_ref()
                    .unwrap()
                    .model_tool_active(),
                "the final tool must stay pending until Resume"
            );
            no_task(fixture.send(id, Message::Pause));
            assert!(!fixture.run(id).paused);
        }
        no_task(fixture.send(
            id,
            Message::ReviewModel(request_review::Message::ToolWeb(
                epoch,
                serial,
                web::Message::Close,
            )),
        ));
        assert!(
            !fixture
                .run(id)
                .model_review
                .as_ref()
                .unwrap()
                .model_tool_active()
        );
    }
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = action {
            drop(fixture.send(actual, message));
        }
    }
    server.join().unwrap();
    assert!(!fixture.run(id).phase.working());
    assert!(
        fixture
            .run(id)
            .activity
            .iter()
            .any(|line| line.contains("Reviewed declines received"))
    );
}

#[test]
fn planning_web_reviews_a_tool_against_the_current_plan_route() {
    let _lock = exclusive_run();
    for route_changes in [false, true] {
        let (listener, _) = listener();
        let mut fixture = web_fixture(&listener);
        let id = fixture.open(Mode::Plan);
        no_task(fixture.send(id, Message::Goal("Search public documentation".into())));
        let start = fixture.send(id, Message::StartPlanWithoutProbe);
        finish_agent_task(&mut fixture, start);
        let epoch = fixture.run(id).epoch;
        let task = fixture.send(
            id,
            Message::ReviewModel(request_review::Message::Send(epoch)),
        );
        let mut socket = accept(&listener);
        assert_eq!(request(&mut socket)["tools"].as_array().unwrap().len(), 2);
        socket
            .get_mut()
            .write_all(
                response(json!({"tool_calls":[{
                    "id":"plan-search","type":"function","function":{
                        "name":"web_search","arguments":r#"{"query":"public planning fixture"}"#
                    }
                }]}))
                .as_bytes(),
            )
            .unwrap();
        drop(socket);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let progress = next(&mut stream, id);
        assert!(matches!(
            progress,
            Message::Planning(PlanMessage::Progress(
                _,
                engine::usage::Progress::WebTool(_)
            ))
        ));
        no_task(fixture.send(id, progress));
        assert!(
            fixture
                .run(id)
                .model_review
                .as_ref()
                .unwrap()
                .model_tool_active()
        );
        if route_changes {
            fixture
                .workspace
                .sessions
                .get_mut(&fixture.session)
                .unwrap()
                .local_security
                .as_mut()
                .unwrap()
                .working_directory = Some(fixture.profile.0.join("changed-route"));
        }
        let epoch = fixture.run(id).epoch;
        no_task(fixture.send(
            id,
            Message::ReviewModel(request_review::Message::ToolWeb(
                epoch,
                1,
                web::Message::Close,
            )),
        ));
        if !route_changes {
            let mut socket = accept(&listener);
            let wire = request(&mut socket);
            assert_eq!(wire["messages"][3]["tool_call_id"], "plan-search");
            assert!(
                wire["messages"][3]["content"]
                    .as_str()
                    .unwrap()
                    .contains("user_declined")
            );
            socket.get_mut().write_all(response(json!({"content":json!({
                "status":"questions","summary":"Clarify the plan","userMessage":"Choose a scope",
                "questions":[{"id":"scope","question":"Which scope?","options":["One","Two"],"allowCustomAnswer":true}]
            }).to_string()})).as_bytes()).unwrap();
        }
        let reply = next(&mut stream, id);
        drop(fixture.send(id, reply));
        let run = fixture.run(id);
        if route_changes {
            assert!(run.phase == Phase::Blocked && run.notice.contains("source route changed"));
        } else {
            assert!(run.phase == Phase::PlanQuestions && run.plan.is_none());
        }
        assert!(run.commands.is_empty());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn triage_reviewed_web_tool_leads_to_an_approach_without_starting_run() {
    let _lock = exclusive_run();
    let (listener, _) = listener();
    let mut fixture = web_fixture(&listener);
    let id = super::triage::complete_probe_with_goal(
        &mut fixture,
        "Search current public documentation",
    );
    assert_eq!(
        fixture.run(id).model_review.as_ref().unwrap().plan_stage(),
        Some(engine::planning::Stage::Triage)
    );
    let epoch = fixture.run(id).epoch;
    let task = fixture.send(
        id,
        Message::ReviewModel(request_review::Message::Send(epoch)),
    );
    let mut first = accept(&listener);
    assert_eq!(request(&mut first)["tools"].as_array().unwrap().len(), 2);
    first
        .get_mut()
        .write_all(
            response(json!({"tool_calls":[{
                "id":"triage-page","type":"function","function":{
                    "name":"web_extract","arguments":r#"{"url":"https://example.com/triage"}"#
                }
            }]}))
            .as_bytes(),
        )
        .unwrap();
    drop(first);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let progress = next(&mut stream, id);
    assert!(matches!(
        progress,
        Message::Progress(_, engine::usage::Progress::WebTool(_))
    ));
    no_task(fixture.send(id, progress));
    let epoch = fixture.run(id).epoch;
    no_task(fixture.send(
        id,
        Message::ReviewModel(request_review::Message::ToolWeb(
            epoch,
            1,
            web::Message::Close,
        )),
    ));
    let mut continuation = accept(&listener);
    let wire = request(&mut continuation);
    assert_eq!(wire["messages"][3]["tool_call_id"], "triage-page");
    assert!(
        wire["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("user_declined")
    );
    continuation
        .get_mut()
        .write_all(
            response(json!({"content":json!({
        "status":"triage","summary":"Compare approaches","userMessage":"Choose an approach",
        "complex":true,"reasons":["Multiple public sources"],"recommended":0,
        "options":[{"title":"Compare sources","summary":"Inspect source scope",
            "feasibility":"Sources are public","risks":[],"prerequisites":[],
            "steps":["Compare source coverage"],"alternatives":[]}]
    }).to_string()}))
            .as_bytes(),
        )
        .unwrap();
    let reply = next(&mut stream, id);
    assert!(matches!(reply, Message::Triage(..)));
    drop(fixture.send(id, reply));
    let run = fixture.run(id);
    assert!(run.phase == Phase::TriageChoice && run.turns == 0 && run.commands.is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
