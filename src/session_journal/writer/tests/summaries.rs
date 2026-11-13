use std::sync::atomic::{AtomicBool, Ordering};

use crate::session_journal::summary_ai::{Card, Reporter};
use crate::session_journal::summary_job::{Closed, Mode, Pass, visit_windows};
use crate::session_journal::summary_store::Progress;
use crate::terminal_logging::{Redactor, Secrets};

use super::*;

#[test]
fn journal_summary_raw_fallback_resumes_after_cancellation_and_preserves_user_entries() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let append = |kind, text| sink.try_write(kind, Zeroizing::new(text), false).unwrap();
    for index in 0..6 {
        append(RecordKind::Input, format!("command-{index}"));
        for line in 0..20 {
            append(RecordKind::Output, format!("recorded-{index}-{line}"));
        }
        append(
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)".into(),
        );
    }
    append(RecordKind::Note, "Keep this manual note".into());
    wait(capture.finish());
    let source = viewer::Source::capture(&capture);
    let original = source.read(None).unwrap().unwrap();
    let note = original
        .timeline
        .iter()
        .find(|entry| entry.kind == "USER_NOTE")
        .unwrap();
    let draft = Draft::read(&source, &note.id).unwrap();
    let marker = draft
        .markers()
        .iter()
        .find(|marker| marker.legacy() == LegacyMarker::Error)
        .unwrap()
        .clone();
    entry_edit::save(&draft.edit(Changes::Marker(marker)).unwrap()).unwrap();

    let stopped = AtomicBool::new(false);
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let mask = |text: &str| Ok(text.to_owned());
    let pass = Pass {
        model: None,
        cancelled: &stopped,
        mask: &mask,
        redactor: &redactor,
        full_output: false,
    };
    let target = || Closed {
        source: &source,
        generation: "0123456789abcdef0123456789abcdef",
        mode: Mode::Closing,
        automatic_title: false,
        screenshots: false,
    };
    let interrupted = pass
        .run_closed(
            target(),
            |outcome| {
                if outcome.windows == 1 {
                    stopped.store(true, Ordering::Release);
                }
            },
            &mut Reporter::default(),
        )
        .unwrap_err();
    assert_eq!(interrupted.kind(), io::ErrorKind::Interrupted);
    let checkpoint = Progress::read(&source).unwrap();
    assert!(checkpoint.cursor > 0 && checkpoint.raw && !checkpoint.finished);
    assert_eq!(checkpoint.last_ai, 0);
    let checkpoint_id = source
        .read(None)
        .unwrap()
        .unwrap()
        .timeline
        .into_iter()
        .find(|entry| entry.kind == "AI_SUMMARY")
        .unwrap()
        .id;

    stopped.store(false, Ordering::Release);
    assert!(
        pass.run_closed(target(), |_| {}, &mut Reporter::default())
            .unwrap()
            .raw
            > 0
    );
    assert!(Progress::read(&source).unwrap().finished);
    let finished = source.read(None).unwrap().unwrap();
    assert!(
        finished
            .timeline
            .iter()
            .any(|entry| entry.id == checkpoint_id)
    );
    let retained = finished
        .timeline
        .iter()
        .find(|entry| entry.id == note.id)
        .unwrap();
    assert_eq!(retained.text, "Keep this manual note");
    assert_eq!(retained.marker, LegacyMarker::Error.xml());
    assert_eq!(
        finished
            .timeline
            .iter()
            .filter(|entry| entry.kind == "SESSION_SUMMARY")
            .count(),
        1
    );
    assert_eq!(
        pass.run_closed(target(), |_| {}, &mut Reporter::default())
            .unwrap()
            .windows,
        0
    );
    assert_eq!(
        source.read(None).unwrap().unwrap().timeline.len(),
        finished.timeline.len()
    );
    let raw = source.capture_page(None, None, None).unwrap();
    assert!(
        raw.records
            .iter()
            .any(|record| record.text == "recorded-0-0")
    );
}

#[test]
fn automation_summary_checkpoints_observed_tokens_and_marks_missing_counts() {
    use crate::local_ai::test_support::{exclusive_run, fixture_http_response, start_test_double};
    use crate::session_journal::summary_ai::Run;
    use serde_json::json;

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let append = |kind, text| sink.try_write(kind, Zeroizing::new(text), false).unwrap();
    for index in 0..6 {
        append(RecordKind::Input, format!("command-{index}"));
        for line in 0..20 {
            append(RecordKind::Output, format!("recorded-{index}-{line}"));
        }
        append(
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)".into(),
        );
    }
    wait(capture.finish());
    let source = viewer::Source::capture(&capture);
    let snapshot = || source.read(None).unwrap().unwrap().summary;
    let stop = AtomicBool::new(false);
    let mut windows = 0;
    visit_windows(
        &source,
        0,
        true,
        false,
        snapshot().ended_at.unwrap(),
        &stop,
        |_, _| {
            windows += 1;
            Ok(())
        },
    )
    .unwrap();
    assert!(windows >= 2);

    let valid =
        r#"{"title":"Checked capture","summary":"Reviewed terminal output.","keywords":[]}"#;
    let stream = |content, usage| {
        let answer =
            json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}]});
        let usage = json!({"choices":[],"usage":usage});
        fixture_http_response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            &format!("data: {answer}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
        )
    };
    let mut responses = vec![fixture_http_response(
        "200 OK",
        "",
        &json!({
            "choices": [{"message":{"content":r#"{"ready":true}"#}}],
            "usage": {"prompt_tokens":1_000,"completion_tokens":1_000}
        })
        .to_string(),
    )];
    for index in 0..windows {
        responses.push(if index == 1 {
            stream("unstructured fixture response", json!({"prompt_tokens":7}))
        } else {
            stream(valid, json!({"prompt_tokens":10,"completion_tokens":4}))
        });
    }
    responses.push(stream(
        valid,
        json!({"prompt_tokens":15,"completion_tokens":6}),
    ));
    let (profile, server) = start_test_double(responses);
    let (run, _cancel) = Run::begin(profile).unwrap();
    run.preflight().unwrap();
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let mask = |text: &str| Ok(text.to_owned());
    let pass = Pass {
        model: Some(&run),
        cancelled: &stop,
        mask: &mask,
        redactor: &redactor,
        full_output: false,
    };
    let target = || Closed {
        source: &source,
        generation: "0123456789abcdef0123456789abcdef",
        mode: Mode::Automation,
        automatic_title: false,
        screenshots: false,
    };
    let error = pass
        .run_closed(
            target(),
            |_| stop.store(true, Ordering::Release),
            &mut Reporter::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let first = snapshot();
    assert_eq!(first.usage.call_count, 1);
    assert_eq!(first.usage.prompt_tokens, 10);
    assert_eq!(first.usage.completion_tokens, 4);
    assert_eq!(first.usage.total_tokens, 14);
    assert!(!first.usage_partial);

    stop.store(false, Ordering::Release);
    let outcome = pass
        .run_closed(target(), |_| {}, &mut Reporter::default())
        .unwrap();
    assert_eq!(outcome.windows, windows - 1);
    assert_eq!(outcome.raw, 1);
    let finished = snapshot();
    let prompt = 10 * (windows as u64 - 1) + 7 + 15;
    let completion = 4 * (windows as u64 - 1) + 6;
    assert_eq!(finished.usage.call_count, windows as u64 + 1);
    assert_eq!(finished.usage.prompt_tokens, prompt);
    assert_eq!(finished.usage.completion_tokens, completion);
    assert_eq!(finished.usage.total_tokens, prompt + completion);
    assert!(finished.usage.local && finished.usage_partial);
    assert!(Progress::read(&source).unwrap().finished);
    assert_eq!(
        pass.run_closed(target(), |_| {}, &mut Reporter::default())
            .unwrap()
            .windows,
        0
    );
    assert_eq!(snapshot().usage.call_count, finished.usage.call_count);
    assert_eq!(server.join().unwrap().len(), windows + 2);
}

#[test]
fn periodic_summary_is_published_by_the_writer_and_retained_on_close() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let source = viewer::Source::capture(&capture);
    capture
        .sink()
        .try_write(
            RecordKind::Output,
            Zeroizing::new("live output".into()),
            false,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while capture.published().0 == 0 {
        assert!(
            Instant::now() < deadline,
            "capture did not publish its output"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let stop = Arc::new(AtomicBool::new(false));
    visit_windows(
        &source,
        0,
        false,
        false,
        Timestamp::now(),
        &stop,
        |window, _| {
            let card = Card::raw(&window);
            assert_eq!(
                wait_edit(capture.publish_summary(
                    0,
                    window,
                    Some(card),
                    Arc::clone(&stop),
                    None
                )?)
                .unwrap(),
                1
            );
            Ok(())
        },
    )
    .unwrap();
    let checkpoint = Progress::read(&source).unwrap();
    assert_eq!(checkpoint.cursor, 1);
    assert!(checkpoint.raw);
    wait(capture.finish());
    let stopped = source.read(None).unwrap().unwrap();
    assert!(stopped.summary.ended_at.is_some());
    assert_eq!(stopped.timeline[0].output, ["live output"]);
    assert_eq!(Progress::read(&source).unwrap(), checkpoint);
}

#[test]
fn journal_summary_full_output_reads_every_reviewed_record_and_writes_one_card() {
    use std::io::Write as _;

    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, listener, request,
    };
    use crate::session_journal::summary_ai::Run;
    use serde_json::{Value, json};

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let write = |kind, text: &str| {
        sink.try_write(kind, Zeroizing::new(text.to_owned()), false)
            .unwrap();
    };
    let finish = || {
        write(
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)",
        )
    };
    write(RecordKind::Input, "ls --help");
    write(RecordKind::Output, "Usage: ignored-test-only-help");
    finish();
    write(RecordKind::Input, "read large output");
    let expected: Vec<_> = (0..291).map(|index| format!("row-{index:03}")).collect();
    for (index, row) in expected.iter().enumerate() {
        if index % 80 == 0 {
            let deadline = Instant::now() + Duration::from_secs(8);
            while capture.published().0 < index as u64 {
                assert!(
                    Instant::now() < deadline,
                    "capture did not publish its output"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        write(RecordKind::Output, row);
    }
    write(RecordKind::Output, "-----BEGIN OPENSSH PRIVATE KEY-----");
    write(RecordKind::Output, "private-key-test-fixture");
    write(RecordKind::Output, "-----END OPENSSH PRIVATE KEY-----");
    let long_line = format!("{}reviewed-long-tail", "a".repeat(5_000));
    write(RecordKind::Output, &long_line);
    write(RecordKind::Output, "reviewed-credential-test");
    finish();
    wait(capture.finish());

    let source = viewer::Source::capture(&capture);
    let (listener, profile) = listener();
    let server = thread::spawn(move || {
        let answer = json!({"choices":[{"index":0,"delta":{"content":
            r#"{"title":"Checked output","summary":"Reviewed output.","keywords":[]}"#
        },"finish_reason":"stop"}]});
        let usage = json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4}});
        let response = fixture_http_response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            &format!("data: {answer}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
        );
        let mut parts = Vec::new();
        loop {
            let mut stream = accept(&listener);
            let body = request(&mut stream);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
            let system = body["messages"][0]["content"].as_str().unwrap();
            if system.starts_with("Write a closing session journal summary") {
                return parts;
            }
            let prompt = body["messages"][1]["content"].as_str().unwrap();
            assert!(prompt.len() <= 28_000);
            assert!(!prompt.contains("ignored-test-only-help"));
            assert!(!prompt.contains("private-key-test-fixture"));
            assert!(!prompt.contains("reviewed-credential-test"));
            parts.push(serde_json::from_str::<Value>(prompt).unwrap());
        }
    });
    let (run, _cancel) = Run::begin(profile).unwrap();
    let stop = AtomicBool::new(false);
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let mask = |text: &str| Ok(text.replace("reviewed-credential-test", "***"));
    let pass = Pass {
        model: Some(&run),
        cancelled: &stop,
        mask: &mask,
        redactor: &redactor,
        full_output: true,
    };
    let outcome = pass
        .run_closed(
            Closed {
                source: &source,
                generation: "0123456789abcdef0123456789abcdef",
                mode: Mode::Automation,
                automatic_title: false,
                screenshots: false,
            },
            |_| {},
            &mut Reporter::default(),
        )
        .unwrap();
    let parts = server.join().unwrap();
    assert!(parts.len() > 2);
    assert!(parts[0]["previous"].is_null());
    assert_eq!(parts[1]["previous"], "Reviewed output.");
    let lines: Vec<_> = parts
        .iter()
        .flat_map(|part| part["evidence"].as_array().unwrap())
        .map(|line| line["text"].as_str().unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .copied()
            .any(|text| text.contains("reviewed-long-tail"))
    );
    assert_eq!(
        lines
            .iter()
            .copied()
            .filter(|text| text.starts_with("row-"))
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(outcome.windows, 1);
    assert_eq!(outcome.raw, 0);
    let finished = source.read(None).unwrap().unwrap();
    assert_eq!(
        finished
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AI_SUMMARY")
            .count(),
        1
    );
    assert_eq!(finished.summary.usage.call_count, parts.len() as u64 + 1);
    assert_eq!(
        finished.summary.usage.prompt_tokens,
        (parts.len() as u64 + 1) * 10
    );
}

#[test]
fn journal_summary_withholds_split_key_material_across_windows_and_resume() {
    use crate::local_ai::test_support::{exclusive_run, fixture_response, start_test_double};
    use crate::session_journal::summary_ai::{Run, Summary};

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    let write = |kind, text: &str, partial| {
        sink.try_write(kind, Zeroizing::new(text.into()), partial)
            .unwrap();
    };
    let finish = || {
        write(
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)",
            false,
        );
    };
    write(RecordKind::Input, "check key formats", false);
    for index in 0..10 {
        write(
            RecordKind::Output,
            &format!("public prelude {index}"),
            false,
        );
    }
    write(RecordKind::Output, "-----BE", true);
    write(RecordKind::Output, "GIN OPENSSH PRIVATE KEY-----", false);
    for index in 0..14 {
        write(
            RecordKind::Output,
            &format!("private segment {index}"),
            false,
        );
    }
    finish();
    for index in 0..3 {
        write(
            RecordKind::Output,
            &format!("more private segment {index}"),
            false,
        );
        finish();
    }
    write(RecordKind::Output, "private-key-body-fixture", false);
    write(
        RecordKind::Output,
        "-----END OPENSSH PRIVATE KEY-----",
        false,
    );
    write(RecordKind::Output, "public after key", false);
    finish();
    wait(capture.finish());

    let source = viewer::Source::capture(&capture);
    let raw = source.capture_page(None, None, None).unwrap();
    let body = raw
        .records
        .iter()
        .find(|row| row.text == "private-key-body-fixture")
        .unwrap();
    let stop = AtomicBool::new(false);
    let collect = |after| {
        let mut windows = Vec::new();
        visit_windows(
            &source,
            after,
            true,
            false,
            Timestamp::now(),
            &stop,
            |window, _| {
                windows.push(window);
                Ok(())
            },
        )
        .unwrap();
        windows
    };
    let mut windows = collect(0);
    assert_eq!(windows.len(), 5);
    let checkpoint = windows[0].checkpoint().unwrap();
    assert!(checkpoint < body.sequence);
    assert!(windows[0].omitted > 0);
    let resumed = collect(checkpoint);
    assert_eq!(resumed.len(), 4);
    windows.extend(resumed);

    let replies = (0..=windows.len())
        .map(|_| {
            fixture_response(
                "200 OK",
                "",
                r#"{"title":"Key check","summary":"Checked capture evidence.","keywords":[]}"#,
            )
        })
        .collect();
    let (profile, server) = start_test_double(replies);
    let (run, _cancel) = Run::begin(profile).unwrap();
    let mask = |text: &str| Ok(text.to_owned());
    for window in &windows {
        let raw = Card::raw_masked(window, &mask);
        assert!(
            raw.output
                .iter()
                .all(|row| !row.contains("private-key-body-fixture"))
        );
        run.window(window, &mask, &mut Reporter::default())
            .result
            .unwrap();
    }
    // Older imported cards can contain an entire opening marker or split text of their own.
    let closing_cards = [
        Summary {
            title: "First card".into(),
            text: "-----BEGIN OPENSSH PRIVATE KEY-----".into(),
            keywords: Vec::new(),
        },
        Summary {
            title: "Second card".into(),
            text: "private-key-body-fixture\n-----END OPENSSH PRIVATE KEY-----".into(),
            keywords: Vec::new(),
        },
        Summary {
            title: "Third card".into(),
            text: "public after key".into(),
            keywords: Vec::new(),
        },
    ];
    run.close(&closing_cards, &mask, &mut Reporter::default())
        .result
        .unwrap();
    let requests = server.join().unwrap();
    for request in &requests {
        let request = request.to_string();
        assert!(!request.contains("private-key-body-fixture"));
        assert!(!request.contains("private segment"));
        assert!(!request.contains("PRIVATE KEY"));
        assert!(!request.contains("-----BE"));
    }
    assert!(
        requests[windows.len() - 1]
            .to_string()
            .contains("public after key")
    );
    assert!(
        requests
            .last()
            .unwrap()
            .to_string()
            .contains("public after key")
    );
}

#[test]
fn saved_journal_summary_queue_deduplicates_targets_and_cancels_a_whole_batch() {
    use crate::session_journal::summary_queue::{Preferences, Queue, Receipt};

    let fixture = Fixture::new();
    let first = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let second = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let mut queue = Queue::load(&fixture.0).unwrap();
    assert!(!queue.preferences().enabled);
    queue
        .set_preferences(Preferences {
            enabled: true,
            ..Preferences::default()
        })
        .unwrap();
    let receipt = |capture, generation: &str| Receipt {
        generation: generation.into(),
        batch: "00112233445566778899aabbccddeeff".into(),
        source: viewer::Source::capture(capture).summary_receipt(),
        title: "Test journal".into(),
        mode: Mode::Closing,
        profile: None,
        automatic_title: false,
        full_output: false,
        screenshots: false,
    };
    let job = receipt(&first, "00000000000000000000000000000001");
    queue
        .enqueue([
            job.clone(),
            receipt(&second, "00000000000000000000000000000002"),
        ])
        .unwrap();
    assert!(
        queue
            .enqueue([receipt(&first, "00000000000000000000000000000003")])
            .is_err()
    );
    let mut restored = Queue::load(&fixture.0).unwrap();
    assert_eq!(restored.pending().len(), 2);
    assert!(
        restored.pending()[0]
            .source
            .source()
            .same_journal(&viewer::Source::capture(&first))
    );
    restored.cancel_batch(&job.batch).unwrap();
    assert!(Queue::load(&fixture.0).unwrap().pending().is_empty());
    wait(first.finish());
    wait(second.finish());
}
