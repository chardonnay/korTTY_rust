use std::cell::Cell;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::test_support::{fixture_http_response, fixture_response, listener, start_test_double};
use super::{
    Client, Error, Profile, ProfileOptions, RemoteProtocol, RequestMessage, StreamDelta,
    UsageAccount, UsageBudget,
};

const MODEL: &str = "proxy/MiniMax-M3";
const UNSUPPORTED: &str = r#"{"error":{"param":"thinking","code":"unsupported_parameter","message":"Unsupported parameter: thinking"}}"#;
const PRIVATE_ERROR_DETAIL: &str = "fixture-private-provider-detail";
const SUCCESSFUL_STREAM: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"accepted\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}\n\n",
    "data: [DONE]\n\n"
);

#[test]
fn minimax_thinking_follows_the_effective_model_endpoint_and_effort() {
    let messages = [RequestMessage::text("user", "question")];
    for (endpoint, model, effort, disabled) in [
        ("https://aggregator.example/v1", MODEL, None, true),
        ("https://api.minimax.io/v1", "alias", None, true),
        ("https://aggregator.example/v1", MODEL, Some("none"), true),
        ("https://api.minimax.io/v1", MODEL, Some("high"), false),
        ("https://aggregator.example/v1", "ordinary", None, false),
        (
            "https://aggregator.example/v1",
            "ordinary",
            Some("none"),
            false,
        ),
    ] {
        let profile = Profile::remote(
            endpoint,
            model,
            RemoteProtocol::OpenAi,
            ProfileOptions {
                reasoning_effort: effort.map(str::to_owned),
                ..Default::default()
            },
        )
        .unwrap();
        let client = Client::new(profile);
        for stream in [false, true] {
            let request: Value =
                serde_json::from_slice(&client.openai_request(&messages, true, stream).unwrap())
                    .unwrap();
            assert_eq!(request["stream"], stream);
            assert_eq!(request["model"], model);
            assert_eq!(request["reasoning_effort"].as_str(), effort);
            if disabled {
                assert_eq!(request["thinking"], json!({"type": "disabled"}));
            } else {
                assert!(request.get("thinking").is_none());
            }
        }
    }
}

#[test]
fn minimax_flash_leaves_default_reasoning_to_the_provider() {
    let messages = [RequestMessage::text("user", "question")];
    for (model, flash) in [
        ("MiniMax-M3.1-Flash", true),
        ("proxy/MINIMAX-M3.1-FLASH:free", true),
        ("MiniMax-M3.1-Flash-Preview", false),
        ("MiniMax-M3.1-Flash/other-model", false),
    ] {
        for effort in [None, Some("none"), Some("high")] {
            let profile = Profile::remote(
                "https://api.minimax.io/v1",
                model,
                RemoteProtocol::OpenAi,
                ProfileOptions {
                    reasoning_effort: effort.map(str::to_owned),
                    ..Default::default()
                },
            )
            .unwrap();
            let client = Client::new(profile);
            let result = client.openai_request(&messages, true, false);
            if flash && effort == Some("none") {
                assert!(
                    matches!(result, Err(Error::Invalid(reason)) if reason.contains("None reasoning"))
                );
                continue;
            }
            let request: Value = serde_json::from_slice(&result.unwrap()).unwrap();
            assert_eq!(request["reasoning_effort"].as_str(), effort);
            assert_eq!(
                request.get("thinking").is_some(),
                !flash && effort != Some("high")
            );
        }
    }
}

#[test]
fn minimax_flash_none_sends_no_http_request_or_usage_charge() {
    let (listener, profile) = listener();
    let root = crate::snippets::tests::Profile::new();
    let account = UsageAccount::new(root.0.clone(), 1, UsageBudget::default()).unwrap();
    let mut profile = Profile::new(
        &profile.external_address().to_string(),
        "MiniMax-M3.1-Flash",
    )
    .unwrap()
    .with_usage_account(account.clone());
    profile.options.reasoning_effort = Some("none".into());
    let client = Client::new(profile);
    for result in [
        client.complete("system", "reviewed input"),
        client.stream_structured(
            "system",
            "reviewed input",
            true,
            Some(Instant::now() + Duration::from_secs(4)),
            || Ok(()),
            |_| panic!("a locally rejected request cannot emit output"),
        ),
    ] {
        assert!(matches!(result, Err(Error::Invalid(reason)) if reason.contains("None reasoning")));
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let usage = account.snapshot().unwrap();
    assert_eq!(usage.counts.requests, 0);
    assert_eq!(usage.in_flight, 0);
    assert_eq!(usage.reserved_output, 0);
}

#[test]
fn minimax_thinking_rejection_retries_the_same_nonstream_request_once() {
    let (profile, server) = start_test_double(vec![
        fixture_http_response("400 Bad Request", "", UNSUPPORTED),
        fixture_response("200 OK", "", "accepted"),
    ]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let checked = Cell::new(0);
    let reply = client.complete_before_guarded(
        "system",
        "reviewed input",
        true,
        Instant::now() + Duration::from_secs(4),
        || {
            checked.set(checked.get() + 1);
            Ok(())
        },
    );
    let requests = server.join().unwrap();
    assert_eq!(reply.unwrap(), "accepted");
    assert_eq!(checked.get(), 2);
    assert_only_removed_thinking(requests, false);
}

#[test]
fn minimax_thinking_rejection_retries_before_emitting_stream_deltas() {
    check_stream_retries_thinking(fixture_http_response("400 Bad Request", "", UNSUPPORTED));
}

#[test]
fn minimax_thinking_rejection_in_the_initial_json_retries_before_streaming() {
    let body = json!({
        "base_resp": {"status_code": 2013, "status_msg": "Unsupported parameter: thinking"},
        "choices": [],
    });
    check_stream_retries_thinking(fixture_http_response(
        "200 OK",
        "Content-Type: application/json\r\n",
        &body.to_string(),
    ));
}

fn check_stream_retries_thinking(rejection: String) {
    let (profile, server) = start_test_double(vec![
        rejection,
        fixture_http_response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            SUCCESSFUL_STREAM,
        ),
    ]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let checked = Cell::new(0);
    let mut text = String::new();
    let reply = client.stream_structured(
        "system",
        "reviewed input",
        true,
        Some(Instant::now() + Duration::from_secs(4)),
        || {
            checked.set(checked.get() + 1);
            Ok(())
        },
        |delta| {
            assert_eq!(checked.get(), 2);
            let StreamDelta::Text(delta) = delta else {
                panic!("expected only the successful response text");
            };
            text.push_str(&delta);
        },
    );
    assert_only_removed_thinking(server.join().unwrap(), true);
    assert_eq!(reply.unwrap(), "accepted");
    assert_eq!(text, "accepted");
    let usage = client.take_usage().unwrap();
    assert_eq!(usage.total_tokens, Some(16));
}

#[test]
fn minimax_thinking_retry_requires_current_stream_authority() {
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "400 Bad Request",
        "",
        UNSUPPORTED,
    )]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let checked = Cell::new(0);
    let reply = client.stream_structured(
        "system",
        "reviewed input",
        true,
        Some(Instant::now() + Duration::from_secs(4)),
        || {
            let seen = checked.replace(checked.get() + 1);
            if seen == 0 {
                Ok(())
            } else {
                Err("The reviewed model changed.".into())
            }
        },
        |_| panic!("revoked retry cannot emit output"),
    );
    let request = server.join().unwrap().pop().unwrap();
    assert_eq!(request["thinking"]["type"], "disabled");
    assert_eq!(reply.unwrap_err().message(), "The reviewed model changed.");
    assert_eq!(checked.get(), 2);
}

#[test]
fn minimax_thinking_retry_is_bounded_to_explicit_http_400_rejection() {
    let long_error = json!({
        "error": {"param": "thinking", "code": "unsupported_parameter"},
        "padding": "x".repeat(super::MAX_ERROR_RESPONSE_BYTES as usize),
    })
    .to_string();
    let cases: [(&str, u16, &str); 3] = [
        (
            "400 Bad Request",
            400,
            r#"{"error":{"message":"The requested model is unavailable"}}"#,
        ),
        ("400 Bad Request", 400, &long_error),
        ("422 Unprocessable Entity", 422, UNSUPPORTED),
    ];
    for (status_line, status, body) in cases {
        let (profile, server) =
            start_test_double(vec![fixture_http_response(status_line, "", body)]);
        let client =
            Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
        let checked = Cell::new(0);
        let reply = client.complete_before_guarded(
            "system",
            "reviewed input",
            true,
            Instant::now() + Duration::from_secs(4),
            || {
                checked.set(checked.get() + 1);
                Ok(())
            },
        );
        server.join().unwrap();
        assert!(matches!(reply, Err(Error::Status(actual)) if actual == status));
        assert_eq!(checked.get(), 1);
    }

    let rejection = fixture_http_response("400 Bad Request", "", UNSUPPORTED);
    let (profile, server) = start_test_double(vec![rejection.clone(), rejection]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let reply = client.complete_before(
        "system",
        "question",
        true,
        Instant::now() + Duration::from_secs(4),
    );
    assert_only_removed_thinking(server.join().unwrap(), false);
    assert!(matches!(reply, Err(Error::Status(400))));
}

#[test]
fn minimax_provider_auth_error_in_successful_http_preserves_usage_without_retry() {
    let body = json!({
        "base_resp": {"status_code": 1004, "status_msg": PRIVATE_ERROR_DETAIL},
        "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14},
    });
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "200 OK",
        "Content-Type: application/json\r\n",
        &body.to_string(),
    )]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let checked = Cell::new(0);
    let result = client.complete_before_guarded(
        "system",
        "reviewed input",
        true,
        Instant::now() + Duration::from_secs(4),
        || {
            checked.set(checked.get() + 1);
            Ok(())
        },
    );
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(checked.get(), 1);
    let error = result.unwrap_err().message();
    assert!(error.contains("MiniMax") && error.contains("authentication"));
    assert!(!error.contains(PRIVATE_ERROR_DETAIL));
    assert_reported_usage(&client);
}

#[test]
fn minimax_provider_error_in_stream_with_empty_choices_is_not_an_answer() {
    let error = json!({
        "base_resp": {"status_code": "2013", "status_msg": "Unsupported parameter: thinking"},
        "choices": [],
    });
    let body = format!("data: {error}\n\ndata: [DONE]\n\n");
    let client = check_provider_error_during_stream("text/event-stream", &body);
    assert!(client.take_usage().is_none());
}

#[test]
fn minimax_thinking_rejection_in_json_with_reported_usage_is_never_retried() {
    let error = json!({
        "base_resp": {"status_code": "2013", "status_msg": "Unsupported parameter: thinking"},
        "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14},
    });
    let client = check_provider_error_during_stream("application/json", &error.to_string());
    assert_reported_usage(&client);
}

fn check_provider_error_during_stream(content_type: &str, body: &str) -> Client {
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "200 OK",
        &format!("Content-Type: {content_type}\r\n"),
        body,
    )]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let checked = Cell::new(0);
    let result = client.stream_structured(
        "system",
        "reviewed input",
        true,
        Some(Instant::now() + Duration::from_secs(4)),
        || {
            checked.set(checked.get() + 1);
            Ok(())
        },
        |_| panic!("provider error cannot emit output"),
    );
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(checked.get(), 1);
    let error = result.unwrap_err().message();
    assert!(error.contains("MiniMax") && error.contains("parameters"));
    assert!(!error.contains(PRIVATE_ERROR_DETAIL));
    client
}

#[test]
fn minimax_provider_explicit_error_overrides_present_choices_in_shared_parser() {
    let body = json!({
        "error": {"message": PRIVATE_ERROR_DETAIL},
        "choices": [{"message": {"content": "must be rejected"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14},
    });
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "200 OK",
        "Content-Type: application/json\r\n",
        &body.to_string(),
    )]);
    let client = Client::new(profile);
    let error = client
        .complete("system", "reviewed input")
        .unwrap_err()
        .message();
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(error.contains("provider reported an error"));
    assert!(!error.contains(PRIVATE_ERROR_DETAIL));
    assert_reported_usage(&client);
}

fn assert_reported_usage(client: &Client) {
    let usage = client.take_usage().unwrap();
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(3));
    assert_eq!(usage.total_tokens, Some(14));
}

#[test]
fn minimax_thinking_does_not_consume_structured_response_format_rejection() {
    let rejection = r#"{"error":{"param":"response_format","code":"unsupported_parameter","message":"Unsupported parameter: response_format"}}"#;
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "400 Bad Request",
        "",
        rejection,
    )]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    let outcome = client.stream_structured(
        "system",
        "reviewed input",
        true,
        Some(Instant::now() + Duration::from_secs(4)),
        || Ok(()),
        |_| panic!("A rejected request cannot emit output"),
    );
    let requests = server.join().unwrap();
    assert_eq!(requests[0]["thinking"]["type"], "disabled");
    assert!(matches!(outcome, Err(Error::UnsupportedResponseFormat)));
}

#[test]
fn minimax_http_rejection_retains_reported_usage_with_numeric_error_code() {
    let body = json!({
        "error": {"code": 2013, "message": "Unsupported parameter: thinking"},
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14},
    });
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "400 Bad Request",
        "",
        &body.to_string(),
    )]);
    let client = Client::new(Profile::new(&profile.external_address().to_string(), MODEL).unwrap());
    assert!(matches!(
        client.complete("system", "request"),
        Err(Error::Status(400))
    ));
    assert_eq!(server.join().unwrap().len(), 1);
    assert_reported_usage(&client);
}

fn assert_only_removed_thinking(mut requests: Vec<Value>, stream: bool) {
    let second = requests.pop().unwrap();
    let mut first = requests.pop().unwrap();
    assert!(requests.is_empty());
    let thinking = first.as_object_mut().unwrap().remove("thinking");
    assert_eq!(thinking, Some(json!({"type": "disabled"})));
    assert_eq!(first["stream"], stream);
    assert_eq!(first["response_format"]["type"], "json_object");
    assert_eq!(second, first);
}
