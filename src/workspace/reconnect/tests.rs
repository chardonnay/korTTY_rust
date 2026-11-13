use iced::widget::pane_grid;

use super::*;
use crate::terminal_effects::{Effect, Selection, Speed};
use crate::workspace::TerminalProfile;

#[cfg(unix)]
#[test]
fn reconnect_replaces_the_primary_shell_preserving_nested_focus_and_history() {
    use std::time::{Duration, Instant};

    fn wait(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for local terminal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn contains(session: &crate::session::Session, marker: &str) -> bool {
        session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(marker)
    }

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(true);
    let tab = workspace.windows[&owner].tab().unwrap();
    let (tab_id, primary) = (tab.id, tab.primary);
    let old = *tab.grid.get(primary).unwrap();
    let old_session = &workspace.sessions[&old].session;
    wait(|| old_session.frame().status == Status::Running);
    let pid = old_session.local_shell_pid().unwrap();
    let done = old_session.shutdown_handle();
    old_session
        .send(b"printf '\\n%s%s\\n' 'previous-' 'screen-marker'\r".to_vec())
        .unwrap();
    wait(|| contains(old_session, "previous-screen-marker"));

    let selection = Selection {
        effect: Effect::AmberCrt90,
        speed: Speed::try_from(2.5).unwrap(),
    };
    workspace.select_terminal_effect(old, Some(selection.effect));
    workspace.set_terminal_effect_speed(old, selection.speed);
    let _ = workspace.split(owner, pane_grid::Axis::Vertical);
    workspace.wait_for_test_split(owner);
    let _ = workspace.split(owner, pane_grid::Axis::Horizontal);
    workspace.wait_for_test_split(owner);
    let tab = workspace.windows[&owner].tab().unwrap();
    let focused = tab.focused;
    let sibling = *tab.grid.get(focused).unwrap();
    assert_eq!(
        workspace
            .pane_effect(sibling)
            .map(|effect| effect.selection),
        Some(selection)
    );
    let sibling_pid = workspace.sessions[&sibling]
        .session
        .local_shell_pid()
        .unwrap();
    workspace.sessions.get_mut(&old).unwrap().font = 19;

    let _ = workspace.reconnect_tab(owner, tab_id);
    let (&candidate, _) = workspace.pending_reconnects.iter().next().unwrap();
    assert_eq!(
        workspace.windows[&owner].tab().unwrap().grid.get(primary),
        Some(&old)
    );
    assert!(!done.is_closed());
    workspace.wait_for_test_reconnect(candidate);

    let tab = workspace.windows[&owner].tab().unwrap();
    assert_eq!(tab.id, tab_id);
    assert_eq!(tab.grid.len(), 3);
    assert_eq!(tab.focused, focused);
    assert_eq!(tab.grid.get(primary), Some(&candidate));
    assert_eq!(tab.grid.get(focused), Some(&sibling));
    assert_eq!(
        workspace.sessions[&sibling].session.local_shell_pid(),
        Some(sibling_pid)
    );
    assert_eq!(workspace.sessions[&candidate].font, 19);
    assert_eq!(
        workspace
            .pane_effect(candidate)
            .map(|effect| effect.selection),
        Some(selection)
    );
    let replacement = &workspace.sessions[&candidate].session;
    assert_ne!(replacement.local_shell_pid(), Some(pid));
    assert!(!contains(replacement, "previous-screen-marker"));
    assert!(replacement.frame().screen.scrollback_lines > 0);
    let grid = workspace.sessions[&candidate].grid;
    replacement.resize(crate::session::GridSize {
        rows: grid.rows + 8,
        ..grid
    });
    wait(|| replacement.frame().screen.rows == grid.rows + 8);
    assert!(!contains(replacement, "previous-screen-marker"));
    replacement.scroll(i32::MAX);
    wait(|| contains(replacement, "previous-screen-marker"));
    wait(|| done.is_closed());
}

#[test]
fn workspace_restore_keeps_primary_and_focus_after_a_pane_moves_to_first_position() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(true);
    let tab = workspace.windows[&owner].tab().unwrap();
    let initial = tab.primary;
    let original = *tab.grid.get(initial).unwrap();
    let extra = workspace
        .store_local_session(TerminalProfile::Vt100, Some(19), None)
        .unwrap();
    let tab = workspace.active_tab_mut(owner).unwrap();
    let (second, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, initial, extra)
        .unwrap();
    tab.focused = second;
    tab.grid.swap(initial, second);
    let original_effect = Selection {
        effect: Effect::AmberCrt90,
        speed: Speed::try_from(3.0).unwrap(),
    };
    let extra_effect = Selection {
        effect: Effect::NeonCity,
        speed: Speed::try_from(7.0).unwrap(),
    };
    for (session, selection) in [(original, original_effect), (extra, extra_effect)] {
        workspace.select_terminal_effect(session, Some(selection.effect));
        workspace.set_terminal_effect_speed(session, selection.speed);
    }
    let layout = workspace.capture_layout().unwrap();
    assert_eq!(layout.windows[0].tab_primary_panes.get(&0), Some(&1));
    assert_eq!(layout.windows[0].tab_terminal_effects[&0][&0], extra_effect);
    assert_eq!(
        layout.windows[0].tab_terminal_effects[&0][&1],
        original_effect
    );
    let mut invalid = layout.clone();
    invalid.windows[0].tab_primary_panes.insert(0, 2);
    assert!(invalid.validate().is_err());

    let mut restored = Workspace::default();
    let (window, _) = restored.restore_layout_into(&layout, None).unwrap();
    let tab = restored.windows[&window].tab().unwrap();
    let primary = tab.grid.get(tab.primary).unwrap();
    let focused = tab.focused_session().unwrap();
    assert_eq!(
        restored.sessions[primary].frame.screen.profile,
        TerminalProfile::Xterm
    );
    assert_eq!(
        restored.sessions[&focused].frame.screen.profile,
        TerminalProfile::Vt100
    );
    assert_eq!(
        restored
            .pane_effect(*primary)
            .map(|effect| effect.selection),
        Some(original_effect)
    );
    assert_eq!(
        restored.pane_effect(focused).map(|effect| effect.selection),
        Some(extra_effect)
    );
}

#[test]
fn transferring_a_tab_cancels_its_hidden_reconnect_candidate() {
    let mut workspace = Workspace::default();
    let (from, _) = workspace.open_window(true);
    let (to, _) = workspace.open_window(false);
    let tab = workspace.windows[&from].tab().unwrap();
    let source = *tab.grid.get(tab.primary).unwrap();
    let target = ReconnectTarget {
        tab: tab.id,
        source,
    };
    let candidate = workspace
        .store_local_session(TerminalProfile::Xterm, Some(18), None)
        .unwrap();
    let _ = workspace.start_reconnect_worker(from, target, candidate, None, Default::default());
    let _ = workspace.transfer(from, Some(to), target.tab);
    let _ = workspace.update(Message::ReconnectHistoryReady(candidate, true));
    assert!(workspace.pending_reconnects.is_empty());
    assert!(!workspace.sessions.contains_key(&candidate));
    assert_eq!(
        workspace.windows[&to].tab().unwrap().focused_session(),
        Some(source)
    );
}
#[cfg(unix)]
#[test]
fn reconnect_worker_replaces_only_the_targeted_secondary_pane() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(true);
    let _ = workspace.split(owner, pane_grid::Axis::Vertical);
    workspace.wait_for_test_split(owner);
    let tab = workspace.windows[&owner].tab().unwrap();
    let primary = tab.primary;
    let secondary = tab.focused;
    assert_ne!(primary, secondary);
    let primary_session = *tab.grid.get(primary).unwrap();
    let source = *tab.grid.get(secondary).unwrap();
    let target = ReconnectTarget {
        tab: tab.id,
        source,
    };
    let old_done = workspace.sessions[&source].session.shutdown_handle();
    let candidate = workspace
        .store_local_session(TerminalProfile::Xterm, Some(14), None)
        .unwrap();
    let _ =
        workspace.start_reconnect_worker(owner, target, candidate, None, KeySnapshots::default());
    workspace.wait_for_test_reconnect(candidate);
    let tab = workspace.windows[&owner].tab().unwrap();
    assert_eq!(tab.primary, primary);
    assert_eq!(tab.focused, secondary);
    assert_eq!(tab.grid.get(primary), Some(&primary_session));
    assert_eq!(tab.grid.get(secondary), Some(&candidate));
    assert!(!workspace.sessions.contains_key(&source));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for id in [primary_session, candidate] {
        workspace.stop_session(id);
    }
    while !old_done.is_closed()
        || workspace
            .pending_shutdown
            .iter()
            .any(|done| !done.is_closed())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "test shells did not stop"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
