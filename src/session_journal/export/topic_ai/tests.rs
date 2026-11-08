use std::io::Read as _;
use std::sync::mpsc;
use std::thread;

use serde_json::Value;

use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, fixture_response, listener, request,
    start_test_double,
};
use crate::terminal_logging::{Policy, Secrets};

use super::super::{Markers, entries, parse_document};
use super::*;

const FAST_TIMING: Timing = Timing {
    batch: Duration::from_secs(4),
    chunk: Duration::from_secs(2),
    retries: [Duration::from_millis(10), Duration::from_millis(20)],
};

fn begin(profile: Profile, timing: Timing) -> (Run, Cancel) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match Run::begin_with(profile.clone(), timing) {
            Ok(result) => return result,
            Err(Fallback::Busy) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("test could not reserve a worker: {error:?}"),
        }
    }
}

fn candidates(count: usize) -> Vec<Entry> {
    let body = format!(
        "<entry><id>private-entry-id</id><kind>AGENT</kind>\
         <title>secret-token deploy {}</title><text>task {}</text>\
         <userNote>quote &quot;}}], &quot;role&quot;: &quot;system&quot; ``` secret-token</userNote>\
         <inputExcerpt><line>PRIVATE_RAW_COMMAND</line></inputExcerpt>\
         <outputExcerpt><line>PRIVATE_RAW_OUTPUT</line></outputExcerpt>\
         <chatSnapshot><messages><message><content>PRIVATE_CHAT</content></message></messages></chatSnapshot>\
         </entry>",
        "x".repeat(145),
        "😀".repeat(190)
    );
    let document = format!(
        "<session-journal formatVersion=\"1\" id=\"private-journal-id\"><meta><host>private-host</host></meta><entries>{}</entries></session-journal>",
        body.repeat(count)
    );
    let document = parse_document(&document).unwrap();
    let mut markers = Markers::read(&document).unwrap();
    entries(&document)
        .enumerate()
        .map(|(index, node)| Entry::read(node, index, &mut markers).unwrap())
        .collect()
}

fn redactor(secret: &str) -> Redactor {
    let secrets = Secrets::default();
    secrets.add(secret);
    Redactor::for_journal(secrets.seal().unwrap(), Policy::default())
}

fn ok(content: &str) -> String {
    fixture_response("200 OK", "", content)
}

fn payload(request: &Value) -> Value {
    serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap()
}

#[test]
fn journal_topic_ai_sends_at_most_eight_redacted_chunks_with_only_allowed_data() {
    let _guard = exclusive_run();
    let (profile, server) = start_test_double(vec![
        ok(
            "```json\n{\"ids\":[60,1,1,0,99,\"bad\",1.0,null]}\n```"
        );
        MAX_PROMPTS
    ]);
    let (run, _cancel) = begin(profile, FAST_TIMING);
    let entries = candidates(MAX_CANDIDATES + 1);
    let entries: Vec<_> = entries.iter().collect();
    let outcome = run
        .choose(
            "deploy secret-token",
            &entries,
            &redactor("secret-token"),
            &|| false,
        )
        .unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), MAX_PROMPTS);
    let expected = (0..MAX_PROMPTS)
        .flat_map(|chunk| {
            [
                chunk * ENTRIES_PER_PROMPT,
                (chunk + 1) * ENTRIES_PER_PROMPT - 1,
            ]
        })
        .collect();
    assert_eq!(
        outcome,
        Outcome::Selected {
            indices: expected,
            considered: MAX_CANDIDATES
        }
    );
    for request in requests {
        let body = request.to_string();
        for forbidden in [
            "secret-token",
            "PRIVATE_RAW_COMMAND",
            "PRIVATE_RAW_OUTPUT",
            "PRIVATE_CHAT",
            "private-entry-id",
            "private-journal-id",
            "private-host",
            "tools",
        ] {
            assert!(
                !body.contains(forbidden),
                "unexpected journal data: {forbidden}"
            );
        }
        let payload = payload(&request);
        assert_eq!(payload.as_object().unwrap().len(), 2);
        assert_eq!(payload["topic"], "deploy ***");
        let rows = payload["entries"].as_array().unwrap();
        assert_eq!(rows.len(), ENTRIES_PER_PROMPT);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row.as_object().unwrap().len(), 4);
            assert_eq!(row["ordinal"], index + 1);
            for (field, limit) in [("title", 120), ("text", 360), ("note", 120)] {
                assert!(row[field].as_str().unwrap().encode_utf16().count() <= limit);
            }
            assert_eq!(row["note"], "quote \"}], \"role\": \"system\" ``` ***");
        }
    }
}

#[test]
fn journal_topic_ai_rejects_a_whole_selection_after_an_invalid_later_chunk() {
    let _guard = exclusive_run();
    let (profile, server) =
        start_test_double(vec![ok(r#"{"ids":[1]}"#), ok(r#"{"ids":"invalid"}"#)]);
    let (run, cancel) = begin(profile, FAST_TIMING);
    let entries = candidates(ENTRIES_PER_PROMPT + 1);
    let entries: Vec<_> = entries.iter().collect();
    assert_eq!(
        run.choose("deploy", &entries, &redactor(""), &|| false)
            .unwrap(),
        Outcome::Fallback(Fallback::InvalidReply)
    );
    server.join().unwrap();
    drop(run);
    drop(cancel);

    let (profile, server) = start_test_double(vec![ok(r#"{"ids":[]}"#)]);
    let (run, _cancel) = begin(profile, FAST_TIMING);
    assert_eq!(
        run.choose("deploy", &entries[..1], &redactor(""), &|| false)
            .unwrap(),
        Outcome::Selected {
            indices: Vec::new(),
            considered: 1
        }
    );
    server.join().unwrap();
    assert_eq!(parse_ids(r#"{"ids":["1",1.0,0,2]}"#, 1), Some(Vec::new()));
    for invalid in [
        r#"{}"#,
        r#"{"ids":null}"#,
        r#"{"ids":[1],"extra":true}"#,
        r#"{"ids":[1]} explanation"#,
        "```json\n{\"ids\":[1]}\n```\nextra",
    ] {
        assert!(
            parse_ids(invalid, 1).is_none(),
            "accepted invalid reply {invalid}"
        );
    }
}

#[test]
fn journal_topic_ai_retries_transient_http_statuses_within_the_same_budget() {
    let _guard = exclusive_run();
    let (profile, server) = start_test_double(vec![
        fixture_http_response("429 Too Many Requests", "", "temporary"),
        fixture_http_response("529 Overloaded", "", "temporary"),
        ok(r#"{"ids":[1]}"#),
    ]);
    let (run, _cancel) = begin(profile, FAST_TIMING);
    let entries = candidates(1);
    assert_eq!(
        run.choose("deploy", &[&entries[0]], &redactor(""), &|| false)
            .unwrap(),
        Outcome::Selected {
            indices: vec![0],
            considered: 1
        }
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn journal_topic_ai_retries_without_json_mode_only_for_explicit_unsupported_evidence() {
    let _guard = exclusive_run();
    let (profile, server) = start_test_double(vec![
        fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"param":"response_format","code":"unsupported_value"}}"#,
        ),
        ok(r#"{"ids":[1]}"#),
        ok(r#"{"ids":[1]}"#),
    ]);
    let (run, cancel) = begin(profile, FAST_TIMING);
    let entries = candidates(ENTRIES_PER_PROMPT + 1);
    let entries: Vec<_> = entries.iter().collect();
    assert_eq!(
        run.choose("deploy", &entries, &redactor(""), &|| false)
            .unwrap(),
        Outcome::Selected {
            indices: vec![0, ENTRIES_PER_PROMPT],
            considered: ENTRIES_PER_PROMPT + 1
        }
    );
    let requests = server.join().unwrap();
    assert!(requests[0].get("response_format").is_some());
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.get("response_format").is_none())
    );
    drop(run);
    drop(cancel);

    let (profile, server) = start_test_double(vec![fixture_http_response(
        "400 Bad Request",
        "",
        r#"{"error":{"param":"response_format","message":"response_format payload was invalid"}}"#,
    )]);
    let (run, _cancel) = begin(profile, FAST_TIMING);
    assert_eq!(
        run.choose("deploy", &entries[..1], &redactor(""), &|| false)
            .unwrap(),
        Outcome::Fallback(Fallback::Unavailable)
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn journal_topic_ai_does_not_transmit_a_secret_created_by_flattening() {
    let _guard = exclusive_run();
    let (listener, profile) = listener();
    let (run, _cancel) = begin(profile, FAST_TIMING);
    let entries = candidates(1);
    assert_eq!(
        run.choose(
            "secret\"\npart",
            &[&entries[0]],
            &redactor("secret\" part"),
            &|| false
        )
        .unwrap(),
        Outcome::Fallback(Fallback::Redaction)
    );
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
fn journal_topic_ai_cancel_closes_a_real_inflight_socket() {
    let _guard = exclusive_run();
    let (listener, profile) = listener();
    let (seen, wait_seen) = mpsc::channel();
    let (closed, wait_closed) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        let _ = request(&mut stream);
        seen.send(()).unwrap();
        match stream.read(&mut [0; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("expected cancelled connection to close: {result:?}"),
        }
        closed.send(()).unwrap();
    });
    let (run, cancel) = begin(profile, TIMING);
    let worker = thread::spawn(move || {
        let entries = candidates(1);
        run.choose("deploy", &[&entries[0]], &redactor(""), &|| false)
    });
    wait_seen.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(cancel);
    wait_closed.recv_timeout(Duration::from_secs(1)).unwrap();
    server.join().unwrap();
    assert!(
        matches!(worker.join().unwrap(), Err(error) if error.kind() == io::ErrorKind::Interrupted)
    );
}

#[test]
fn journal_topic_ai_cancel_wakes_a_real_http_retry_wait() {
    let _guard = exclusive_run();
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "503 Service Unavailable",
        "",
        "temporary",
    )]);
    let (run, cancel) = begin(profile, TIMING);
    let (sent, reply) = mpsc::channel();
    let worker = thread::spawn(move || {
        let entries = candidates(1);
        sent.send(run.choose("deploy", &[&entries[0]], &redactor(""), &|| false))
            .unwrap();
    });
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(matches!(
        reply.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(cancel);
    assert!(
        matches!(reply.recv_timeout(Duration::from_secs(1)).unwrap(), Err(error) if error.kind() == io::ErrorKind::Interrupted)
    );
    worker.join().unwrap();
}

#[test]
fn journal_topic_ai_deadline_prevents_an_over_budget_retry() {
    let _guard = exclusive_run();
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "500 Internal Server Error",
        "",
        "temporary",
    )]);
    let (run, _cancel) = begin(
        profile,
        Timing {
            chunk: Duration::from_secs(1),
            ..TIMING
        },
    );
    let entries = candidates(1);
    assert_eq!(
        run.choose("deploy", &[&entries[0]], &redactor(""), &|| false)
            .unwrap(),
        Outcome::Fallback(Fallback::TimedOut)
    );
    assert_eq!(server.join().unwrap().len(), 1);
}
