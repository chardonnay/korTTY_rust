use std::fs;

use crate::session_journal::tree::SourceKind;
use crate::session_journal::{MetadataEdit, catalog, edit_metadata};
use crate::terminal_logging::{Policy, Secrets};

use super::retention::Sweep;
use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-automation-journal-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn settings(&self) -> CaptureSettings {
        CaptureSettings {
            root: self.0.join("journals"),
            format: LogFormat::default(),
            max_parts: None,
            markers: Arc::new(Compiled::default()),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run(&self, offset: i64, options: Options) -> Run {
        Run::new(
            Kind::Job,
            "nightly".into(),
            "Nightly validation".into(),
            "COMMAND",
            Timestamp::from_second(Timestamp::now().as_second() + offset).unwrap(),
            Options {
                enabled: true,
                ..options
            },
            Caps::default(),
        )
        .unwrap()
    }

    fn start(&self, run: Run, target: &str) -> Recorder {
        let secrets = Secrets::default();
        secrets.add("fixture-password");
        Recorder::start(
            &self.settings(),
            run,
            Destination {
                key: target.into(),
                label: format!("Host {target}"),
                host: "example.test".into(),
                port: Some(22),
                username: "operator".into(),
            },
            Arc::new(Redactor::for_journal(
                secrets.seal().unwrap(),
                Policy::default(),
            )),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn automation_field(artifact: &Artifact, name: &str) -> Option<String> {
    artifact
        .source
        .with_document(|_, document| {
            let metadata = super::super::child(document.root_element(), "meta").unwrap();
            super::super::field(metadata, name).map(|value| value.map(|value| value.into_owned()))
        })
        .unwrap()
}

fn ordered_fixture(
    fixture: &Fixture,
    offset: i64,
    capture_input: bool,
    commands: &[&str],
) -> Recorder {
    let recorder = fixture
        .start(
            fixture.run(
                offset,
                Options {
                    capture_input,
                    ..Options::default()
                },
            ),
            "ordered",
        )
        .ordered_commands();
    for command in commands {
        recorder.command(command);
    }
    recorder.output("shared fixture output", false);
    recorder
}

#[test]
fn file_automation_outcome_is_a_masked_system_note_without_a_shell_end() {
    use crate::session_journal::CaptureKind;

    let fixture = Fixture::new();
    let recorder = fixture
        .start(fixture.run(-60, Options::default()), "fixture-target")
        .ordered_commands();
    recorder.command("sftp_upload fixture.txt");
    recorder.system_note("Blocked fixture-pass\u{202e}word\n漢");
    recorder
        .capture
        .sink()
        .try_write(
            RecordKind::Note,
            Zeroizing::new("Operator note".into()),
            false,
        )
        .unwrap();
    let artifact = recorder.finish_status(RunStatus::Blocked, false);
    assert!(artifact.complete);
    assert!(automation_field(&artifact, COMMAND_DIGEST).is_some());

    let saved = artifact.source.read(None).unwrap().unwrap();
    assert_eq!(saved.summary.run_status, Some(RunStatus::Blocked));
    assert_eq!(saved.timeline.len(), 2);
    let outcome = saved
        .timeline
        .iter()
        .find(|entry| entry.kind == "SYSTEM")
        .unwrap();
    assert_eq!(outcome.text, "Blocked ***\n漢");
    assert!(outcome.extra_details.iter().any(|detail| detail == "raw"));
    assert!(outcome.log_start.is_some());
    assert!(
        saved
            .timeline
            .iter()
            .any(|entry| { entry.kind == "USER_NOTE" && entry.text == "Operator note" })
    );
    let page = artifact.source.capture_page(None, None, None).unwrap();
    assert_eq!(
        page.records
            .iter()
            .map(|record| (record.kind, record.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (CaptureKind::Note, "Blocked ***\n漢"),
            (CaptureKind::Note, "Operator note")
        ]
    );
}

#[test]
fn oversized_file_automation_outcome_truncates_and_keeps_run_status() {
    let fixture = Fixture::new();
    let recorder = fixture
        .start(fixture.run(-60, Options::default()), "fixture-target")
        .ordered_commands();
    let done = recorder.capture.done();
    let note = format!(
        "fixture-password{}",
        "x".repeat(super::super::MAX_RECORD_BYTES)
    );
    recorder.system_note(&note);
    let _ = done.recv_blocking();
    let artifact = recorder.finish_status(RunStatus::Cancelled, false);
    assert!(!artifact.complete);
    let saved = artifact.source.read(None).unwrap().unwrap();
    assert_eq!(saved.summary.run_status, Some(RunStatus::Cancelled));
    assert_eq!(saved.timeline.len(), 1);
    assert!(saved.timeline[0].text.contains("capture stopped early"));
    assert!(!saved.timeline[0].text.contains("fixture-password"));
    assert!(
        artifact
            .source
            .capture_page(None, None, None)
            .unwrap()
            .records
            .is_empty()
    );
}

#[test]
fn ordered_command_digest_preserves_masked_boundaries_and_ignores_capture_input() {
    let fixture = Fixture::new();
    let recorder = ordered_fixture(&fixture, -60, false, &["echo fixture-password", "pwd"]);
    let pending = recorder.begin_finish(RunStatus::Success, Some(0), false);
    assert_ne!(pending.capture.status(), Status::Recording);
    let hidden = iced::futures::executor::block_on(pending.complete());
    assert!(hidden.complete);
    assert_eq!(hidden.outcome, RunStatus::Success);

    let visible = ordered_fixture(&fixture, -50, true, &["echo ***", "pwd"])
        .finish(RunStatus::Success, Some(0));
    let mut expected = ORDERED_COMMAND_DOMAIN.to_vec();
    for command in ["echo ***", "pwd"] {
        expected.extend_from_slice(&(command.len() as u64).to_le_bytes());
        expected.extend_from_slice(command.as_bytes());
    }
    let expected = Some(identity_digest(&expected).unwrap());
    assert_eq!(automation_field(&hidden, COMMAND_DIGEST), expected);
    assert_eq!(automation_field(&visible, COMMAND_DIGEST), expected);
    for (artifact, expected_inputs) in [(&hidden, vec![]), (&visible, vec!["echo ***", "pwd"])] {
        let page = artifact.source.capture_page(None, None, None).unwrap();
        let inputs: Vec<_> = page
            .records
            .iter()
            .filter(|record| record.kind == super::super::CaptureKind::Input)
            .map(|record| record.text.as_str())
            .collect();
        assert_eq!(inputs, expected_inputs);
    }

    let legacy = fixture.start(fixture.run(-40, Options::default()), "legacy");
    legacy.command("echo fixture-password");
    legacy.command("pwd");
    let legacy = legacy.finish(RunStatus::Success, Some(0));
    assert_eq!(
        automation_field(&legacy, COMMAND_DIGEST),
        Some(identity_digest(b"echo ***").unwrap())
    );
}

#[test]
fn automation_dedup_keeps_different_command_order_and_boundaries() {
    let fixture = Fixture::new();
    let original =
        ordered_fixture(&fixture, -60, false, &["ab", "c"]).finish(RunStatus::Success, Some(0));
    let duplicate =
        ordered_fixture(&fixture, -50, false, &["ab", "c"]).finish(RunStatus::Success, Some(0));
    let reversed =
        ordered_fixture(&fixture, -40, false, &["c", "ab"]).finish(RunStatus::Success, Some(0));
    let reframed =
        ordered_fixture(&fixture, -30, false, &["a", "bc"]).finish(RunStatus::Success, Some(0));
    let settings = fixture.settings();
    let sweep = Sweep {
        root: &settings.root,
        caps: Caps::default(),
        user_delete: true,
        summarize: false,
        cancelled_closing: &[],
        protected: &[],
        completed: &[],
        recover: true,
        now: Timestamp::now(),
        cancelled: &AtomicBool::new(false),
    };
    let settled = sweep.run().unwrap();
    assert_eq!(settled.warnings, 0);
    assert_eq!(settled.removed, 1);
    assert!(original.source.directory().exists());
    assert!(!duplicate.source.directory().exists());
    assert!(reversed.source.directory().exists());
    assert!(reframed.source.directory().exists());
}

#[test]
fn an_unmasked_command_marks_the_finished_artifact_incomplete() {
    let fixture = Fixture::new();
    let recorder = ordered_fixture(&fixture, -60, false, &["pwd"]);
    let oversized = format!("fixture-password{}", "x".repeat(64 * 1024));
    recorder.command(&oversized);
    let pending = recorder.begin_finish(RunStatus::Failed, Some(7), false);
    let artifact = iced::futures::executor::block_on(pending.complete());
    assert_eq!(artifact.outcome, RunStatus::Failed);
    assert!(!artifact.complete);
    assert_eq!(
        automation_field(&artifact, COMPLETE).as_deref(),
        Some("false")
    );
    assert_eq!(
        automation_field(&artifact, "runStatus").as_deref(),
        Some("FAILED")
    );
    assert!(automation_field(&artifact, COMMAND_DIGEST).is_some());
}

#[test]
fn ordered_capture_repairs_a_writer_sealed_before_the_outcome() {
    let fixture = Fixture::new();
    let recorder = ordered_fixture(&fixture, -60, false, &["pwd"]);
    let metadata = recorder.metadata.clone();
    let source = Source::capture(&recorder.capture);
    let done = recorder.capture.done();
    recorder.capture.sink().mark_truncated();
    let _ = done.recv_blocking();
    let early = source.read(None).unwrap().unwrap().summary;
    let ended = early.ended_at.unwrap();
    assert_eq!(early.run_status, None);

    let pending = recorder.begin_finish(RunStatus::Failed, Some(7), false);
    let artifact = iced::futures::executor::block_on(pending.complete());
    assert!(!artifact.complete);
    assert_eq!(artifact.outcome, RunStatus::Failed);
    let finished = source.read(None).unwrap().unwrap().summary;
    assert_eq!(finished.ended_at, Some(ended));
    assert_eq!(finished.run_status, Some(RunStatus::Failed));
    assert_eq!(
        automation_field(&artifact, COMPLETE).as_deref(),
        Some("false")
    );
    let expiry = metadata
        .run
        .options()
        .expiry(ended, metadata.run.0.caps)
        .unwrap();
    assert_eq!(
        automation_field(&artifact, "expiresAt"),
        expiry.map(|time| time.to_string())
    );

    let mut wrong_target = metadata.clone();
    wrong_target.target = "different-target".into();
    assert!(reconcile_terminal_metadata(&source, &wrong_target, RunStatus::Cancelled).is_err());
    reconcile_terminal_metadata(&source, &metadata, RunStatus::Cancelled).unwrap();
    assert_eq!(
        automation_field(&artifact, "runStatus").as_deref(),
        Some("FAILED")
    );
}

#[test]
fn dense_swarm_transcript_is_masked_before_bounded_capture_chunks() {
    let fixture = Fixture::new();
    let recorder = fixture.start(fixture.run(-10, Options::default()), "dense");
    let transcript = "fixture-password on one line\n".repeat(1_500);
    recorder.output(&transcript, false);
    let artifact = recorder.finish(RunStatus::Success, Some(0));
    assert!(artifact.complete);
    let capture = artifact.source.capture_page(None, None, None).unwrap();
    let output = capture
        .records
        .iter()
        .filter(|row| row.kind == super::super::CaptureKind::Output)
        .map(|row| row.text.as_str())
        .collect::<String>();
    assert_eq!(output, "*** on one line\n".repeat(1_500));
}

#[test]
fn automation_records_share_a_durable_run_and_dedup_uses_the_command_and_target() {
    let fixture = Fixture::new();
    let options = Options {
        closing: Closing::Always,
        ..Options::default()
    };
    let run = fixture.run(-100, options.clone());
    let first = fixture.start(run.clone(), "first");
    first.command("check --password fixture-password");
    first.output("success fixture-password", false);
    let first = first.finish(RunStatus::Success, Some(0));
    let second = fixture.start(run, "second");
    second.command("check --password fixture-password");
    second.output("missing package", false);
    let second = second.finish(RunStatus::Failed, Some(2));
    assert!(first.complete && second.complete);

    let duplicate = fixture.start(fixture.run(-90, options.clone()), "first");
    duplicate.command("check --password fixture-password");
    duplicate.output("success fixture-password", false);
    let duplicate = duplicate.finish(RunStatus::Success, Some(0));
    let different = fixture.start(fixture.run(-80, options), "first");
    different.command("other-check");
    different.output("success fixture-password", false);
    let different = different.finish(RunStatus::Success, Some(0));
    let stop = AtomicBool::new(false);
    let settings = fixture.settings();
    let sweep = Sweep {
        root: &settings.root,
        caps: Caps::default(),
        user_delete: true,
        summarize: true,
        cancelled_closing: &[],
        protected: &[],
        completed: &[],
        recover: true,
        now: Timestamp::now(),
        cancelled: &stop,
    };
    let settled = sweep.run().unwrap();
    assert_eq!(settled.warnings, 0);
    assert_eq!(settled.removed, 1);
    assert!(!duplicate.source.directory().exists());
    assert!(different.source.directory().exists());
    assert_eq!(settled.closing.len(), 3);
    let a = first.source.read(None).unwrap().unwrap().summary;
    let b = second.source.read(None).unwrap().unwrap().summary;
    assert_eq!(a.source_kind, SourceKind::Job);
    assert_eq!(a.run_id, b.run_id);
    assert_eq!(a.run_id.as_ref().unwrap().len(), 36);
    assert_eq!(a.run_status, Some(RunStatus::Success));
    assert_eq!(b.run_status, Some(RunStatus::Failed));
    assert_eq!(a.duplicate_run_count, 1);
    assert!(a.storage_bytes > 0);
    let log = first.source.capture_page(None, None, None).unwrap();
    assert!(
        log.records
            .iter()
            .all(|record| record.kind != super::super::CaptureKind::Input)
    );
    assert!(
        log.records
            .iter()
            .any(|record| record.text == "success ***")
    );
    assert_eq!(sweep.run().unwrap().removed, 0);
    assert_eq!(
        first
            .source
            .read(None)
            .unwrap()
            .unwrap()
            .summary
            .duplicate_run_count,
        1
    );

    let job = kortty_jobs::JobId::new("nightly").unwrap();
    let refs = [
        second.source.id().to_owned(),
        duplicate.source.id().to_owned(),
    ];
    let linked = run_links::resolve(&settings.root, &job, &refs).unwrap();
    assert_eq!(linked.missing(), 0);
    assert!(!linked.limited);
    let linked_entries: Vec<_> = linked.entries().collect();
    assert_eq!(linked_entries.len(), 2);
    assert!(linked_entries[0].source.same_journal(&second.source));
    assert_eq!(linked_entries[0].title, "Nightly validation · Host second");
    assert_eq!(linked_entries[0].status, b.run_status);
    assert!(!linked_entries[0].duplicate);
    assert!(linked_entries[1].source.same_journal(&first.source));
    assert!(linked_entries[1].duplicate);
    assert_eq!(linked_entries[1].status, a.run_status);
    assert!(linked_entries[1].started_at.is_some());
    assert_eq!(linked.rows[1].id, duplicate.source.id());
    let wrong_job = kortty_jobs::JobId::new("other-job").unwrap();
    let other = run_links::resolve(&settings.root, &wrong_job, &refs).unwrap();
    assert!(other.entries().next().is_none());
    assert_eq!(other.missing(), 2);

    let missing_path = duplicate.source.directory();
    fs::create_dir(missing_path).unwrap();
    let unavailable = run_links::resolve(&settings.root, &job, &refs).unwrap();
    assert_eq!(unavailable.missing(), 1);
    assert!(
        unavailable
            .entries()
            .next()
            .unwrap()
            .source
            .same_journal(&second.source)
    );
    fs::remove_dir(missing_path).unwrap();

    // A second retained native journal cannot claim the same receipt.
    let path = different.source.directory().join("journal.xml");
    let xml = fs::read_to_string(&path).unwrap();
    let document = parse_document(&xml).unwrap();
    let meta = child(document.root_element(), "meta").unwrap();
    let receipts = serde_json::to_string(&[duplicate.source.id()]).unwrap();
    let mut patches = Vec::new();
    patch_scalars(
        &xml,
        meta,
        [(DUPLICATE_RECEIPTS, Some(receipts.as_str()))],
        &mut patches,
    )
    .unwrap();
    fs::write(&path, apply_patches(&xml, patches).unwrap()).unwrap();
    let ambiguous = run_links::resolve(&settings.root, &job, &refs).unwrap();
    assert_eq!(ambiguous.missing(), 1);
    assert!(
        ambiguous
            .entries()
            .next()
            .unwrap()
            .source
            .same_journal(&second.source)
    );
}

#[test]
fn automation_retention_preserves_pinned_pending_live_and_interactive_journals() {
    let fixture = Fixture::new();
    let options = Options {
        keep: Keep::Failures,
        retention: Retention::Days { days: 1 },
        ..Options::default()
    };
    let success = fixture
        .start(fixture.run(-100, options.clone()), "success")
        .finish(RunStatus::Success, Some(0));
    let pinned = fixture
        .start(fixture.run(-95, options.clone()), "pinned")
        .finish(RunStatus::Failed, Some(1));
    edit_metadata(
        pinned.source.directory(),
        pinned.source.id(),
        &MetadataEdit::Pinned(true),
    )
    .unwrap();
    let pending = fixture
        .start(fixture.run(-90, options.clone()), "pending")
        .finish(RunStatus::Failed, Some(1));
    let abandoned = fixture.start(fixture.run(-85, options.clone()), "abandoned");
    abandoned.output("recoverable activity", false);
    let abandoned_source = Source::capture(&abandoned.capture);
    let done = abandoned.capture.done();
    drop(abandoned);
    let _ = done.recv_blocking();
    let live = fixture.start(fixture.run(-80, options), "live");
    let live_source = Source::capture(&live.capture);
    let settings = fixture.settings();
    let interactive = Capture::start(settings.root.clone(), Metadata::default()).unwrap();
    let _ = interactive.finish().recv_blocking();

    let stop = AtomicBool::new(false);
    let protect = [pending.source.clone(), live_source];
    let initial = Sweep {
        root: &settings.root,
        caps: Caps::default(),
        user_delete: true,
        summarize: false,
        cancelled_closing: &[],
        protected: &protect,
        completed: &[],
        recover: true,
        now: Timestamp::now(),
        cancelled: &stop,
    };
    assert_eq!(initial.run().unwrap().removed, 1);
    assert!(!success.source.directory().exists());
    assert_eq!(
        abandoned_source
            .read(None)
            .unwrap()
            .unwrap()
            .summary
            .run_status,
        Some(RunStatus::Cancelled)
    );
    let later = Sweep {
        now: Timestamp::now()
            .checked_add(Duration::from_secs(172_800))
            .unwrap(),
        ..initial
    };
    assert_eq!(later.run().unwrap().removed, 1);
    assert!(pinned.source.directory().exists());
    assert!(pending.source.directory().exists());
    assert!(live.capture.directory().exists());
    assert!(interactive.directory().exists());
    assert_eq!(catalog(&settings.root).unwrap().journals.len(), 4);
    let _ = live.finish(RunStatus::Cancelled, None);
}
