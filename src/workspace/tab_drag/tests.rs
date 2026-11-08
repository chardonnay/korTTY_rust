use iced::widget::pane_grid;

use super::super::{SessionId, Tab, TabEntry, tool_tabs};
use super::*;

fn tab(id: u64) -> Tab {
    // Test tabs use session IDs without transport workers.
    let (mut grid, first) = pane_grid::State::new(SessionId(id * 2));
    let (focused, _) = grid
        .split(pane_grid::Axis::Vertical, first, SessionId(id * 2 + 1))
        .unwrap();
    Tab {
        id,
        custom_title: None,
        group: None,
        connection_group_baseline: None,
        watch: Default::default(),
        reopen_origin: None,
        grid,
        remote_files: Default::default(),
        primary: first,
        focused,
        broadcast: true,
        command_timestamps: false,
        incognito: false,
        tunnel_owner: Some(SessionId(id * 2)),
    }
}

#[test]
fn drag_insertion_preserves_the_owned_tab_and_its_focused_pane() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let mut source_tab = tab(10);
    source_tab.incognito = true;
    workspace
        .windows
        .get_mut(&source)
        .unwrap()
        .tabs
        .push(source_tab.into());
    workspace
        .windows
        .get_mut(&destination)
        .unwrap()
        .tabs
        .extend([tab(1), tab(2)].map(TabEntry::from));
    let _ = workspace.transfer_at(source, Some(destination), 10, Some(1));
    assert!(workspace.windows[&source].tabs.is_empty());

    let window = &workspace.windows[&destination];
    assert_eq!(
        window.tabs.iter().map(TabEntry::id).collect::<Vec<_>>(),
        [1, 10, 2]
    );
    let moved = window.tab().unwrap();
    assert_eq!(moved.focused_session(), Some(SessionId(21)));
    assert_eq!(moved.grid.len(), 2);
    assert!(moved.broadcast);
    assert!(moved.incognito);
}

#[test]
fn surface_local_drag_requires_motion_and_a_drop_inside_the_same_window() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (other, _) = workspace.open_window(false);
    workspace
        .windows
        .get_mut(&source)
        .unwrap()
        .tabs
        .extend([tab(1), tab(2)].map(TabEntry::from));
    let start = Point::new(5.0, 5.0);
    let end = Point::new(700.0, 5.0);

    let _ = workspace.update_tab_drag(Message::Arm(source, 1, start.into()));
    let _ = workspace.update_tab_drag(Message::Release(source));
    assert_eq!(workspace.windows[&source].tabs[0].id(), 1);

    let _ = workspace.update_tab_drag(Message::Arm(source, 1, start.into()));
    let _ = workspace.update_tab_drag(Message::Move(source, end));
    let _ = workspace.update_tab_drag(Message::Release(other));
    assert_eq!(workspace.windows[&source].tabs[0].id(), 1);
    assert!(workspace.windows[&other].tabs.is_empty());

    let _ = workspace.update_tab_drag(Message::Arm(source, 1, start.into()));
    let _ = workspace.update_tab_drag(Message::Move(source, end));
    let _ = workspace.update_tab_drag(Message::Release(source));
    assert_eq!(workspace.windows[&source].tabs[1].id(), 1);
    assert_eq!(workspace.windows[&source].tab().unwrap().id, 1);
}

#[test]
fn appkit_drop_uses_the_delivered_release_even_when_hover_was_in_the_source() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    workspace
        .windows
        .get_mut(&source)
        .unwrap()
        .tabs
        .push(tab(7).into());
    let start = Point::new(5.0, 5.0);
    let drop = Point::new(120.0, 140.0);
    let _ = workspace.update_tab_drag(Message::Arm(source, 7, start.into()));
    let ticket = workspace.tab_drag.ticket;

    // Test doubles for the globally resolved AppKit hits.
    let _ = workspace.apply_mac_tab_sample(
        ticket,
        Some(MacDragProgress {
            held: true,
            moved: true,
            hit: Some((source, drop)),
        }),
    );
    assert_eq!(workspace.tab_drag.hover(), Some(source));
    let _ = workspace.apply_mac_tab_sample(
        ticket,
        Some(MacDragProgress {
            held: false,
            moved: true,
            hit: Some((destination, drop)),
        }),
    );
    assert!(workspace.windows[&source].tabs.is_empty());
    assert_eq!(workspace.windows[&destination].active_tab_id(), Some(7));
    assert!(workspace.tab_drag.gesture.is_none());
}

#[test]
fn appkit_click_occluded_drop_and_stale_release_do_not_transfer() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    workspace
        .windows
        .get_mut(&source)
        .unwrap()
        .tabs
        .push(tab(7).into());
    let start = Point::new(5.0, 5.0);
    let release = Point::new(120.0, 140.0);

    // Test doubles for an explicit click, then a hit hidden by another app.
    let _ = workspace.update_tab_drag(Message::Arm(source, 7, start.into()));
    let old = workspace.tab_drag.ticket;
    let _ = workspace.apply_mac_tab_sample(
        old,
        Some(MacDragProgress {
            held: false,
            moved: false,
            hit: Some((source, start)),
        }),
    );
    assert!(workspace.tab_drag.gesture.is_none());

    let _ = workspace.update_tab_drag(Message::Arm(source, 7, start.into()));
    let current = workspace.tab_drag.ticket;
    let _ = workspace.apply_mac_tab_sample(
        old,
        Some(MacDragProgress {
            held: false,
            moved: true,
            hit: Some((destination, release)),
        }),
    );
    assert_eq!(
        workspace
            .tab_drag
            .gesture
            .as_ref()
            .map(|gesture| gesture.ticket),
        Some(current)
    );

    let _ = workspace.apply_mac_tab_sample(
        current,
        Some(MacDragProgress {
            held: true,
            moved: true,
            hit: Some((destination, release)),
        }),
    );
    let _ = workspace.apply_mac_tab_sample(
        current,
        Some(MacDragProgress {
            held: false,
            moved: true,
            hit: None,
        }),
    );
    assert_eq!(workspace.windows[&source].tabs[0].id(), 7);
    assert!(workspace.windows[&destination].tabs.is_empty());
    assert!(workspace.tab_drag.gesture.is_none());
}

#[test]
fn a_tool_drag_uses_the_pane_position_and_appends_from_window_chrome() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let (empty, _) = workspace.open_window(false);
    // The hosted instance ID is a test double. No tool or transport needs to run.
    let instance = window::Id::unique();
    let _ = workspace.host_tool(source, tool_tabs::Kind::OpenPgp, instance);
    let tool = workspace.windows[&source].active_tab_id().unwrap();
    workspace
        .windows
        .get_mut(&destination)
        .unwrap()
        .tabs
        .extend([tab(100), tab(101)].map(TabEntry::from));
    workspace.tab_drag.pane_bounds.record(
        destination,
        Some(Rectangle::new(
            Point::new(100.0, 170.0),
            Size::new(800.0, 400.0),
        )),
    );

    let _ = workspace.drop_tab_at(source, destination, tool, Point::new(-5.0, 250.0));
    assert_eq!(workspace.hosted_tool_owner(instance), Some(source));

    let _ = workspace.drop_tab_at(source, destination, tool, Point::new(425.0, 300.0));
    assert_eq!(workspace.windows[&destination].tabs[1].id(), tool);
    assert_eq!(workspace.windows[&destination].active_tab_id(), Some(tool));

    let _ = workspace.drop_tab_at(destination, empty, tool, Point::new(30.0, 200.0));
    assert_eq!(workspace.windows[&empty].tabs[0].id(), tool);

    let _ = workspace.drop_tab_at(empty, destination, tool, Point::new(20.0, 90.0));
    assert_eq!(workspace.windows[&destination].tabs[2].id(), tool);
    assert_eq!(workspace.windows[&destination].active_tab_id(), Some(tool));
    assert!(
        workspace
            .hosted_tab_at(destination, tool)
            .is_some_and(|tab| tab.instance == instance)
    );
}

#[test]
fn scrolled_tab_header_arms_with_the_unshifted_pointer_position() {
    use iced::advanced::clipboard;
    use iced::advanced::widget::{Id, operation};
    use iced::widget::{button, row, scrollable, space};

    let (source, _) = window::open(window::Settings::default());
    let scrollbar = Id::unique();
    let tab = header(
        button("Tab").width(100).on_press(WorkspaceMessage::Noop),
        source,
        7,
    );
    let mut view = DragState::default().strip(
        source,
        scrollable(row![
            space::horizontal().width(125),
            tab,
            space::horizontal().width(200)
        ])
        .id(scrollbar.clone())
        .width(200)
        .height(50)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new(),
        ))
        .into(),
    );
    let mut tree = Tree::new(view.as_widget());
    let renderer = Renderer::new(iced::Font::DEFAULT, 16.0.into());
    let size = Size::new(200.0, 50.0);
    let bounds = Rectangle::with_size(size);
    let node =
        view.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let layout = Layout::new(&node);
    let mut scroll = operation::scrollable::scroll_to::<()>(
        scrollbar,
        scrollable::AbsoluteOffset {
            x: Some(100.0),
            y: None,
        },
    );
    view.as_widget_mut()
        .operate(&mut tree, layout, &renderer, &mut scroll);
    let origin = Point::new(40.0, 10.0);
    let mut messages = Vec::new();
    view.as_widget_mut().update(
        &mut tree,
        &Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
        layout,
        mouse::Cursor::Available(origin),
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &bounds,
    );
    assert!(matches!(
        messages.as_slice(),
        [WorkspaceMessage::TabDrag(Message::Arm(id, 7, position))]
            if *id == source && position.point == origin
    ));
}
