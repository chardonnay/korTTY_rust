use std::cell::Cell;
use std::io::Write as _;
use std::thread;
use std::time::Duration;

use crate::local_ai::{
    ProfileOptions, RemoteProtocol, StreamDelta,
    test_support::{accept, fixture_http_response, listener, wire_request},
};

use super::*;

const MODEL: &str = "proxy/custom-model";
const JSON_REPLY: &str = r#"{"id":"accepted-response","stop_reason":"end_turn","content":[{"type":"text","text":"answer"}],"usage":{"input_tokens":12,"output_tokens":3}}"#;
const SSE_REPLY: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"accepted-response\",\"usage\":{\"input_tokens\":12}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

fn options(effort: Option<&str>, max_output_tokens: Option<u32>) -> ProfileOptions {
    ProfileOptions {
        max_output_tokens,
        reasoning_effort: effort.map(str::to_owned),
        ..ProfileOptions::default()
    }
}

#[test]
fn anthropic_legacy_budget_reserves_answer_space_and_keeps_modern_adaptive_effort() {
    for (model, effort, configured, expected) in [
        (
            "us.anthropic.claude-haiku-4-5-20251001",
            "high",
            3_000,
            Some(json!({"type": "enabled", "budget_tokens": 1_976})),
        ),
        ("claude-3-7-sonnet-20250219", "xhigh", 2_047, None),
        (
            "claude-opus-4-7",
            "medium",
            32_000,
            Some(json!({"type": "adaptive"})),
        ),
        (
            "claude-sonnet-4-5",
            "none",
            4_096,
            Some(json!({"type": "disabled"})),
        ),
    ] {
        let profile = Profile::remote(
            "https://gateway.example/v1/messages",
            model,
            RemoteProtocol::Anthropic,
            options(Some(effort), Some(configured)),
        )
        .unwrap();
        let client = Client::new(profile);
        let messages = [RequestMessage::text("user", "question")];
        for stream in [false, true] {
            let request = request(&client, &messages, false, stream).unwrap();
            let body: Value = serde_json::from_slice(&request).unwrap();
            let limit = if stream {
                configured
            } else {
                configured.min(BUFFERED_OUTPUT)
            };
            assert_eq!(body["max_tokens"], limit);
            assert_eq!(body.get("thinking"), expected.as_ref(), "{model}");
            if body["thinking"]["type"] == "adaptive" {
                assert_eq!(body["output_config"]["effort"], effort);
            } else {
                assert!(body.get("output_config").is_none());
            }
        }
    }

    let profile = Profile::remote(
        "https://gateway.example/v1/messages",
        MODEL,
        RemoteProtocol::Anthropic,
        ProfileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        profile.with_output_limit(8_192).effective_output_limit(),
        DEFAULT_OUTPUT
    );
}

fn rejection(message: &str) -> String {
    json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
        .to_string()
}

fn messages_fixture(responses: Vec<String>) -> (Client, thread::JoinHandle<Vec<Value>>) {
    let (listener, _) = listener();
    let endpoint = format!(
        "http://{}/tenant/v1/messages",
        listener.local_addr().unwrap()
    );
    let profile =
        Profile::loopback_anthropic(&endpoint, MODEL, options(Some("high"), Some(12_000))).unwrap();
    let server = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
                let mut stream = accept(&listener);
                let body = wire_request(&mut stream, "POST /tenant/v1/messages HTTP/1.1");
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                serde_json::from_slice(&body).unwrap()
            })
            .collect()
    });
    (Client::new(profile), server)
}

#[test]
fn anthropic_retries_mode_and_output_rejections_before_json_or_stream_delivery() {
    for streaming in [false, true] {
        let reject = |message| {
            fixture_http_response(
                "400 Bad Request",
                "X-Request-Id: rejected-response\r\n",
                &rejection(message),
            )
        };
        let (media_type, reply) = if streaming {
            ("Content-Type: text/event-stream\r\n", SSE_REPLY)
        } else {
            ("Content-Type: application/json\r\n", JSON_REPLY)
        };
        let (client, server) = messages_fixture(vec![
            reject("Unsupported parameter thinking.type.adaptive"),
            reject("max_tokens: 12000 > 6000, which is the maximum"),
            reject("This model does not support budget_tokens"),
            fixture_http_response("200 OK", media_type, reply),
        ]);
        let checks = Cell::new(0);
        let authorize = || {
            checks.set(checks.get() + 1);
            Ok(())
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut text = String::new();
        let response = if streaming {
            client.stream_structured(
                "system",
                "fixture prompt",
                true,
                Some(deadline),
                authorize,
                |delta| {
                    assert_eq!(checks.get(), 4);
                    if let StreamDelta::Text(part) = delta {
                        text.push_str(&part);
                    }
                },
            )
        } else {
            client.complete_before_guarded("system", "fixture prompt", true, deadline, authorize)
        };
        assert_eq!(response.unwrap(), "answer");
        if streaming {
            assert_eq!(text, "answer");
        }
        assert_eq!(checks.get(), 4);
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["thinking"], json!({"type": "adaptive"}));
        assert_eq!(
            requests[1]["thinking"],
            json!({"type": "enabled", "budget_tokens": 8_192})
        );
        assert_eq!(
            requests[2]["thinking"],
            json!({"type": "enabled", "budget_tokens": 4_500})
        );
        assert!(requests[3].get("thinking").is_none());
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request["messages"], requests[0]["messages"]);
            assert_eq!(request["system"], requests[0]["system"]);
            assert_eq!(request["model"], MODEL);
            assert_eq!(request["stream"], streaming);
            assert_eq!(
                request["max_tokens"],
                if index < 2 { 12_000 } else { 6_000 }
            );
        }
        let usage = client.take_usage().unwrap();
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.request_id.as_deref(), Some("accepted-response"));
    }
}

#[test]
fn anthropic_retry_requires_current_review_and_a_pre_generation_error() {
    let reject = rejection("Unsupported parameter thinking.type.adaptive");
    let (client, server) =
        messages_fixture(vec![fixture_http_response("400 Bad Request", "", &reject)]);
    let checks = Cell::new(0);
    let reply = client.complete_before_guarded(
        "system",
        "fixture prompt",
        true,
        Instant::now() + Duration::from_secs(4),
        || {
            let prior = checks.replace(checks.get() + 1);
            if prior == 0 {
                Ok(())
            } else {
                Err("The reviewed profile changed.".into())
            }
        },
    );
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(
        reply.unwrap_err().message(),
        "The reviewed profile changed."
    );

    let mut generated: Value = serde_json::from_str(&reject).unwrap();
    generated["usage"] = json!({"output_tokens": 1});
    let mut openai_shaped: Value = serde_json::from_str(&reject).unwrap();
    openai_shaped["choices"] = json!([{"message": {"content": "started"}}]);
    for (status, body, expected) in [
        ("400 Bad Request", generated.to_string(), 400),
        ("400 Bad Request", openai_shaped.to_string(), 400),
        (
            "400 Bad Request",
            rejection("max_tokens must be at least 14000"),
            400,
        ),
        ("401 Unauthorized", reject.clone(), 401),
        (
            "400 Bad Request",
            "x".repeat(crate::local_ai::MAX_ERROR_RESPONSE_BYTES as usize + 1),
            400,
        ),
    ] {
        let (client, server) = messages_fixture(vec![fixture_http_response(status, "", &body)]);
        let checks = Cell::new(0);
        let reply = client.complete_before_guarded(
            "system",
            "fixture prompt",
            true,
            Instant::now() + Duration::from_secs(4),
            || {
                checks.set(checks.get() + 1);
                Ok(())
            },
        );
        assert_eq!(server.join().unwrap().len(), 1);
        assert_eq!(checks.get(), 1);
        assert!(matches!(reply, Err(Error::Status(actual)) if actual == expected));
    }

    let profile = Profile::remote(
        "https://gateway.example/v1/messages",
        "claude-fable-5",
        RemoteProtocol::AnthropicExact,
        options(Some("none"), None),
    )
    .unwrap();
    let mut request = Request::new(&profile, false);
    assert!(matches!(
        request.retry("thinking.type.disabled is not supported by this model"),
        Err(Error::Invalid(reason)) if reason.contains("thinking Off")
    ));
}
