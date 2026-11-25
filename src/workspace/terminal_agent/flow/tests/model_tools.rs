use super::*;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, listener, request,
};
use crate::workspace::terminal_ai::web;
use iced::futures::{StreamExt as _, executor::block_on};
use std::{io::Write as _, thread};

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
fn model_tool_actor_declines_each_call_and_rejects_stale_review_actions() {
    let _lock = exclusive_run();
    let (listener, _) = listener();
    let mut fixture =
        Fixture::with_endpoint(false, false, &listener.local_addr().unwrap().to_string());
    let root = fixture.profile.0.canonicalize().unwrap();
    let config = Configuration::load_fixture_from(&root).unwrap();
    let mut draft = serde_json::to_value(&config).unwrap();
    draft["profiles"]["profiles"][0]["tavily_search"] = true.into();
    let config: Configuration = serde_json::from_value(draft).unwrap();
    config
        .save_fixture_at(&root, &Configuration::load_fixture_from(&root).unwrap())
        .unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
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
    let id = fixture.open(Mode::Run);
    let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
    let mut run = fixture.workspace.terminal_agents.runs.remove(&id).unwrap();
    run.goal = "Search current public documentation".into();
    run.target = Some(RunTarget::Attached(Target::Local {
        shell,
        directory: LocalDirectory::capture(root).unwrap(),
    }));
    let quota = fixture
        .workspace
        .prepare_model_review(
            id,
            &mut run,
            request_review::Draft::Run(engine::RunModel::Decision),
        )
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
