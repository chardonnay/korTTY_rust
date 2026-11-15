//! Test doubles for the local chat endpoint. Production still uses the real HTTP client.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{MutexGuard, mpsc};
use std::time::Instant;

use serde_json::Value;

use super::*;
use serde_json::json;

pub(crate) fn exclusive_run() -> MutexGuard<'static, ()> {
    static RUN_TEST: Mutex<()> = Mutex::new(());
    RUN_TEST.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn fixture_response(status: &str, headers: &str, content: &str) -> String {
    let body = json!({"choices": [{"message": {"content": content}}]}).to_string();
    fixture_http_response(status, headers, &body)
}

pub(crate) fn fixture_http_response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

pub(crate) fn start_test_double(
    responses: Vec<String>,
) -> (Profile, thread::JoinHandle<Vec<Value>>) {
    let (listener, profile) = listener();
    let worker = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
                let mut stream = accept(&listener);
                let body = request(&mut stream);
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                body
            })
            .collect()
    });
    (profile, worker)
}

pub(crate) fn listener() -> (TcpListener, Profile) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let profile =
        Profile::new(&listener.local_addr().unwrap().to_string(), "fixture:local").unwrap();
    listener.set_nonblocking(true).unwrap();
    (listener, profile)
}

pub(crate) fn accept(listener: &TcpListener) -> BufReader<TcpStream> {
    accept_with_timeout(listener, Duration::from_secs(4))
}

pub(crate) fn accept_with_timeout(
    listener: &TcpListener,
    timeout: Duration,
) -> BufReader<TcpStream> {
    let deadline = Instant::now() + timeout;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture connection missing: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    BufReader::new(stream)
}

pub(crate) fn request(stream: &mut BufReader<TcpStream>) -> Value {
    serde_json::from_slice(&wire_request(stream, "POST /v1/chat/completions HTTP/1.1")).unwrap()
}

pub(crate) fn wire_request(stream: &mut BufReader<TcpStream>, request_line: &str) -> Vec<u8> {
    wire_request_headers(stream, request_line).1
}

fn wire_request_headers(
    stream: &mut BufReader<TcpStream>,
    request_line: &str,
) -> (Vec<(String, String)>, Vec<u8>) {
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    assert_eq!(line.trim_end(), request_line);
    let mut headers = Vec::new();
    loop {
        line.clear();
        assert!(stream.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map_or(0, |(_, value)| value.parse::<usize>().unwrap());
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    (headers, bytes)
}

#[test]
fn loopback_anthropic_sends_exact_messages_and_reads_json_and_sse() {
    let (listener, _) = listener();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://localhost:{port}/KorTTY/V1/Messages");
    let profile =
        Profile::loopback_anthropic(&endpoint, "explicit/model", ProfileOptions::default())
            .unwrap();
    let server = thread::spawn(move || {
        let stream = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"server-label\",\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Stream reply\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        for (streaming, extra_headers, response) in [
            (
                false,
                "Content-Type: application/json\r\n",
                r#"{"model":"server-label","content":[{"type":"text","text":"JSON reply"}],"stop_reason":"end_turn"}"#,
            ),
            (true, "Content-Type: text/event-stream\r\n", stream),
        ] {
            let mut socket = accept(&listener);
            let (headers, body) =
                wire_request_headers(&mut socket, "POST /KorTTY/V1/Messages HTTP/1.1");
            assert!(headers.contains(&("anthropic-version".into(), ANTHROPIC_VERSION.into())));
            assert!(!headers.iter().any(|(name, _)| name == "authorization"));
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "explicit/model");
            assert_eq!(body["stream"], streaming);
            assert_eq!(body["system"], "system");
            assert_eq!(
                body["messages"],
                json!([{"role":"user","content":"question"}])
            );
            assert!(body.get("integrations").is_none());
            socket
                .get_mut()
                .write_all(fixture_http_response("200 OK", extra_headers, response).as_bytes())
                .unwrap();
        }
    });
    let client = Client::new(profile);
    assert_eq!(
        client.complete_chat("system", &[], "question").unwrap(),
        "JSON reply"
    );
    let mut streamed = String::new();
    assert_eq!(
        client
            .stream_chat(
                "system",
                &[],
                "question",
                || Ok(()),
                |delta| {
                    if let StreamDelta::Text(text) = delta {
                        streamed.push_str(&text);
                    }
                }
            )
            .unwrap(),
        "Stream reply"
    );
    assert_eq!(streamed, "Stream reply");
    assert_eq!(client.model(), "explicit/model");
    server.join().unwrap();
}

#[test]
fn loopback_anthropic_checks_policy_on_a_cached_connection() {
    let (listener, _) = listener();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}/v1/messages");
    let client =
        Client::new(Profile::loopback_anthropic(&endpoint, "fixture", Default::default()).unwrap());
    assert!(client.transport.get().is_some());
    assert!(matches!(
        client.transport_with_policy(None, || ClipboardPolicy {
            ai_allowed: false,
            ..ClipboardPolicy::default()
        }),
        Err(Error::Review(message)) if message.contains("Managed policy blocks")
    ));
    assert!(
        client
            .transport_with_policy(None, ClipboardPolicy::default)
            .is_ok()
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn server_default_model_sends_no_model_or_inventory_request_on_loopback() {
    for route in ["external", "compatible", "native"] {
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let profile = if route == "external" {
            Profile::configured_with_server_default(&endpoint, ProfileOptions::default())
        } else {
            Profile::lm_studio_with_server_default(
                &endpoint,
                ProfileOptions::default(),
                route == "native",
            )
        }
        .unwrap();
        let native = route == "native";
        let server = thread::spawn(move || {
            let (request_line, response) = if native {
                (
                    "POST /api/v1/chat HTTP/1.1",
                    r#"{"model_instance_id":"reported-only","output":[{"type":"message","content":"answer"}]}"#,
                )
            } else {
                (
                    "POST /v1/chat/completions HTTP/1.1",
                    r#"{"model":"reported-only","choices":[{"finish_reason":"stop","message":{"content":"answer"}}]}"#,
                )
            };
            for streaming in [false, true] {
                let mut socket = accept(&listener);
                let body: Value =
                    serde_json::from_slice(&wire_request(&mut socket, request_line)).unwrap();
                assert!(body.get("model").is_none());
                assert!(body.get("reasoning").is_none());
                assert!(body.get("reasoning_effort").is_none());
                assert_eq!(body["stream"], streaming);
                socket
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", response).as_bytes())
                    .unwrap();
            }
        });
        let client = Client::new(profile);
        assert_eq!(
            client.complete_chat("system", &[], "question").unwrap(),
            "answer"
        );
        assert_eq!(
            client.take_usage().unwrap().model.as_deref(),
            Some("reported-only")
        );
        assert_eq!(
            client
                .stream_chat("system", &[], "question", || Ok(()), |_| {})
                .unwrap(),
            "answer"
        );
        assert_eq!(
            client.take_usage().unwrap().model.as_deref(),
            Some("reported-only")
        );
        assert!(client.profile().uses_server_default_model());
        assert!(client.model().is_empty());
        assert!(client.profile().reviewed_model_fingerprint().is_none());
        server.join().unwrap();
    }
}

#[test]
fn server_default_model_rejects_auto_vision_and_embedding_before_connecting() {
    let (listener, ordinary) = listener();
    let endpoint = ordinary.external_address().to_string();
    let profile =
        Profile::configured_with_server_default(&endpoint, ProfileOptions::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let client = Client::new(profile.clone());
    assert!(matches!(
        client.require_vision_supported(deadline, || Ok(())),
        Err(Error::Review(error)) if error.contains("server selects the model")
    ));
    assert!(client.embedding_revision().is_none());
    assert!(client.embed(&["test"]).is_err());
    assert!(
        Client::new(profile.with_vision_mode(VisionMode::Enabled))
            .require_vision_supported(deadline, || Ok(()))
            .is_ok()
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn lm_studio_exact_chat_paths_are_sent_verbatim_after_review() {
    for (native, suffix, expected) in [
        (false, "/home/V1/", "/home/V1/chat/completions"),
        (false, "/V1/Chat/Completions/", "/V1/Chat/Completions/"),
        (true, "/tenant/API/V1/Chat/", "/tenant/API/V1/Chat/"),
    ] {
        let (listener, _) = listener();
        let endpoint = format!("http://{}{suffix}", listener.local_addr().unwrap());
        let profile = if native {
            Profile::lm_studio(
                &endpoint,
                "pinned",
                ProfileOptions {
                    reasoning_effort: Some("low".into()),
                    ..ProfileOptions::default()
                },
            )
        } else {
            Profile::lm_studio_compatible(&endpoint, "pinned", ProfileOptions::default())
        }
        .unwrap();
        assert!(resolve_lm_studio_auto(&endpoint, "", None, native).is_err());
        let worker = thread::spawn(move || {
            let mut socket = accept(&listener);
            let body: Value = serde_json::from_slice(&wire_request(
                &mut socket,
                &format!("POST {expected} HTTP/1.1"),
            ))
            .unwrap();
            assert_eq!(body["model"], "pinned");
            let response = if native {
                assert_eq!(body["reasoning"], "low");
                r#"{"output":[{"type":"message","content":"answer"}]}"#
            } else {
                r#"{"choices":[{"message":{"content":"answer"}}]}"#
            };
            socket
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", response).as_bytes())
                .unwrap();
        });
        assert_eq!(
            Client::new(profile)
                .complete_chat("system", &[], "question")
                .unwrap(),
            "answer"
        );
        worker.join().unwrap();
    }
}

#[test]
fn lm_studio_reasoning_reuses_exact_capabilities_and_preserves_model_default() {
    let (server, _) = listener();
    let endpoint = server.local_addr().unwrap().to_string();
    let profile = |reasoning: Option<&str>| {
        Profile::lm_studio(
            &endpoint,
            "ReasoningAlias",
            ProfileOptions {
                reasoning_effort: reasoning.map(str::to_owned),
                ..ProfileOptions::default()
            },
        )
        .unwrap()
    };
    let worker = thread::spawn(move || {
        let mut models = accept(&server);
        assert!(wire_request(&mut models, "GET /api/v1/models HTTP/1.1").is_empty());
        models.get_mut().write_all(fixture_http_response("200 OK", "Content-Type: application/json\r\n", r#"{"models":[{"type":"llm","key":"vendor/model","loaded_instances":[{"id":"ReasoningAlias"}],"capabilities":{"vision":false,"reasoning":{"allowed_options":["off","on"],"default":"off"}}}]}"#).as_bytes()).unwrap();
        drop(models);
        for expected in [Some("on"), None] {
            let mut chat = accept(&server);
            let body: Value =
                serde_json::from_slice(&wire_request(&mut chat, "POST /api/v1/chat HTTP/1.1"))
                    .unwrap();
            assert_eq!(body["model"], "ReasoningAlias");
            assert_eq!(body["store"], false);
            assert_eq!(body.get("reasoning").and_then(Value::as_str), expected);
            chat.get_mut()
                .write_all(
                    fixture_http_response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        r#"{"output":[{"type":"message","content":"answer"}]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
    });
    let unsupported = Client::new(profile(Some("high"))).complete("System", "Input");
    assert!(
        matches!(unsupported, Err(Error::Review(error)) if error.contains("does not support this reasoning setting"))
    );
    assert_eq!(
        Client::new(profile(Some("on")))
            .complete("System", "Input")
            .unwrap(),
        "answer"
    );
    assert_eq!(
        Client::new(profile(None))
            .complete("System", "Input")
            .unwrap(),
        "answer"
    );
    worker.join().unwrap();
}

#[test]
fn lm_studio_reasoning_rechecks_review_after_a_metadata_lookup() {
    let (server, _) = listener();
    let profile = Profile::lm_studio(
        &server.local_addr().unwrap().to_string(),
        "review-fixture",
        ProfileOptions {
            reasoning_effort: Some("low".into()),
            ..ProfileOptions::default()
        },
    )
    .unwrap();
    let client = Client::new(profile);
    let approved = Arc::new(AtomicBool::new(true));
    let review = Arc::clone(&approved);
    let worker = thread::spawn(move || {
        let mut stream = accept(&server);
        let _ = wire_request(&mut stream, "GET /api/v1/models HTTP/1.1");
        approved.store(false, Ordering::Release);
        stream.get_mut().write_all(fixture_http_response("200 OK", "Content-Type: application/json\r\n", r#"{"models":[{"type":"llm","key":"review-fixture","capabilities":{"vision":false,"reasoning":{"allowed_options":["low"]}}}]}"#).as_bytes()).unwrap();
        server
    });
    let result = client.complete_text_before_guarded(
        "System",
        "private review",
        false,
        Instant::now() + Duration::from_secs(4),
        || {
            review
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "The review was revoked".into())
        },
    );
    assert!(matches!(result, Err(Error::Review(error)) if error == "The review was revoked"));
    assert_eq!(
        worker.join().unwrap().accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn reviewed_https_completion_rejects_a_different_credential_before_connecting() {
    let (listener, _) = listener();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let profile = Profile::remote(
        &endpoint,
        "fixture",
        RemoteProtocol::OpenAi,
        ProfileOptions::default(),
    )
    .unwrap();
    let result = Client::new(profile).complete_reviewed_https_text_before(
        "workflow system",
        "reviewed evidence",
        Instant::now() + Duration::from_secs(4),
        Some(Zeroizing::new("reviewed-fixture-key".into())),
        || Ok(()),
    );
    assert!(
        matches!(result, Err(Error::Review(message)) if message.contains("credential changed"))
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn guarded_text_completion_checks_authority_before_dispatch() {
    let (listener, profile) = listener();
    let result = Client::new(profile).complete_text_before_guarded(
        "workflow system",
        "reviewed evidence",
        false,
        Instant::now() + Duration::from_secs(4),
        || Err("Managed workflow header changed".into()),
    );
    assert!(
        matches!(result, Err(Error::Review(reason)) if reason == "Managed workflow header changed")
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn an_oversized_local_ai_body_is_an_invalid_response() {
    let large = "x".repeat(MAX_RESPONSE_BYTES as usize);
    let (profile, server) = start_test_double(vec![fixture_response("200 OK", "", &large)]);
    assert!(matches!(
        Client::new(profile).complete("fixture", "request"),
        Err(Error::Invalid("response exceeds 256 KiB"))
    ));
    server.join().unwrap();
}

#[test]
fn malformed_openai_choices_keep_reported_usage_and_remain_invalid() {
    let malformed = [
        r#"{
            "choices": [{"message": {"content": "valid first choice"}}, {"message": {"content": 42}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14},
            "model": "fixture:reported", "id": "later-choice"
        }"#,
        r#"{
            "usage": {"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16},
            "model": "fixture:reported", "id": "duplicate-content",
            "choices": [{"message": {"content": "first", "content": "duplicate"}}]
        }"#,
    ];
    let (profile, server) = start_test_double(
        malformed
            .into_iter()
            .map(|body| fixture_http_response("200 OK", "", body))
            .collect(),
    );
    let client = Client::new(profile);
    for (id, input, output, total) in [
        ("later-choice", 11, 3, 14),
        ("duplicate-content", 12, 4, 16),
    ] {
        assert!(matches!(
            client.complete_before(
                "fixture",
                "request",
                true,
                Instant::now() + Duration::from_secs(4),
            ),
            Err(Error::Invalid("invalid chat response"))
        ));
        let usage = client.take_usage().unwrap();
        assert_eq!(usage.request_id.as_deref(), Some(id));
        assert_eq!(usage.model.as_deref(), Some("fixture:reported"));
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.total_tokens),
            (Some(input), Some(output), Some(total))
        );
    }
    assert_eq!(server.join().unwrap().len(), malformed.len());
}

#[test]
fn dropping_local_ai_cancel_closes_an_inflight_http_socket() {
    let (listener, profile) = listener();
    let (request_seen, wait_for_request) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        let _ = request(&mut stream);
        request_seen.send(()).unwrap();
        match stream.read(&mut [0_u8; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("expected cancellation to close the socket: {result:?}"),
        }
    });
    let pending = start(profile, |client| {
        client
            .complete("fixture", "request")
            .map_err(Error::message)
    })
    .unwrap();
    wait_for_request
        .recv_timeout(Duration::from_secs(4))
        .unwrap();
    drop(pending.cancel);
    server.join().unwrap();
    assert_eq!(
        pending.reply.recv_blocking().unwrap().unwrap_err(),
        Error::Cancelled.message()
    );
}

#[test]
fn cancelling_native_lm_studio_while_reading_the_body_keeps_observed_usage() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let profile = Profile::lm_studio(
        &listener.local_addr().unwrap().to_string(),
        "fixture:native",
        ProfileOptions::default(),
    )
    .unwrap();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        let _ = wire_request(&mut socket, "POST /api/v1/chat HTTP/1.1");
        socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 200\r\nX-Request-Id: native-fixture\r\nConnection: close\r\n\r\n{").unwrap();
        socket.get_mut().flush().unwrap();
        match socket.read(&mut [0_u8; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => {
                panic!("expected cancellation to close the native response socket: {result:?}")
            }
        }
    });
    let (headers_read, wait_for_headers) = mpsc::channel();
    let pending = start(profile, move |client| {
        let result = client.with_completion(|attempt| {
            let deadline = Some(Instant::now() + Duration::from_secs(6));
            let transport = client.transport(deadline)?;
            let body = providers::lm_request(
                client,
                &chat_messages("system", &[], "fixture"),
                true,
                false,
            )?;
            let mut response = client.completion_response(transport, &body, deadline, attempt)?;
            headers_read.send(()).unwrap();
            client.cancellable_response_bytes(&mut response, true)
        });
        Ok((result, client.take_usage()))
    })
    .unwrap();
    wait_for_headers
        .recv_timeout(Duration::from_secs(4))
        .unwrap();
    drop(pending.cancel);
    let (result, usage) = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    let usage = usage.unwrap();
    assert_eq!(usage.request_id.as_deref(), Some("native-fixture"));
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (None, None, None)
    );
    server.join().unwrap();
}

#[test]
fn configured_local_chat_sends_the_selected_output_limit_and_reasoning() {
    let (listener, profile) = listener();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        let body = request(&mut stream);
        assert_eq!(body["max_tokens"], 1_048_576);
        assert_eq!(body["reasoning_effort"], "minimal");
        stream
            .get_mut()
            .write_all(fixture_response("200 OK", "", "reply").as_bytes())
            .unwrap();
    });
    let endpoint = format!("http://{}/v1/", profile.external_address());
    let configured = Profile::configured(
        &endpoint,
        "model",
        ProfileOptions {
            max_output_tokens: Some(1_048_576),
            reasoning_effort: Some("minimal".into()),
            ..ProfileOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        Client::new(configured)
            .complete("fixture", "request")
            .unwrap(),
        "reply"
    );
    server.join().unwrap();
}

#[test]
fn configured_profiles_only_accept_numeric_loopback_and_explicit_safe_credential_names() {
    for bad_endpoint in [
        "localhost:11434",
        "https://127.0.0.1:11434",
        "127.0.0.1:0",
        "192.0.2.1:11434",
        "http://127.0.0.1:11434/v1/chat/completions",
        "http://user@127.0.0.1:11434/v1",
        "http://127.0.0.1:11434/v1?proxy=1",
    ] {
        assert!(Profile::configured(bad_endpoint, "model", ProfileOptions::default()).is_err());
    }
    assert!(
        Profile::configured("http://[::1]:11434/v1", "model", ProfileOptions::default()).is_ok()
    );
    assert!(
        Profile::configured(
            DEFAULT_ENDPOINT,
            "model",
            ProfileOptions {
                credential_env: Some("bad=NAME".into()),
                ..ProfileOptions::default()
            },
        )
        .is_err()
    );
}
