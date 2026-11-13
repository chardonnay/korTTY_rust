use serde_json::json;

use super::*;
use crate::local_ai::PromptPreset;
use crate::local_ai::test_support::{
    fixture_http_response, fixture_response, listener, start_test_double,
};
use crate::terminal_logging::{Policy, Secrets};

#[test]
fn journal_note_translation_rejects_sensitive_language_before_transport() {
    let (listener, profile) = listener();
    let secrets = Secrets::default();
    secrets.add("fixture-target-secret");
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
    let client = Client::new(profile);
    for language in ["fixture-target-secret", "\nen"] {
        assert!(translate(&client, "A note", language, Some(&redactor), 256).is_err());
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_note_translation_sends_only_the_redacted_multiline_note_and_target() {
    const SECRET: &str = "translation-fixture-secret";
    let source =
        format!("Inspect the command.\r\n\r\n```sh\r\ncd /var/log && printf \"{SECRET}\"\r\n```");
    let expected = "Revisa el comando.\n\n```sh\ncd /var/log && printf \"***\"\n```";
    let translation = json!({"translation": expected.replace("***", SECRET)});
    let answer =
        format!("<think>Reasoning unavailable to the user</think>\n```json\n{translation}\n```");
    let unsupported = json!({"error":{"param":"response_format","code":"unsupported_parameter"}});
    let accepted = json!({
        "choices": [{"message": {"content": answer}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 81, "completion_tokens": 19, "total_tokens": 100},
    });
    let (profile, server) = start_test_double(vec![
        fixture_http_response("422 Unprocessable Entity", "", &unsupported.to_string()),
        fixture_http_response("200 OK", "", &accepted.to_string()),
    ]);
    let secrets = Secrets::default();
    secrets.add(SECRET);
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
    let profile = profile.with_prompt_preset(PromptPreset::Qwen);
    let expected_system = profile.render_system_prompt(SYSTEM);
    let client = Client::new(profile);
    let result = translate(&client, &source, " Español (México) ", Some(&redactor), 256);
    let requests = server.join().unwrap();
    assert_eq!(result.unwrap(), expected);
    let usage = client.take_usage().unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (Some(81), Some(19), Some(100))
    );
    assert_eq!(requests[0]["response_format"]["type"], "json_object");
    assert!(requests[1].get("response_format").is_none());
    assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    let request = &requests[0]["messages"];
    assert_eq!(
        request[0]["content"].as_str(),
        Some(expected_system.as_ref())
    );
    let user: Value = serde_json::from_str(request[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["target_language"], "Español (México)");
    assert_eq!(
        user["note"],
        "Inspect the command.\n\n```sh\ncd /var/log && printf \"***\"\n```"
    );
    assert!(!requests[0].to_string().contains(SECRET));
}

#[test]
fn journal_note_translation_rejects_truncation_without_changing_legacy_text_completion() {
    let content = r#"{"translation":"Ein Teil"}"#;
    let response = json!({"choices":[{"message":{"content":content},"finish_reason":"length"}]});
    let response = fixture_http_response("200 OK", "", &response.to_string());
    let (profile, server) = start_test_double(vec![response.clone(), response]);
    let client = Client::new(profile);
    let translated = translate(&client, "Partial", "de", None, 64);
    let ordinary = client.complete("fixture", "existing text caller");
    let requests = server.join().unwrap();
    assert!(translated.unwrap_err().contains("token limit"));
    assert_eq!(ordinary.unwrap(), content);
    assert_eq!(requests[0]["messages"][0]["content"], SYSTEM);
}

#[test]
fn journal_note_translation_accepts_prose_and_rejects_unusable_or_oversized_results() {
    let (profile, server) = start_test_double(vec![
        fixture_response(
            "200 OK",
            "",
            "<think>private thought</think>\n```text\nHola\n```",
        ),
        fixture_response("200 OK", "", r#"{"description":"wrong field"}"#),
        fixture_response("200 OK", "", r#"{"translation":"こんにちは"}"#),
        fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"message":"Private service error"}}"#,
        ),
    ]);
    let client = Client::new(profile);
    let good = translate(&client, "Hello", "es", None, 8);
    let missing = translate(&client, "Hello", "es", None, 8);
    let oversized = translate(&client, "Hello", "日本語", None, 8);
    let error = translate(&client, "Hello", "es", None, 8);
    server.join().unwrap();
    assert_eq!(good.unwrap(), "Hola");
    assert!(missing.unwrap_err().contains("no usable translation"));
    assert!(oversized.unwrap_err().contains("size limit"));
    assert_eq!(error.unwrap_err(), "The local AI server returned HTTP 400");
}

#[test]
fn journal_note_translation_validates_target_and_input_before_opening_a_socket() {
    assert!(valid_language("pt-BR"));
    assert!(valid_language("简体中文"));
    for invalid in [
        " ",
        "de\nen",
        "de\u{202e}en",
        &"x".repeat(MAX_LANGUAGE_BYTES + 1),
    ] {
        assert!(!valid_language(invalid));
    }
    let (listener, profile) = listener();
    let client = Client::new(profile);
    assert!(translate(&client, "Hello", "de\nen", None, 8).is_err());
    assert!(translate(&client, "  \t", "de", None, 8).is_err());
    assert!(translate(&client, "Too long", "de", None, 7).is_err());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
