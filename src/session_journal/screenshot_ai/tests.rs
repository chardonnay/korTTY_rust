use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::local_ai::test_support::{
    accept, fixture_http_response, listener, request, start_test_double, wire_request,
};
use crate::local_ai::{Profile, ProfileOptions, PromptPreset, VisionMode};
use crate::private_files::{Directory, nonce};
use crate::session_journal::{DOCUMENT, viewer::Source};
use crate::terminal_logging::{Policy, Secrets};

const REFERENCE: &str = "screenshots/capture.png";
const SECRET: &str = "test-secret-89de";
const CAPTION: &str = "The user selected a file. \"```\" test-secret-89de";

struct Fixture {
    root: PathBuf,
    screenshot: Screenshot,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-screenshot-ai-{}", nonce().unwrap()));
        let journal = root.join("journal");
        fs::create_dir_all(journal.join("screenshots")).unwrap();
        fs::write(
            journal.join(DOCUMENT),
            format!(
                r#"<session-journal id="local-vision-fixture" formatVersion="1">
                <meta><title>Private journal</title><appLanguageCode>de-DE</appLanguageCode>
                    <host>host-private.example</host><username>user-private</username></meta>
                <entries><entry><id>image</id><kind>SCREENSHOT</kind>
                    <screenshotFile>{REFERENCE}</screenshotFile>
                    <createdAt>2026-11-07T07:00:00Z</createdAt>
                    <text>{CAPTION}</text>
                    <userNote>unrelated-private-note</userNote>
                </entry></entries></session-journal>"#
            ),
        )
        .unwrap();
        let mut raster = Pixmap::new(1301, 10).unwrap();
        raster.fill(Color::from_rgba8(10, 40, 70, 255));
        fs::write(journal.join(REFERENCE), raster.encode_png().unwrap()).unwrap();
        raster.fill(Color::from_rgba8(220, 0, 0, 255));
        fs::write(
            journal.join("screenshots/capture.orig.png"),
            raster.encode_png().unwrap(),
        )
        .unwrap();
        let opened = Directory::open(&journal, false).unwrap();
        let source =
            Source::from_open_directory(&journal, "local-vision-fixture", &opened).unwrap();
        let screenshot = Screenshot::review(&source, "image", REFERENCE).unwrap();
        Self { root, screenshot }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn journal_screenshot_analysis_sends_visible_png_and_redacted_caption_to_loopback() {
    let fixture = Fixture::new();
    let secrets = Secrets::default();
    secrets.add(SECRET);
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
    let unsupported = json!({"error":{"param":"response_format","code":"unsupported_parameter"}});
    let answer = json!({
        "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15},
        "choices":[{"message":{"content":[
        {"type":"text","text":"<think>Internal reasoning only</think>\n```json"},
        {"type":"text","text":format!(r##"{{"description":"Dateimanager zeigt eine Datei. {SECRET}",
            "tags":["#Midnight-Commander","DATEIMANAGER","dateimanager","linux","Red_Theme",
            "user-private","host-private",{{}},"datei","auswahl","ignored"]}}"##)},
        {"type":"text","text":"```"}
    ]},"finish_reason":"stop"}]});
    let (profile, server) = start_test_double(vec![
        fixture_http_response("400 Bad Request", "", &unsupported.to_string()),
        fixture_http_response("200 OK", "", &answer.to_string()),
    ]);
    let profile = profile.with_prompt_preset(PromptPreset::Gemma);
    let base = system_prompt("de-DE");
    let expected_system = profile.render_system_prompt(&base);
    let analyzed = analyze(&Client::new(profile), &fixture.screenshot, Some(&redactor));
    let usage = analyzed.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(3));
    assert_eq!(usage.total_tokens, Some(15));
    assert_eq!(usage.calls, 1);
    assert!(usage.local);
    assert!(!usage.partial);
    let analysis = analyzed.result.unwrap();
    let requests = server.join().unwrap();
    assert_eq!(analysis.model, "fixture:local");
    assert!(
        analysis
            .description
            .starts_with("Dateimanager zeigt eine Datei.")
    );
    assert!(!analysis.description.contains(SECRET));
    assert_eq!(
        analysis.tags,
        ["midnight-commander", "dateimanager", "datei", "auswahl"]
    );
    assert_eq!(requests[0]["response_format"]["type"], "json_object");
    assert!(requests[1].get("response_format").is_none());
    assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    let messages = &requests[0]["messages"];
    assert_eq!(
        messages[0]["content"].as_str(),
        Some(expected_system.as_ref())
    );
    let parts = messages[1]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["type"], "text");
    let text = parts[0]["text"].as_str().unwrap();
    let caption: String = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert!(caption.starts_with("The user selected a file. \"```\""));
    for private in [
        SECRET,
        "user-private",
        "host-private",
        "unrelated-private-note",
        REFERENCE,
    ] {
        assert!(!text.contains(private));
    }
    assert_eq!(parts[1]["type"], "image_url");
    let image = parts[1]["image_url"]["url"].as_str().unwrap();
    let bytes = STANDARD
        .decode(image.strip_prefix(PNG_URL_PREFIX).unwrap())
        .unwrap();
    let uploaded = Pixmap::decode_png(&bytes).unwrap();
    assert_eq!((uploaded.width(), uploaded.height()), (1200, 9));
    assert_eq!(&uploaded.data()[480..484], &[10, 40, 70, 255]);
}

#[test]
fn journal_screenshot_analysis_rejects_truncation_and_omits_unredacted_caption() {
    let fixture = Fixture::new();
    let answer = json!({
        "usage":{"prompt_tokens":20,"completion_tokens":4,"total_tokens":24},
        "choices":[{"message":{"content":"The screenshot shows"},"finish_reason":"length"}],
    });
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "200 OK",
        "",
        &answer.to_string(),
    )]);
    let result = analyze(&Client::new(profile), &fixture.screenshot, None);
    let requests = server.join().unwrap();
    assert!(result.result.unwrap_err().contains("token limit"));
    let usage = result.usage.unwrap();
    assert_eq!(usage.total_tokens, Some(24));
    assert_eq!(usage.calls, 1);
    assert_eq!(
        requests[0]["messages"][0]["content"],
        system_prompt("de-DE")
    );
    assert_eq!(
        requests[0]["messages"][1]["content"][0]["text"],
        "Analyze the attached screenshot."
    );
}

#[test]
fn journal_screenshot_analysis_rejects_text_only_or_unknown_lm_studio_models_before_png_preparation()
 {
    let fixture = Fixture::new();
    // A reviewed PNG header without its image data can open in the journal but cannot be decoded.
    fs::write(
        fixture.root.join("journal").join(REFERENCE),
        &fixture.screenshot.bytes()[..33],
    )
    .unwrap();
    let screenshot = Screenshot::review(fixture.screenshot.source(), "image", REFERENCE).unwrap();
    assert!(prepare_png(screenshot.bytes()).is_err());

    for (model, diagnostic) in [
        ("text-instance", "does not support image input"),
        ("image-neighbor-preview", "could not be determined"),
    ] {
        let (endpoint, _) = listener();
        let profile = Profile::lm_studio(
            &endpoint.local_addr().unwrap().to_string(),
            model,
            ProfileOptions::default(),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let mut metadata = accept(&endpoint);
            assert!(wire_request(&mut metadata, "GET /api/v1/models HTTP/1.1").is_empty());
            let reply = fixture_http_response(
                "200 OK",
                "",
                r#"{"models":[
                    {"type":"llm","key":"image-neighbor","capabilities":{"vision":true}},
                    {"type":"llm","key":"text-key","loaded_instances":[{"id":"text-instance"}],"capabilities":{"vision":false}}
                ]}"#,
            );
            metadata.get_mut().write_all(reply.as_bytes()).unwrap();
            endpoint
        });
        let mut work_events = Vec::new();
        let mut work = |event| work_events.push(event);
        let mut reporter = Reporter::new(false, None, Some(&mut work));
        let analyzed = analyze_with_progress(
            &Client::new(profile),
            &screenshot,
            None,
            None,
            || Ok(()),
            &mut reporter,
        );
        let endpoint = server.join().unwrap();
        assert!(analyzed.result.unwrap_err().contains(diagnostic));
        assert!(analyzed.usage.is_none());
        assert!(work_events.is_empty());
        assert_eq!(
            endpoint.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn journal_screenshot_analysis_lm_studio_checks_exact_capability_and_honors_explicit_enable() {
    let fixture = Fixture::new();
    for mode in [VisionMode::Auto, VisionMode::Enabled] {
        let (endpoint, _) = listener();
        let profile = Profile::lm_studio(
            &endpoint.local_addr().unwrap().to_string(),
            "instance-for-image",
            ProfileOptions::default(),
        )
        .unwrap()
        .with_vision_mode(mode);
        let server = thread::spawn(move || {
            if mode == VisionMode::Auto {
                let mut metadata = accept(&endpoint);
                assert!(wire_request(&mut metadata, "GET /api/v1/models HTTP/1.1").is_empty());
                let reply = fixture_http_response(
                    "200 OK",
                    "",
                    r#"{"models":[{"type":"llm","key":"installed-image-model",
                        "loaded_instances":[{"id":"instance-for-image"}],"capabilities":{"vision":true}}]}"#,
                );
                metadata.get_mut().write_all(reply.as_bytes()).unwrap();
            }
            // A screenshot burst uses one capability GET for the exact same profile.
            for _ in 0..2 {
                let mut stream = accept(&endpoint);
                let body = wire_request(&mut stream, "POST /api/v1/chat HTTP/1.1");
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["model"], "instance-for-image");
                assert_eq!(body["store"], false);
                let parts = body["input"].as_array().unwrap();
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0]["type"], "text");
                assert_eq!(parts[1]["type"], "image");
                let image = parts[1]["data_url"].as_str().unwrap();
                let bytes = STANDARD
                    .decode(image.strip_prefix(PNG_URL_PREFIX).unwrap())
                    .unwrap();
                assert_eq!(Pixmap::decode_png(&bytes).unwrap().width(), 1200);
                let reply = fixture_http_response(
                    "200 OK",
                    "",
                    r#"{"output":[{"type":"message","content":"{\"description\":\"Dateiliste.\",\"tags\":[\"Datei\"]}"}]}"#,
                );
                stream.get_mut().write_all(reply.as_bytes()).unwrap();
            }
        });
        let client = Client::new(profile);
        for _ in 0..2 {
            let result = analyze(&client, &fixture.screenshot, None).result.unwrap();
            assert_eq!(result.model, "instance-for-image");
            assert_eq!(result.description, "Dateiliste.");
            assert_eq!(result.tags, ["datei"]);
        }
        server.join().unwrap();
    }
}

#[test]
fn journal_screenshot_analysis_cancels_lm_studio_metadata_without_sending_an_image() {
    let fixture = Fixture::new();
    let (endpoint, _) = listener();
    let profile = Profile::lm_studio(
        &endpoint.local_addr().unwrap().to_string(),
        "fixture:model",
        ProfileOptions::default(),
    )
    .unwrap();
    let (client, cancel) = Client::new_cancellable(profile);
    let (received, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut metadata = accept(&endpoint);
        assert!(wire_request(&mut metadata, "GET /api/v1/models HTTP/1.1").is_empty());
        metadata
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 200\r\nConnection: close\r\n\r\n{")
            .unwrap();
        metadata.get_mut().flush().unwrap();
        received.send(()).unwrap();
        match metadata.read(&mut [0_u8; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("cancellation did not close the metadata connection: {result:?}"),
        }
        endpoint
    });
    let analysis = thread::spawn(move || analyze(&client, &fixture.screenshot, None));
    wait.recv_timeout(Duration::from_secs(4)).unwrap();
    drop(cancel);
    let analyzed = analysis.join().unwrap();
    let endpoint = server.join().unwrap();
    assert_eq!(analyzed.result.unwrap_err(), Error::Cancelled.message());
    assert!(analyzed.usage.is_none());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_screenshot_analysis_does_not_send_a_replaced_reviewed_png() {
    use crate::session_journal::summary_ai::Work;

    let fixture = Fixture::new();
    let path = fixture.root.join("journal").join(REFERENCE);
    let mut replacement = Pixmap::new(2, 2).unwrap();
    replacement.fill(Color::WHITE);
    fs::write(path, replacement.encode_png().unwrap()).unwrap();
    let (listener, profile) = listener();
    let mut events = Vec::<Work>::new();
    let mut work = |event| events.push(event);
    let mut reporter = Reporter::new(false, None, Some(&mut work));
    let result = analyze_with_progress(
        &Client::new(profile),
        &fixture.screenshot,
        None,
        None,
        || Ok(()),
        &mut reporter,
    );
    assert_eq!(result.result.unwrap_err(), SCREENSHOT_UNAVAILABLE,);
    assert!(result.usage.is_none());
    assert!(events.is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_screenshot_analysis_rechecks_the_png_before_minimax_thinking_retry() {
    let fixture = Fixture::new();
    let path = fixture.root.join("journal").join(REFERENCE);
    let (endpoint, _) = listener();
    let profile = Profile::configured(
        &endpoint.local_addr().unwrap().to_string(),
        "MiniMax-M3",
        ProfileOptions {
            timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        },
    )
    .unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept(&endpoint);
        let request = request(&mut stream);
        assert_eq!(request["thinking"]["type"], "disabled");
        assert_eq!(request["messages"][1]["content"][1]["type"], "image_url");
        let mut replacement = Pixmap::new(2, 2).unwrap();
        replacement.fill(Color::WHITE);
        fs::write(path, replacement.encode_png().unwrap()).unwrap();
        let rejection = fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"param":"thinking","code":"unsupported_parameter"}}"#,
        );
        stream.get_mut().write_all(rejection.as_bytes()).unwrap();
        endpoint
    });
    let result = analyze(&Client::new(profile), &fixture.screenshot, None);
    let endpoint = server.join().unwrap();
    assert_eq!(result.result.unwrap_err(), SCREENSHOT_UNAVAILABLE);
    assert!(result.usage.is_none());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_screenshot_analysis_rechecks_authority_before_a_format_retry() {
    use crate::session_journal::summary_ai::Work;

    let fixture = Fixture::new();
    let (endpoint, profile) = listener();
    let revoked = Arc::new(AtomicBool::new(false));
    let revoke = Arc::clone(&revoked);
    let server = thread::spawn(move || {
        let mut stream = accept(&endpoint);
        let request = request(&mut stream);
        assert_eq!(request["response_format"]["type"], "json_object");
        assert_eq!(request["messages"][1]["content"][1]["type"], "image_url");
        revoke.store(true, Ordering::Release);
        let refusal = fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"param":"response_format","code":"unsupported_parameter"}}"#,
        );
        stream.get_mut().write_all(refusal.as_bytes()).unwrap();
        endpoint
    });
    let mut work_events = Vec::new();
    let mut work = |event| work_events.push(event);
    let mut reporter = Reporter::new(false, None, Some(&mut work));
    let result = analyze_with_progress(
        &Client::new(profile),
        &fixture.screenshot,
        None,
        None,
        || {
            if revoked.load(Ordering::Acquire) {
                return Err("Vision consent changed".into());
            }
            Ok(())
        },
        &mut reporter,
    );
    let endpoint = server.join().unwrap();
    assert_eq!(result.result.unwrap_err(), "Vision consent changed");
    let [
        Work::Started {
            stage: Stage::Screenshot,
            weight,
            at: started,
        },
        Work::Finished {
            stage: Stage::Screenshot,
            at: finished,
            elapsed,
            ..
        },
    ] = work_events.as_slice()
    else {
        panic!("unexpected screenshot work events: {work_events:?}");
    };
    assert_eq!(*weight, 1.5);
    assert!(finished.duration_since(*started) >= *elapsed);
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_screenshot_analysis_parser_recovers_prose_and_rejects_reasoning_only() {
    let context = Context {
        language: "en".into(),
        username: String::new(),
        host: String::new(),
        caption: Zeroizing::new(String::new()),
    };
    let prose = response::parse(
        "<think>hidden</think>\r\n A file manager.\r\n",
        &context,
        "fixture:local",
        None,
    )
    .unwrap();
    assert_eq!(prose.description, "A file manager.");
    assert!(prose.tags.is_empty());
    assert!(response::parse("<think>hidden", &context, "fixture:local", None).is_err());
    assert!(
        response::parse(
            r#"{"tags":["terminal", "rot", "windows"]}"#,
            &context,
            "fixture:local",
            None
        )
        .is_err()
    );
    let harmony = response::parse(
        r#"<|channel|>analysis<|message|>hidden<|end|><|channel|>final<|message|>{"tags":["Dateiliste"]}<|return|>"#,
        &context, "fixture:local", None,
    ).unwrap();
    assert_eq!(harmony.description, "");
    assert_eq!(harmony.tags, ["dateiliste"]);
}
