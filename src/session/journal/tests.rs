use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use async_channel::bounded;
use kortty_terminal::{ShellPosition, TerminalProfile, TextEncoding};

use crate::session::{Event, IbmKind, Session, StreamInput, WorkerEvents, write_input};
use crate::session_journal::{Capture, CaptureKind, capture_page};
use crate::snippets::tests::Profile;
use crate::terminal_logging::Secrets;

use super::*;

#[test]
fn input_capture_redacts_verified_commands_and_suppresses_password_prompts() {
    let profile = Profile::new();
    let capture = Capture::start(profile.0.join("journals"), Metadata::default()).unwrap();
    let secrets = Secrets::default();
    secrets.add("known-secret");
    let mut actor = Actor {
        secrets: Some(secrets.seal().unwrap()),
        active: None,
    };
    let mut terminal = Terminal::with_profile_and_encoding(
        80,
        24,
        TerminalProfile::Xterm,
        TextEncoding::Windows1252,
    );
    actor
        .attach(
            &mut terminal,
            1,
            capture.sink(),
            Policy::default(),
            false,
            true,
        )
        .unwrap();
    actor.bind_existing_prompt(Some(6));
    let permit = actor
        .permit(&terminal, Some(6), None, true, b"suffix", false)
        .unwrap();
    actor.written(permit, b"suffix");
    assert!(
        actor
            .permit(&terminal, Some(6), None, true, b"\r", true)
            .unwrap()
            .confirmation
            .is_none()
    );
    assert!(
        actor
            .permit(&terminal, Some(7), None, false, b"lost first key", false)
            .is_none()
    );
    let permit = actor
        .permit(&terminal, Some(7), None, true, b"suffix", false)
        .unwrap();
    actor.written(permit, b"suffix");
    assert!(
        actor
            .permit(&terminal, Some(7), None, true, b"\r", true)
            .unwrap()
            .confirmation
            .is_none()
    );

    let permit = actor
        .permit(
            &terminal,
            Some(8),
            None,
            true,
            b"echo known-secret \xe9",
            false,
        )
        .unwrap();
    actor.written(permit, b"echo known-secret \xe9");
    let confirmation = actor
        .permit(&terminal, Some(8), None, true, b"\r\n", true)
        .unwrap()
        .confirmation
        .unwrap();
    let permit = actor
        .permit(&terminal, Some(9), None, true, b"partial command", false)
        .unwrap();
    actor.written(permit, b"partial command");
    actor.suppress_input();
    let permit = actor
        .permit(&terminal, Some(9), None, true, b"continuation", false)
        .unwrap();
    actor.written(permit, b"continuation");
    assert!(
        actor
            .permit(&terminal, Some(9), None, true, b"\r", true)
            .unwrap()
            .confirmation
            .is_none()
    );
    let position = ShellPosition {
        row: 0,
        column: 0,
        scrollback_lines: 0,
        alternate_screen: false,
    };
    actor.output(
        &terminal,
        &[ShellEvent {
            kind: ShellEventKind::CommandFinished {
                exit_status: Some(3),
            },
            position,
        }],
    );

    terminal.advance(b"\r\nPassword: ");
    assert!(terminal.cursor_line_is_password_prompt());
    assert!(
        actor
            .permit(&terminal, Some(9), None, true, b"password", false)
            .is_none()
    );
    actor.stop(&mut terminal);
    let done = capture.finish();
    confirmation.try_send(()).unwrap();
    assert!(done.recv_blocking().is_err());

    let page = capture_page(capture.directory(), None, None, None).unwrap();
    let input: Vec<_> = page
        .records
        .iter()
        .filter(|record| record.kind == CaptureKind::Input)
        .map(|record| record.text.as_str())
        .collect();
    assert_eq!(input, ["echo *** é"]);
    let submitted = page
        .records
        .iter()
        .position(|record| record.kind == CaptureKind::Input)
        .unwrap();
    let ended = page
        .records
        .iter()
        .position(|record| {
            record.kind == CaptureKind::Note
                && record.text == "shell integration: command finished (exit 3)"
        })
        .unwrap();
    assert!(submitted < ended);
    assert!(
        page.records
            .iter()
            .all(|record| !record.text.contains("known-secret"))
    );
}

#[test]
fn failed_physical_write_never_reaches_the_journal_actor() {
    struct FailedWriter;
    impl Write for FailedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("test write failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (input, inputs) = bounded(1);
    let (sender, events) = bounded(4);
    let worker = WorkerEvents {
        sender,
        input: input.clone(),
        actor: thread::current(),
        recording: Arc::new(Default::default()),
        output_activity: Arc::new(Default::default()),
        observe_shell: true,
        journal_input: Arc::new(AtomicBool::new(true)),
    };
    let session = Session::disconnected_ibm(IbmKind::Tn3270e);
    let shared = Arc::clone(&session.shared);
    let write = thread::spawn(move || write_input(FailedWriter, &inputs, &worker, &shared, None));
    input
        .send_blocking(StreamInput::Keyboard {
            bytes: b"\r".to_vec(),
            enter: true,
            generation: None,
            physical: true,
        })
        .unwrap();
    let Ok(Event::KeyboardInput {
        journal: Some(reply),
        ..
    }) = events.recv_blocking()
    else {
        panic!("physical keyboard was not prepared");
    };
    let (confirmation, receipt) = std::sync::mpsc::sync_channel(1);
    reply
        .send_blocking(Some(Permit {
            id: 1,
            prompt: 7,
            output: None,
            confirmation: Some(confirmation),
            encoding: TextEncoding::Utf8,
        }))
        .unwrap();
    assert!(matches!(events.recv_blocking(), Ok(Event::IoError(_))));
    write.join().unwrap();
    assert!(matches!(
        receipt.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected)
    ));
    assert!(events.is_empty());
}
