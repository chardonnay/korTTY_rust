use std::sync::atomic::{AtomicBool, Ordering};

use crate::session_journal::summary_ai::{Card, ObservedUsage, Reporter};
use crate::session_journal::summary_job::{Closed, Mode, Pass, visit_windows};
use crate::session_journal::summary_store::Progress;
use crate::terminal_logging::{Redactor, Secrets};

use super::*;

#[test]
fn journal_summary_effective_accounts_survive_sampling_and_appear_once_on_close() {
    let fixture = Fixture::new();
    let capture = Capture::start(
        fixture.journals(),
        Metadata {
            username: "daniel".into(),
            ..Metadata::default()
        },
    )
    .unwrap();
    let sink = capture.sink();
    let write = |kind, text: &str, partial| {
        sink.try_write(kind, Zeroizing::new(text.into()), partial)
            .unwrap();
    };
    let ended = || {
        write(
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)",
            false,
        );
    };
    write(RecordKind::Input, "sudo id", false);
    write(RecordKind::Output, "uid=0(root)", false);
    write(RecordKind::Output, "daniel@fedora:~$", true);
    ended();
    write(RecordKind::Input, "sudo -i", false);
    for index in 0..14 {
        write(RecordKind::Output, &format!("prelude {index}"), false);
    }
    write(RecordKind::Output, "[root@fedora ~]#", true);
    for index in 0..14 {
        write(RecordKind::Output, &format!("later {index}"), false);
    }
    ended();
    write(RecordKind::Output, "[root@fedora ~]#", true);
    write(RecordKind::Input, "id", false);
    write(RecordKind::Output, "uid=0(root)", false);
    ended();
    write(RecordKind::Input, "review", false);
    write(RecordKind::Output, "sealedaccount@fedora:~$", false);
    ended();
    wait(capture.finish());

    let source = viewer::Source::capture(&capture);
    let cancelled = AtomicBool::new(false);
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    Pass {
        model: None,
        cancelled: &cancelled,
        mask: &|text: &str| Ok(text.replace("sealedaccount", "***")),
        redactor: &redactor,
        full_output: false,
        evidence_limits: None,
    }
    .run_closed(
        Closed {
            source: &source,
            generation: "0123456789abcdef0123456789abcdef",
            mode: Mode::Closing,
            automatic_title: false,
            allow_title: &|| false,
            screenshots: false,
            automation_vision: None,
        },
        |_| {},
        &mut Reporter::default(),
    )
    .unwrap();

    let result = source.read(None).unwrap().unwrap();
    let mut cards: Vec<_> = result
        .timeline
        .iter()
        .filter(|row| row.kind == "AI_SUMMARY")
        .collect();
    cards.sort_by_key(|row| row.log_start);
    assert_eq!(
        cards
            .iter()
            .map(|row| row.run_as_user.as_deref())
            .collect::<Vec<_>>(),
        [None, Some("root"), Some("root"), None]
    );
    assert!(
        cards[1]
            .extra_details
            .iter()
            .any(|detail| detail == "Run as · root")
    );
    assert!(cards[1].output.iter().all(|line| !line.contains("root@")));
    let closing = result
        .timeline
        .iter()
        .find(|row| row.kind == "SESSION_SUMMARY")
        .unwrap();
    assert!(closing.text.ends_with("Accounts · daniel, root"));
    assert!(!closing.text.contains("sealedaccount"));
}

#[test]
fn journal_summary_effective_account_uses_current_managed_mask_on_adjacent_records() {
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    for (kind, text) in [
        (RecordKind::Input, "check accounts"),
        (RecordKind::Output, "hidden account follows"),
        (RecordKind::Output, "[root@fedora ~]#"),
        (RecordKind::Output, "deploy@fedora:~$"),
        (
            RecordKind::CommandEnd,
            "shell integration: command finished",
        ),
    ] {
        sink.try_write(kind, Zeroizing::new(text.to_owned()), false)
            .unwrap();
    }
    wait(capture.finish());
    let source = viewer::Source::capture(&capture);
    let cancelled = AtomicBool::new(false);
    let rules =
        "[[session-journal.replace]]\npattern = 'hidden account follows\\n\\[root'\nregex = true"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
    let mut policy = crate::terminal_logging::Policy::default();
    policy.read_rule(rules.as_table()).unwrap();
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), policy);
    let mask = |text: &str| {
        redactor
            .journal_text(text, Instant::now() + Duration::from_secs(1))
            .map(|value| value.to_string())
            .ok_or_else(|| io::Error::other("Test mask failed"))
    };
    assert!(mask("[root@fedora ~]#").unwrap().contains("root"));
    assert!(
        !mask("hidden account follows\n[root@fedora ~]#")
            .unwrap()
            .contains("root")
    );
    let pass = Pass {
        model: None,
        cancelled: &cancelled,
        mask: &mask,
        redactor: &redactor,
        full_output: false,
        evidence_limits: Some(Default::default()),
    };
    let mut observed = None;
    pass.visit_live_windows(&source, 0, jiff::Timestamp::now(), false, |window, _| {
        let (card, _) = pass.window_reply(&source, &window, "en", &mut Reporter::default())?;
        observed = card.map(|card| card.run_as_user);
        Ok(())
    })
    .unwrap();
    assert_eq!(observed, Some(None)); // Redaction replaced a record boundary, so mapping is unsafe.
}

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
        evidence_limits: None,
    };
    let target = || Closed {
        source: &source,
        generation: "0123456789abcdef0123456789abcdef",
        mode: Mode::Closing,
        automatic_title: false,
        allow_title: &|| false,
        screenshots: false,
        automation_vision: None,
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
    assert_eq!(checkpoint.language.as_deref(), Some("en"));
    let path = source.directory().join("journal.xml");
    let xml = fs::read_to_string(&path).unwrap();
    assert!(xml.contains("<appLanguageCode>en</appLanguageCode>"));
    fs::write(
        &path,
        xml.replace(
            "<appLanguageCode>en</appLanguageCode>",
            "<appLanguageCode>fr</appLanguageCode>",
        ),
    )
    .unwrap();
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
    let checkpoint = Progress::read(&source).unwrap();
    assert!(checkpoint.finished);
    assert_eq!(checkpoint.language.as_deref(), Some("en"));
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
    pass.run_closed(
        Closed {
            generation: "fedcba9876543210fedcba9876543210",
            ..target()
        },
        |_| {},
        &mut Reporter::default(),
    )
    .unwrap();
    assert_eq!(
        Progress::read(&source).unwrap().language.as_deref(),
        Some("fr")
    );
    let raw = source.capture_page(None, None, None).unwrap();
    assert!(
        raw.records
            .iter()
            .any(|record| record.text == "recorded-0-0")
    );
}

#[test]
fn closing_journal_title_resumes_after_durable_wrap_up_and_uses_masked_evidence() {
    use crate::local_ai::test_support::{exclusive_run, fixture_response, start_test_double};
    use crate::session_journal::summary_ai::{Run, Stage};

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let capture = Capture::start(
        fixture.journals(),
        Metadata {
            title: "Test private-value".into(),
            connection_name: "Test private-value".into(),
            ..Metadata::default()
        },
    )
    .unwrap();
    capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("An evidence note".into()),
            false,
        )
        .unwrap();
    wait(capture.finish());
    let source = viewer::Source::capture(&capture);
    let xml = source.directory().join("journal.xml");
    fs::write(
        &xml,
        fs::read_to_string(&xml).unwrap().replace(
            "<appLanguageCode>en</appLanguageCode>",
            "<appLanguageCode>de</appLanguageCode>",
        ),
    )
    .unwrap();
    let stopped = AtomicBool::new(false);
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let mask = |text: &str| Ok(text.replace("private-value", "[redacted]"));
    let target = || Closed {
        source: &source,
        generation: "0123456789abcdef0123456789abcdef",
        mode: Mode::Closing,
        automatic_title: true,
        allow_title: &|| true,
        screenshots: false,
        automation_vision: None,
    };
    let response = |content| fixture_response("200 OK", "", content);
    let (profile, server) = start_test_double(vec![response(
        r#"{"title":"Wrap-up heading","summary":"Reviewed a journal note.","keywords":[]}"#,
    )]);
    let (run, _cancel) = Run::begin(profile).unwrap();
    let mut interrupt = |preview: Option<crate::session_journal::summary_ai::Preview>| {
        if preview.is_some_and(|preview| preview.stage == Stage::Title) {
            let persisted = source.read(None).unwrap().unwrap();
            assert!(
                persisted
                    .timeline
                    .iter()
                    .any(|entry| entry.title == "Wrap-up heading")
            );
            stopped.store(true, Ordering::Release);
        }
    };
    let pass = Pass {
        model: None,
        cancelled: &stopped,
        mask: &mask,
        redactor: &redactor,
        full_output: false,
        evidence_limits: None,
    };
    let error = (Pass {
        model: Some(&run),
        ..pass
    })
    .run_closed(
        target(),
        |_| {},
        &mut Reporter::new(false, Some(&mut interrupt), None),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(server.join().unwrap().len(), 1);
    let pending = Progress::read(&source).unwrap();
    assert!(!pending.finished);
    let before = source.read(None).unwrap().unwrap();
    assert_eq!(before.summary.title, "Test private-value");
    assert_eq!(
        before
            .timeline
            .iter()
            .filter(|entry| entry.kind == "SESSION_SUMMARY")
            .count(),
        1
    );

    drop(run);
    stopped.store(false, Ordering::Release);
    let (profile, server) =
        start_test_double(vec![response(r#"{"title":"Separate journal title"}"#)]);
    let (run, _cancel) = Run::begin(profile).unwrap();
    (Pass {
        model: Some(&run),
        ..pass
    })
    .run_closed(target(), |_| {}, &mut Reporter::default())
    .unwrap();
    let calls = server.join().unwrap();
    assert_eq!(calls.len(), 1);
    let system = calls[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Name this terminal session journal"));
    assert!(system.contains("Answer in language code de."));
    let input = calls[0]["messages"][1]["content"].as_str().unwrap();
    assert!(input.contains("Test [redacted]"));
    assert!(input.contains("An evidence note"));
    assert!(!input.contains("private-value") && !input.contains("Wrap-up heading"));
    let after = source.read(None).unwrap().unwrap();
    assert_eq!(after.summary.title, "Separate journal title");
    assert_eq!(
        after
            .timeline
            .iter()
            .find(|entry| entry.kind == "SESSION_SUMMARY")
            .unwrap()
            .title,
        "Wrap-up heading"
    );
    assert!(Progress::read(&source).unwrap().finished);
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
        evidence_limits: None,
    };
    let target = || Closed {
        source: &source,
        generation: "0123456789abcdef0123456789abcdef",
        mode: Mode::Automation,
        automatic_title: false,
        allow_title: &|| false,
        screenshots: false,
        automation_vision: None,
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
                    None,
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
fn live_summary_token_usage_follows_card_publication_and_survives_close() {
    use crate::session_journal::summary_plan::End;

    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let source = viewer::Source::capture(&capture);
    let stop = Arc::new(AtomicBool::new(false));
    let append = |kind, text: &str| {
        capture
            .sink()
            .try_write(kind, Zeroizing::new(text.into()), false)
            .unwrap();
    };
    append(RecordKind::Input, "build project");
    append(RecordKind::Output, "compiling");
    let deadline = Instant::now() + Duration::from_secs(8);
    let wait_for_records = |expected| {
        while capture.published().0 < expected {
            assert!(
                Instant::now() < deadline,
                "capture did not publish its output"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    wait_for_records(2);
    let now = Timestamp::from_second(Timestamp::now().as_second() + 3_600).unwrap();
    let latest = || {
        let mut windows = Vec::new();
        visit_windows(&source, 0, false, false, now, &stop, |window, _| {
            windows.push(window);
            Ok(())
        })
        .unwrap();
        assert_eq!(windows.len(), 1);
        windows.pop().unwrap()
    };
    let publish = |window: &crate::session_journal::summary_plan::Window, usage| {
        wait_edit(
            capture
                .publish_summary(
                    0,
                    window.clone(),
                    Some(Card::raw(window)),
                    Some(usage),
                    Arc::clone(&stop),
                    None,
                )
                .unwrap(),
        )
    };
    let preview = latest();
    assert_eq!(preview.end, End::Running);
    let preview_usage = ObservedUsage {
        input_tokens: Some(12),
        output_tokens: Some(5),
        total_tokens: Some(17),
        calls: 1,
        partial: false,
        local: true,
        cost: None,
        profile: None,
    };
    let staged = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&staged).unwrap();
    assert!(publish(&preview, preview_usage.clone()).is_err());
    let unpublished = source.read(None).unwrap().unwrap();
    assert!(unpublished.timeline.is_empty());
    assert_eq!(unpublished.summary.usage.call_count, 0);
    fs::remove_dir(&staged).unwrap();
    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    assert!(
        publish(&preview, preview_usage)
            .unwrap_err()
            .contains("Injected journal directory sync failure")
    );
    let saved = source.read(None).unwrap().unwrap();
    assert_eq!(saved.summary.usage.call_count, 1);
    assert_eq!(saved.summary.usage.total_tokens, 17);
    assert!(saved.summary.usage.local);
    assert!(!saved.summary.usage_partial);
    assert_eq!(Progress::read(&source).unwrap().cursor, 0);

    append(
        RecordKind::CommandEnd,
        "shell integration: command finished (exit 0)",
    );
    wait_for_records(3);
    let completed = latest();
    assert_eq!(completed.end, End::Complete);
    let completion_usage = ObservedUsage {
        input_tokens: Some(9),
        output_tokens: None,
        total_tokens: Some(11),
        calls: 1,
        partial: true,
        local: false,
        cost: None,
        profile: None,
    };
    assert_eq!(publish(&completed, completion_usage.clone()).unwrap(), 3);
    assert!(publish(&completed, completion_usage).is_err());
    wait(capture.finish());
    let stopped = source.read(None).unwrap().unwrap();
    assert_eq!(stopped.timeline.len(), 1);
    assert_eq!(stopped.timeline[0].id, saved.timeline[0].id);
    assert_eq!(stopped.summary.usage.call_count, 2);
    assert_eq!(stopped.summary.usage.prompt_tokens, 21);
    assert_eq!(stopped.summary.usage.completion_tokens, 5);
    assert_eq!(stopped.summary.usage.total_tokens, 28);
    assert!(!stopped.summary.usage.local);
    assert!(stopped.summary.usage_partial);
    assert_eq!(Progress::read(&source).unwrap().cursor, 3);
}

#[test]
fn journal_usage_updates_recording_and_closed_metadata_without_adding_cards() {
    use crate::local_ai::{JournalCost, JournalProfileIdentity};
    use crate::session_journal::JournalProfileSelection;
    use crate::session_journal::summary_ai::ObservedUsage;

    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let source = viewer::Source::capture(&capture);
    let usage = ObservedUsage {
        input_tokens: Some(12),
        output_tokens: Some(5),
        total_tokens: Some(17),
        calls: 1,
        partial: false,
        local: false,
        cost: Some(JournalCost {
            numerator: 250_000_000_000,
            currency: *b"USD",
            partial: false,
        }),
        profile: Some(Arc::new(JournalProfileIdentity {
            reference: kortty_jobs::ProfileRef::Numeric(17),
            name: "Imported & reviewed".into(),
            java_id: Some("java-source".into()),
        })),
    };

    let pending = capture.directory().join(".journal.xml.pending");
    fs::create_dir(&pending).unwrap();
    assert!(wait_edit(capture.record_ai_usage(usage.clone()).unwrap()).is_err());
    fs::remove_dir(&pending).unwrap();

    assert!(
        capture
            .sink
            .shared
            .input
            .try_send(Item::FailNextMetadataSync)
            .is_ok()
    );
    assert!(wait_edit(capture.record_ai_usage(usage.clone()).unwrap()).is_err());
    let recording = source.read(None).unwrap().unwrap();
    assert_eq!(recording.summary.usage.call_count, 1);
    assert_eq!(recording.summary.usage.total_tokens, 17);
    assert_eq!(recording.summary.usage.cost, 0.25);
    assert_eq!(
        recording.summary.usage_profile_name.as_deref(),
        Some("Imported & reviewed")
    );
    assert_eq!(
        recording.summary.usage_profile,
        Some(JournalProfileSelection::Migrated(
            kortty_jobs::ProfileRef::Numeric(17),
            "java-source".into()
        ))
    );
    assert!(recording.timeline.is_empty());

    wait(capture.finish());
    let xml = fs::read_to_string(capture.directory().join("journal.xml")).unwrap();
    assert!(xml.contains("<aiProfileId>java-source</aiProfileId>"));
    source
        .record_ai_usage(ObservedUsage {
            profile: None,
            local: true,
            ..usage.clone()
        })
        .unwrap();
    let stopped = source.read(None).unwrap().unwrap();
    assert_eq!(stopped.summary.usage.call_count, 2);
    assert_eq!(stopped.summary.usage.total_tokens, 34);
    assert_eq!(stopped.summary.usage.cost, 0.5);
    assert_eq!(stopped.summary.usage.currency.as_deref(), Some("USD"));
    assert_eq!(
        stopped.summary.usage_profile,
        recording.summary.usage_profile
    );
    assert_eq!(
        stopped.summary.usage_profile_name,
        recording.summary.usage_profile_name
    );
    assert!(!stopped.summary.usage.cost_partial);
    assert!(!stopped.summary.usage.local);
    assert!(stopped.timeline.is_empty());

    source
        .record_ai_usage(ObservedUsage {
            profile: Some(Arc::new(JournalProfileIdentity {
                reference: kortty_jobs::ProfileRef::Policy("policy-team-model".into()),
                name: "Team model".into(),
                java_id: None,
            })),
            ..usage
        })
        .unwrap();
    let updated = source.read(None).unwrap().unwrap();
    assert_eq!(
        updated.summary.usage_profile_name.as_deref(),
        Some("Team model")
    );
    assert_eq!(
        updated.summary.usage_profile,
        Some(JournalProfileSelection::Saved(
            kortty_jobs::ProfileRef::Policy("policy-team-model".into())
        ))
    );
    let xml = fs::read_to_string(capture.directory().join("journal.xml")).unwrap();
    assert!(!xml.contains("<aiProfileId>"));
}

#[test]
fn journal_summary_full_output_reads_every_reviewed_record_and_writes_one_card() {
    use crate::session_journal::summary_plan::EvidenceLimits;

    check_journal_summary_full_output(None);
    check_journal_summary_full_output(Some(EvidenceLimits {
        max_lines_per_direction: 37,
        context_tokens: 2_400,
    }));
}

#[test]
fn configured_journal_full_output_keeps_grouped_evidence_after_exact_budget_overflow() {
    use std::io::Write as _;

    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_response, listener, request,
    };
    use crate::session_journal::summary_ai::Run;
    use crate::session_journal::summary_plan::EvidenceLimits;
    use serde_json::Value;

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let capture = Capture::start(fixture.journals(), Metadata::default()).unwrap();
    let sink = capture.sink();
    for (input, output, end) in [
        (
            "first",
            "EXPAND_FIRST",
            "shell integration: command finished (exit 0)",
        ),
        (
            "second",
            "EXPAND_SECOND",
            "shell integration: command finished (exit 2)",
        ),
    ] {
        for (kind, text) in [
            (RecordKind::Input, input),
            (RecordKind::Output, output),
            (RecordKind::CommandEnd, end),
        ] {
            sink.try_write(kind, Zeroizing::new(text.into()), false)
                .unwrap();
        }
    }
    wait(capture.finish());

    let source = viewer::Source::capture(&capture);
    let (listener, profile) = listener();
    let path = source.directory().join("journal.xml");
    let xml = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        xml.replace(
            "<appLanguageCode>en</appLanguageCode>",
            "<appLanguageCode>de-DE</appLanguageCode>",
        ),
    )
    .unwrap();
    let tokenizer = crate::local_ai::Tokenizer::O200kBase;
    let limits = EvidenceLimits {
        max_lines_per_direction: 2,
        context_tokens: 1_000,
    };
    let server = thread::spawn(move || {
        let mut results = Vec::new();
        loop {
            let mut stream = accept(&listener);
            let body = request(&mut stream);
            stream
                .get_mut()
                .write_all(
                    fixture_response(
                        "200 OK",
                        "",
                        r#"{"title":"Completed checks","summary":"Checked output.","keywords":[]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
            let system = body["messages"][0]["content"].as_str().unwrap();
            let prompt = body["messages"][1]["content"].as_str().unwrap();
            assert!(system.contains("Answer in language code de-DE."));
            assert!(tokenizer.count_messages([system, prompt]) <= limits.input_token_budget());
            if system.starts_with("Write a closing session journal summary") {
                return results;
            }
            assert!(system.starts_with("Summarize the observed tasks and outcomes"));
            assert!(!prompt.contains("EXPAND_"));
            results.push(serde_json::from_str::<Value>(prompt).unwrap());
        }
    });
    let (run, _cancel) = Run::begin(profile.with_tokenizer(tokenizer)).unwrap();
    let stop = AtomicBool::new(false);
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
    let expansion = "楽".repeat(900);
    let first = format!("{expansion} first-tail");
    let second = format!("{expansion} second-tail");
    let mask = |text: &str| {
        Ok(text
            .replace("EXPAND_FIRST", &first)
            .replace("EXPAND_SECOND", &second))
    };
    let pass = Pass {
        model: Some(&run),
        cancelled: &stop,
        mask: &mask,
        redactor: &redactor,
        full_output: true,
        evidence_limits: Some(limits),
    };
    let mut live = Vec::new();
    pass.visit_live_windows(&source, 0, jiff::Timestamp::now(), false, |window, _| {
        live.push((window.checkpoint(), window.commands.len()));
        Ok(())
    })
    .unwrap();
    assert_eq!(live, [(Some(6), 2)]);

    let generation = "0123456789abcdef0123456789abcdef";
    let outcome = pass
        .run_closed(
            Closed {
                source: &source,
                generation,
                mode: Mode::Automation,
                automatic_title: false,
                allow_title: &|| false,
                screenshots: false,
                automation_vision: None,
            },
            |_| {},
            &mut Reporter::default(),
        )
        .unwrap();
    let parts = server.join().unwrap();
    assert!(parts.len() > 2);
    let mut output = std::collections::BTreeMap::<u64, String>::new();
    let mut exits = Vec::new();
    for part in parts {
        exits.extend(
            part["command_exits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|command| {
                    (
                        command["sequence"].as_u64().unwrap(),
                        command["exit"].as_i64().unwrap(),
                    )
                }),
        );
        for line in part["evidence"].as_array().unwrap() {
            if line["kind"] == "output" {
                output
                    .entry(line["sequence"].as_u64().unwrap())
                    .or_default()
                    .push_str(line["text"].as_str().unwrap());
            }
        }
    }
    assert_eq!(exits, [(1, 0), (4, 2)]);
    assert_eq!(
        output.values().map(String::as_str).collect::<Vec<_>>(),
        [first.as_str(), second.as_str()]
    );
    assert_eq!(outcome.windows, 1);
    assert_eq!(outcome.raw, 0);
    let checkpoint = Progress::read(&source).unwrap();
    assert!(checkpoint.finished);
    assert_eq!(checkpoint.cursor, 6);
    assert_eq!(
        pass.run_closed(
            Closed {
                source: &source,
                generation,
                mode: Mode::Automation,
                automatic_title: false,
                allow_title: &|| false,
                screenshots: false,
                automation_vision: None,
            },
            |_| {},
            &mut Reporter::default()
        )
        .unwrap()
        .windows,
        0
    );
}

fn check_journal_summary_full_output(
    evidence_limits: Option<crate::session_journal::summary_plan::EvidenceLimits>,
) {
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
            let prompt = body["messages"][1]["content"].as_str().unwrap();
            if let Some(limits) = evidence_limits {
                assert!(
                    crate::local_ai::Tokenizer::default().count_messages([system, prompt])
                        <= limits.input_token_budget()
                );
            }
            if system.starts_with("Write a closing session journal summary") {
                return parts;
            }
            assert!(prompt.len() <= 28_000);
            assert!(!prompt.contains("ignored-test-only-help"));
            assert!(!prompt.contains("private-key-test-fixture"));
            assert!(!prompt.contains("reviewed-credential-test"));
            let part = serde_json::from_str::<Value>(prompt).unwrap();
            if let Some(limits) = evidence_limits {
                let lines = part["evidence"].as_array().unwrap();
                for direction in ["input", "output"] {
                    assert!(
                        lines
                            .iter()
                            .filter(|line| line["kind"] == direction)
                            .count()
                            <= limits.max_lines_per_direction as usize
                    );
                }
            }
            parts.push(part);
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
        evidence_limits,
    };
    let outcome = pass
        .run_closed(
            Closed {
                source: &source,
                generation: "0123456789abcdef0123456789abcdef",
                mode: Mode::Automation,
                automatic_title: false,
                allow_title: &|| false,
                screenshots: false,
                automation_vision: None,
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
        source_approval: None,
        raw_only: false,
        automatic_title: false,
        title_approval: None,
        full_output: false,
        evidence_limits: None,
        screenshots: false,
        automation_vision: None,
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
