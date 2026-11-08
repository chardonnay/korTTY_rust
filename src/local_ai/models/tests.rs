//! Test doubles serve fixed HTTP responses over real loopback connections.

use std::io::Write;
use std::thread;

use serde_json::{Value, json};

use super::*;
use crate::local_ai::{start, test_support};

#[test]
fn discovery_uses_the_models_endpoint_and_filters_duplicate_or_invalid_names() {
    let _exclusive = test_support::exclusive_run();
    let (listener, profile) = test_support::listener();
    let server = thread::spawn(move || {
        let mut stream = test_support::accept(&listener);
        assert!(test_support::wire_request(&mut stream, "GET /v1/models HTTP/1.1").is_empty());
        let body = json!({"data": [
            {"id": "zeta:1"},
            {"id": "alpha"},
            {"id": "zeta:1"},
            {"id": "control\u{001b}model"},
            {"id": ""}
        ]});
        let response = test_support::fixture_http_response("200 OK", "", &body.to_string());
        stream.get_mut().write_all(response.as_bytes()).unwrap();
    });
    let pending = discover(&format!("http://{}/v1", profile.address)).unwrap();
    assert_eq!(
        pending.reply.recv_blocking().unwrap().unwrap(),
        ["alpha", "zeta:1"]
    );
    server.join().unwrap();
}

#[test]
fn embedding_batch_preserves_input_order_and_rejects_invalid_shapes() {
    let _exclusive = test_support::exclusive_run();
    let (listener, profile) = test_support::listener();
    let server = thread::spawn(move || {
        for embeddings in [json!([[0.5, 0.7], [0.3, 0.1]]), json!([[1.0], [1.0, 2.0]])] {
            let mut stream = test_support::accept(&listener);
            let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"model": "fixture:local", "input": ["first", "second"], "truncate": false})
            );
            let body = json!({"embeddings": embeddings});
            let response = test_support::fixture_http_response("200 OK", "", &body.to_string());
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    let pending = start(profile, |client| {
        let input = ["first", "second"];
        assert_eq!(client.embed(&input)?, [vec![0.5, 0.7], vec![0.3, 0.1]]);
        client.embed(&input)
    })
    .unwrap();
    assert!(
        pending
            .reply
            .recv_blocking()
            .unwrap()
            .unwrap_err()
            .contains("invalid embeddings")
    );
    server.join().unwrap();
}
