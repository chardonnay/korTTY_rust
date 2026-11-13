use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::local_ai::PromptPreset;
use crate::local_ai::test_support::{fixture_http_response, listener, start_test_double};
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
    let answer = json!({"choices":[{"message":{"content":[
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
    let analysis = analyze(&Client::new(profile), &fixture.screenshot, Some(&redactor)).unwrap();
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
    let answer = json!({"choices":[{"message":{"content":"The screenshot shows"},"finish_reason":"length"}]});
    let (profile, server) = start_test_double(vec![fixture_http_response(
        "200 OK",
        "",
        &answer.to_string(),
    )]);
    let result = analyze(&Client::new(profile), &fixture.screenshot, None);
    let requests = server.join().unwrap();
    assert!(result.unwrap_err().contains("token limit"));
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
fn journal_screenshot_analysis_does_not_send_a_replaced_reviewed_png() {
    let fixture = Fixture::new();
    let path = fixture.root.join("journal").join(REFERENCE);
    let mut replacement = Pixmap::new(2, 2).unwrap();
    replacement.fill(Color::WHITE);
    fs::write(path, replacement.encode_png().unwrap()).unwrap();
    let (listener, profile) = listener();
    assert_eq!(
        analyze(&Client::new(profile), &fixture.screenshot, None).unwrap_err(),
        SCREENSHOT_UNAVAILABLE,
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
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
