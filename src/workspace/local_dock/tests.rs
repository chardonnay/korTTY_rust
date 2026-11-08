use std::sync::atomic::{AtomicU64, Ordering};

use iced::advanced::{Layout, layout, widget::Tree};
use iced::widget::{canvas::Program, container};
use iced::{Event, Point, Rectangle, Renderer, Size, event, mouse};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = env::temp_dir().join(format!(
                "kortty-local-dock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_into(browser: &mut Browser, path: &Path, navigation: Navigation) {
    let _task = browser.load(path.to_path_buf(), navigation);
    browser.listed(
        browser.request,
        navigation,
        list_local(path)
            .map(Arc::new)
            .map_err(|error| error.to_string()),
    );
}

#[test]
fn real_entries_keep_native_paths_and_failed_navigation_keeps_history() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let root = fs::canonicalize(&scratch.0)?;
    let folder = root.join("Folder");
    fs::create_dir(&folder)?;
    fs::write(root.join("Read Me.txt"), "hello")?;
    fs::write(root.join(".secret"), "hidden")?;

    let mut browser = Browser::default();
    read_into(&mut browser, &root, Navigation::Visit);
    assert_eq!(browser.current_path(), Some(root.as_path()));
    assert_eq!(browser.visible.len(), 2);
    let original = browser.displayed;
    browser.filter = "READ".into();
    browser.filter_entries();
    assert_eq!(browser.visible.len(), 1);
    let index = browser.visible[0];
    assert!(matches!(
        browser.activate(original, index),
        Open::File(path) if path == root.join("Read Me.txt")
    ));

    browser.filter.clear();
    browser.filter_entries();
    let folder_index = browser.visible[0];
    assert!(matches!(
        browser.activate(original, folder_index),
        Open::Directory(path) if path == folder
    ));
    read_into(&mut browser, &folder, Navigation::Visit);
    assert_eq!(browser.back.back(), Some(&root));
    assert!(matches!(
        browser.activate(original, folder_index),
        Open::None
    ));

    read_into(&mut browser, &root.join("missing"), Navigation::Visit);
    assert!(browser.status.is_some());
    assert_eq!(browser.current_path(), Some(folder.as_path()));
    assert_eq!(browser.back.back(), Some(&root));

    read_into(&mut browser, &root, Navigation::Back);
    assert!(browser.back.is_empty());
    assert_eq!(browser.forward.back(), Some(&folder));
    browser.show_hidden = true;
    browser.filter_entries();
    assert_eq!(browser.visible.len(), 3);
    read_into(&mut browser, &folder, Navigation::Forward);
    assert_eq!(browser.back.back(), Some(&root));
    assert!(browser.forward.is_empty());
    Ok(())
}

#[test]
fn persisted_preference_is_bounded_and_preserves_width_on_a_small_window() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("local-dock.json");
    let preference = Preference {
        side: Some(Side::Right),
        width: 700.0,
    };
    profile::save_atomic(&path, &serde_json::to_vec(&preference)?)?;
    assert_eq!(Preference::read(&path)?, preference);
    assert_eq!(preference.pane_width(700.0), 358.0);
    assert_eq!(preference.pane_width(1400.0), 700.0);
    assert_eq!(preference.width, 700.0);

    for invalid in [
        b"{\"side\":\"top\",\"width\":700}".as_slice(),
        b"{\"side\":null,\"width\":-1}".as_slice(),
        b"{\"side\":null,\"width\":280,\"unknown\":true}".as_slice(),
    ] {
        fs::write(&path, invalid)?;
        assert!(Preference::read(&path).is_err());
    }
    fs::write(&path, vec![b' '; PREFERENCE_LIMIT as usize + 1])?;
    assert!(Preference::read(&path).is_err());
    Ok(())
}

#[test]
fn widget_layout_keeps_the_live_terminal_in_the_same_tree_slot() {
    let mut workspace = Workspace::default();
    let (id, _) = workspace.open_window(false);
    let renderer = Renderer::new(iced::Font::DEFAULT, 16.0.into());
    let size = Size::new(986.0, 480.0);

    let terminal = || {
        container(text("Terminal"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };
    let view = workspace.local_dock_view(id, &workspace.windows[&id], terminal());
    let mut tree = Tree::new(view.as_widget());
    drop(view);
    for (side, terminal_x, terminal_width) in [
        (None, 0.0, 986.0),
        (Some(Side::Left), 388.0, 598.0),
        (Some(Side::Right), 0.0, 598.0),
        (None, 0.0, 986.0),
    ] {
        workspace.local_dock.side = side;
        let mut view = workspace.local_dock_view(id, &workspace.windows[&id], terminal());
        tree.diff(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let children: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(children.len(), 3);
        let terminal = children[1].bounds();
        assert_eq!((terminal.x, terminal.width), (terminal_x, terminal_width));
    }
}

#[test]
fn divider_keeps_its_press_origin_and_finishes_one_resize_on_each_side() {
    let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    let start = Point::new(300.0, 50.0);
    let next = Point::new(340.0, 50.0);
    let approach = Event::Mouse(mouse::Event::CursorMoved { position: start });
    let movement = Event::Mouse(mouse::Event::CursorMoved { position: next });
    let bounds = Rectangle {
        x: 298.0,
        y: 0.0,
        width: 8.0,
        height: 100.0,
    };
    for (side, expected) in [(Side::Left, 420.0), (Side::Right, 340.0)] {
        let divider = divider_program(side, 380.0);
        let mut drag = Default::default();
        let cursor = mouse::Cursor::Available(next);
        assert!(
            divider
                .update(&mut drag, &approach, bounds, cursor)
                .is_none()
        );
        let action = divider
            .update(&mut drag, &press, bounds, cursor)
            .unwrap()
            .into_inner();
        assert_eq!(action.2, event::Status::Captured);
        let action = divider
            .update(&mut drag, &movement, bounds, mouse::Cursor::Available(next))
            .unwrap()
            .into_inner();
        assert!(
            matches!(action.0, Some(Message::Resize(actual_side, width)) if actual_side == side && width == expected)
        );
        let action = divider
            .update(&mut drag, &release, bounds, mouse::Cursor::Available(next))
            .unwrap()
            .into_inner();
        assert!(
            matches!(action.0, Some(Message::FinishResize(actual_side)) if actual_side == side)
        );
        assert!(
            divider
                .update(&mut drag, &release, bounds, mouse::Cursor::Available(next))
                .is_none()
        );
    }
}

#[test]
fn dock_side_and_native_scale_map_tab_drag_coordinates() {
    let mut preference = Preference {
        side: Some(Side::Left),
        width: 380.0,
    };
    let (x, width) = preference.tab_drop_coordinates(1000.0, 1000.0, 2000.0);
    assert!((x - 224.0).abs() < 0.001);
    assert!((width - 1224.0).abs() < 0.001);
    preference.side = Some(Side::Right);
    let (x, width) = preference.tab_drop_coordinates(1000.0, 1000.0, 2000.0);
    assert_eq!(x, 1000.0);
    assert!((width - 1224.0).abs() < 0.001);
}
