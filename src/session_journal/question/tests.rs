use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::ai_manager::Configuration;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Client;
use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, fixture_response, listener, start_test_double,
    wire_request,
};
use crate::session_journal::{catalog, tree};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-question-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn journal(&self, id: &str, title: &str, parts: u32, note: &str) -> PathBuf {
        let path = self.0.join(id);
        fs::create_dir(&path).unwrap();
        write_document(&path, id, title, parts, note);
        path
    }

    fn source(&self, id: &str) -> Source {
        let catalog = catalog(&self.0).unwrap();
        let summary = catalog.journals.iter().find(|row| row.id == id).unwrap();
        Source::catalog(summary).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_document(path: &Path, id: &str, title: &str, parts: u32, note: &str) {
    fs::write(path.join("journal.xml"), format!(
        "<session-journal formatVersion=\"1\" id=\"{id}\"><meta><title>{title}</title><logParts>{parts}</logParts></meta>\
         <entries><entry><id>note</id><kind>USER_NOTE</kind><title>Operator note</title><text>{note}</text></entry></entries></session-journal>"
    )).unwrap();
}

fn write_log(path: &Path, id: &str, part: u32, lines: &[(u64, &str)]) {
    let name = if part == 1 {
        "session-log.json".into()
    } else {
        format!("session-log-{part}.json")
    };
    let mut output =
        json!({"type":"meta", "formatVersion":1, "journalId":id, "part":part}).to_string();
    output.push('\n');
    for (sequence, text) in lines {
        output.push_str(
            &json!({"seq":sequence, "t":"2026-10-01T08:00:00Z", "k":"out", "x":text}).to_string(),
        );
        output.push('\n');
    }
    output.push_str("{\"type\":\"end\"}\n");
    fs::write(path.join(name), output).unwrap();
}

fn masked(value: &str) -> io::Result<String> {
    Ok(value.replace("fixture-secret", "***"))
}

#[test]
fn exact_capture_citations_distinguish_rotated_parts_and_reject_a_changed_row() {
    let fixture = Fixture::new();
    let path = fixture.journal("source", "Journal", 2, "Check build failure");
    write_log(&path, "source", 1, &[(1, "failure first"), (2, "-----BE")]);
    write_log(
        &path,
        "source",
        2,
        &[
            (1, "GIN OPENSSH PRIVATE KEY-----"),
            (2, "failure hidden-key-body"),
            (3, "-----END OPENSSH PRIVATE KEY-----"),
            (1, "failure in second part"),
        ],
    );
    let source = fixture.source("source");
    let review = Review::prepare(
        Question::new("What failure?", &masked).unwrap(),
        vec![source.clone()],
        false,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(!review.prompt.contains("hidden-key-body"));
    assert!(
        review
            .citations
            .iter()
            .any(|citation| matches!(&citation.target, Target::Entry(id) if id == "note"))
    );
    let position = review
        .citations
        .iter()
        .find_map(|citation| match &citation.target {
            Target::Capture(position) if position.part == 2 && position.sequence == 1 => {
                Some(position)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        resolve_capture(&source, position, &|| false).unwrap().text,
        "failure in second part"
    );
    assert!(review.verify(&|| false).is_ok());
    write_log(&path, "source", 2, &[(1, "failure changed in second part")]);
    assert!(resolve_capture(&source, position, &|| false).is_err());
    assert!(review.verify(&|| false).is_err());
}

#[test]
fn journal_answer_transmits_only_the_review_and_rejects_invented_quotes() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let path = fixture.journal(
        "source",
        "Review",
        1,
        "The deploy failed with exit 7. fixture-secret",
    );
    write_log(&path, "source", 1, &[(1, "A different log line")]);
    let source = fixture.source("source");
    let review = Review::prepare(
        Question::new("What deploy failed? fixture-secret", &masked).unwrap(),
        vec![source.clone()],
        false,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(!review.prompt.contains("fixture-secret"));
    assert!(!review.prompt.contains("A different log line"));
    assert!(review.prompt.len() <= MAX_PROMPT_BYTES);

    let (profile, server) = start_test_double(vec![
        fixture_http_response(
            "422 Unprocessable Content",
            "",
            r#"{"error":{"code":"unsupported_response_format"}}"#,
        ),
        fixture_response(
            "200 OK",
            "",
            r#"{"findings":[{"citation":1,"quote":"The deploy failed with exit 7."}]}"#,
        ),
        fixture_response(
            "200 OK",
            "",
            r#"{"findings":[{"citation":1,"quote":"The deploy succeeded."}]}"#,
        ),
    ]);
    let (worker, _cancel) = Answering::begin(profile).unwrap();
    assert!(worker.answer(&review, &|| false, |_| {}).unwrap().ai_used);
    assert!(!worker.answer(&review, &|| false, |_| {}).unwrap().ai_used);
    let requests = server.join().unwrap();
    assert_eq!(requests[0]["response_format"]["type"], "json_object");
    assert!(requests[1].get("response_format").is_none());
    assert_eq!(requests[2]["response_format"]["type"], "json_object");
    for request in requests {
        assert_eq!(request["stream"], true);
        assert_eq!(request["messages"][1]["content"], review.prompt.as_str());
    }
    drop(worker);

    let (endpoint, profile) = listener();
    let (worker, _cancel) = Answering::begin(profile).unwrap();
    assert_eq!(
        worker.answer(&review, &|| true, |_| {}).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    write_document(&path, "source", "Changed", 1, "New note");
    assert!(worker.answer(&review, &|| false, |_| {}).is_err());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_native_answer_transmits_the_bound_system_once() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    fixture.journal(
        "native",
        "Native review",
        1,
        "The deploy failed with exit 7.",
    );
    let mut review = Review::prepare(
        Question::new("Why did the deploy fail?", &masked).unwrap(),
        vec![fixture.source("native")],
        false,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(!review.citations.is_empty());
    let (endpoint, _) = listener();
    let profile = Profile::lm_studio(
        &endpoint.local_addr().unwrap().to_string(),
        "fixture:local",
        Default::default(),
    )
    .unwrap();
    let (worker, _cancel) = Answering::begin(profile.clone()).unwrap();
    assert!(worker.answer(&review, &|| false, |_| {}).is_err());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    review.bind_answer_profile(&profile);
    let [system, input] = review.answer_messages().map(str::to_owned);
    let suffix = profile.structured_system_suffix().unwrap();
    assert!(system.ends_with(suffix));
    assert_eq!(system.matches(suffix).count(), 1);
    let server = std::thread::spawn(move || {
        let mut socket = accept(&endpoint);
        let body: Value =
            serde_json::from_slice(&wire_request(&mut socket, "POST /api/v1/chat HTTP/1.1"))
                .unwrap();
        assert_eq!(body["system_prompt"], system);
        assert_eq!(body["input"], input);
        assert_eq!(body["max_output_tokens"], ANSWER_OUTPUT_LIMIT);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        let event = json!({
            "type": "chat.end",
            "result": {"output": [{"type": "message", "content": r#"{"findings":[]}"#}]}
        });
        socket
            .get_mut()
            .write_all(
                fixture_http_response(
                    "200 OK",
                    "Content-Type: text/event-stream\r\n",
                    &format!("data: {event}\n\n"),
                )
                .as_bytes(),
            )
            .unwrap();
    });
    assert!(worker.answer(&review, &|| false, |_| {}).unwrap().ai_used);
    server.join().unwrap();
}

#[test]
fn journal_https_answer_rejects_an_unreviewed_or_stale_payload_before_tls() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let source = fixture.journal(
        "https",
        "Build review",
        1,
        "The deploy failed with exit 7. fixture-secret",
    );
    let (endpoint, _) = listener();
    let url = format!("https://{}/v1", endpoint.local_addr().unwrap());
    let configuration = |model: &str| -> Configuration {
        serde_json::from_value(json!({
            "profiles": {
                "profiles": [
                    {
                        "id": 1, "name": "Reviewed HTTPS", "endpoint": url, "model": model,
                        "connection_mode": "anthropic", "trusted_remote_endpoint": true
                    },
                    {"id": 2, "name": "Local", "endpoint": "127.0.0.1:8080", "model": "local"}
                ],
                "default": 2, "last_id": 2
            }
        }))
        .unwrap()
    };
    configuration("fixture-reviewed")
        .save_fixture_at(&fixture.0, &Configuration::default())
        .unwrap();
    let saved = Arc::new(Configuration::load_fixture_from(&fixture.0).unwrap());
    let profiles: Vec<_> = saved.journal_question_https_profiles().collect();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].3, format!("{url}/messages"));
    assert!(saved.journal_question_https_transport(2).is_err());
    let (profile, _) = saved.journal_question_https_transport(1).unwrap();
    assert!(Answering::begin(profile.clone()).is_err());
    let mut policy = ClipboardPolicy::default();
    let authority =
        HttpsAuthority::capture(Arc::clone(&saved), 1, &profile, &policy, None).unwrap();
    assert!(authority.matches(&policy, None));
    policy.session_journal_allowed = false;
    assert!(!authority.matches(&policy, None));
    let (worker, _cancel) = Answering::begin_reviewed_https(profile.clone(), authority).unwrap();
    let untouched = |text: &str| Ok(text.to_owned());
    let review = || {
        Review::prepare(
            Question::new("Why did deploy fail? fixture-secret", &untouched).unwrap(),
            vec![fixture.source("https")],
            false,
            &untouched,
            &|| false,
        )
        .unwrap()
    };
    let mut original = review();
    assert!(!original.citations.is_empty());
    assert!(worker.answer(&original, &|| false, |_| {}).is_err());
    let mut too_small = original.clone();
    assert!(too_small.mask_for_https(&profile, 1, &masked).is_err());
    assert_eq!(too_small.answer_messages(), original.answer_messages());
    original.mask_for_https(&profile, 250, &masked).unwrap();
    assert!(!original.citations.is_empty());
    let [system, prompt] = original.answer_messages();
    assert_eq!(
        system
            .matches(profile.structured_system_suffix().unwrap())
            .count(),
        1
    );
    assert!(!prompt.contains("fixture-secret"));
    assert!(prompt.contains("***"));
    assert!(prompt.len() <= 250);

    let mut edited = original.clone();
    edited.citations[0].excerpt.push_str(" added after review");
    assert!(worker.answer(&edited, &|| false, |_| {}).is_err());
    assert_eq!(
        worker
            .answer(&original, &|| true, |_| {})
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );

    write_document(
        &source,
        "https",
        "Changed evidence",
        1,
        "The deploy failed again.",
    );
    assert!(
        worker
            .answer(&original, &|| false, |_| {})
            .unwrap_err()
            .to_string()
            .contains("journal changed")
    );
    let mut fresh = review();
    fresh.mask_for_https(&profile, 250, &masked).unwrap();
    configuration("fixture-changed")
        .save_fixture_at(&fixture.0, &saved)
        .unwrap();
    assert!(
        worker
            .answer(&fresh, &|| false, |_| {})
            .unwrap_err()
            .to_string()
            .contains("saved HTTPS profile")
    );
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "requires the synthetic loopback HTTPS provider fixture and its private CA"]
fn journal_https_native_tls_transmits_only_the_reviewed_answer() {
    let _exclusive = exclusive_run();
    let endpoint: std::net::SocketAddr = std::env::var("KORTTY_TEST_JOURNAL_HTTPS_ENDPOINT")
        .unwrap()
        .parse()
        .unwrap();
    assert!(endpoint.ip().is_loopback());
    let ca = fs::read(std::env::var_os("KORTTY_TEST_JOURNAL_HTTPS_CA").unwrap()).unwrap();
    let key_env = "KORTTY_TEST_JOURNAL_HTTPS_KEY";
    let key = crate::local_ai::credential_value(key_env).unwrap();
    for (mode, model) in [
        ("remote-open-ai", "journal-fixture-openai"),
        ("anthropic", "journal-fixture-anthropic"),
    ] {
        let fixture = Fixture::new();
        fixture.journal(
            "tls",
            "TLS journal",
            1,
            "The deploy failed with exit 7. fixture-secret",
        );
        let configured: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Loopback HTTPS fixture", "model": model,
                    "endpoint": format!("https://{endpoint}/v1"), "connection_mode": mode,
                    "trusted_remote_endpoint": true, "credential_env": key_env
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        configured
            .save_fixture_at(&fixture.0, &Configuration::default())
            .unwrap();
        let saved = Arc::new(Configuration::load_fixture_from(&fixture.0).unwrap());
        let (profile, limit) = saved.journal_question_https_transport(1).unwrap();
        let authority =
            HttpsAuthority::capture(saved, 1, &profile, &ClipboardPolicy::default(), None).unwrap();
        assert_eq!(authority.completion_secret(), Some(key.as_str()));
        let unchanged = |text: &str| Ok(text.to_owned());
        let mut review = Review::prepare(
            Question::new(
                &format!("Why did the deploy fail? fixture-secret {}", key.as_str()),
                &unchanged,
            )
            .unwrap(),
            vec![fixture.source("tls")],
            false,
            &unchanged,
            &|| false,
        )
        .unwrap();
        let mask = |text: &str| Ok(masked(text)?.replace(key.as_str(), "***"));
        review.mask_for_https(&profile, limit, &mask).unwrap();
        assert!(!review.answer_messages()[1].contains(key.as_str()));
        let (mut worker, _cancel) = Answering::begin_reviewed_https(profile, authority).unwrap();
        worker.session.trust_test_server(&ca).unwrap();
        let answer = worker.answer(&review, &|| false, |_| {}).unwrap();
        assert!(answer.ai_used, "expected an HTTPS answer from {mode}");
        assert_eq!(answer.findings.len(), 1);
        assert_eq!(answer.findings[0].quote, "The deploy failed with exit 7.");
    }
}

#[test]
fn cross_journal_ranks_masked_curated_notes_and_excludes_changed_catalog_sources() {
    let fixture = Fixture::new();
    for index in 0..8 {
        fixture.journal(
            &format!("distractor-{index}"),
            "Maintenance",
            1,
            "Routine notes",
        );
    }
    fixture.journal(
        "z-note",
        "Maintenance",
        1,
        "The missing artifact is artifact-ZX99. fixture-secret",
    );
    let replaced = fixture.journal("replaced", "artifact-ZX99", 1, "Earlier artifact-ZX99");
    let catalog = catalog(&fixture.0).unwrap();
    write_document(
        &replaced,
        "replaced",
        "Current record",
        1,
        "No longer relevant",
    );

    let ranking = Ranking::prepare(
        Question::new("Where is artifact-ZX99?", &masked).unwrap(),
        &catalog,
        tree::Filter::All,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(ranking.partial);
    assert_eq!(ranking.lexical()[0].id(), "z-note");
    assert!(
        !ranking
            .candidates
            .iter()
            .any(|candidate| candidate.source.id() == "replaced")
    );
    let card = &ranking.candidates[0].card;
    assert!(card.contains("artifact-ZX99") && card.contains("***"));
    assert!(!card.contains("fixture-secret"));

    let review = Review::prepare(
        ranking.question.clone(),
        ranking.lexical(),
        false,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(review.citations.iter().any(|citation| {
        citation.source.id() == "z-note"
            && matches!(&citation.target, Target::Entry(entry) if entry == "note")
    }));
}

#[test]
fn cross_journal_ranks_and_cites_only_masked_recorded_chat_messages() {
    let fixture = Fixture::new();
    for index in 0..8 {
        fixture.journal(
            &format!("distractor-{index}"),
            "Maintenance",
            1,
            "Routine note",
        );
    }
    let path = fixture.journal("z-chat", "Maintenance", 1, "Routine note");
    let document = path.join("journal.xml");
    let xml = fs::read_to_string(&document).unwrap().replace(
        "</entries>",
        r#"
      <entry><id>agent-card</id><kind>AGENT</kind><title>Agent card</title>
        <chatId>fixture-chat</chatId><chatSnapshot><messages>
          <message><role>assistant</role><content>Artifact-ZX91 was cached. fixture-secret</content>
            <steps><step><type>ACTION</type><command>step-only-command</command>
              <output>step-only-output</output></step></steps></message>
          <message><content>-----BEGIN OPENSSH PRIVATE KEY-----</content></message>
          <message><content>Artifact-ZX91 private-key-body</content></message>
          <message><content>-----END OPENSSH PRIVATE KEY-----</content></message>
        </messages></chatSnapshot>
      </entry>
      <entry><id>malformed-card</id><kind>AGENT</kind><title>Incomplete card</title>
        <chatSnapshot><messages>
          <message><content>Artifact-ZX91 malformed-only</content></message>
          <message><content>one</content><content>two</content></message>
        </messages></chatSnapshot>
      </entry></entries>"#,
    );
    fs::write(&document, &xml).unwrap();

    let catalog = catalog(&fixture.0).unwrap();
    let ranking = Ranking::prepare(
        Question::new("Where was Artifact-ZX91?", &masked).unwrap(),
        &catalog,
        tree::Filter::All,
        &masked,
        &|| false,
    )
    .unwrap();
    assert!(ranking.partial);
    assert_eq!(ranking.lexical()[0].id(), "z-chat");
    let card = &ranking.candidates[0].card;
    assert!(card.contains("Artifact-ZX91 was cached. ***"));
    let review = Review::prepare(
        ranking.question.clone(),
        ranking.lexical(),
        false,
        &masked,
        &|| false,
    )
    .unwrap();
    let citation = review.citations.iter().find(|citation| {
        matches!(&citation.target, Target::ChatEntry(entry) if entry == "agent-card")
    }).unwrap();
    assert_eq!(citation.excerpt, "Artifact-ZX91 was cached. ***");
    assert!(citation.verify_document().is_ok());
    assert!(review.verify(&|| false).is_ok());
    for excluded in [
        "fixture-secret",
        "step-only-",
        "private-key-body",
        "malformed-only",
    ] {
        assert!(!card.contains(excluded));
        assert!(!review.prompt.contains(excluded));
    }

    fs::write(
        &document,
        xml.replace("was cached", "was removed from cache"),
    )
    .unwrap();
    assert!(citation.verify_document().is_err());
    assert!(review.verify(&|| false).is_err());
    let (endpoint, profile) = listener();
    assert!(
        !ranking
            .semantic(&Client::new(profile), &|| false, &|| true)
            .1
    );
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn cross_journal_lexical_fallback_and_prompt_limits_use_only_reviewed_cards() {
    let fixture = Fixture::new();
    let note = format!("Build failure {}", "t".repeat(560));
    for (id, title) in [("one", "Build failure"), ("two", "Routine update")] {
        let path = fixture.journal(id, title, 1, &note);
        let rows: Vec<_> = (1..=8)
            .map(|n| format!("Build failure {n} {}", "x".repeat(570)))
            .collect();
        let lines: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| (i as u64 + 1, row.as_str()))
            .collect();
        write_log(&path, id, 1, &lines);
    }
    let catalog = catalog(&fixture.0).unwrap();
    let question = Question::new("Where was the build failure?", &masked).unwrap();
    let ranking =
        Ranking::prepare(question, &catalog, tree::Filter::All, &masked, &|| false).unwrap();
    let lexical = ranking.lexical();
    assert_eq!(lexical[0].id(), "one");

    for (body, expected) in [
        (r#"{"embeddings":[[1,0],[0,0],[0,1]]}"#, false),
        (r#"{"embeddings":[[1,0],[0,1],[1,0]]}"#, true),
    ] {
        let (endpoint, profile) = listener();
        let server = std::thread::spawn(move || {
            let mut stream = accept(&endpoint);
            let input = wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            let reply = fixture_http_response("200 OK", "", body);
            stream.get_mut().write_all(reply.as_bytes()).unwrap();
            serde_json::from_slice::<Value>(&input).unwrap()
        });
        let (sources, used) = ranking.semantic(&Client::new(profile), &|| false, &|| true);
        assert_eq!(used, expected);
        // Reversing the two semantic ranks ties their fused scores. Lexical order breaks the tie.
        assert!(sources.iter().zip(&lexical).all(|(a, b)| a.same_journal(b)));
        let request = server.join().unwrap();
        let inputs = request["input"].as_array().unwrap();
        assert_eq!(inputs.len(), ranking.candidates.len() + 1);
        assert_eq!(inputs[0], ranking.question.as_str());
        for (input, candidate) in inputs.iter().skip(1).zip(&ranking.candidates) {
            assert_eq!(input.as_str(), Some(candidate.card.as_str()));
        }
    }

    let review = Review::prepare(ranking.question, lexical, false, &masked, &|| false).unwrap();
    assert!(review.partial);
    assert!(review.prompt.len() <= MAX_PROMPT_BYTES);
    assert!(
        review
            .citations
            .iter()
            .any(|citation| citation.source.id() == "one")
    );
    assert!(
        review
            .citations
            .iter()
            .any(|citation| citation.source.id() == "two")
    );
}

#[test]
fn semantic_journal_vectors_persist_across_questions_and_follow_exact_scope_and_revision() {
    let fixture = Fixture::new();
    let pinned = fixture.journal(
        "pinned",
        "Maintenance",
        1,
        "Artifact cached. fixture-secret",
    );
    fixture.journal("other", "Independent", 1, "Separate subject");
    let document = pinned.join("journal.xml");
    let xml = fs::read_to_string(&document)
        .unwrap()
        .replace("</meta>", "<pinned>true</pinned></meta>");
    fs::write(&document, &xml).unwrap();
    let prepare = |question, filter| {
        Ranking::prepare(
            Question::new(question, &masked).unwrap(),
            &catalog(&fixture.0).unwrap(),
            filter,
            &masked,
            &|| false,
        )
        .unwrap()
    };

    let (endpoint, profile) = listener();
    let server_endpoint = endpoint.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let mut stream = accept(&server_endpoint);
            let body = wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            let request = serde_json::from_slice::<Value>(&body).unwrap();
            let input = request["input"].as_array().unwrap().clone();
            let reply = json!({"embeddings": vec![[1, 0]; input.len()]}).to_string();
            let response = fixture_http_response("200 OK", "", &reply);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
            requests.push(input);
        }
        requests
    });

    let initial = prepare("What artifact was cached?", tree::Filter::All);
    assert!(
        initial
            .semantic(&Client::new(profile.clone()), &|| false, &|| true)
            .1
    );
    let vector = pinned.join(".semantic-index").join("embedding-v1");
    let saved = fs::read(&vector).unwrap();
    assert!(
        !saved
            .windows(b"fixture-secret".len())
            .any(|window| window == b"fixture-secret")
    );
    assert!(
        !saved
            .windows(b"Artifact cached".len())
            .any(|window| window == b"Artifact cached")
    );

    let scoped = prepare("Where is the package?", tree::Filter::Pinned);
    let (sources, used) = scoped.semantic(&Client::new(profile.clone()), &|| false, &|| true);
    assert!(used);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id(), "pinned");
    assert_eq!(fs::read(&vector).unwrap(), saved);

    let modified = fs::metadata(&document).unwrap().modified().unwrap();
    fs::write(&document, xml.replace("cached", "edited")).unwrap();
    fs::File::options()
        .write(true)
        .open(&document)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = prepare("What was edited?", tree::Filter::Pinned);
    assert!(
        !scoped
            .semantic(&Client::new(profile.clone()), &|| false, &|| true)
            .1
    );
    assert!(
        !changed
            .semantic(&Client::new(profile.clone()), &|| false, &|| false)
            .1
    );
    assert!(
        changed
            .semantic(&Client::new(profile), &|| false, &|| true)
            .1
    );
    assert_ne!(fs::read(&vector).unwrap(), saved);

    let requests = server.join().unwrap();
    assert_eq!(requests[0].len(), 3);
    assert_eq!(requests[1], [json!("Where is the package?")]);
    assert_eq!(requests[2].len(), 2);
    let latest = requests[2][1].as_str().unwrap();
    assert!(latest.contains("Artifact edited. ***"));
    assert!(!latest.contains("Separate subject"));
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
