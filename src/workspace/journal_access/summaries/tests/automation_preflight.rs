use super::*;
use crate::session_journal::automation::{
    self, Caps, CaptureSettings, Closing, Destination, Kind, Options, Recorder,
};
use crate::session_journal::tree::RunStatus;

#[test]
fn automation_journal_preflight_shares_failure_across_targets_and_keeps_raw_cards() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (scratch, mut workspace, terminal, source) = seeded(endpoint.local_addr().unwrap());
    finish_capture(&terminal);
    let profile_root = scratch.0.canonicalize().unwrap();
    let root = source.directory().parent().unwrap().to_owned();
    workspace
        .window_settings
        .save(&profile_root.join("window-settings.json"))
        .unwrap();
    let run = automation::Run::new(
        Kind::Job,
        "preflight-job".into(),
        "Preflight test".into(),
        "COMMAND",
        Timestamp::now(),
        Options {
            enabled: true,
            closing: Closing::Always,
            ..Default::default()
        },
        Caps::default(),
    )
    .unwrap();
    let settings = CaptureSettings {
        root: root.clone(),
        format: Default::default(),
        max_parts: None,
        markers: Default::default(),
        stopped: Default::default(),
    };
    let artifacts = ["first", "second"].map(|target| {
        let recorder = Recorder::start(
            &settings,
            run.clone(),
            Destination {
                key: target.into(),
                label: target.into(),
                host: "fixture.invalid".into(),
                port: Some(22),
                username: "fixture".into(),
            },
            Arc::new(Redactor::for_journal(
                Secrets::default().seal().unwrap(),
                Default::default(),
            )),
        )
        .unwrap();
        recorder.output("fixture-private-output", false);
        recorder.finish(RunStatus::Success, Some(0))
    });
    let stop = AtomicBool::new(false);
    let sweep = |completed: &[automation::Artifact]| {
        automation::retention::Sweep {
            root: &root,
            caps: Caps::default(),
            user_delete: true,
            summarize: true,
            cancelled_closing: &[],
            protected: &[],
            completed,
            recover: false,
            now: Timestamp::now(),
            cancelled: &stop,
        }
        .run()
        .unwrap()
    };
    let report = sweep(&artifacts);
    assert_eq!(report.closing.len(), 2);

    // Keep the transport listening after the first response: a second target must use the
    // run outcome and may not retry the failed probe or transmit captured evidence.
    let probe = thread::spawn(move || {
        let mut stream = accept(&endpoint);
        let body = request(&mut stream);
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains(r#"{"ready":true}"#)
        );
        assert_eq!(body["messages"][1]["content"], "{}");
        stream
            .get_mut()
            .write_all(
                fixture_response(
                    "200 OK",
                    "",
                    r#"{"ready":false,"error":"provider-private-error"}"#,
                )
                .as_bytes(),
            )
            .unwrap();
        drop(stream);
        endpoint
    });
    let task = workspace.enqueue_automation_closing_summaries(&report.closing);
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    for target in 0..2 {
        let next = loop {
            let event = next_worker(&mut actions);
            if let Desktop::JournalSummary(Worker::Finished(_, Ok(outcome))) = &event {
                assert!(outcome.automation_ai_unreachable);
                assert!(outcome.raw > 0);
                break workspace.update(event);
            }
            assert!(!matches!(
                event,
                Desktop::JournalSummary(Worker::Finished(..))
            ));
            let _ = workspace.update(event);
        };
        if target == 0 {
            actions = iced_runtime::task::into_stream(next).unwrap();
        }
    }
    let endpoint = probe.join().unwrap();
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    for artifact in &artifacts {
        let snapshot = artifact.source.read(None).unwrap().unwrap();
        assert!(snapshot.summary.needs_summary);
        assert!(snapshot.timeline.iter().any(|row| row.kind == "AI_SUMMARY"));
        assert!(
            !snapshot
                .timeline
                .iter()
                .any(|row| row.kind == "SESSION_SUMMARY")
        );
        let warnings = snapshot
            .timeline
            .iter()
            .filter(|row| row.kind == "SYSTEM" && row.title == "AI not reachable")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1);
        assert!(!warnings[0].text.contains("provider-private-error"));
        assert!(Progress::read(&artifact.source).unwrap().finished);
        assert!(
            artifact
                .source
                .capture_page(None, None, None)
                .unwrap()
                .records
                .iter()
                .any(|record| record.text == "fixture-private-output")
        );
    }
    assert!(sweep(&[]).closing.is_empty());
}
