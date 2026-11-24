use std::io::Write;
use std::path::Path;

use iced::futures::StreamExt;
use iced_runtime::{Action, task};

use crate::connection_store::SerialProfile;
use crate::serial_dialog;
use crate::session::{SerialConnection, Session, Status as SessionStatus, serial_test_pair};
use crate::session_journal::CaptureKind;

use super::tests::{Scratch, deliver_journal_attachment, wait};
use super::*;

#[test]
fn serial_journal_keeps_masked_output_across_a_same_device_reconnect() {
    use kortty_terminal::TerminalProfile::{self, Hp2392, Hp70092, Petscii, Tvi925, Vt100, Wy60};

    for profile in [Vt100, Petscii, Wy60, Tvi925, Hp2392, Hp70092] {
        for required in [false, true] {
            assert_serial_journal(profile, required);
        }
    }

    fn assert_serial_journal(profile: TerminalProfile, required: bool) {
        let scratch = Scratch::new();
        let (mut peer, device) = serial_test_pair().unwrap();
        let (mut other_peer, other_device) = serial_test_pair().unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy = crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.session-journal]\nenforced = {required}\n\
             [[rule.session-journal.replace]]\npattern = 'SERIAL PRIVATE'\nreplacement = 'SERIAL MASKED'",
        ))
        .unwrap();
        workspace.window_settings.session_journal.directory =
            scratch.0.join("journals").display().to_string();
        let (owner, _) = workspace.open_window(false);
        let mut saved = SerialProfile::new(
            "Virtual serial fixture",
            SerialConnection {
                device,
                local_echo: true,
                ..Default::default()
            },
            profile,
        );
        saved.journal.auto_start = !required;
        saved.journal.capture_input = true;
        let connect = workspace.open_saved_serial_with_privacy(owner, saved, required);
        let active = workspace.windows[&owner].tab().unwrap();
        assert_eq!(active.incognito, required);
        let tab = active.id;
        let source = active.focused_session().unwrap();
        let shell = &workspace.sessions[&source].session;
        wait(|| shell.journal_available() && shell.frame().status == SessionStatus::Running);
        let start = Task::batch([connect, workspace.update(Desktop::SessionUpdate(source))]);
        let started = journal_event(start, |event| matches!(event, Message::Started(..)));
        let attach = workspace.update_journal_capture(started);
        deliver_journal_attachment(&mut workspace, attach);
        assert!(!workspace.journal_captures[&tab].capture_input);
        assert_eq!(workspace.active_dock_journal(owner).is_none(), required);
        let capture = workspace.journal_captures[&tab]
            .recording()
            .unwrap()
            .clone();

        let shell = &workspace.sessions[&source].session;
        peer.write_all(b"SERIAL PRIVATE BEFORE\r\n").unwrap();
        wait_for_text(shell, "SERIAL PRIVATE BEFORE");
        shell
            .send_user_input(b"LOCAL KEYBOARD ONLY".to_vec(), false)
            .unwrap();
        wait_for_text(shell, "LOCAL KEYBOARD ONLY");
        disconnect(&mut workspace, source);
        assert_eq!(capture.status(), Status::Recording);

        let missing_device = scratch.0.join("unavailable-device");
        let _ = connect_from_pane(&mut workspace, owner, source, Some(&missing_device));
        let active = workspace.windows[&owner].tab().unwrap();
        assert_eq!(active.focused_session(), Some(source));
        assert_eq!(capture.status(), Status::Recording);

        let attach = connect_from_pane(&mut workspace, owner, source, None);
        deliver_journal_attachment(&mut workspace, attach);
        let active = workspace.windows[&owner].tab().unwrap();
        assert_eq!(active.id, tab);
        let replacement = active.focused_session().unwrap();
        assert_ne!(replacement, source);
        assert!(Arc::ptr_eq(
            workspace.journal_captures[&tab].recording().unwrap(),
            &capture
        ));
        peer.write_all(b"SERIAL PRIVATE AFTER\r\n").unwrap();
        wait_for_text(
            &workspace.sessions[&replacement].session,
            "SERIAL PRIVATE AFTER",
        );
        disconnect(&mut workspace, replacement);

        // Choosing another port from the disconnected pane starts a separate stream.
        let _ = connect_from_pane(&mut workspace, owner, replacement, Some(&other_device));
        let other = workspace.windows[&owner]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        assert_ne!(other, replacement);
        wait(|| capture.done().is_closed());
        other_peer.write_all(b"OTHER PORT ONLY\r\n").unwrap();
        wait_for_text(&workspace.sessions[&other].session, "OTHER PORT ONLY");

        let page = ViewerSource::capture(&capture)
            .capture_page(None, None, None)
            .unwrap();
        assert!(page.status.notice().is_none(), "{page:?}");
        let records: Vec<_> = page
            .records
            .iter()
            .map(|record| (record.kind, record.text.as_str()))
            .collect();
        assert_eq!(
            records,
            [
                (CaptureKind::Note, "isolation: none"),
                (CaptureKind::Output, "SERIAL MASKED BEFORE"),
                (CaptureKind::Note, "session reconnected"),
                (CaptureKind::Note, "isolation: none"),
                (CaptureKind::Output, "SERIAL MASKED AFTER"),
            ],
            "{profile:?}, required={required}",
        );
        workspace.stop_session(other);
        let _ = workspace.reconcile_journal_sources();
        for done in std::mem::take(&mut workspace.pending_shutdown) {
            let _ = done.recv_blocking();
        }
    }
}

fn disconnect(workspace: &mut Workspace, id: SessionId) {
    let released = workspace.sessions[&id].session.disconnect_serial().unwrap();
    wait(|| released.is_closed());
    let retire = workspace.update(Desktop::SessionUpdate(id));
    let retired = journal_event(retire, |event| matches!(event, Message::SourceRetired(..)));
    let _ = workspace.update_journal_capture(retired);
}

fn connect_from_pane(
    workspace: &mut Workspace,
    owner: window::Id,
    source: SessionId,
    device: Option<&Path>,
) -> Task<Desktop> {
    let _ = workspace.update(Desktop::ShowSerial(owner, Some(source)));
    assert!(workspace.windows[&owner].pending_serial.is_some());
    if let Some(device) = device {
        let _ = workspace.update(Desktop::Serial(
            owner,
            serial_dialog::Message::Device(device.display().to_string()),
        ));
    }
    let connect = workspace.update(Desktop::Serial(owner, serial_dialog::Message::Connect));
    if let Some(candidate) = workspace.pending_reconnects.keys().next().copied() {
        workspace.complete_test_reconnect(candidate)
    } else {
        connect
    }
}

fn journal_event(source: Task<Desktop>, expected: impl Fn(&Message) -> bool) -> Message {
    let mut events = task::into_stream(source).unwrap();
    loop {
        if let Action::Output(Desktop::JournalCapture(event)) =
            block_on(events.next()).expect("serial journal lifecycle event")
            && expected(&event)
        {
            return event;
        }
    }
}

fn wait_for_text(session: &Session, expected: &str) {
    wait(|| {
        session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(expected)
    });
}
