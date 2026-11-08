use super::*;
use crate::session::{GridSize, Status};
use crate::workspace::{SerialClose, TabEntry, tool_tabs};
use std::time::{Duration, Instant};

fn add_shell(
    workspace: &mut Workspace,
    owner: window::Id,
    private: bool,
    title: &str,
) -> (u64, SessionId) {
    let session = workspace
        .store_local_session(TerminalProfile::Xterm, Some(16), None)
        .expect("real local shell worker");
    workspace.add_tab_with_privacy(owner, session, private);
    let tab = workspace.active_tab_mut(owner).expect("new tab");
    tab.custom_title = Some(title.into());
    (tab.id, session)
}

fn wait_running(workspace: &Workspace, session: SessionId) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let actor = &workspace.sessions[&session].session;
    loop {
        match &actor.frame().status {
            Status::Running => return,
            Status::Starting if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            status => panic!("session did not start: {status:?}"),
        }
    }
}

fn run_immediate_members(workspace: &mut Workspace) {
    while let Some(flight) = workspace
        .recently_closed
        .flight
        .as_ref()
        .filter(|flight| flight.phase == Phase::Ready)
    {
        let owner = flight.owner;
        let entry = flight.entry;
        let _ = workspace.update_recently_closed(Message::Advance(owner, entry));
    }
}

#[test]
fn reopens_a_fresh_single_pane_shell_with_its_title_tab_group_and_original_font() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (tab, original) = add_shell(&mut workspace, owner, false, "original shell");
    wait_running(&workspace, original);
    let pid = workspace.sessions[&original].session.local_shell_pid();
    assert!(pid.is_some());
    let extra = workspace
        .store_local_session(TerminalProfile::Vt100, Some(22), None)
        .unwrap();
    let tab_state = workspace.active_tab_mut(owner).unwrap();
    tab_state.group = Some("Operations".into());
    let (split, _) = tab_state
        .grid
        .split(
            iced::widget::pane_grid::Axis::Vertical,
            tab_state.focused,
            extra,
        )
        .unwrap();
    tab_state.focused = split;

    let _ = workspace.update(Desktop::CloseTab(owner, tab));
    let rows = workspace.recently_closed_menu_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "original shell");
    let _ = workspace.reopen_closed_tab(Some(owner), Some(rows[0].0));
    run_immediate_members(&mut workspace);
    let opened = workspace.windows[&owner].tab().unwrap();
    let session = opened.focused_session().unwrap();
    wait_running(&workspace, session);
    assert_eq!(opened.grid.len(), 1);
    assert_eq!(opened.custom_title.as_deref(), Some("original shell"));
    assert_eq!(opened.group.as_deref(), Some("Operations"));
    assert_eq!(workspace.sessions[&session].font, 16);
    assert_ne!(workspace.sessions[&session].session.local_shell_pid(), pid);
    assert!(workspace.recently_closed_menu_rows().is_empty());
}

#[test]
fn close_all_cancels_or_remembers_only_confirmed_public_targets_in_one_entry() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (first, _) = add_shell(&mut workspace, owner, false, "first");
    let (private, _) = add_shell(&mut workspace, owner, true, "private");
    let (second, _) = add_shell(&mut workspace, owner, false, "second");
    // The clean hosted instance is a test double. History must remain terminal-only.
    let _ = workspace.host_tool(owner, tool_tabs::Kind::OpenPgp, window::Id::unique());
    let tool = workspace.windows[&owner].active_tab_id().unwrap();
    let ask = |workspace: &mut Workspace| {
        let _ = workspace.request_close_all_tabs(owner);
        let (request, SerialClose::TabBatch(batch)) =
            workspace.windows[&owner].pending_serial_close.unwrap()
        else {
            panic!("expected close-all confirmation")
        };
        assert_eq!(workspace.tab_batch_all_count(owner, batch), Some(4));
        assert_eq!(workspace.tool_targets_for_batch(owner, batch), [tool]);
        request
    };
    let request = ask(&mut workspace);
    let _ = workspace.update(Desktop::ConfirmSerialClose(owner, request, false));
    assert_eq!(workspace.windows[&owner].tabs.len(), 4);
    assert!(workspace.recently_closed_menu_rows().is_empty());

    let request = ask(&mut workspace);
    let (later, _) = add_shell(&mut workspace, owner, false, "later");
    let _ = workspace.update(Desktop::ConfirmSerialClose(owner, request, true));
    assert_eq!(
        workspace.windows[&owner]
            .tabs
            .iter()
            .map(TabEntry::id)
            .collect::<Vec<_>>(),
        [later]
    );
    let rows = workspace.recently_closed_menu_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "first, second");
    assert_ne!(first, second);
    assert_ne!(private, later);
    let id = rows[0].0;
    let _ = workspace.clear_recently_closed();
    let _ = workspace.reopen_closed_tab(Some(owner), Some(id));
    assert_eq!(workspace.windows[&owner].tabs.len(), 1);
}

#[test]
fn explicit_window_close_is_one_entry_and_reopens_in_another_window_when_occupied() {
    let mut workspace = Workspace::default();
    let (closing, _) = workspace.open_window(false);
    let (requesting, _) = workspace.open_window(false);
    add_shell(&mut workspace, requesting, false, "existing");
    add_shell(&mut workspace, closing, false, "first");
    add_shell(&mut workspace, closing, false, "second");

    let _ = workspace.request_close(closing, SerialClose::Window);
    let rows = workspace.recently_closed_menu_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "Window: first, second");
    let _ = workspace.reopen_closed_tab(Some(requesting), Some(rows[0].0));
    run_immediate_members(&mut workspace);
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[&requesting].tabs.len(), 1);
    let (_, reopened) = workspace
        .windows
        .iter()
        .find(|(id, _)| **id != requesting)
        .unwrap();
    let names = reopened
        .terminal_tabs()
        .filter_map(|tab| tab.custom_title.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(names, ["first", "second"]);
}

#[test]
fn ibm_reopen_keeps_the_destination_but_requires_new_plaintext_approval() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let session = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn5250),
        GridSize::default(),
        14,
    );
    workspace.sessions.get_mut(&session).unwrap().ibm_origin = Some(IbmOrigin {
        kind: IbmKind::Tn5250,
        address: Some("127.0.0.1:2323".parse().unwrap()),
        device_name: "DISPLAY1".into(),
    });
    workspace.add_tab(owner, session);
    let tab = workspace.windows[&owner].tab().unwrap().id;
    let _ = workspace.update(Desktop::CloseTab(owner, tab));
    let id = workspace.recently_closed_menu_rows()[0].0;
    let _ = workspace.reopen_closed_tab(Some(owner), Some(id));
    let pending = workspace.windows[&owner].pending_ibm.as_ref().unwrap();
    assert_eq!(pending.address, "127.0.0.1:2323");
    assert_eq!(pending.device_name, "DISPLAY1");
    assert!(pending.connection().is_none());
    let _ = workspace.update(Desktop::CancelIbm(owner));
    run_immediate_members(&mut workspace);
    assert_eq!(workspace.recently_closed_menu_rows()[0].0, id);
}

#[cfg(unix)]
#[test]
fn serial_reopen_waits_for_the_previous_real_port_and_ignores_a_stale_resume() {
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let peer = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&peer, FdFlags::CLOEXEC).unwrap();
    grantpt(&peer).unwrap();
    unlockpt(&peer).unwrap();
    let device = ptsname(&peer, Vec::new()).unwrap().into_string().unwrap();
    let connection = SerialConnection {
        device: device.clone().into(),
        ..SerialConnection::default()
    };
    let grid = initial_grid(TerminalProfile::Xterm);
    let serial =
        Session::start_serial(connection, grid, TerminalProfile::Xterm, true, None).unwrap();
    let first = workspace.store_session(serial, grid, 17);
    workspace.add_tab(owner, first);
    wait_running(&workspace, first);

    let close = |workspace: &mut Workspace| {
        let tab = workspace.windows[&owner].tab().unwrap().id;
        let _ = workspace.update(Desktop::CloseTab(owner, tab));
        let (request, SerialClose::Tab(_)) =
            workspace.windows[&owner].pending_serial_close.unwrap()
        else {
            panic!("serial close must require confirmation")
        };
        let _ = workspace.update(Desktop::ConfirmSerialClose(owner, request, true));
    };
    close(&mut workspace);
    let _ = workspace.reopen_closed_tab(Some(owner), None);
    assert!(workspace.windows[&owner].tabs.is_empty());
    let Some(Phase::Serial(token)) = workspace
        .recently_closed
        .flight
        .as_ref()
        .map(|flight| flight.phase)
    else {
        panic!("waiting for the original serial worker")
    };
    let stopped = &workspace.recently_closed.entries[0].tabs[0].serial_shutdown[0];
    let deadline = Instant::now() + Duration::from_secs(4);
    while !stopped.is_closed() {
        assert!(Instant::now() < deadline, "old serial worker did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = workspace.update_recently_closed(Message::SerialReleased(owner, token));
    run_immediate_members(&mut workspace);
    let reopened = workspace.windows[&owner]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    assert_ne!(reopened, first);
    wait_running(&workspace, reopened);
    assert_eq!(
        workspace.sessions[&reopened]
            .session
            .frame()
            .serial
            .as_ref()
            .unwrap()
            .device,
        std::path::PathBuf::from(&device)
    );

    close(&mut workspace);
    let _ = workspace.reopen_closed_tab(Some(owner), None);
    let Some(Phase::Serial(token)) = workspace
        .recently_closed
        .flight
        .as_ref()
        .map(|flight| flight.phase)
    else {
        panic!("waiting for the reopened serial worker")
    };
    let _ = workspace.clear_recently_closed();
    let _ = workspace.update_recently_closed(Message::SerialReleased(owner, token));
    assert!(workspace.windows[&owner].tabs.is_empty());
    assert!(workspace.recently_closed_menu_rows().is_empty());
}

#[test]
fn transfer_and_programmatic_removal_leave_no_recent_entry() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let (tab, _) = add_shell(&mut workspace, source, false, "moved");
    let _ = workspace.update(Desktop::Transfer {
        from: source,
        to: Some(destination),
        tab,
    });
    assert!(workspace.recently_closed_menu_rows().is_empty());
    workspace.close_tab(destination, tab);
    assert!(workspace.recently_closed_menu_rows().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn native_file_action_with_no_open_window_reopens_the_entry_in_a_new_window() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (tab, _) = add_shell(&mut workspace, source, false, "restore without a window");
    let _ = workspace.update(Desktop::CloseTab(source, tab));
    let _ = workspace.windows.remove(&source);
    assert!(workspace.has_recently_closed());

    let _ = workspace.update(Desktop::NativeMenu(
        super::super::chrome::Action::ReopenClosedTab as u16,
    ));
    run_immediate_members(&mut workspace);
    let (&opened, window) = workspace.windows.iter().next().unwrap();
    assert_ne!(opened, source);
    assert_eq!(
        window.tab().unwrap().custom_title.as_deref(),
        Some("restore without a window")
    );
    assert!(!workspace.has_recently_closed_entries());
}
