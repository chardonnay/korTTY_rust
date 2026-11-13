use super::*;
use crate::snippets::tests::Profile;

#[test]
fn activity_report_formats_serialize_the_same_escaped_run() {
    use std::time::Duration;

    use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome};

    let at = jiff::Timestamp::UNIX_EPOCH;
    let activity =
        "Grüße & <command>\n\n[render](https://fixture.invalid)\ninclude::/fixture[]\n----";
    let run = Run {
        mode: "Run",
        status: "Completed",
        destination: "Local shell".into(),
        profile_and_model: "Local · fixture".into(),
        prompt: "Check <output>".into(),
        started_at: at.to_string(),
        finished_at: at.to_string(),
        elapsed_seconds: 2,
        activity: vec![activity.into()],
        model_usage: Some(ModelUsage {
            attempts: 2,
            prompt_tokens: TokenCount::new(17, 2),
            completion_tokens: TokenCount::new(0, 0),
            total_tokens: TokenCount::new(0, 1),
        }),
        model_attempts: vec![
            ModelAttempt::from(&Attempt {
                kind: AttemptKind::Initial,
                outcome: AttemptOutcome::InvalidReply,
                elapsed: Duration::from_millis(12),
                input_tokens: Some(0),
                output_tokens: None,
                total_tokens: Some(0),
            }),
            ModelAttempt::from(&Attempt {
                kind: AttemptKind::Repair,
                outcome: AttemptOutcome::Completed,
                elapsed: Duration::from_millis(50),
                input_tokens: Some(17),
                output_tokens: None,
                total_tokens: None,
            }),
        ],
    };
    for format in Format::ALL {
        let bytes = render(format, at, std::slice::from_ref(&run), &|| false).unwrap();
        if format == Format::Pdf {
            assert!(bytes.starts_with(b"%PDF-"));
            let pdf = lopdf::Document::load_mem(&bytes).unwrap();
            let text = pdf.extract_text(&[1]).unwrap();
            assert!(
                text.contains("Reported tokens across 2 model attempts"),
                "{text}"
            );
            assert!(
                text.contains("0 total (reported subtotal, 1/2 attempts)"),
                "{text}"
            );
            continue;
        }
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Grüße"));
        match format {
            Format::Json => {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                let run = &value["runs"][0];
                assert_eq!(run["activity"][0], activity);
                assert_eq!(run["elapsedSeconds"], 2);
                assert_eq!(run["modelUsage"]["attempts"], 2);
                assert_eq!(run["modelUsage"]["totalTokens"]["reported"], 0);
                assert_eq!(run["modelUsage"]["totalTokens"]["attemptsWithCount"], 1);
                assert!(run["modelUsage"]["completionTokens"]["reported"].is_null());
                assert_eq!(run["modelAttempts"][0]["promptTokens"], 0);
                assert_eq!(run["modelAttempts"][0]["elapsedMillis"], 12);
                assert!(run["modelAttempts"][0]["completionTokens"].is_null());
                assert_eq!(run["modelAttempts"][1]["kind"], "repair");
                assert!(run["modelAttempts"][1]["totalTokens"].is_null());
            }
            Format::Xml => {
                let xml = roxmltree::Document::parse(&text).unwrap();
                assert_eq!(xml.root_element().tag_name().name(), "terminalAgentExport");
                assert_eq!(
                    xml.descendants()
                        .find(|node| node.has_tag_name("entry"))
                        .and_then(|node| node.text()),
                    Some(activity)
                );
                assert!(!xml.descendants().any(|node| node.has_tag_name("command")));
                let summary = xml
                    .descendants()
                    .find(|node| node.has_tag_name("modelUsage"))
                    .unwrap();
                let total = summary
                    .children()
                    .find(|node| node.has_tag_name("totalTokens"))
                    .unwrap();
                assert_eq!(
                    total
                        .children()
                        .find(|node| node.has_tag_name("reported"))
                        .unwrap()
                        .text(),
                    Some("0")
                );
                assert_eq!(
                    total
                        .children()
                        .find(|node| node.has_tag_name("attemptsWithCount"))
                        .unwrap()
                        .text(),
                    Some("1")
                );
                let attempt = xml
                    .descendants()
                    .find(|node| node.has_tag_name("attempt"))
                    .unwrap();
                assert_eq!(
                    attempt
                        .children()
                        .find(|node| node.has_tag_name("promptTokens"))
                        .unwrap()
                        .text(),
                    Some("0")
                );
                assert_eq!(
                    attempt
                        .children()
                        .find(|node| node.has_tag_name("completionTokens"))
                        .unwrap()
                        .attribute("unavailable"),
                    Some("true")
                );
            }
            Format::Yaml => {
                assert!(text.contains("- \"Grüße & <command>\\n\\n"));
                assert!(text.contains("\"totalTokens\":{\"reported\":0,\"attemptsWithCount\":1}"));
                assert!(text.contains("\"promptTokens\":0,\"completionTokens\":null"));
                assert!(text.contains("\"kind\":\"repair\""));
            }
            Format::Markdown => assert!(text.contains("\n    [render](https://fixture.invalid)\n")),
            Format::Asciidoctor => assert!(text.contains("\n \\include::/fixture[]\n")),
            Format::Text | Format::Pdf => {}
        }
        if matches!(
            format,
            Format::Text | Format::Markdown | Format::Asciidoctor
        ) {
            assert!(text.contains("0 total (reported subtotal, 1/2 attempts)"));
            assert!(text.contains("Model attempt 1"));
            assert!(text.contains("initial · invalid reply · 12 ms"));
            assert!(text.contains("0 input · output unavailable · 0 total"));
        }
    }
}

#[test]
fn activity_report_writes_only_new_absolute_files_in_the_reviewed_format() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("activity.json");
    assert!(!valid_path(Path::new("relative.json"), Format::Json));
    // Joining onto a verbatim Windows path can remove the parent component.
    let traversal = Path::new(if cfg!(windows) {
        r"C:\outside\..\activity.json"
    } else {
        "/outside/../activity.json"
    });
    assert!(traversal.is_absolute());
    assert!(
        traversal
            .components()
            .any(|part| part == Component::ParentDir)
    );
    assert!(!valid_path(traversal, Format::Json));
    assert!(!valid_path(&path, Format::Markdown));
    let destination = Destination::review(path.clone(), Format::Json).unwrap();
    let checks = std::cell::Cell::new(0);
    let result = destination.publish(b"cancelled", &|| {
        checks.set(checks.get() + 1);
        checks.get() == 3
    });
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    destination.publish(b"{}", &|| false).unwrap();
    assert!(destination.publish(b"replacement", &|| false).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"{}");

    let selected = root.join("selected");
    let moved = root.join("moved");
    std::fs::create_dir(&selected).unwrap();
    let destination = Destination::review(selected.join("activity.json"), Format::Json).unwrap();
    std::fs::rename(&selected, &moved).unwrap();
    std::fs::create_dir(&selected).unwrap();
    let result = destination.publish(b"{}", &|| false);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("directory changed")
    );
    assert!(std::fs::read_dir(&selected).unwrap().next().is_none());
    assert!(std::fs::read_dir(&moved).unwrap().next().is_none());
}
