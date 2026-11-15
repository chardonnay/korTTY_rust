//! Test doubles serve fixed HTTP responses over real loopback connections.

use std::io::Write;
use std::thread;

use serde_json::{Value, json};

use super::*;

#[test]
fn managed_embedding_indices_must_cover_each_input_once() {
    let response: ManagedResponse = serde_json::from_str(
        r#"{"data":[{"index":1,"embedding":[4,5]},{"index":0,"embedding":[2,3]}]}"#,
    )
    .unwrap();
    assert_eq!(
        response.ordered(2).unwrap(),
        [vec![2.0, 3.0], vec![4.0, 5.0]]
    );
    for indices in [[0, 0], [0, 2], [1, 2]] {
        let response = ManagedResponse {
            data: indices
                .into_iter()
                .map(|index| ManagedVector {
                    index,
                    embedding: vec![1.0],
                })
                .collect(),
        };
        assert!(response.ordered(2).is_err());
    }
}
use crate::local_ai::{start, test_support};

#[test]
fn local_server_scan_uses_only_loopback_and_anonymous_model_requests() {
    use std::io::BufRead as _;

    let _exclusive = test_support::exclusive_run();
    let (listener, profile) = test_support::listener();
    let endpoint = profile.external_address();
    let server = thread::spawn(move || {
        let mut stream = test_support::accept(&listener);
        let mut first = String::new();
        stream.read_line(&mut first).unwrap();
        assert_eq!(first.trim_end(), "GET /v1/models HTTP/1.1");
        loop {
            let mut header = String::new();
            assert_ne!(stream.read_line(&mut header).unwrap(), 0);
            if header == "\r\n" {
                break;
            }
            assert!(!header.to_ascii_lowercase().starts_with("authorization:"));
        }
        let reply =
            test_support::fixture_http_response("200 OK", "", r#"{"data":[{"id":"local-model"}]}"#);
        stream.get_mut().write_all(reply.as_bytes()).unwrap();
    });
    assert!(scan_local_models(vec!["192.0.2.1:11434".parse().unwrap()]).is_err());
    let pending = scan_local_models(vec![endpoint]).unwrap();
    let servers = pending.reply.recv_blocking().unwrap().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].endpoint, endpoint);
    assert_eq!(servers[0].models.as_ref().unwrap(), &["local-model"]);
    server.join().unwrap();
}

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
    let pending = discover(&format!("http://{}/v1", profile.external_address())).unwrap();
    assert_eq!(
        pending.reply.recv_blocking().unwrap().unwrap(),
        ["alpha", "zeta:1"]
    );
    server.join().unwrap();
}

#[test]
fn lm_studio_catalog_resolves_exact_keys_and_loaded_instance_ids() {
    use LmStudioReasoningOption::{High, Low, Off, On};

    let response: NativeModels = serde_json::from_value(json!({"models":[
        {"type":"embedding","key":"ignored","capabilities":{"vision":true}},
        {"type":"llm","key":"invalid model"},
        "invalid row",
        {"type":"llm","key":"plain","loaded_instances":[{"id":"plain:one"}]},
        {"type":"llm","key":"Vendor/Image","loaded_instances":[{}, {"id":"image:one"}],
         "capabilities":{"vision":true,"reasoning":{
            "allowed_options":["off", "on", "low", "high", "off", "future-option", 1],
            "default":"high"
         }}},
        {"type":"vlm","key":"older-vision","loaded_instances":[{"id":"duplicate"}]},
        {"type":"vlm","key":"disabled-vision","capabilities":{"vision":false}},
        {"type":"llm","key":"another","loaded_instances":[{"id":"duplicate"}],
         "capabilities":{"vision":false,"reasoning":{"allowed_options":["low"],"default":"bogus"}}}
    ]}))
    .unwrap();
    let catalog = response.catalog();
    assert_eq!(
        catalog.names(),
        [
            "Vendor/Image",
            "another",
            "disabled-vision",
            "older-vision",
            "plain"
        ]
    );
    let image = catalog.capability("Vendor/Image").unwrap();
    assert_eq!(catalog.resolve_loaded("Vendor/Image"), Ok("Vendor/Image"));
    assert!(catalog.resolve_loaded("image:one").is_err());
    assert!(catalog.resolve_loaded("disabled-vision").is_err());
    assert_eq!(catalog.capability("image:one"), Some(image));
    assert_eq!(image.vision, Some(true));
    assert_eq!(
        image.reasoning.as_ref().unwrap().allowed,
        [Off, On, Low, High]
    );
    assert_eq!(image.reasoning.as_ref().unwrap().default, Some(High));
    assert_eq!(catalog.capability("plain:one").unwrap().vision, Some(false));
    assert_eq!(
        catalog.capability("plain").unwrap().reasoning,
        Some(LmStudioReasoning::default())
    );
    assert_eq!(
        catalog.capability("older-vision").unwrap().vision,
        Some(true)
    );
    assert_eq!(
        catalog.capability("disabled-vision").unwrap().vision,
        Some(false)
    );
    assert_eq!(
        catalog
            .capability("another")
            .unwrap()
            .reasoning
            .as_ref()
            .unwrap()
            .default,
        None
    );
    for reference in [
        "Image",
        "vendor/image",
        "duplicate",
        "ignored",
        "invalid model",
    ] {
        assert_eq!(catalog.capability(reference), None);
    }
}

#[test]
fn lm_studio_catalog_prefers_the_only_loaded_chat_model_over_a_saved_preference() {
    let response: NativeModels = serde_json::from_value(json!({"models":[
        {"type":"embedding","key":"embedding","loaded_instances":[{"id":"embedding"}]},
        {"type":"llm","key":"preferred","loaded_instances":[]},
        {"type":"vlm","key":"loaded","loaded_instances":[{"id":"one"},{"id":"two"}]},
        {"type":"llm","key":"loaded","loaded_instances":[{"id":"three"}]}
    ]}))
    .unwrap();
    assert_eq!(response.catalog().resolve_loaded("preferred"), Ok("loaded"));
    let response: NativeModels = serde_json::from_value(json!({"models":[
        {"type":"llm","key":"downloaded","loaded_instances":[]}
    ]}))
    .unwrap();
    assert!(response.catalog().resolve_loaded("downloaded").is_err());
}

#[test]
fn lm_studio_catalog_falls_back_on_unsupported_native_endpoint_and_masks_echo() {
    use std::io::BufRead as _;

    let (listener, profile) = test_support::listener();
    let server = thread::spawn(move || {
        for (path, status, body) in [
            ("/api/v1/models", "404 Not Found", "{}"),
            (
                "/api/v0/models",
                "200 OK",
                r#"{"data":[{"type":"vlm","id":"vision-legacy","loaded_instances":[{"id":"ref-fixture-secret"}]},{"type":"llm","id":"text"},{"type":"llm","id":"fixture-secret"}]}"#,
            ),
        ] {
            let mut stream = test_support::accept(&listener);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), format!("GET {path} HTTP/1.1"));
            let mut authorized = false;
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorized = value.trim() == "Bearer fixture-secret";
                }
            }
            assert!(authorized);
            let response = test_support::fixture_http_response(status, "", body);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    let connection = Connection::new(profile.external_address(), DISCOVERY_TIMEOUT);
    let catalog = lm_studio_catalog_guarded(
        &connection,
        Some("fixture-secret"),
        Instant::now() + DISCOVERY_TIMEOUT,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(catalog.names(), ["text", "vision-legacy"]);
    assert_eq!(
        catalog.capability("vision-legacy").unwrap().vision,
        Some(true)
    );
    assert_eq!(catalog.capability("text").unwrap().vision, None);
    assert_eq!(catalog.capability("text").unwrap().reasoning, None);
    assert_eq!(catalog.capability("ref-fixture-secret"), None);
    server.join().unwrap();
}

#[test]
fn lm_studio_catalog_does_not_fall_back_on_authorization_failure() {
    let (listener, profile) = test_support::listener();
    let server = thread::spawn(move || {
        let mut stream = test_support::accept(&listener);
        assert!(test_support::wire_request(&mut stream, "GET /api/v1/models HTTP/1.1").is_empty());
        let response = test_support::fixture_http_response("401 Unauthorized", "", "{}");
        stream.get_mut().write_all(response.as_bytes()).unwrap();
        listener
    });
    let connection = Connection::new(profile.external_address(), DISCOVERY_TIMEOUT);
    assert!(matches!(
        lm_studio_catalog_guarded(
            &connection,
            None,
            Instant::now() + DISCOVERY_TIMEOUT,
            || Ok(())
        ),
        Err(Error::Status(401))
    ));
    assert_eq!(
        server.join().unwrap().accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn lm_studio_localhost_discovery_preserves_authority_and_refuses_redirects() {
    use std::io::BufRead as _;

    let _exclusive = test_support::exclusive_run();
    let (listener, _) = test_support::listener();
    let authority = format!("localhost:{}", listener.local_addr().unwrap().port());
    let endpoint =
        LmStudioEndpoint::parse(&format!("http://{authority}/v1/chat/completions"), false).unwrap();
    let server = thread::spawn(move || {
        let mut stream = test_support::accept(&listener);
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "GET /api/v1/models HTTP/1.1");
        let mut host = None;
        loop {
            line.clear();
            assert!(stream.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("host")
            {
                host = Some(value.trim().to_owned());
            }
        }
        assert_eq!(host.as_deref(), Some(authority.as_str()));
        let response = test_support::fixture_http_response(
            "302 Found",
            &format!("Location: http://{authority}/unexpected\r\n"),
            "{}",
        );
        stream.get_mut().write_all(response.as_bytes()).unwrap();
        listener
    });
    let pending = discover_lm_studio_catalog(&endpoint, None).unwrap();
    assert!(pending.reply.recv_blocking().unwrap().is_err());
    assert_eq!(
        server.join().unwrap().accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn lm_studio_https_discovery_starts_tls_and_never_falls_back_to_http() {
    use std::io::Read as _;

    let _exclusive = test_support::exclusive_run();
    let (listener, _) = test_support::listener();
    let endpoint = LmStudioEndpoint::parse(
        &format!(
            "https://localhost:{}/api/v1/chat",
            listener.local_addr().unwrap().port()
        ),
        true,
    )
    .unwrap();
    let server = thread::spawn(move || {
        let mut stream = test_support::accept(&listener);
        let mut record = [0; 6];
        stream.read_exact(&mut record).unwrap();
        assert_eq!(&record[..2], &[22, 3]); // TLS handshake record with a TLS version.
        assert_eq!(record[5], 1); // ClientHello handshake type.
        listener
    });
    let pending = discover_lm_studio_catalog(&endpoint, None).unwrap();
    assert!(pending.reply.recv_blocking().unwrap().is_err());
    assert_eq!(
        server.join().unwrap().accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
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
