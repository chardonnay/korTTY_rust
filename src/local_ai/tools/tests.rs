use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::local_ai::{Profile, ProfileOptions, RemoteProtocol, test_support};

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}})
}

fn response(content: Value, calls: Option<Value>, reason: &str) -> String {
    let mut message = json!({"role":"assistant","content":content});
    if let Some(calls) = calls {
        message["tool_calls"] = calls;
    }
    test_support::fixture_http_response(
        "200 OK",
        "",
        &json!({
            "choices":[{"finish_reason":reason,"message":message}],
            "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}
        })
        .to_string(),
    )
}

fn send(
    client: &Client,
    history: &[Message],
    offer: bool,
) -> (Result<Reply, Error>, Option<CompletionLimit>) {
    let (result, stop, _) = client.complete_agent_turn_before_guarded(
        "Reviewed system",
        "Reviewed local task",
        Some(Request { history, offer }),
        true,
        Instant::now() + Duration::from_secs(3),
        || Ok(()),
    );
    (result, stop)
}

#[test]
fn web_tool_turns_bind_calls_results_definitions_and_final_text_over_http() {
    let _exclusive = test_support::exclusive_run();
    let calls = json!([
        call("s1", "web_search", json!({"query":"reviewed public query"})),
        call(
            "p1",
            "web_extract",
            json!({"url":"https://example.com/document"})
        ),
    ]);
    let (profile, server) = test_support::start_test_double(vec![
        response(Value::Null, Some(calls), "tool_calls"),
        response(json!("reviewed final answer"), None, "stop"),
    ]);
    let client = Client::new(profile);
    let (first, stop) = send(&client, &[], true);
    assert_eq!(stop, None);
    let first = first.unwrap();
    let Reply::Calls {
        content,
        calls,
        omitted,
    } = &first
    else {
        panic!("expected tool requests")
    };
    assert_eq!(calls.len(), 2);
    assert_eq!(*omitted, 0);
    assert_eq!(client.take_usage().unwrap().total_tokens, Some(12));
    let history = [
        Message::Assistant {
            content: content.clone(),
            tool_calls: calls.clone(),
        },
        Message::Tool {
            tool_call_id: "s1".into(),
            content: "reviewed search excerpts".into(),
        },
        Message::Tool {
            tool_call_id: "p1".into(),
            content: "reviewed page evidence".into(),
        },
    ];
    let final_answer = send(&client, &history, false)
        .0
        .unwrap()
        .into_text()
        .unwrap();
    assert_eq!(final_answer, "reviewed final answer");
    assert_eq!(client.take_usage().unwrap().total_tokens, Some(12));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    let definitions = requests[0]["tools"].as_array().unwrap();
    assert_eq!(definitions.len(), 2);
    for (tool, name, argument) in [
        (&definitions[0], "web_search", "query"),
        (&definitions[1], "web_extract", "url"),
    ] {
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], name);
        assert_eq!(
            tool["function"]["parameters"]["properties"][argument]["type"],
            "string"
        );
        assert_eq!(
            tool["function"]["parameters"]["required"],
            json!([argument])
        );
    }
    assert_eq!(requests[0]["tool_choice"], "auto");
    assert_eq!(requests[0]["response_format"]["type"], "json_object");
    assert!(requests[1].get("tools").is_none());
    assert!(requests[1].get("tool_choice").is_none());
    assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 5);
    assert_eq!(requests[1]["messages"][2]["tool_calls"][0]["id"], "s1");
    assert_eq!(
        requests[1]["messages"][3],
        json!({"role":"tool","tool_call_id":"s1","content":"reviewed search excerpts"})
    );
    assert_eq!(requests[1]["messages"][4]["tool_call_id"], "p1");
    if let Some(path) = std::env::var_os("KORTTY_WEB_TOOL_TURN_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&json!({
            "requests":requests, "final_answer":final_answer,
            "requested_call_ids":["s1","p1"], "provider_dispatch":"none; reviewed fixture results supplied by test"
        })).unwrap()).unwrap();
    }
}

#[test]
fn web_tool_turns_bound_batches_and_leave_unknown_tools_for_explicit_failure_results() {
    let _exclusive = test_support::exclusive_run();
    let calls: Vec<_> = (0..5)
        .map(|i| call(&format!("c{i}"), "unknown_tool", json!({"query":"public"})))
        .collect();
    let (profile, server) = test_support::start_test_double(vec![response(
        Value::Null,
        Some(json!(calls)),
        "tool_calls",
    )]);
    let reply = send(&Client::new(profile), &[], true).0.unwrap();
    let Reply::Calls { calls, omitted, .. } = &reply else {
        panic!("expected calls")
    };
    assert_eq!(calls.len(), 3);
    assert_eq!(*omitted, 2);
    assert_eq!(
        calls
            .iter()
            .map(|call| call.id.as_str())
            .collect::<Vec<_>>(),
        ["c0", "c1", "c2"]
    );
    assert_eq!(calls[0].function.name, "unknown_tool");
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn web_tool_turns_reject_unoffered_duplicate_replayed_and_malformed_calls() {
    let _exclusive = test_support::exclusive_run();
    let valid = call("same", "web_search", json!({"query":"public"}));
    let cases = [
        (json!([valid.clone()]), false),
        (json!([valid.clone(), valid.clone()]), true),
        (
            json!([{"id":"bad id","type":"function","function":{"name":"web_search","arguments":"{}"}}]),
            true,
        ),
        (
            json!([{"id":"x","type":"function","function":{"name":"web_search","arguments":"not json"}}]),
            true,
        ),
        (
            json!([{"id":"x","type":"function","function":{"name":"web_search","arguments":"[]"}}]),
            true,
        ),
    ];
    for (calls, offer) in cases {
        let (profile, server) =
            test_support::start_test_double(vec![response(Value::Null, Some(calls), "tool_calls")]);
        assert!(send(&Client::new(profile), &[], offer).0.is_err());
        assert_eq!(server.join().unwrap().len(), 1);
    }
    let history = [
        Message::Assistant {
            content: None,
            tool_calls: vec![serde_json::from_value(valid.clone()).unwrap()],
        },
        Message::Tool {
            tool_call_id: "same".into(),
            content: "reviewed result".into(),
        },
    ];
    let (profile, server) = test_support::start_test_double(vec![response(
        Value::Null,
        Some(json!([valid])),
        "tool_calls",
    )]);
    assert!(send(&Client::new(profile), &history, true).0.is_err());
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn web_tool_history_requires_ordered_complete_unique_and_bounded_results_before_dispatch() {
    let make = || Message::Assistant {
        content: None,
        tool_calls: vec![
            serde_json::from_value(call("one", "web_search", json!({"query":"public"}))).unwrap(),
        ],
    };
    let result = || Message::Tool {
        tool_call_id: "one".into(),
        content: "reviewed result".into(),
    };
    let histories = [
        vec![make()],
        vec![result()],
        vec![
            make(),
            Message::Tool {
                tool_call_id: "other".into(),
                content: "wrong result".into(),
            },
        ],
        vec![make(), result(), make(), result()],
        vec![
            make(),
            Message::Tool {
                tool_call_id: "one".into(),
                content: "x".repeat(MAX_HISTORY_BYTES),
            },
        ],
    ];
    for history in histories {
        let (listener, profile) = test_support::listener();
        let client = Client::new(profile);
        let checked = Cell::new(0);
        let (reply, _, dispatched) = client.complete_agent_turn_before_guarded(
            "system",
            "task",
            Some(Request {
                history: &history,
                offer: true,
            }),
            true,
            Instant::now() + Duration::from_millis(500),
            || {
                checked.set(checked.get() + 1);
                Ok(())
            },
        );
        assert!(reply.is_err());
        assert!(!dispatched);
        assert_eq!(checked.get(), 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn web_tool_turns_bound_the_encoded_request_before_sending() {
    let (listener, profile) = test_support::listener();
    let client = Client::new(profile);
    let oversized = "\u{1}".repeat(MAX_REQUEST_BYTES / 5);
    let (reply, _, dispatched) = client.complete_agent_turn_before_guarded(
        "system",
        &oversized,
        Some(Request {
            history: &[],
            offer: true,
        }),
        true,
        Instant::now() + Duration::from_millis(500),
        || Ok(()),
    );
    assert!(matches!(reply, Err(Error::Invalid(_))));
    assert!(!dispatched);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn web_tool_history_can_finish_after_the_limit_group_without_offering_more_tools() {
    let _exclusive = test_support::exclusive_run();
    let mut history = Vec::new();
    for round in 0..3 {
        let id = format!("call-{round}");
        history.push(Message::Assistant {
            content: None,
            tool_calls: vec![
                serde_json::from_value(call(&id, "web_search", json!({"query":"public"}))).unwrap(),
            ],
        });
        history.push(Message::Tool {
            tool_call_id: id,
            content: if round == 2 {
                "Tool round limit reached"
            } else {
                "Reviewed evidence"
            }
            .into(),
        });
    }
    let (profile, server) = test_support::start_test_double(vec![response(
        json!("final bounded answer"),
        None,
        "stop",
    )]);
    let client = Client::new(profile);
    assert!(send(&client, &history, true).0.is_err());
    assert_eq!(
        send(&client, &history, false)
            .0
            .unwrap()
            .into_text()
            .unwrap(),
        "final bounded answer"
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].get("tools").is_none());
    assert_eq!(requests[0]["messages"].as_array().unwrap().len(), 8);
}

#[test]
fn web_tool_turns_recheck_authority_after_reply_and_before_continuing() {
    let _exclusive = test_support::exclusive_run();
    let (profile, server) = test_support::start_test_double(vec![response(
        Value::Null,
        Some(json!([call("c1", "web_search", json!({"query":"public"}))])),
        "tool_calls",
    )]);
    let client = Client::new(profile);
    let checks = Cell::new(0);
    let (reply, _, dispatched) = client.complete_agent_turn_before_guarded(
        "system",
        "task",
        Some(Request {
            history: &[],
            offer: true,
        }),
        true,
        Instant::now() + Duration::from_secs(3),
        || {
            checks.set(checks.get() + 1);
            if checks.get() > 1 {
                Err("Authority revoked".into())
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(reply, Err(Error::Review(_))));
    assert!(dispatched);
    assert_eq!(checks.get(), 2);
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(client.take_usage().unwrap().total_tokens, Some(12));
}

#[test]
fn web_tool_turns_preserve_thinking_retry_tools_and_json_format_refusals() {
    let _exclusive = test_support::exclusive_run();
    let unsupported = test_support::fixture_http_response(
        "400 Bad Request",
        "",
        r#"{"error":{"param":"thinking","message":"Unsupported parameter: thinking"}}"#,
    );
    let (mut profile, server) = test_support::start_test_double(vec![
        unsupported,
        response(
            Value::Null,
            Some(json!([call("c1", "web_search", json!({"query":"public"}))])),
            "tool_calls",
        ),
    ]);
    profile.model = "proxy/MiniMax-M3".into();
    let client = Client::new(profile);
    assert!(matches!(
        send(&client, &[], true).0,
        Ok(Reply::Calls { .. })
    ));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].get("thinking").is_some());
    assert!(requests[1].get("thinking").is_none());
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
    assert_eq!(requests[1]["tool_choice"], "auto");
    let (profile, server) =
        test_support::start_test_double(vec![test_support::fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"code":"unsupported_response_format","param":"response_format"}}"#,
        )]);
    assert!(matches!(
        send(&Client::new(profile), &[], true).0,
        Err(Error::UnsupportedResponseFormat)
    ));
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn web_tool_turns_do_not_dispatch_cancelled_or_unsupported_transports() {
    for (protocol, endpoint) in [
        (RemoteProtocol::Anthropic, "https://model.example/v1"),
        (
            RemoteProtocol::MiniMaxNative,
            crate::local_ai::MINIMAX_NATIVE_ENDPOINT,
        ),
    ] {
        let profile =
            Profile::remote(endpoint, "fixture", protocol, ProfileOptions::default()).unwrap();
        let checks = Cell::new(0);
        let result = Client::new(profile)
            .complete_agent_turn_before_guarded(
                "system",
                "task",
                Some(Request {
                    history: &[],
                    offer: true,
                }),
                true,
                Instant::now() + Duration::from_secs(1),
                || {
                    checks.set(checks.get() + 1);
                    Ok(())
                },
            )
            .0;
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert_eq!(checks.get(), 0);
    }
    let (listener, profile) = test_support::listener();
    let (client, cancel) = Client::new_cancellable(profile);
    drop(cancel);
    assert!(matches!(send(&client, &[], true).0, Err(Error::Cancelled)));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn web_tool_turns_reject_truncation_and_keep_usage_and_limit_metadata() {
    let _exclusive = test_support::exclusive_run();
    let (profile, server) = test_support::start_test_double(vec![response(
        Value::Null,
        Some(json!([call("c1", "web_search", json!({"query":"public"}))])),
        "length",
    )]);
    let client = Client::new(profile);
    let (reply, stop) = send(&client, &[], true);
    assert!(reply.is_err());
    assert_eq!(stop, Some(CompletionLimit::Output));
    assert_eq!(client.take_usage().unwrap().total_tokens, Some(12));
    server.join().unwrap();
}

#[test]
fn ordinary_text_completions_keep_tools_absent_and_reject_unsolicited_calls() {
    let _exclusive = test_support::exclusive_run();
    for (calls, rejected) in [
        (json!([]), false),
        (
            json!([call("x", "web_search", json!({"query":"public"}))]),
            true,
        ),
    ] {
        let (profile, server) = test_support::start_test_double(vec![response(
            json!("text plus unsolicited tool"),
            Some(calls),
            "stop",
        )]);
        let reply = Client::new(profile)
            .complete_agent_before(
                "system",
                "task",
                true,
                Instant::now() + Duration::from_secs(3),
            )
            .0;
        assert_eq!(reply.is_err(), rejected);
        if !rejected {
            assert_eq!(reply.unwrap(), "text plus unsolicited tool");
        }
        let requests = server.join().unwrap();
        assert!(requests[0].get("tools").is_none());
        assert!(requests[0].get("tool_choice").is_none());
    }
}

#[test]
fn web_tool_calls_do_not_redact_credential_echo_into_a_different_action() {
    let call: Call =
        serde_json::from_value(call("x", "web_search", json!({"query":"fixture-secret"}))).unwrap();
    let reply = Reply::Calls {
        content: None,
        calls: vec![call],
        omitted: 0,
    };
    assert!(matches!(
        reply.require_no_credential_echo(Some("fixture-secret")),
        Err(Error::Review(_))
    ));
    let call: Call = serde_json::from_value(json!({"id":"x","type":"function","function":{
        "name":"web_search","arguments":r#"{"query":"fixture\u002dsecret"}"#,
    }}))
    .unwrap();
    let reply = Reply::Calls {
        content: None,
        calls: vec![call],
        omitted: 0,
    };
    assert!(
        reply
            .require_no_credential_echo(Some("fixture-secret"))
            .is_err()
    );
    assert!(!format!("{reply:?}").contains("fixture"));
}
