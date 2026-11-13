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
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    assert_eq!(line.trim_end(), request_line);
    let mut length = None;
    loop {
        line.clear();
        assert!(stream.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut bytes = vec![0; length.unwrap_or_default()];
    stream.read_exact(&mut bytes).unwrap();
    bytes
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
