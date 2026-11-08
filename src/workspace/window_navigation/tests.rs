use super::*;
use crate::session::SessionId;
use crate::workspace::{DEFAULT_WINDOW, tab_titles::WindowSettings};
use iced::keyboard::Location;

fn three_tabs() -> (Workspace, window::Id, [u64; 3]) {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let tabs = [41, 42, 43].map(|session| {
        workspace.add_tab(window, SessionId(session));
        selected(&workspace, window)
    });
    let _ = workspace.update(Message::SelectTab(window, tabs[0]));
    let _ = workspace.update(Message::SelectTab(window, tabs[1]));
    (workspace, window, tabs)
}

fn selected(workspace: &Workspace, window: window::Id) -> u64 {
    workspace.windows[&window].active_tab_id().unwrap()
}

fn press(workspace: &mut Workspace, window: window::Id, key: Named, modifiers: Modifiers) {
    let _ = workspace.update(Message::Key(
        window,
        Key::Named(key),
        Location::Standard,
        modifiers,
        None,
    ));
}

#[test]
fn ctrl_tab_defaults_to_tab_strip_order_and_wraps() {
    let (mut workspace, window, [first, second, third]) = three_tabs();
    assert_eq!(workspace.windows[&window].tab_mru, [second, first, third]);
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, window), third);
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, window), first);
    press(
        &mut workspace,
        window,
        Named::Tab,
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    assert_eq!(selected(&workspace, window), third);
    assert_eq!(workspace.windows[&window].tab_mru, [third, first, second]);
}

#[test]
fn ctrl_tab_selects_and_promotes_tools_in_the_same_mru_cycle() {
    let (mut workspace, owner, [first, second, third]) = three_tabs();
    // The hosted instance is a test double for navigation, with no running tool.
    let _ = workspace.host_tool(
        owner,
        crate::workspace::tool_tabs::Kind::OpenPgp,
        window::Id::unique(),
    );
    let tool = selected(&workspace, owner);
    assert!(workspace.windows[&owner].tab().is_none());
    press(&mut workspace, owner, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, owner), first);

    workspace.window_settings.tab_switch_most_recent_first = true;
    press(&mut workspace, owner, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, owner), tool);
    press(&mut workspace, owner, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, owner), second);
    assert_eq!(
        workspace.windows[&owner].tab_mru,
        [first, tool, second, third]
    );
    let _ = workspace.update(Message::CommitTabCycle(owner));
    assert_eq!(
        workspace.windows[&owner].tab_mru,
        [second, first, tool, third]
    );
}

#[test]
fn mru_steps_hold_order_and_promote_only_the_tab_on_release() {
    let (mut workspace, window, [first, second, third]) = three_tabs();
    workspace.window_settings.tab_switch_most_recent_first = true;
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, window), first);
    press(
        &mut workspace,
        window,
        Named::Shift,
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    press(
        &mut workspace,
        window,
        Named::Tab,
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    assert_eq!(selected(&workspace, window), second);
    press(
        &mut workspace,
        window,
        Named::Tab,
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    assert_eq!(selected(&workspace, window), third);
    assert_eq!(workspace.windows[&window].tab_mru, [second, first, third]);
    assert!(ends_tab_cycle_on_release(
        &Key::Named(Named::Control),
        Modifiers::CTRL
    ));
    assert!(!ends_tab_cycle_on_release(
        &Key::Named(Named::Shift),
        Modifiers::CTRL
    ));
    assert!(ends_tab_cycle_on_release(
        &Key::Named(Named::Tab),
        Modifiers::empty()
    ));
    let _ = workspace.update(Message::CommitTabCycle(window));
    assert_eq!(workspace.windows[&window].tab_mru, [third, second, first]);
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    assert_eq!(selected(&workspace, window), second);
}

#[test]
fn ordinary_press_focus_loss_and_one_shot_each_commit() {
    let (mut workspace, window, [first, second, third]) = three_tabs();
    workspace.window_settings.tab_switch_most_recent_first = true;
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    press(&mut workspace, window, Named::ArrowLeft, Modifiers::CTRL);
    assert_eq!(workspace.windows[&window].tab_mru, [first, second, third]);
    press(&mut workspace, window, Named::Tab, Modifiers::CTRL);
    let _ = workspace.update(Message::Window(window, window::Event::Unfocused));
    assert_eq!(workspace.windows[&window].tab_mru, [second, first, third]);
    workspace.switch_tab_once(window, true);
    assert_eq!(workspace.windows[&window].tab_mru, [third, second, first]);
    workspace.switch_tab_once(window, false);
    assert_eq!(workspace.windows[&window].tab_mru, [second, third, first]);
}

#[test]
fn snapshot_skips_removed_tabs_and_admits_new_tabs_after_all_old_ones_leave() {
    let mut cycle = TabCycle::default();
    assert_eq!(cycle.advance(&[1, 2, 3], &[2, 1], 1, false), Some(2));
    assert_eq!(cycle.advance(&[2, 3, 4], &[4, 2], 2, false), Some(3));
    assert_eq!(cycle.advance(&[2, 3, 4], &[4, 2], 3, false), Some(2));
    assert_eq!(cycle.advance(&[4, 5], &[4, 5], 4, false), Some(5));
    assert_eq!(cycle.advance(&[], &[], 5, false), None);
    assert!(!cycle.clear());
}

#[test]
fn mru_setting_defaults_off_for_legacy_profiles_and_round_trips() {
    let mut preferences: WindowSettings = serde_json::from_str("{}").unwrap();
    assert!(!preferences.tab_switch_most_recent_first);
    preferences.tab_switch_most_recent_first = true;
    let saved = serde_json::to_string(&preferences).unwrap();
    assert!(
        serde_json::from_str::<WindowSettings>(&saved)
            .unwrap()
            .tab_switch_most_recent_first
    );
}

fn windowed() -> WindowMode {
    WindowMode {
        windowed: true,
        normal_geometry: true,
        ..WindowMode::default()
    }
}

fn fullscreen() -> WindowMode {
    WindowMode {
        fullscreen: true,
        ..WindowMode::default()
    }
}

#[test]
fn terminal_only_entry_keeps_live_window_size_through_native_transition() {
    let (mut workspace, window, [_, second, _]) = three_tabs();
    let live_size = Size::new(829.0, 501.0);
    workspace.windows.get_mut(&window).unwrap().size = live_size;
    let _ = workspace.toggle_terminal_only_fullscreen(window);
    let TerminalOnly::Inspecting { request, .. } = workspace.windows[&window].terminal_only else {
        panic!("expected pending native mode lookup");
    };
    let _ = workspace.update(Message::TerminalOnlyInspected(
        window,
        request + 1,
        windowed(),
    ));
    assert!(workspace.windows[&window].terminal_only.size().is_none());
    let _ = workspace.update(Message::TerminalOnlyInspected(window, request, windowed()));
    assert_eq!(
        workspace.windows[&window].terminal_only.size(),
        Some(live_size)
    );
    assert!(!workspace.terminal_only_fullscreen_active(window));
    let _ = workspace.observe_terminal_only_mode(window, fullscreen());
    assert!(workspace.terminal_only_fullscreen_active(window));
    assert_eq!(selected(&workspace, window), second);
    let native_size = Size::new(1600.0, 900.0);
    let owner = workspace.windows.get_mut(&window).unwrap();
    owner.size = native_size;
    let bounds = owner.content_bounds();
    assert_eq!(bounds.width, live_size.width);
    assert_eq!(bounds.height, live_size.height);
    assert_eq!(bounds.x, (native_size.width - live_size.width) / 2.0);
    assert_eq!(bounds.y, (native_size.height - live_size.height) / 2.0);
    let _ = workspace.toggle_window_fullscreen(window);
    assert!(!workspace.terminal_only_fullscreen_active(window));
    assert_eq!(selected(&workspace, window), second);
}

#[test]
fn terminal_only_in_regular_fullscreen_uses_normal_geometry_and_restores_fullscreen() {
    let (mut workspace, window, _) = three_tabs();
    let placement = workspace.windows[&window].placement();
    let mut state = TerminalOnly::Inspecting {
        request: 11,
        live_size: Size::new(1920.0, 1080.0),
    };
    assert!(!state.inspected(11, fullscreen(), placement));
    assert_eq!(state.size(), Some(DEFAULT_WINDOW));
    assert!(!state.observe(WindowMode::default())); // Hiding the window is not a mode exit.
    assert_eq!(state.size(), Some(DEFAULT_WINDOW));
    assert!(state.observe(windowed())); // External exit restores previous native fullscreen.
    assert!(state.size().is_none());
    workspace.windows.get_mut(&window).unwrap().terminal_only = TerminalOnly::Inspecting {
        request: 12,
        live_size: DEFAULT_WINDOW,
    };
    let _ = workspace.update(Message::TerminalOnlyInspected(window, 12, fullscreen()));
    assert!(workspace.terminal_only_fullscreen_active(window));
    let _ = workspace.toggle_window_fullscreen(window);
    assert!(!workspace.terminal_only_fullscreen_active(window));
}

#[test]
fn denied_fullscreen_request_clears_centering_only_for_its_own_request() {
    let mut state = TerminalOnly::Entering {
        request: 24,
        size: DEFAULT_WINDOW,
    };
    state.timed_out(23, windowed());
    assert_eq!(state.size(), Some(DEFAULT_WINDOW));
    state.timed_out(24, windowed());
    assert!(!state.needs_native_mode());
}
