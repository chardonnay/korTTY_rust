use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use kortty_ssh::{Endpoint, HostKey};

use super::{
    ExecutionPolicy, RunOutcome, TrustStore, automation, run, scheduled_request, virtual_terminal,
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::scheduler_window::JournalDetail;
use crate::session_journal::CaptureKind;
use crate::session_journal::automation::{Artifact, Caps, CaptureSettings, Kind, Options, Run};

#[test]
#[ignore = "requires an explicit pinned KORTTY_SSH_EXEC_* OpenSSH fixture"]
fn scheduled_command_publishes_split_redacted_output() {
    let fixture = |name| {
        std::env::var(format!("KORTTY_SSH_EXEC_{name}"))
            .expect("provide the explicit SSH exec fixture")
    };
    let address: SocketAddr = fixture("ADDRESS").parse().unwrap();
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let host_key = HostKey::from_openssh(&fixture("HOST_KEY")).unwrap();
    let (_, mut request) = scheduled_request();
    let action = Arc::make_mut(&mut request.action);
    action.host = address.ip().to_string();
    action.port = address.port();
    action.username = fixture("USER");
    action.key_file = PathBuf::from(fixture("PRIVATE_KEY"));
    action.journal_detail = JournalDetail::LimitedRedacted;
    action.command = concat!(
        "test ! -t 1 || exit 81; printf 'begin\\nnote\\033[0m\\007\\n'; ",
        "printf '%5000s' ''; printf 'ignored suffix\\n'; printf 'separate stderr\\n' >&2; exit 23",
    )
    .into();
    let completion = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(true),
        Ok(None),
        Ok(None),
        || TrustStore::from_pins(vec![(endpoint, host_key)]),
    );
    assert_eq!(
        completion.outcome,
        RunOutcome::Failed {
            exit_code: Some(23)
        }
    );
    let output = completion
        .output
        .as_ref()
        .and_then(|output| output.current(None))
        .unwrap();
    assert!(output.stdout.starts_with("begin\nnote\n"));
    assert!(output.stdout.ends_with("…[output truncated]"));
    assert_eq!(output.stdout.encode_utf16().count(), 4_000);
    assert!(!output.stdout.contains("ignored suffix") && !output.stdout.contains(['\x1b', '\x07']));
    assert_eq!(output.stderr, "separate stderr\n");
    assert!(completion.recording.is_none());
}

#[test]
#[ignore = "requires an explicit pinned KORTTY_SSH_EXEC_* OpenSSH fixture"]
fn scheduled_virtual_terminal_publishes_automation_journal_screenshots() {
    let fixture = |name| {
        std::env::var(format!("KORTTY_SSH_EXEC_{name}"))
            .expect("provide the explicit SSH exec fixture")
    };
    let address: SocketAddr = fixture("ADDRESS").parse().unwrap();
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let host_key = HostKey::from_openssh(&fixture("HOST_KEY")).unwrap();
    let username = fixture("USER");
    let key_file = PathBuf::from(fixture("PRIVATE_KEY"));
    let profile = crate::snippets::tests::Profile::new();

    for capture_journal in [true, false] {
        let (_, mut request) = scheduled_request();
        let action = Arc::make_mut(&mut request.action);
        action.host = address.ip().to_string();
        action.port = address.port();
        action.username.clone_from(&username);
        action.key_file.clone_from(&key_file);
        action.command =
            "stty size; printf 'first frame\\n'; sleep 2; printf '\\033[32mfinal frame\\033[0m\\n'"
                .into();
        action.virtual_terminal = Some(virtual_terminal::Options {
            columns: 40,
            rows: 10,
            interval_seconds: 1,
            max_frames: 2,
            ..Default::default()
        });
        let journals = profile.0.join(if capture_journal {
            "journals-enabled"
        } else {
            "journals-disabled"
        });
        let plan = capture_journal.then(|| {
            let options = Options {
                enabled: true,
                ..Options::default()
            };
            action.automation_journal = Some(options.clone());
            let scope = Run::new(
                Kind::Job,
                request.job.as_str().into(),
                "Scheduled virtual terminal fixture".into(),
                "COMMAND",
                request.started_at.try_into().unwrap(),
                options,
                Caps::default(),
            )
            .unwrap();
            automation::Plan::new(
                CaptureSettings {
                    root: journals.clone(),
                    format: Default::default(),
                    max_parts: None,
                    markers: Default::default(),
                    stopped: Arc::default(),
                },
                scope,
                ClipboardPolicy::load().terminal_logging,
            )
        });
        let recordings = profile.0.join("recordings");
        let completion = run(
            &request,
            Some(recordings.clone()),
            None,
            &ExecutionPolicy {
                automation: plan.clone(),
                ..ExecutionPolicy::unrestricted(true)
            },
            Ok(None),
            Ok(None),
            || TrustStore::from_pins(vec![(endpoint.clone(), host_key.clone())]),
        );
        assert_eq!(completion.outcome, RunOutcome::Success, "{completion:?}");
        let recording = completion.recording.expect("the replay was saved");
        let replay = kortty_recording::Replay::open(virtual_terminal::path_for(
            &recordings,
            request.id,
            recording,
        ))
        .unwrap();
        assert!(replay.is_complete());
        assert_eq!(replay.frame_count(), 2);

        let catalog = crate::session_journal::catalog(&journals).unwrap();
        assert_eq!(catalog.journals.len(), usize::from(capture_journal));
        let Some(plan) = plan else { continue };
        let [artifact]: [Artifact; 1] = plan.take_completed().try_into().unwrap();
        assert!(artifact.complete);
        let source = &artifact.source;
        let journal = source.read(None).unwrap().unwrap();
        assert_eq!(
            journal.summary.source_kind,
            crate::session_journal::tree::SourceKind::Job
        );
        let images: Vec<_> = journal
            .timeline
            .iter()
            .filter(|entry| entry.kind == "SCREENSHOT")
            .collect();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].text, "Virtual terminal screenshot");
        assert_eq!(images[1].text, "Final virtual terminal screenshot");
        let read_image = |entry: &crate::session_journal::TimelineEntry| {
            let start = entry.log_start.unwrap();
            let end = entry.log_end.unwrap();
            let capture = source.capture_page(None, Some(start..=end), None).unwrap();
            assert_eq!(capture.records.len(), 1);
            assert_eq!(capture.records[0].kind, CaptureKind::Screenshot);
            assert_eq!(capture.records[0].text, entry.text);
            let reference = entry.screenshot.as_deref().unwrap();
            let png = crate::session_journal::read_screenshot(source.directory(), reference)
                .expect("the journal screenshot exists and is valid");
            assert!(tiny_skia::Pixmap::decode_png(&png).is_ok());
            png
        };
        assert!(images[0].log_start.unwrap() < images[1].log_start.unwrap());
        assert_ne!(read_image(images[0]), read_image(images[1]));
        let capture = source.capture_page(None, None, None).unwrap();
        let captured = capture
            .records
            .iter()
            .map(|record| record.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(captured.contains("10 40"));
    }
}
