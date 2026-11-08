use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::session::{IbmAid, IbmInput, IbmKind};

const TIMEOUT: Duration = Duration::from_secs(5);

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a session update"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn primary(workspace: &Workspace, owner: window::Id) -> SessionId {
    let tab = workspace.windows[&owner].tab().unwrap();
    *tab.grid.get(tab.primary).unwrap()
}

#[cfg(unix)]
#[test]
fn active_serial_reconnect_requires_confirmation_and_waits_for_the_real_port() {
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    use crate::session::SerialConnection;
    use crate::workspace::{TerminalProfile, initial_grid};

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&master, FdFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let device = ptsname(&master, Vec::new()).unwrap().into_string().unwrap();
    let mut master = std::fs::File::from(master);
    let profile = TerminalProfile::Xterm;
    let grid = initial_grid(profile);
    let connection = SerialConnection {
        device: device.into(),
        ..SerialConnection::default()
    };
    let session = Session::start_serial(connection, grid, profile, true, None).unwrap();
    let old = workspace.store_session(session, grid, 20);
    workspace.add_tab(owner, old);
    let tab = workspace.windows[&owner].tab().unwrap().id;
    wait_for(|| workspace.sessions[&old].session.frame().status == Status::Running);
    let write_history = |master: &mut std::fs::File| {
        master
            .write_all(b"\x1b[31mstyled-serial-history\x1b[0m\r\n")
            .unwrap();
        master.write_all(&b"serial-line\r\n".repeat(60)).unwrap();
    };
    let contains = |workspace: &Workspace, session: SessionId, marker: &str| {
        workspace.sessions[&session]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(marker)
    };
    write_history(&mut master);
    wait_for(|| {
        workspace.sessions[&old]
            .session
            .frame()
            .screen
            .scrollback_lines
            > 0
    });

    let pending = |workspace: &Workspace| {
        let state = workspace.windows[&owner]
            .pending_serial
            .as_ref()
            .unwrap()
            .reconnect
            .as_ref()
            .unwrap();
        (state.token, state.released.clone())
    };
    let _ = workspace.reconnect_tab(owner, tab);
    let (canceled, _) = pending(&workspace);
    let _ = workspace.update(Message::Serial(owner, serial_dialog::Message::Connect));
    let _ = workspace.update(Message::Serial(
        owner,
        serial_dialog::Message::Reconnect(canceled + 1),
    ));
    assert!(
        workspace.sessions[&old]
            .session
            .send(b"not-stopped".to_vec())
            .is_ok()
    );
    let _ = workspace.update(Message::Serial(
        owner,
        serial_dialog::Message::Reconnect(canceled),
    ));
    assert_eq!(primary(&workspace, owner), old);
    assert_eq!(workspace.sessions.len(), 1);
    let (_, released) = pending(&workspace);
    let released = released.unwrap();
    let _ = workspace.update(Message::Serial(owner, serial_dialog::Message::Cancel));
    wait_for(|| released.is_closed());
    let _ = workspace.update(Message::SerialReconnectReleased(owner, canceled));
    assert_eq!(primary(&workspace, owner), old);
    assert_eq!(
        workspace.sessions[&old].session.frame().status,
        Status::Disconnected
    );
    assert!(
        !workspace.sessions[&old]
            .session
            .shutdown_handle()
            .is_closed()
    );
    let search = workspace.sessions[&old]
        .session
        .search("styled-serial-history", false)
        .unwrap();
    wait_for(|| {
        workspace.sessions[&old]
            .session
            .frame()
            .search_result()
            .is_some_and(|result| result.request == search && result.results.matches.len() == 1)
    });
    workspace.sessions[&old].session.scroll(1000);
    wait_for(|| contains(&workspace, old, "styled-serial-history"));

    let _ = workspace.reconnect_tab(owner, tab);
    let (stopped, _) = pending(&workspace);
    assert!(
        !workspace.windows[&owner]
            .pending_serial
            .as_ref()
            .unwrap()
            .reconnect
            .as_ref()
            .unwrap()
            .disconnect
    );
    let _ = workspace.update(Message::Serial(
        owner,
        serial_dialog::Message::Reconnect(stopped),
    ));
    let candidate = workspace
        .sessions
        .keys()
        .copied()
        .find(|id| *id != old)
        .unwrap();
    workspace.wait_for_test_reconnect(candidate);
    let old = primary(&workspace, owner);
    assert_eq!(old, candidate);
    workspace.sessions[&old].session.scroll(1000);
    wait_for(|| contains(&workspace, old, "styled-serial-history"));
    workspace.sessions[&old].session.scroll(-1000);
    let pane = workspace.sessions.get_mut(&old).unwrap();
    pane.strict_escapes = true;
    pane.session.set_strict_escapes(true);
    write_history(&mut master);
    wait_for(|| {
        workspace.sessions[&old]
            .session
            .frame()
            .screen
            .scrollback_lines
            > 0
    });

    let _ = workspace.reconnect_tab(owner, tab);
    let (confirmed, _) = pending(&workspace);
    let _ = workspace.update(Message::Serial(
        owner,
        serial_dialog::Message::Reconnect(confirmed),
    ));
    let _ = workspace.update(Message::SerialReconnectReleased(owner, canceled));
    assert_eq!(primary(&workspace, owner), old);
    assert_eq!(workspace.sessions.len(), 1);
    let (_, released) = pending(&workspace);
    let released = released.unwrap();
    wait_for(|| released.is_closed());
    let _ = workspace.update(Message::SerialReconnectReleased(owner, confirmed));
    let candidate = workspace
        .sessions
        .keys()
        .copied()
        .find(|id| *id != old)
        .unwrap();
    workspace.wait_for_test_reconnect(candidate);
    let new = primary(&workspace, owner);
    assert_eq!(new, candidate);
    assert_eq!(workspace.sessions[&new].font, 20);
    assert!(workspace.sessions[&new].strict_escapes);
    workspace.sessions[&new].session.scroll(1000);
    wait_for(|| contains(&workspace, new, "styled-serial-history"));
    assert!(
        workspace.sessions[&new]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .any(|cell| cell.text == "s"
                && cell.style.foreground == kortty_terminal::Color::Indexed(1))
    );
    workspace.sessions[&new].session.scroll(-1000);
    master.write_all(b"fresh-serial-connection").unwrap();
    wait_for(|| contains(&workspace, new, "fresh-serial-connection"));
}

// Loopback Telnet peer used only to verify the real desktop IBM session worker.
fn accept_peer(listener: &TcpListener) -> TcpStream {
    let mut peer = None;
    wait_for(|| match listener.accept() {
        Ok((stream, _)) => {
            peer = Some(stream);
            true
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        Err(error) => panic!("IBM loopback accept failed: {error}"),
    });
    let peer = peer.unwrap();
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(TIMEOUT)).unwrap();
    peer.set_write_timeout(Some(TIMEOUT)).unwrap();
    peer
}

fn exchange(peer: &mut TcpStream, sent: &[u8], expected: &[u8]) {
    peer.write_all(sent).unwrap();
    let mut received = vec![0; expected.len()];
    peer.read_exact(&mut received).unwrap();
    assert_eq!(received, expected);
}

fn negotiate_5250(peer: &mut TcpStream) {
    exchange(
        peer,
        b"\xff\xfd\x27\xff\xfa\x27\x01\x03DEVNAME\xff\xf0",
        b"\xff\xfb\x27\xff\xfa\x27\x00\x03DEVNAME\x01DESK_01\xff\xf0",
    );
    exchange(
        peer,
        &[
            255, 253, 24, 255, 253, 25, 255, 251, 25, 255, 253, 0, 255, 251, 0,
        ],
        &[
            255, 251, 24, 255, 251, 25, 255, 253, 25, 255, 251, 0, 255, 253, 0,
        ],
    );
    exchange(
        peer,
        b"\xff\xfa\x18\x01\xff\xf0",
        b"\xff\xfa\x18\x00IBM-3179-2\xff\xf0",
    );
}

#[test]
fn active_ibm_reconnect_requires_fresh_consent_and_commits_after_negotiation() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.update(Message::ShowIbm(owner, IbmKind::Tn5250, None));
    let _ = workspace.update(Message::IbmAddress(owner, address.clone()));
    let _ = workspace.update(Message::IbmDeviceName(owner, "DESK_01".into()));
    let _ = workspace.update(Message::IbmPlaintext(owner, true));
    let _ = workspace.update(Message::ConnectIbm(owner));
    let mut original_peer = accept_peer(&listener);
    negotiate_5250(&mut original_peer);
    let old = primary(&workspace, owner);
    let tab = workspace.windows[&owner].tab().unwrap().id;
    wait_for(|| workspace.sessions[&old].session.frame().status == Status::Running);
    let _ = workspace.update(Message::SessionUpdate(old));

    let _ = workspace.reconnect_tab(owner, tab);
    let _ = workspace.update(Message::IbmAddress(owner, "127.0.0.1:1".into()));
    let _ = workspace.update(Message::IbmDeviceName(owner, "OTHER".into()));
    let _ = workspace.update(Message::IbmDevice(owner, IbmKind::Tn5250Wide));
    let dialog = workspace.windows[&owner].pending_ibm.as_ref().unwrap();
    assert_eq!(
        (&dialog.address, dialog.device_name.as_str()),
        (&address, "DESK_01")
    );
    assert!(!dialog.plaintext);
    assert_eq!(dialog.kind, IbmKind::Tn5250);
    let _ = workspace.update(Message::ConnectIbm(owner));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let _ = workspace.update(Message::IbmPlaintext(owner, true));
    let _ = workspace.update(Message::ConnectIbm(owner));
    drop(accept_peer(&listener));
    let failed = *workspace.sessions.keys().find(|&&id| id != old).unwrap();
    wait_for(|| {
        matches!(
            workspace.sessions[&failed].session.frame().status,
            Status::Error(_)
        )
    });
    let _ = workspace.update(Message::SessionUpdate(failed));
    assert_eq!(primary(&workspace, owner), old);
    assert_eq!(
        workspace.sessions[&old].session.frame().status,
        Status::Running
    );
    assert!(!workspace.sessions.contains_key(&failed));
    workspace.sessions[&old]
        .session
        .send_ibm(IbmInput::Aid(IbmAid::Attention))
        .unwrap();
    let mut attention = [0; 12];
    original_peer.read_exact(&mut attention).unwrap();
    assert_eq!(
        attention,
        [0, 10, 0x12, 0xa0, 0, 0, 4, 0x40, 0, 0, 255, 239]
    );

    let _ = workspace.reconnect_tab(owner, tab);
    let _ = workspace.update(Message::IbmPlaintext(owner, true));
    let _ = workspace.update(Message::ConnectIbm(owner));
    let mut replacement_peer = accept_peer(&listener);
    assert_eq!(primary(&workspace, owner), old);
    let replacement = *workspace.sessions.keys().find(|&&id| id != old).unwrap();
    assert_eq!(
        workspace.sessions[&replacement].session.frame().status,
        Status::Starting
    );
    negotiate_5250(&mut replacement_peer);
    wait_for(|| workspace.sessions[&replacement].session.frame().status == Status::Running);
    let _ = workspace.update(Message::SessionUpdate(replacement));
    assert_eq!(primary(&workspace, owner), replacement);
    assert!(!workspace.sessions.contains_key(&old));
}

#[test]
fn restored_ibm_reconnect_with_no_recorded_endpoint_allows_entering_one() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let old = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn5250),
        crate::workspace::GridSize::default(),
        20,
    );
    workspace.add_tab(owner, old);
    let tab = workspace.windows[&owner].tab().unwrap().id;
    let _ = workspace.reconnect_tab(owner, tab);
    let _ = workspace.update(Message::IbmAddress(owner, "127.0.0.1:2300".into()));
    let dialog = workspace.windows[&owner].pending_ibm.as_ref().unwrap();
    assert!(dialog.can_edit_route());
    assert_eq!(dialog.address, "127.0.0.1:2300");
    assert!(dialog.connection().is_none());
    assert_eq!(primary(&workspace, owner), old);
}
