use std::fs;
use std::io::{BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

use super::super::super::{ProfileRef, RetiredIndex, Role, State};
use super::super::{Embedding, Location, Passage, Store, sha256, source::SyncMode, sync};
use super::*;
use crate::ai_manager::Configuration;
use crate::local_ai::test_support;
use crate::snippets::tests::Profile;
use crate::vault_window::VaultWindow;

const GET_COLLECTION: &str = "GET /collections/fixture HTTP/1.1";
const PUT_POINTS: &str = "PUT /collections/fixture/points?wait=true&ordering=strong HTTP/1.1";
const DELETE_POINTS: &str =
    "POST /collections/fixture/points/delete?wait=true&ordering=strong HTTP/1.1";
const QUERY_POINTS: &str = "POST /collections/fixture/points/query?consistency=all HTTP/1.1";
const MASKED_QUERY: &str = "Find the approved text for ***";

fn fixture(
    root: &Path,
    address: SocketAddr,
    texts: &[&str],
    vault_key: Option<String>,
) -> (Scope, Index) {
    let mut store = Store::new("Qdrant fixture").unwrap();
    store.qdrant = Some(qdrant::Settings {
        endpoint: format!("http://{address}"),
        collection: "fixture".into(),
        dimensions: 2,
        vault_key,
        ..qdrant::Settings::default()
    });
    let mut sources = Vec::new();
    let mut entries = Vec::new();
    for (source, &text) in texts.iter().enumerate() {
        let path = root.join(format!("source-{source}.txt"));
        fs::write(&path, text).unwrap();
        store.add_source(path).unwrap();
        sources.push(Some(sha256(text.as_bytes()).unwrap()));
        let vector = if source == 0 {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        };
        entries.push((
            Passage {
                source,
                location: Location::Line(1),
                text: text.into(),
            },
            vector,
        ));
    }
    let scope = Scope::new(&store, "127.0.0.1:1234", "fixture:embedding").unwrap();
    let index = Index::from_parts(scope.digest, sources, entries, 2, None).unwrap();
    (scope, index)
}

fn respond(socket: &mut BufReader<TcpStream>, result: Value) {
    let body = json!({"status":"ok", "result":result}).to_string();
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "Content-Type: application/json\r\n",
                &body,
            )
            .as_bytes(),
        )
        .unwrap();
}

fn receive_json(listener: &TcpListener, request: &str) -> (BufReader<TcpStream>, Value) {
    let mut socket = test_support::accept(listener);
    let body = test_support::wire_request(&mut socket, request);
    (socket, serde_json::from_slice(&body).unwrap())
}

fn respond_collection(listener: &TcpListener, strict: Value) {
    let mut collection = test_support::accept(listener);
    assert!(test_support::wire_request(&mut collection, GET_COLLECTION).is_empty());
    respond(
        &mut collection,
        json!({"config":{
            "params":{"vectors":{"size":2,"distance":"Cosine"}},
            "strict_mode_config":strict,
        }}),
    );
}

fn staged_points(listener: &TcpListener, before_ack: impl FnOnce()) -> Vec<Value> {
    respond_collection(listener, Value::Null);
    let (mut socket, upsert) = receive_json(listener, PUT_POINTS);
    let points = upsert["points"].as_array().unwrap().clone();
    before_ack();
    respond(&mut socket, json!({"status":"completed"}));
    points
}

#[test]
fn qdrant_publication_keeps_sources_beyond_the_legacy_receipt_mask() {
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let (mut scope, index) = fixture(
        &profile.0,
        listener.local_addr().unwrap(),
        &["Reviewed source text"; 65],
        None,
    );
    scope.enabled.fill(false);
    for position in [1, 32, 64] {
        scope.enabled[position] = true;
    }
    let worker = thread::spawn(move || {
        let points = staged_points(&listener, || {});
        assert_eq!(
            points
                .iter()
                .map(|point| point["payload"]["source"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 32, 64],
        );
    });
    let cache = Cache::new(profile.0.clone());
    publish(
        &cache,
        &scope,
        &index,
        scope.enabled_sources(),
        None,
        None,
        || Ok(()),
    )
    .unwrap();
    let reopened = cache.load(&scope).unwrap();
    assert_eq!(
        require_coverage(&reopened, &scope).unwrap().sources,
        scope.enabled_sources()
    );
    scope.enabled[63] = true;
    assert!(require_coverage(&reopened, &scope).is_err());
    worker.join().unwrap();
}

#[test]
fn scheduled_qdrant_authority_reuses_reviewed_evidence_without_network() {
    for authenticated in [false, true] {
        exercise_scheduled_qdrant_authority(authenticated);
    }
}

fn exercise_scheduled_qdrant_authority(authenticated: bool) {
    use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

    const MASTER: &[u8] = b"scheduled-qdrant-fixture-master";
    let fixture = Profile::new();
    let root = &fixture.0;
    let source = root.join("guide.txt");
    let text = "Reviewed guide says to deploy on Tuesday.";
    fs::write(&source, text).unwrap();
    let (embed_listener, _) = test_support::listener();
    let (qdrant_listener, _) = test_support::listener();
    let embed = embed_listener.local_addr().unwrap().to_string();
    let qdrant = format!("http://{}", qdrant_listener.local_addr().unwrap());
    let vault_path = crate::profile::vault_path_at(root);
    let credential = |id| CredentialInput {
        id,
        label: "Scheduled Qdrant test key".into(),
        kind: CredentialKind::QdrantApiKey,
        ..CredentialInput::default()
    };
    let (vault, key_id) = if authenticated {
        fs::create_dir(vault_path.parent().unwrap()).unwrap();
        let mut vault = Vault::create(&vault_path, MASTER).unwrap();
        let id = vault
            .save(credential(None), Secret::new("initial-key".into()))
            .unwrap();
        (
            Some(VaultWindow::with_unlocked_fixture(
                vault_path.clone(),
                vault,
            )),
            Some(id),
        )
    } else {
        (None, None)
    };
    let make_config = |collection, key_id: Option<&str>, env: Option<&str>| {
        let mut store = Store::new("Scheduled Qdrant fixture").unwrap();
        store.autonomous_enabled = true;
        store.add_source(source.clone()).unwrap();
        store.qdrant = Some(qdrant::Settings {
            endpoint: qdrant.clone(),
            collection,
            dimensions: 2,
            vault_key: key_id.map(str::to_owned),
            credential_env: env.unwrap_or_default().into(),
        });
        let mut config: Configuration = serde_json::from_value(json!({
            "profiles": {"profiles": [{"id":1, "name":"Fixture AI profile", "model":"fixture",
                "endpoint":"127.0.0.1:11434"}], "default":1, "last_id":1}
        }))
        .unwrap();
        config.local = State {
            endpoint: embed.clone(),
            embedding: "fixture".into(),
            stores: vec![store],
            ..State::default()
        };
        config
    };
    let config = make_config("fixture".into(), key_id.as_deref(), None);
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    let current = || Configuration::load_fixture_from(root).unwrap();
    let plan = || current().autonomous_knowledge(1, root).pop().unwrap();
    assert!(plan().supports_standing_review());
    assert!(
        !make_config("fixture".into(), None, Some("KORTTY_QDRANT_FIXTURE_KEY"))
            .autonomous_knowledge(1, root)
            .pop()
            .unwrap()
            .supports_standing_review()
    );
    if authenticated {
        assert!(
            plan()
                .for_standing_review()
                .start(MASKED_QUERY.into(), 4096, None)
                .is_err()
        );
    }
    let saved = current();
    let scope = saved.local.scope(&saved.local.stores[0], root).unwrap();
    let index = Index::from_parts(
        scope.digest,
        vec![Some(sha256(text.as_bytes()).unwrap())],
        vec![(
            Passage {
                source: 0,
                location: Location::Line(1),
                text: text.into(),
            },
            vec![1.0, 0.0],
        )],
        2,
        None,
    )
    .unwrap();
    let cache = Cache::new(root.clone());
    let embed_server = embed_listener.try_clone().unwrap();
    let embed_worker = thread::spawn(move || {
        let (mut socket, wire) = receive_json(&embed_server, "POST /api/embed HTTP/1.1");
        assert_eq!(wire["input"], json!([MASKED_QUERY]));
        let response =
            test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1.0,0.0]]}"#);
        socket.get_mut().write_all(response.as_bytes()).unwrap();
    });
    let qdrant_server = qdrant_listener.try_clone().unwrap();
    let qdrant_worker = thread::spawn(move || {
        let points = staged_points(&qdrant_server, || {});
        assert_eq!(points.len(), 1);
        respond_collection(&qdrant_server, Value::Null);
        let (mut socket, query) = receive_json(&qdrant_server, QUERY_POINTS);
        assert_eq!(query["query"], json!([1.0, 0.0]));
        assert_eq!(query["limit"], 1);
        respond(
            &mut socket,
            json!({"points":[{"id":points[0]["id"],"score":1.0,"payload":points[0]["payload"]}]}),
        );
    });
    let settings = scope.qdrant.as_ref().unwrap();
    let key = settings.resolve_vault_key(vault.as_ref(), None).unwrap();
    publish(&cache, &scope, &index, 1.into(), None, key, || Ok(())).unwrap();
    let receipt = cache.load(&scope).unwrap().qdrant_receipt.unwrap();
    let request = plan()
        .for_standing_review()
        .start(MASKED_QUERY.into(), 4096, vault.as_ref())
        .unwrap();
    let retrieved = request.reply.recv_blocking().unwrap().unwrap();
    embed_worker.join().unwrap();
    qdrant_worker.join().unwrap();
    assert!(retrieved.context().contains("Tuesday"));
    let wire = serde_json::to_value(retrieved.authority().unwrap()).unwrap();
    assert_eq!(wire.get("vault_envelope").is_some(), authenticated);
    assert!(!wire.to_string().contains("initial-key"));
    let authority = serde_json::from_value(wire).unwrap();
    drop(vault);
    plan().require_authority(&authority, || Ok(())).unwrap();
    assert!(
        plan()
            .require_authority(&authority, || Err("Fixture policy revoked".into()))
            .is_err()
    );
    let error = plan()
        .require_authority_with_policy(
            &authority,
            || Ok(()),
            || ClipboardPolicy {
                ai_allowed: false,
                ..ClipboardPolicy::default()
            },
        )
        .unwrap_err();
    assert!(error.contains("disallows Qdrant"), "{error}");

    fs::write(&source, "Changed source after review.").unwrap();
    assert!(plan().require_authority(&authority, || Ok(())).is_err());
    fs::write(&source, text).unwrap();
    cache
        .save_with_receipt(
            &index,
            &QdrantReceipt {
                generation: [7; 16],
                sources: 1.into(),
            },
            || Ok(()),
        )
        .unwrap();
    assert!(plan().require_authority(&authority, || Ok(())).is_err());
    cache
        .save_with_receipt(&index, &receipt, || Ok(()))
        .unwrap();
    plan().require_authority(&authority, || Ok(())).unwrap();
    let new_route = make_config("changed".into(), key_id.as_deref(), None);
    new_route.save_fixture_at(root, &current()).unwrap();
    assert!(plan().require_authority(&authority, || Ok(())).is_err());
    config.save_fixture_at(root, &current()).unwrap();

    if let Some(id) = key_id {
        let mut vault = Vault::unlock(&vault_path, MASTER).unwrap();
        vault
            .save(credential(Some(id)), Secret::new("rotated-key".into()))
            .unwrap();
        let error = plan().require_authority(&authority, || Ok(())).unwrap_err();
        assert!(error.contains("vault envelope changed"), "{error}");
    }
    for listener in [embed_listener, qdrant_listener] {
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn qdrant_retirement_waits_until_the_new_settings_are_durable() {
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let (scope, index) = fixture(
        &profile.0,
        listener.local_addr().unwrap(),
        &["Reviewed previous source"],
        None,
    );
    let cache = Cache::new(profile.0.clone());
    let receipt = QdrantReceipt {
        generation: [9; 16],
        sources: 1.into(),
    };
    cache
        .save_with_receipt(&index, &receipt, || Ok(()))
        .unwrap();
    let indexes = super::super::super::SaveIndexes {
        cache: cache.clone(),
        retained: Vec::new(),
        staged: Vec::new(),
        retired: vec![RetiredIndex {
            scope: scope.clone(),
            vault_key: Ok(None),
        }],
        automatic: None,
    };
    let warning = indexes
        .commit(
            || Ok(()),
            || Err(io::Error::other("Fixture synchronization failure")),
            None,
            true,
        )
        .unwrap()
        .unwrap();
    assert!(warning.contains("previous Qdrant generations were retained"));
    assert_eq!(cache.load(&scope).unwrap().qdrant_receipt, Some(receipt));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn qdrant_publication_failure_after_staging_preserves_the_prior_receipt() {
    require_policy().unwrap();
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let (scope, index) = fixture(
        &profile.0,
        listener.local_addr().unwrap(),
        &["Verified source text"],
        None,
    );
    let source = scope.sources[0].clone();
    let server = thread::spawn(move || {
        let first = staged_points(&listener, || {});
        let second = staged_points(&listener, || {
            fs::write(source, "Changed during upsert").unwrap()
        });
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        for point in [&first[0], &second[0]] {
            assert_eq!(point["vector"], json!([1.0, 0.0]));
            assert_eq!(point["payload"]["position"], 0);
            assert_eq!(point["payload"]["source"], 0);
            assert_eq!(point["payload"].as_object().unwrap().len(), 3);
            assert!(!point.to_string().contains("Verified source text"));
        }
        let prior = first[0]["payload"]["generation"].as_str().unwrap();
        let abandoned = second[0]["payload"]["generation"].as_str().unwrap();
        assert_ne!(prior, abandoned);
        let (mut socket, deletion) = receive_json(&listener, DELETE_POINTS);
        assert_eq!(
            deletion,
            json!({"filter":{"must":[{"key":"generation","match":{"value":abandoned}}]}})
        );
        respond(&mut socket, json!({"status":"completed"}));
        prior.to_owned()
    });
    let cache = Cache::new(profile.0.clone());
    assert!(
        !publish(
            &cache,
            &scope,
            &index,
            scope.enabled_sources(),
            None,
            None,
            || Ok(())
        )
        .unwrap()
        .cleanup_warning
    );
    let before = cache.load(&scope).unwrap().qdrant_receipt.unwrap();
    let error = publish(
        &cache,
        &scope,
        &index,
        scope.enabled_sources(),
        None,
        None,
        || Ok(()),
    )
    .err()
    .expect("source revalidation must reject the staged generation");
    assert!(error.to_string().contains("source changed"), "{error}");
    let wire_generation = server.join().unwrap();
    assert_eq!(
        wire_generation,
        format!("{:032x}", u128::from_be_bytes(before.generation))
    );
    assert_eq!(cache.load(&scope).unwrap().qdrant_receipt, Some(before));
}

#[test]
fn revoked_vault_key_cannot_activate_a_staged_qdrant_generation() {
    require_policy().unwrap();
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let server_listener = listener.try_clone().unwrap();
    let id = "01".repeat(16);
    let (scope, index) = fixture(
        &profile.0,
        listener.local_addr().unwrap(),
        &["Verified source text"],
        Some(id.clone()),
    );
    let revision = Arc::new(());
    let vault_key = qdrant::VaultKey::for_test(id, "synthetic-key", Arc::downgrade(&revision));
    let cache = Cache::new(profile.0.clone());
    let before = QdrantReceipt {
        generation: [9; 16],
        sources: 1.into(),
    };
    cache.save_with_receipt(&index, &before, || Ok(())).unwrap();
    let server = thread::spawn(move || {
        staged_points(&server_listener, || drop(revision));
    });
    let error = publish(
        &cache,
        &scope,
        &index,
        1.into(),
        None,
        Some(vault_key),
        || Ok(()),
    )
    .err()
    .expect("a revoked Qdrant key must invalidate publication");
    assert!(error.to_string().contains("vault credential"), "{error}");
    server.join().unwrap();
    assert_eq!(cache.load(&scope).unwrap().qdrant_receipt, Some(before));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
fn qdrant_query_requires_coverage_and_resolves_only_verified_local_positions() {
    require_policy().unwrap();
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let server_listener = listener.try_clone().unwrap();
    let (mut scope, index) = fixture(
        &profile.0,
        listener.local_addr().unwrap(),
        &[
            "First verified text",
            "Second verified text",
            "Retained private text",
        ],
        None,
    );
    scope.enabled[2] = false;
    let server = thread::spawn(move || {
        let points = staged_points(&server_listener, || {});
        assert_eq!(points.len(), 2);
        let generation = points[0]["payload"]["generation"].as_str().unwrap();
        assert_eq!(points[1]["payload"]["generation"], generation);
        let first = json!({"id":points[0]["id"],"score":0.1,"payload":points[0]["payload"]});
        let second = json!({"id":points[1]["id"],"score":0.9,"payload":points[1]["payload"]});
        let mut mismatched = second.clone();
        mismatched["payload"]["source"] = json!(0);
        for (maximum, limit, hits) in [
            (10, 2, vec![second.clone(), first.clone()]),
            (1, 1, vec![second.clone()]),
            (1, 1, Vec::new()),
            (1, 1, vec![second, first]),
            (1, 1, vec![mismatched]),
        ] {
            respond_collection(
                &server_listener,
                json!({"enabled": true, "max_query_limit": maximum}),
            );
            let (mut socket, wire) = receive_json(&server_listener, QUERY_POINTS);
            assert_eq!(
                wire,
                json!({
                    "query":[1.0,0.0], "limit":limit,
                    "with_payload":["generation","source","position"], "with_vector":false,
                    "filter":{"must":[
                        {"key":"generation","match":{"value":generation}},
                        {"key":"source","match":{"any":[0,1]}}
                    ]}
                })
            );
            respond(&mut socket, json!({"points":hits}));
        }
    });
    let cache = Cache::new(profile.0.clone());
    publish(
        &cache,
        &scope,
        &index,
        scope.enabled_sources(),
        None,
        None,
        || Ok(()),
    )
    .unwrap();
    let restored = cache.load(&scope).unwrap();
    assert_eq!(
        restored.qdrant_receipt.as_ref().unwrap().sources,
        0b011.into()
    );
    let client = Client::new(scope.profile.clone());
    let vector = [1.0, 0.0];
    let search = || query(&client, &scope, &restored, &vector, 2, None, || Ok(()));
    assert_eq!(search().unwrap(), [(1.0, 0), (0.0, 1)]);
    assert_eq!(search().unwrap(), [(0.0, 1)]);
    for error in [
        "incomplete generation",
        "invalid or unsuccessful response",
        "outside the verified knowledge index",
    ] {
        assert!(search().unwrap_err().contains(error));
    }
    server.join().unwrap();

    scope.enabled[2] = true;
    assert!(
        query(&client, &scope, &restored, &vector, 2, None, || Ok(()))
            .unwrap_err()
            .contains("include the enabled sources")
    );
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
fn automatic_qdrant_coverage_carries_only_previous_approved_enabled_sources() {
    let profile = Profile::new();
    let (mut saved, mut index) = fixture(
        &profile.0,
        "127.0.0.1:6333".parse().unwrap(),
        &[
            "New automatic text",
            "Retained approved text",
            "Unapproved manual text",
            "Disabled retained text",
        ],
        None,
    );
    saved.enabled[3] = false;
    let mut reviewed = saved.clone();
    reviewed.enabled = vec![true, false, false, false];
    let mut previous = saved.clone();
    previous.sources.rotate_left(1);
    index.qdrant_receipt = Some(QdrantReceipt {
        generation: [7; 16],
        sources: 0b0101.into(),
    });
    assert_eq!(
        automatic_coverage(&saved, &reviewed, &previous, Some(&index)),
        0b0011.into()
    );
    assert_eq!(
        automatic_coverage(&saved, &reviewed, &previous, None),
        0b0001.into()
    );
}

#[test]
fn automatic_qdrant_sync_embeds_only_changed_source_and_keeps_approved_manual_vectors() {
    let _exclusive = test_support::exclusive_run();
    require_policy().unwrap();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let automatic = root.join("automatic.txt");
    let manual = root.join("manual.txt");
    fs::write(&automatic, "Original automatic text").unwrap();
    fs::write(&manual, "Approved manual text").unwrap();
    let (embedding_server, _) = test_support::listener();
    let (qdrant_server, _) = test_support::listener();
    let (mut vault, key_id) = VaultWindow::qdrant_fixture(&root, "synthetic-automatic-key");

    let mut store = Store::new("Automatic Qdrant fixture").unwrap();
    store.add_source(automatic.clone()).unwrap();
    store.add_source(manual).unwrap();
    store.set_file_sync(automatic.clone(), SyncMode::Automatic);
    store.coding_profile = Some(ProfileRef::Numeric(1));
    store.embedding = Some(Embedding {
        endpoint: embedding_server.local_addr().unwrap().to_string(),
        model: "embedding-fixture".into(),
        managed_model: None,
    });
    store.automatic_embedding = store.embedding.clone();
    store.qdrant = Some(qdrant::Settings {
        endpoint: format!("http://{}", qdrant_server.local_addr().unwrap()),
        collection: "fixture".into(),
        dimensions: 2,
        vault_key: Some(key_id),
        ..qdrant::Settings::default()
    });
    store.automatic_qdrant = store.qdrant.clone();
    let mut baseline: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [{"id":1, "name":"Fixture coding profile", "model":"fixture:local",
            "endpoint":embedding_server.local_addr().unwrap().to_string()}], "default":1, "last_id":1}
    })).unwrap();
    baseline.local = State {
        stores: vec![store],
        ..State::default()
    };
    baseline.local.roles[Role::Coding as usize] = Some(ProfileRef::Numeric(1));
    baseline
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let baseline = Arc::new(Configuration::load_fixture_from(&root).unwrap());
    assert!(sync::available(&baseline));
    let scope = baseline
        .local
        .scope(&baseline.local.stores[0], &root)
        .unwrap();
    let cache = Cache::new(root.clone());

    let embedding = thread::spawn(move || {
        for (input, vectors) in [
            (
                json!(["Original automatic text", "Approved manual text"]),
                json!([[1.0, 0.0], [0.0, 1.0]]),
            ),
            (json!(["Edited automatic text"]), json!([[-1.0, 0.0]])),
            (json!([MASKED_QUERY]), json!([[0.0, 1.0]])),
            (json!([MASKED_QUERY]), json!([[0.0, 1.0]])),
        ] {
            let (mut socket, wire) = receive_json(&embedding_server, "POST /api/embed HTTP/1.1");
            assert_eq!(
                wire,
                json!({"model":"embedding-fixture","input":input,"truncate":false})
            );
            socket
                .get_mut()
                .write_all(
                    test_support::fixture_http_response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        &json!({"embeddings":vectors}).to_string(),
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
    });
    let server_cache = cache.clone();
    let server_scope = scope.clone();
    let remote = thread::spawn(move || {
        let prior = staged_points(&qdrant_server, || {});
        let current = staged_points(&qdrant_server, || {});
        assert_eq!(prior.len(), 2);
        assert_eq!(current.len(), 2);
        for (position, point) in current.iter().enumerate() {
            assert_eq!(point["payload"]["position"], position);
            assert_eq!(point["payload"]["source"], position);
        }
        assert_eq!(prior[0]["vector"], json!([1.0, 0.0]));
        assert_eq!(current[0]["vector"], json!([-1.0, 0.0]));
        assert_eq!(prior[1]["vector"], json!([0.0, 1.0]));
        assert_eq!(current[1]["vector"], prior[1]["vector"]);
        let old = prior[0]["payload"]["generation"].as_str().unwrap();
        let new = current[0]["payload"]["generation"].as_str().unwrap();
        assert_ne!(old, new);
        assert_eq!(current[1]["payload"]["generation"], new);
        let (mut socket, deletion) = receive_json(&qdrant_server, DELETE_POINTS);
        assert_eq!(
            deletion,
            json!({"filter":{"must":[{"key":"generation","match":{"value":old}}]}})
        );
        let receipt = server_cache
            .load(&server_scope)
            .unwrap()
            .qdrant_receipt
            .unwrap();
        assert_eq!(
            format!("{:032x}", u128::from_be_bytes(receipt.generation)),
            new
        );
        assert_eq!(receipt.sources, 0b11.into());
        respond(&mut socket, json!({"status":"completed"}));
        let automatic = json!({"id":current[0]["id"],"score":0.9,"payload":current[0]["payload"]});
        let manual = json!({"id":current[1]["id"],"score":0.1,"payload":current[1]["payload"]});
        for (strict, hits) in [
            (
                json!({"enabled":false,"max_query_limit":1}),
                vec![automatic, manual.clone()],
            ),
            (json!({"enabled":true,"max_query_limit":1}), vec![manual]),
        ] {
            respond_collection(&qdrant_server, strict);
            let (mut socket, wire) = receive_json(&qdrant_server, QUERY_POINTS);
            assert_eq!(wire["query"], json!([0.0, 1.0]));
            assert_eq!(wire["limit"], hits.len());
            assert_eq!(
                wire["filter"]["must"],
                json!([
                    {"key":"generation","match":{"value":new}},
                    {"key":"source","match":{"any":[0,1]}}
                ])
            );
            respond(&mut socket, json!({"points":hits}));
        }
    });

    let client = Client::new(scope.profile.clone());
    let index = Index::build(&client, &scope).unwrap();
    let key = scope
        .qdrant
        .as_ref()
        .unwrap()
        .resolve_vault_key(Some(&vault), None)
        .unwrap();
    publish(
        &cache,
        &scope,
        &index,
        scope.enabled_sources(),
        None,
        key,
        || Ok(()),
    )
    .unwrap();
    let prior = cache.load(&scope).unwrap().qdrant_receipt.unwrap();
    fs::write(&automatic, "Edited automatic text").unwrap();
    let pending = sync::start(Arc::clone(&baseline), 0, Some(&vault))
        .unwrap()
        .pending
        .unwrap();
    let result = pending.reply.recv_blocking().unwrap().unwrap();
    drop(pending);
    assert!(
        result.changed && result.saved.is_none(),
        "{}",
        result.status
    );
    assert!(
        result.status.contains("updated 1 files"),
        "{}",
        result.status
    );
    let updated = cache.load(&scope).unwrap();
    assert_ne!(updated.qdrant_receipt, Some(prior));
    assert_eq!(updated.entries[0].0.text, "Edited automatic text");
    assert_eq!(updated.entries[1].0.text, "Approved manual text");
    assert_eq!(updated.entries[1].1, [0.0, 1.0]);
    updated.check_sources(&scope, || Ok(())).unwrap();

    let reopened = Configuration::load_fixture_from(&root).unwrap();
    let plan = reopened
        .knowledge_for(Role::Coding, 1, &root)
        .pop()
        .unwrap();
    assert!(
        plan.qdrant_destination()
            .unwrap()
            .ends_with("/collections/fixture")
    );
    let retrieve = || {
        let pending = plan
            .clone()
            .start(MASKED_QUERY.into(), 16_384, Some(&vault))
            .unwrap();
        pending.reply.recv_blocking().unwrap().unwrap()
    };
    let retrieved = retrieve();
    assert_eq!(
        retrieved
            .citations()
            .iter()
            .map(|citation| citation.file.as_str())
            .collect::<Vec<_>>(),
        ["manual.txt", "automatic.txt"]
    );
    assert!(retrieved.context().contains("Approved manual text"));
    assert!(retrieved.context().contains("Edited automatic text"));
    let retrieved = retrieve();
    assert_eq!(retrieved.citations().len(), 1);
    assert_eq!(retrieved.citations()[0].file, "manual.txt");
    assert!(retrieved.context().contains("Approved manual text"));
    assert!(!retrieved.context().contains("Edited automatic text"));
    retrieved.require_current(&client).unwrap();
    let _ = vault.update(crate::vault_window::Message::Lock);
    assert!(
        retrieved
            .require_current(&client)
            .unwrap_err()
            .contains("vault credential")
    );
    assert!(
        plan.start(MASKED_QUERY.into(), 16_384, Some(&vault))
            .is_err()
    );
    assert!(
        sync::start(baseline, 0, Some(&vault))
            .unwrap()
            .pending
            .is_err()
    );
    embedding.join().unwrap();
    remote.join().unwrap();
}
