use std::time::{Duration, Instant};

use kortty_terminal::TerminalProfile;

use crate::session::{GridSize, Session};

use super::*;

fn local(workspace: &mut Workspace) -> SessionId {
    let grid = GridSize::default();
    workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    )
}

fn wait_for(workspace: &mut Workspace, session: SessionId, condition: impl Fn(&Status) -> bool) {
    let until = Instant::now() + Duration::from_secs(6);
    loop {
        let _ = workspace.update(Desktop::SessionUpdate(session));
        if condition(&workspace.sessions[&session].frame.status) {
            break;
        }
        assert!(
            Instant::now() < until,
            "session did not publish its expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn dashboard_pane_reconnect_uses_the_tab_primary_and_keeps_selection() {
    use crate::session::IbmKind;

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let disconnected = |workspace: &mut Workspace| {
        workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        )
    };
    let primary = disconnected(&mut workspace);
    let secondary = disconnected(&mut workspace);
    workspace.add_tab(owner, primary);
    let tab = workspace.active_tab_mut(owner).unwrap();
    let tab_id = tab.id;
    let (second_pane, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, tab.focused, secondary)
        .unwrap();
    tab.focused = second_pane;
    let other = disconnected(&mut workspace);
    workspace.add_tab(owner, other);
    let other_tab = workspace.windows[&owner].tab().unwrap().id;
    let target = Target {
        tab: tab_id,
        session: Some(secondary),
    };

    assert_eq!(
        workspace.dashboard_actions(owner, target),
        [Action::Focus, Action::Reconnect],
    );
    let _ = workspace.perform_dashboard_action(owner, target, Action::Reconnect);
    let window = &workspace.windows[&owner];
    assert_eq!(window.tab().unwrap().id, other_tab);
    assert_eq!(window.terminal_tabs().next().unwrap().focused, second_pane);
    assert_eq!(
        window
            .pending_ibm
            .as_ref()
            .and_then(|form| form.reconnect.as_ref())
            .map(|reconnect| reconnect.target.source),
        Some(primary),
    );
}

#[test]
fn dashboard_lists_live_split_panes_and_rejects_actions_after_a_tab_moves() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (other, _) = workspace.open_window(false);
    let first = local(&mut workspace);
    let second = local(&mut workspace);
    workspace.add_tab(owner, first);
    let tab = workspace.active_tab_mut(owner).unwrap();
    let tab_id = tab.id;
    let (second_pane, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, tab.focused, second)
        .unwrap();
    tab.grid.maximize(tab.focused);
    wait_for(&mut workspace, first, |status| *status == Status::Running);
    wait_for(&mut workspace, second, |status| *status == Status::Running);

    let _ = workspace.update(Desktop::Dashboard(owner, Message::Toggle));
    let tree = workspace.dashboard_tree(&workspace.windows[&owner]);
    assert_eq!((tree.total, tree.connected), (1, 1));
    assert_eq!(
        tree.direct[0]
            .panes
            .iter()
            .map(|row| row.session)
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(
        workspace.dashboard_tree(&workspace.windows[&other]).total,
        0
    );
    workspace.set_tab_group(owner, tab_id, Some("Operations".into()));
    let grouped = workspace.dashboard_tree(&workspace.windows[&owner]);
    assert!(grouped.direct.is_empty());
    assert_eq!(grouped.groups["Operations"][0].id, tab_id);
    workspace.set_tab_group(owner, tab_id, None);

    let _ = workspace.update(Desktop::Dashboard(owner, Message::ExpandAll));
    assert!(
        !workspace.windows[&owner]
            .dashboard
            .expanded(&Branch::Window)
    );
    let _ = workspace.update(Desktop::Dashboard(owner, Message::ExpandAll));
    assert!(
        workspace.windows[&owner]
            .dashboard
            .expanded(&Branch::Window)
    );

    let target = Target {
        tab: tab_id,
        session: Some(second),
    };
    workspace.window_settings.open_tool_windows_as_tabs = true;
    let _ = workspace.show_openpgp(owner);
    let selected_tool = workspace.windows[&owner].active_tab_id().unwrap();
    assert_ne!(selected_tool, tab_id);
    let _ = workspace.update(Desktop::Dashboard(owner, Message::Menu(target)));
    let _ = workspace.update(Desktop::Dashboard(
        owner,
        Message::Perform(target, Action::MultiExec),
    ));
    assert!(workspace.multi_exec_checked(owner, target.multi_exec()));
    let tab_target = Target::tab(tab_id);
    assert!(!workspace.multi_exec_checked(owner, tab_target.multi_exec()));
    let _ = workspace.update(Desktop::Dashboard(
        owner,
        Message::Perform(tab_target, Action::MultiExec),
    ));
    assert!(workspace.multi_exec_checked(owner, tab_target.multi_exec()));
    assert_eq!(
        workspace.windows[&owner].active_tab_id(),
        Some(selected_tool)
    );
    assert_eq!(
        workspace.windows[&owner]
            .terminal_tabs()
            .next()
            .and_then(Tab::focused_session),
        Some(first)
    );

    let _ = workspace.update(Desktop::Dashboard(owner, Message::Focus(target)));
    let tab = workspace.active_tab_mut(owner).unwrap();
    assert_eq!(tab.focused, second_pane);
    assert_eq!(tab.grid.maximized(), None);

    let _ = workspace.transfer_at(owner, Some(other), tab_id, None);
    let _ = workspace.perform_dashboard_action(owner, Target::tab(tab_id), Action::Close);
    let _ = workspace.update(Desktop::Dashboard(
        owner,
        Message::Perform(tab_target, Action::MultiExec),
    ));
    assert!(workspace.multi_exec_checked(other, tab_target.multi_exec()));
    assert_eq!(
        workspace.dashboard_tree(&workspace.windows[&owner]).total,
        0
    );
    assert_eq!(
        workspace.dashboard_tree(&workspace.windows[&other]).total,
        1
    );
    assert!(workspace.sessions.contains_key(&second));
}

#[test]
fn dashboard_footer_tracks_real_pty_exit_and_duplicate_uses_normal_close_path() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let session = local(&mut workspace);
    workspace.add_tab(owner, session);
    wait_for(&mut workspace, session, |status| *status == Status::Running);
    let target = Target::tab(workspace.windows[&owner].tab().unwrap().id);
    let _ = workspace.update(Desktop::Dashboard(owner, Message::Toggle));

    let connected = workspace.dashboard_tree(&workspace.windows[&owner]);
    assert_eq!((connected.connected, connected.total), (1, 1));
    workspace.sessions[&session]
        .session
        .send(b"exit\r".to_vec())
        .unwrap();
    wait_for(&mut workspace, session, |status| {
        matches!(status, Status::Exited(_))
    });
    let ended = workspace.dashboard_tree(&workspace.windows[&owner]);
    assert_eq!((ended.connected, ended.total), (0, 1));
    assert_eq!(ended.direct[0].state, ConnectionState::Ended);

    let _ = workspace.perform_dashboard_action(owner, target, Action::Duplicate);
    let duplicate = workspace.windows[&owner]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    assert_ne!(duplicate, session);
    wait_for(&mut workspace, duplicate, |status| {
        *status == Status::Running
    });
    let _ = workspace.perform_dashboard_action(owner, target, Action::Close);
    assert!(!workspace.sessions.contains_key(&session));
    assert_eq!(workspace.windows[&owner].tabs.len(), 1);
    assert!(workspace.sessions.contains_key(&duplicate));
}
