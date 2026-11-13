use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{BufRead, ErrorKind, Read, Write};
use std::net::TcpListener;
use std::thread;

use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};
use crate::snippets::tests::Profile;

use super::*;

const GENERATION_ID: Generation = [17; 16];
const VECTOR: [f32; 2] = [0.0, 1.0];
const COLLECTION: &str = "GET /collections/fixture HTTP/1.1";
const QUERY: &str = "POST /collections/fixture/points/query?consistency=all HTTP/1.1";
const FIXTURE_KEY: &str = "synthetic-qdrant-key";
const SCALE_POINTS: usize = 2_048;
const SCALE_DIMENSIONS: usize = 768;

fn fixture() -> (TcpListener, Connection) {
    let (listener, _) = listener();
    let connection = Connection::new(&fixture_settings(&listener), None).unwrap();
    (listener, connection)
}

fn fixture_settings(listener: &TcpListener) -> Settings {
    Settings {
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        collection: "fixture".into(),
        dimensions: VECTOR.len(),
        ..Settings::default()
    }
}

fn exchange(listener: &TcpListener, line: &str, status: &str, response: Value) -> Value {
    let mut stream = accept(listener);
    let body = wire_request(&mut stream, line);
    let response = fixture_http_response(status, "", &response.to_string());
    stream.get_mut().write_all(response.as_bytes()).unwrap();
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    }
}

fn success(result: Value) -> Value {
    json!({"status": "ok", "result": result})
}

fn collection_info() -> Value {
    json!({"config": {"params": {"vectors": {"size": 2, "distance": "Cosine"}}}})
}

fn exchange_collection(listener: &TcpListener, collection: Value) {
    assert!(exchange(listener, COLLECTION, "200 OK", success(collection)).is_null());
}

fn hit(position: usize, source: usize) -> Value {
    json!({
        "id": point_id(GENERATION_ID, position).unwrap(),
        "score": 0.75,
        "payload": {
            "generation": generation_text(GENERATION_ID),
            "position": position,
            "source": source,
        },
    })
}

fn scale_source(position: usize) -> usize {
    position % MAX_SOURCES
}

fn scale_filtered_sources() -> [bool; MAX_SOURCES] {
    std::array::from_fn(|source| source.is_multiple_of(4) || source == MAX_SOURCES - 1)
}

fn scale_positions(enabled: &[bool; MAX_SOURCES]) -> impl DoubleEndedIterator<Item = usize> + '_ {
    (0..SCALE_POINTS).filter(move |&position| enabled[scale_source(position)])
}

fn assert_complete_scale_hits(hits: &[(f32, usize, usize)], enabled: &[bool; MAX_SOURCES]) {
    let mut positions: Vec<_> = hits
        .iter()
        .map(|&(_, position, source)| {
            assert_eq!(source, scale_source(position));
            assert!(enabled[source]);
            position
        })
        .collect();
    positions.sort_unstable();
    assert_eq!(positions, scale_positions(enabled).collect::<Vec<_>>());
}

#[test]
fn settings_restrict_plaintext_to_numeric_loopback_and_show_the_exact_collection() {
    let default = Settings::default();
    default.validate().unwrap();
    assert_eq!(
        default.destination(),
        "http://127.0.0.1:6333/collections/kortty_rag",
    );
    for (endpoint, expected) in [
        (
            "http://[::1]:6333",
            "http://[::1]:6333/collections/kortty_rag",
        ),
        (
            "https://example.invalid/rag/",
            "https://example.invalid/rag/collections/kortty_rag",
        ),
    ] {
        let settings = Settings {
            endpoint: endpoint.into(),
            ..default.clone()
        };
        settings.validate().unwrap();
        assert_eq!(settings.destination(), expected);
    }
    for endpoint in [
        "http://localhost:6333",
        "http://192.0.2.1:6333",
        "https://user:password@example.invalid",
        "https://@example.invalid",
        "https://example.invalid?credential=secret",
        "https://example.invalid/#fragment",
    ] {
        let settings = Settings {
            endpoint: endpoint.into(),
            ..default.clone()
        };
        assert!(settings.validate().is_err());
        assert_eq!(settings.destination(), "Invalid Qdrant destination");
    }
    for settings in [
        Settings {
            collection: "../private".into(),
            ..default.clone()
        },
        Settings {
            dimensions: MAX_DIMENSIONS + 1,
            ..default.clone()
        },
        Settings {
            credential_env: "bad=name".into(),
            ..default
        },
    ] {
        assert!(settings.validate().is_err());
    }
}

#[test]
fn vault_key_settings_preserve_existing_json_and_reject_ambiguous_credentials() {
    let mut legacy = json!({
        "endpoint": "http://127.0.0.1:6333",
        "collection": "fixture",
        "dimensions": 2,
    });
    for _ in 0..2 {
        let settings: Settings = serde_json::from_value(legacy.clone()).unwrap();
        settings.validate().unwrap();
        assert!(settings.resolve_vault_key(None, None).unwrap().is_none());
        assert_eq!(serde_json::to_value(&settings).unwrap(), legacy);
        legacy["credential_env"] = json!("KORTTY_QDRANT_FIXTURE");
    }

    let id = "01".repeat(16);
    legacy["vault_key"] = json!(&id);
    assert!(
        serde_json::from_value::<Settings>(legacy)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut settings = Settings {
        vault_key: Some(id.clone()),
        ..Settings::default()
    };
    settings.validate().unwrap();
    assert!(settings.resolve_vault_key(None, None).is_err());
    assert_eq!(
        serde_json::to_value(&settings).unwrap()["vault_key"],
        json!(id)
    );
    assert!(!format!("{settings:?}").contains(&id));
    settings.vault_key = Some("not-a-native-vault-id".into());
    assert!(settings.validate().is_err());
}

#[test]
fn vault_binding_must_match_the_selected_id_and_be_live_before_dispatch() {
    let (listener, _) = listener();
    let mut settings = fixture_settings(&listener);
    let id = "01".repeat(16);
    let revision = Arc::new(());
    let proof = Arc::downgrade(&revision);
    let key = VaultKey::for_test(id.clone(), FIXTURE_KEY, proof.clone());
    assert!(Weak::ptr_eq(&key.revision(), &proof));
    assert!(settings.require_vault_key(Some(&key)).is_err());
    settings.vault_key = Some("02".repeat(16));
    assert!(settings.require_vault_key(None).is_err());
    assert!(settings.require_vault_key(Some(&key)).is_err());
    settings.vault_key = Some(id);
    let connection = Connection::new_with_vault(&settings, None, Some(key)).unwrap();
    assert!(connection.require_credential_current().is_ok());
    drop(revision);
    let result = connection.query(GENERATION_ID, &VECTOR, &[false, true], 1, || Ok(()));
    assert_eq!(result.unwrap_err(), VAULT_CHANGED);
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

#[test]
fn resolves_the_unlocked_vault_against_the_exact_review_and_revokes_on_lock() {
    let profile = Profile::new();
    let (mut vault, id) = VaultWindow::qdrant_fixture(&profile.0, FIXTURE_KEY);
    let settings = Settings {
        vault_key: Some(id),
        ..Settings::default()
    };
    let reviewed = vault.credential_revision().unwrap();
    let binding = settings
        .resolve_vault_key(Some(&vault), Some(&reviewed))
        .unwrap()
        .unwrap();
    assert!(Weak::ptr_eq(&binding.revision(), &reviewed));
    assert!(
        settings
            .resolve_vault_key(Some(&vault), None)
            .unwrap()
            .is_some()
    );

    let different = Arc::new(());
    assert!(
        settings
            .resolve_vault_key(Some(&vault), Some(&Arc::downgrade(&different)))
            .is_err()
    );
    let _ = vault.update(crate::vault_window::Message::Lock);
    assert_eq!(binding.require_current(), Err(VAULT_CHANGED.into()));
    assert!(
        settings
            .resolve_vault_key(Some(&vault), Some(&reviewed))
            .is_err()
    );
}

fn authenticated_exchange(
    listener: &TcpListener,
    line: &str,
    result: Value,
    before_response: impl FnOnce(),
) {
    let mut stream = accept(listener);
    let mut headers = String::new();
    loop {
        let start = headers.len();
        assert!(stream.read_line(&mut headers).unwrap() > 0);
        if &headers[start..] == "\r\n" {
            break;
        }
    }
    let mut lines = headers.lines();
    assert_eq!(lines.next(), Some(line));
    let field = |name: &str| {
        lines
            .clone()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.trim()))
    };
    assert_eq!(field("api-key"), Some(FIXTURE_KEY));
    let length = field("content-length").unwrap_or("0").parse().unwrap();
    assert_eq!(length == 0, line == COLLECTION);
    stream.read_exact(&mut vec![0; length]).unwrap();
    before_response();
    let response = success(result);
    let response = fixture_http_response("200 OK", "", &response.to_string());
    stream.get_mut().write_all(response.as_bytes()).unwrap();
}

#[test]
fn uses_the_vault_api_key_header_and_rejects_an_in_flight_revocation() {
    let (listener, _) = listener();
    let mut settings = fixture_settings(&listener);
    let id = "01".repeat(16);
    settings.vault_key = Some(id.clone());
    let revision = Arc::new(());
    let key = VaultKey::for_test(id, FIXTURE_KEY, Arc::downgrade(&revision));
    let connection = Connection::new_with_vault(&settings, None, Some(key)).unwrap();
    let worker = thread::spawn(move || {
        let points = json!({"points": [hit(4, 1)]});
        authenticated_exchange(&listener, COLLECTION, collection_info(), || {});
        authenticated_exchange(&listener, QUERY, points.clone(), || {});
        authenticated_exchange(&listener, COLLECTION, collection_info(), || {});
        authenticated_exchange(&listener, QUERY, points, || drop(revision));
    });
    let query = || connection.query(GENERATION_ID, &VECTOR, &[false, true], 1, || Ok(()));
    assert_eq!(query().unwrap().hits, [(0.75, 4, 1)]);
    assert_eq!(query().unwrap_err(), VAULT_CHANGED);
    worker.join().unwrap();
}

#[test]
fn stages_queries_and_deletes_only_the_reviewed_generation() {
    let (listener, connection) = fixture();
    let worker = thread::spawn(move || {
        let done = success(json!({"status": "completed"}));
        assert!(exchange(&listener, COLLECTION, "404 Not Found", json!({})).is_null());
        assert_eq!(
            exchange(
                &listener,
                "PUT /collections/fixture HTTP/1.1",
                "200 OK",
                success(json!(true)),
            ),
            json!({"vectors": {"size": 2, "distance": "Cosine"}}),
        );
        exchange_collection(&listener, collection_info());
        let generation = generation_text(GENERATION_ID);
        let point = |position, source| {
            json!({
                "id": point_id(GENERATION_ID, position).unwrap(),
                "vector": VECTOR,
                "payload": {"generation": generation, "source": source, "position": position},
            })
        };
        assert_eq!(
            exchange(
                &listener,
                "PUT /collections/fixture/points?wait=true&ordering=strong HTTP/1.1",
                "200 OK",
                done.clone(),
            ),
            json!({"points": [point(9, 1), point(4, 2)]}),
        );
        let generation = json!({"key": "generation", "match": {"value": generation}});
        exchange_collection(&listener, collection_info());
        assert_eq!(
            exchange(
                &listener,
                QUERY,
                "200 OK",
                success(json!({"points": [hit(9, 1), hit(4, 2)]})),
            ),
            json!({
                "query": VECTOR,
                "limit": 2,
                "with_payload": ["generation", "source", "position"],
                "with_vector": false,
                "filter": {"must": [generation, {"key": "source", "match": {"any": [1, 2]}}]},
            }),
        );
        assert_eq!(
            exchange(
                &listener,
                "POST /collections/fixture/points/delete?wait=true&ordering=strong HTTP/1.1",
                "200 OK",
                done,
            ),
            json!({"filter": {"must": [generation]}}),
        );
    });
    let points = [
        Point {
            source: 1,
            position: 9,
            vector: &VECTOR,
        },
        Point {
            source: 2,
            position: 4,
            vector: &VECTOR,
        },
    ];
    connection.stage(GENERATION_ID, &points, || Ok(())).unwrap();
    assert!(connection.staging_attempted());
    assert_eq!(
        connection
            .query(GENERATION_ID, &VECTOR, &[false, true, true], 2, || Ok(()))
            .unwrap()
            .hits,
        [(0.75, 4, 2), (0.75, 9, 1)],
    );
    connection.delete(GENERATION_ID, || Ok(())).unwrap();
    worker.join().unwrap();
}

#[test]
fn qdrant_transports_all_2048_reviewed_points_in_bounded_batches() {
    let (listener, connection) = fixture();
    let worker = thread::spawn(move || {
        let mut collection = collection_info();
        collection["config"]["strict_mode_config"] = json!({
            "enabled": true,
            "unindexed_filtering_retrieve": false,
            "unindexed_filtering_update": false,
        });
        collection["payload_schema"] = json!({
            "generation": {"data_type": "keyword"},
            "source": {"data_type": "integer", "params": {"lookup": true}},
        });
        exchange_collection(&listener, collection.clone());

        let done = success(json!({"status": "completed"}));
        let generation = generation_text(GENERATION_ID);
        let mut ids = BTreeSet::new();
        for start in (0..SCALE_POINTS).step_by(MAX_BATCH_POINTS) {
            let batch = exchange(
                &listener,
                "PUT /collections/fixture/points?wait=true&ordering=strong HTTP/1.1",
                "200 OK",
                done.clone(),
            );
            let points = batch["points"].as_array().unwrap();
            assert_eq!(points.len(), MAX_BATCH_POINTS.min(SCALE_POINTS - start));
            assert!(batch.to_string().len() <= MAX_REQUEST_BYTES);
            for (offset, point) in points.iter().enumerate() {
                let position = start + offset;
                let id = point_id(GENERATION_ID, position).unwrap();
                assert!(ids.insert(id.clone()));
                assert_eq!(
                    *point,
                    json!({
                        "id": id,
                        "vector": VECTOR,
                        "payload": {
                            "generation": generation,
                            "source": scale_source(position),
                            "position": position,
                        },
                    })
                );
            }
        }
        assert_eq!(ids.len(), SCALE_POINTS);

        let generation = generation_condition(&generation);
        for enabled in [[true; MAX_SOURCES], scale_filtered_sources()] {
            exchange_collection(&listener, collection.clone());
            let sources: Vec<_> = (0..MAX_SOURCES).filter(|&source| enabled[source]).collect();
            let hits: Vec<_> = scale_positions(&enabled)
                .rev()
                .map(|position| {
                    let mut hit = hit(position, scale_source(position));
                    hit["version"] = json!(u64::MAX);
                    hit
                })
                .collect();
            let limit = hits.len();
            let reply = success(json!({"points": hits}));
            assert!((reply.to_string().len() as u64) < MAX_RESPONSE_BYTES);
            let request = exchange(&listener, QUERY, "200 OK", reply);
            assert_eq!(
                request,
                json!({
                    "query": VECTOR,
                    "limit": limit,
                    "with_payload": ["generation", "source", "position"],
                    "with_vector": false,
                    "filter": {
                        "must": [generation, {"key": "source", "match": {"any": sources}}],
                    },
                })
            );
        }
        assert_eq!(
            exchange(
                &listener,
                "POST /collections/fixture/points/delete?wait=true&ordering=strong HTTP/1.1",
                "200 OK",
                done,
            ),
            json!({"filter": {"must": [generation]}}),
        );
    });
    let points: Vec<_> = (0..SCALE_POINTS)
        .map(|position| Point {
            source: scale_source(position),
            position,
            vector: &VECTOR,
        })
        .collect();
    connection.stage(GENERATION_ID, &points, || Ok(())).unwrap();
    for enabled in [[true; MAX_SOURCES], scale_filtered_sources()] {
        let hits = connection
            .query(
                GENERATION_ID,
                &VECTOR,
                &enabled,
                scale_positions(&enabled).count(),
                || Ok(()),
            )
            .unwrap()
            .hits;
        assert_complete_scale_hits(&hits, &enabled);
        assert!(hits.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }
    connection.delete(GENERATION_ID, || Ok(())).unwrap();
    worker.join().unwrap();
}

#[test]
fn queries_reject_duplicate_and_out_of_scope_hits() {
    let (listener, connection) = fixture();
    let worker = thread::spawn(move || {
        let wrong = |field: &str, value| {
            let mut point = hit(4, 1);
            point[field] = value;
            point
        };
        let mut stale = hit(4, 1);
        stale["payload"]["generation"] = json!(generation_text([23; 16]));
        for points in [
            vec![hit(4, 1), hit(4, 1)],
            vec![hit(4, 0)],
            vec![stale],
            vec![wrong("id", json!(point_id(GENERATION_ID, 5).unwrap()))],
            vec![wrong("score", json!(1.2))],
            vec![wrong("score", json!("NaN"))],
            vec![hit(4, 1), hit(5, 1), hit(6, 1)],
        ] {
            exchange_collection(&listener, collection_info());
            exchange(
                &listener,
                QUERY,
                "200 OK",
                success(json!({"points": points})),
            );
        }
    });
    for _ in 0..7 {
        let result = connection.query(GENERATION_ID, &VECTOR, &[false, true], 2, || Ok(()));
        assert_eq!(result.unwrap_err(), RESPONSE);
    }
    worker.join().unwrap();
}

#[test]
fn accepts_cosine_rounding_overshoot_without_inflating_the_score() {
    let (listener, connection) = fixture();
    let worker = thread::spawn(move || {
        let mut point = hit(4, 1);
        point["score"] = json!(1.0000001192092896_f64);
        exchange_collection(&listener, collection_info());
        exchange(
            &listener,
            QUERY,
            "200 OK",
            success(json!({"points": [point]})),
        );
    });
    let hits = connection
        .query(GENERATION_ID, &VECTOR, &[false, true], 1, || Ok(()))
        .unwrap()
        .hits;
    assert_eq!(hits, [(1.0, 4, 1)]);
    worker.join().unwrap();
}

#[test]
fn zero_or_invalid_strict_query_limit_prevents_vector_dispatch() {
    let (listener, connection) = fixture();
    let server = listener.try_clone().unwrap();
    let worker = thread::spawn(move || {
        for maximum in [json!(0), json!("1")] {
            let mut collection = collection_info();
            collection["config"]["strict_mode_config"] = json!({
                "enabled": true,
                "max_query_limit": maximum,
            });
            exchange_collection(&server, collection);
        }
    });
    for error in [QUERY_DISABLED, RESPONSE] {
        assert_eq!(
            connection
                .query(GENERATION_ID, &VECTOR, &[false, true], 1, || Ok(()))
                .unwrap_err(),
            error
        );
    }
    worker.join().unwrap();
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

#[test]
fn checks_authority_before_dispatch_and_discards_a_response_if_authority_changed() {
    let (listener, connection) = fixture();
    let result = connection.query(GENERATION_ID, &VECTOR, &[false, true], 1, || {
        Err("Review revoked".into())
    });
    assert_eq!(result.unwrap_err(), "Review revoked");
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));

    let worker = thread::spawn(move || {
        exchange_collection(&listener, collection_info());
        exchange(
            &listener,
            QUERY,
            "200 OK",
            success(json!({"points": [hit(4, 1)]})),
        );
    });
    let checks = Cell::new(0);
    let result = connection.query(GENERATION_ID, &VECTOR, &[false, true], 1, || {
        checks.set(checks.get() + 1);
        if checks.get() >= 5 {
            Err("Review revoked".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err(), "Review revoked");
    worker.join().unwrap();
}

#[test]
fn rejects_an_existing_collection_with_different_dimensions_before_writing() {
    let (listener, connection) = fixture();
    let worker = thread::spawn(move || {
        exchange(
            &listener,
            COLLECTION,
            "200 OK",
            success(json!({"config": {"params": {"vectors": {"size": 3, "distance": "Cosine"}}}})),
        );
        listener
    });
    let result = connection.stage(
        GENERATION_ID,
        &[Point {
            source: 1,
            position: 4,
            vector: &VECTOR,
        }],
        || Ok(()),
    );
    assert!(matches!(result, Err(message) if message.contains("vector dimensions")));
    assert!(!connection.staging_attempted());
    let listener = worker.join().unwrap();
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

#[test]
fn requires_strict_payload_indexes_before_attempting_point_writes() {
    let (listener, connection) = fixture();
    let mut collection = collection_info();
    collection["config"]["strict_mode_config"] = json!({
        "enabled": true,
        "unindexed_filtering_retrieve": false,
        "unindexed_filtering_update": false,
    });
    collection["payload_schema"] = json!({
        "generation": {"data_type": "keyword"},
        "source": {"data_type": "integer", "params": {"type": "integer", "lookup": false}},
    });
    let mut ready = collection.clone();
    ready["payload_schema"]["source"]["params"]["lookup"] = json!(true);
    connection.validate_collection(&ready).unwrap();
    let worker = thread::spawn(move || {
        exchange(&listener, COLLECTION, "200 OK", success(collection));
        listener
    });
    let result = connection.stage(
        GENERATION_ID,
        &[Point {
            source: 1,
            position: 4,
            vector: &VECTOR,
        }],
        || Ok(()),
    );
    assert!(matches!(result, Err(message) if message.contains("index with lookup for source")));
    assert!(!connection.staging_attempted());
    let listener = worker.join().unwrap();
    assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
}

struct LiveCollection<'a> {
    connection: &'a Connection,
    name: &'a str,
    owner: String,
    active: bool,
}

impl LiveCollection<'_> {
    fn request(&self, method: Method, body: Option<&Value>) -> Result<Option<Value>, String> {
        self.connection.request(
            method,
            &self.connection.collection,
            body,
            &Authority::new(|| Ok(())),
            None,
        )
    }

    fn owns_current(&self) -> Result<bool, String> {
        let Some(current) = self.request(Method::GET, None)? else {
            return Ok(false);
        };
        if current["config"]["metadata"]["kortty_test_owner"].as_str() != Some(self.owner.as_str())
        {
            return Err("The collection ownership marker changed or is missing.".into());
        }
        Ok(true)
    }

    fn create_index(&self, field: &str, schema: Value) {
        assert!(self.owns_current().unwrap());
        let url = self.connection.route(&["index"], Some(WRITE_ORDERING));
        let body = json!({"field_name": field, "field_schema": schema});
        require_completed(
            self.connection
                .request(
                    Method::PUT,
                    &url,
                    Some(&body),
                    &Authority::new(|| Ok(())),
                    None,
                )
                .unwrap(),
        )
        .expect("Create the synthetic collection's payload index.");
    }

    fn remove(&mut self) -> Result<(), String> {
        if self.owns_current()?
            && !matches!(
                self.request(Method::DELETE, None)?,
                None | Some(Value::Bool(true))
            )
        {
            return Err("Qdrant did not confirm collection cleanup.".into());
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for LiveCollection<'_> {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.remove()
        {
            let _ = writeln!(
                std::io::stderr().lock(),
                "Could not clean up {}. {error}",
                self.name
            );
        }
    }
}

#[test]
#[ignore = "requires an explicit writable Qdrant v1.18.2 test service"]
fn live_qdrant_stages_queries_and_deletes_a_synthetic_generation() {
    const KEY_ENV: &str = "KORTTY_TEST_QDRANT_API_KEY";
    let scale = std::env::var_os("KORTTY_TEST_QDRANT_SCALE").is_some_and(|value| value == "1");
    let strict = std::env::var_os("KORTTY_TEST_QDRANT_STRICT").is_some_and(|value| value == "1");
    let mut settings = Settings {
        endpoint: std::env::var("KORTTY_TEST_QDRANT_ENDPOINT")
            .expect("Set KORTTY_TEST_QDRANT_ENDPOINT to the dedicated Qdrant test service."),
        collection: format!("kortty_test_{}", generation_text(new_generation().unwrap())),
        dimensions: if scale {
            SCALE_DIMENSIONS
        } else {
            VECTOR.len()
        },
        credential_env: std::env::var_os(KEY_ENV).map_or_else(String::new, |_| KEY_ENV.into()),
        vault_key: None,
    };
    let use_vault = std::env::var_os("KORTTY_TEST_QDRANT_VAULT").is_some_and(|value| value == "1");
    let profile = use_vault.then(Profile::new);
    let vault = profile.as_ref().map(|profile| {
        let secret = local_ai::credential_value(KEY_ENV)
            .expect("Set KORTTY_TEST_QDRANT_API_KEY to the synthetic service key for Vault mode.");
        let (vault, id) = VaultWindow::qdrant_fixture(&profile.0, &secret);
        settings.credential_env.clear();
        settings.vault_key = Some(id);
        vault
    });
    let revision = vault.as_ref().and_then(VaultWindow::credential_revision);
    let binding = settings
        .resolve_vault_key(vault.as_ref(), revision.as_ref())
        .unwrap();
    let connection = Connection::new_with_vault(&settings, None, binding).unwrap();
    let mut collection = LiveCollection {
        connection: &connection,
        name: &settings.collection,
        owner: generation_text(new_generation().unwrap()),
        active: false,
    };
    assert!(collection.request(Method::GET, None).unwrap().is_none());
    // Check the unique owner marker before cleanup, even if the create reply is lost.
    collection.active = true;
    let mut body = json!({
        "vectors": {"size": settings.dimensions, "distance": "Cosine"},
        "metadata": {"kortty_test_owner": collection.owner},
    });
    let strict_limit = if scale { SCALE_POINTS } else { 1 };
    if strict {
        body["strict_mode_config"] = json!({
            "enabled": true,
            "max_query_limit": strict_limit,
            "unindexed_filtering_retrieve": false,
            "unindexed_filtering_update": false,
        });
    }
    assert_eq!(
        collection.request(Method::PUT, Some(&body)).unwrap(),
        Some(json!(true))
    );
    assert!(collection.owns_current().unwrap());

    if strict {
        verify_live_strict_indexes(&collection, settings.dimensions, strict_limit);
    }
    if scale {
        round_trip_live_scale(&connection);
    } else {
        round_trip_live_small(&connection, strict);
    }
    collection
        .remove()
        .expect("Remove the owned synthetic Qdrant collection.");
}

fn verify_live_strict_indexes(
    collection: &LiveCollection<'_>,
    dimensions: usize,
    strict_limit: usize,
) {
    let details = collection.request(Method::GET, None).unwrap().unwrap();
    let strict = &details["config"]["strict_mode_config"];
    assert_eq!(strict["enabled"], json!(true));
    assert_eq!(strict["max_query_limit"], json!(strict_limit));
    assert_eq!(strict["unindexed_filtering_retrieve"], json!(false));
    assert_eq!(strict["unindexed_filtering_update"], json!(false));

    let mut vector = vec![0.0; dimensions];
    vector[0] = 1.0;
    let points = [Point {
        source: 0,
        position: 0,
        vector: &vector,
    }];
    let generation = new_generation().unwrap();
    let reject_stage = |reason| {
        let error = collection
            .connection
            .stage(generation, &points, || Ok(()))
            .unwrap_err();
        assert!(error.contains(reason), "{error}");
        assert!(!collection.connection.staging_attempted());
    };
    reject_stage("keyword payload index for generation");
    collection.create_index("generation", json!("keyword"));
    reject_stage("integer payload index with lookup for source");
    collection.create_index(
        "source",
        json!({"type": "integer", "lookup": true, "range": false}),
    );

    let details = collection.request(Method::GET, None).unwrap().unwrap();
    let schema = &details["payload_schema"];
    assert_eq!(schema["generation"]["data_type"], json!("keyword"));
    assert_eq!(schema["source"]["data_type"], json!("integer"));
    assert_eq!(schema["source"]["params"]["lookup"], json!(true));
}

fn round_trip_live_small(connection: &Connection, strict: bool) {
    let generation = new_generation().unwrap();
    let points = [
        Point {
            source: 0,
            position: 3,
            vector: &VECTOR,
        },
        Point {
            source: 1,
            position: 7,
            vector: &[1.0, 0.0],
        },
    ];
    connection.stage(generation, &points, || Ok(())).unwrap();
    let expected_limit = if strict { 1 } else { 2 };
    let query = |enabled| {
        let result = connection
            .query(generation, &VECTOR, enabled, 2, || Ok(()))
            .unwrap();
        assert_eq!(result.limit, expected_limit);
        result.hits
    };
    let all = [(1.0, 3, 0), (0.0, 7, 1)];
    assert_eq!(query(&[true, true]), all[..expected_limit]);
    assert_eq!(query(&[false, true]), [(0.0, 7, 1)]);
    connection.delete(generation, || Ok(())).unwrap();
    assert!(query(&[true, true]).is_empty());
}

fn round_trip_live_scale(connection: &Connection) {
    let generation = new_generation().unwrap();
    let vector = vec![1.0 / (SCALE_DIMENSIONS as f32).sqrt(); SCALE_DIMENSIONS];
    let mut other = vector.clone();
    for component in &mut other[SCALE_DIMENSIONS / 2..] {
        *component = -*component;
    }
    let points: Vec<_> = (0..SCALE_POINTS)
        .map(|position| {
            let source = scale_source(position);
            Point {
                source,
                position,
                vector: if source.is_multiple_of(2) {
                    &vector
                } else {
                    &other
                },
            }
        })
        .collect();
    connection.stage(generation, &points, || Ok(())).unwrap();
    let all = [true; MAX_SOURCES];
    for enabled in [all, scale_filtered_sources()] {
        let hits = connection
            .query(
                generation,
                &vector,
                &enabled,
                scale_positions(&enabled).count(),
                || Ok(()),
            )
            .unwrap()
            .hits;
        assert_complete_scale_hits(&hits, &enabled);
    }
    connection.delete(generation, || Ok(())).unwrap();
    assert!(
        connection
            .query(generation, &vector, &all, SCALE_POINTS, || Ok(()))
            .unwrap()
            .hits
            .is_empty()
    );
}
