use iced::Pixels;
use iced::advanced::clipboard;
use iced::widget::{button, column, row, scrollable};

use super::super::Location;
use super::*;

fn remote_listing() -> Arc<Directory> {
    Arc::new(Directory::sorted(
        Location::Remote(b"/upload".to_vec()),
        vec![Entry {
            name: EntryName::Remote(b"folder".to_vec()),
            display: "folder".into(),
            hidden: false,
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            owner: None,
            group: None,
            mode: None,
        }],
    ))
}

#[cfg(target_os = "linux")]
#[test]
fn wayland_drop_uses_the_scrolled_folder_and_rejects_an_outside_position() {
    let listing = remote_listing();
    let mut panel = FilePanel::default();
    panel.load(Arc::clone(&listing));
    let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
    let id = iced::widget::Id::unique();
    let mut surface = scope(|zone| {
        let rows = column![
            button("Before").width(120).height(28),
            zone.item(
                button("Folder").width(120).height(28),
                Some(Item::desktop(
                    Side::Remote,
                    Some(&listing),
                    Some(listing.entries[0].name.clone())
                )),
            ),
            button("After").width(120).height(28),
        ];
        zone.item(
            scrollable(rows).id(id.clone()).height(28),
            Some(Item::desktop(Side::Remote, Some(&listing), None)),
        )
    });
    let mut tree = Tree::new(surface.as_widget());
    let node = surface.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(120.0, 28.0)),
    );
    let mut scroll = iced::advanced::widget::operation::scrollable::scroll_to(
        id,
        iced::advanced::widget::operation::scrollable::AbsoluteOffset {
            x: None,
            y: Some(28.0),
        },
    );
    surface
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
    for (position, expected) in [
        (
            Point::new(30.0, 14.0),
            Some(Location::Remote(b"/upload/folder".to_vec())),
        ),
        (Point::new(30.0, 42.0), None),
    ] {
        let actual = Arc::new(std::sync::Mutex::new(None));
        let send = Arc::clone(&actual);
        let mut operation = iced::advanced::widget::operation::map(
            desktop_drop_target(Some(position)),
            move |target| *send.lock().unwrap() = Some(target),
        );
        surface
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut operation);
        assert!(matches!(
            operation.finish(),
            iced::advanced::widget::operation::Outcome::Some(())
        ));
        let actual = actual.lock().unwrap().take().expect("target result");
        assert_eq!(
            actual.and_then(|target| target.current_path(&panel)),
            expected
        );
    }
}

#[test]
fn desktop_drop_prefers_the_folder_row_and_captures_denied_drops() {
    let listing = remote_listing();
    let folder = listing.entries[0].name.clone();
    let mut panel = FilePanel::default();
    panel.load(Arc::clone(&listing));
    for (position, allowed, expected) in [
        (Point::new(30.0, 12.0), true, Some(&b"/upload/folder"[..])),
        (Point::new(30.0, 52.0), true, Some(&b"/upload"[..])),
        (Point::new(30.0, 12.0), false, None),
    ] {
        let allowed_listing = allowed.then_some(&listing);
        let mut surface = scope(|zone| {
            let rows = column![
                zone.item(
                    button("Folder").width(120).height(28),
                    Some(Item::desktop(
                        Side::Remote,
                        allowed_listing,
                        Some(folder.clone())
                    )),
                ),
                button("File").width(120).height(28),
            ];
            zone.item(
                rows,
                Some(Item::desktop(Side::Remote, allowed_listing, None)),
            )
        });
        let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
        let mut tree = Tree::new(surface.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0));
        let node = surface
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let path = std::env::temp_dir().join("desktop.txt");
        surface.as_widget_mut().update(
            &mut tree,
            &Event::Window(window::Event::FileDropped(path.clone())),
            Layout::new(&node),
            mouse::Cursor::Available(position),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &node.bounds(),
        );
        assert!(shell.is_event_captured());
        if let Some(expected) = expected {
            let [Message::DesktopDrop(drop)] = messages.as_slice() else {
                panic!("expected exactly one upload intent");
            };
            assert_eq!(drop.path, path);
            assert_eq!(
                drop.destination.current_path(&panel),
                Some(Location::Remote(expected.to_vec()))
            );
            let mut replaced = FilePanel::default();
            replaced.load(Arc::new((*listing).clone()));
            assert_eq!(drop.destination.current_path(&replaced), None);
        } else {
            assert!(messages.is_empty());
        }
    }
}

#[test]
fn desktop_drop_never_uses_relative_or_traversing_paths() {
    let target = Location::Remote(b"/upload".to_vec());
    let relative = PathBuf::from("desktop.txt");
    let traversing = std::env::temp_dir().join("..").join("desktop.txt");
    let (transfers, rejected) = local_transfers([relative, traversing].into_iter(), &target);
    assert!(transfers.is_empty());
    assert_eq!(rejected, 2);
}

#[test]
fn sftp_local_drop_is_available_without_a_worker_or_transfer_permission() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    std::fs::create_dir(root.join("folder")).unwrap();
    let target = serde_json::from_value(serde_json::json!({
        "host": "localhost", "port": 22, "username": "fixture"
    }))
    .unwrap();
    let mut state = SftpWindow::new(
        target,
        None,
        false,
        crate::clipboard_policy::RemoteEditorPolicy::Deny,
    );
    let listing = Arc::new(super::super::files::list_local(&root).unwrap());
    state.local.load(Arc::clone(&listing));
    assert!(state.desktop_drop_available(Side::Local));
    assert!(!state.desktop_drop_available(Side::Remote));

    let mut surface = scope(|zone| {
        zone.item(
            button("Local folder").width(120).height(28),
            Some(Item::desktop(
                Side::Local,
                Some(&listing),
                Some(EntryName::Local("folder".into())),
            )),
        )
    });
    let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
    let mut tree = Tree::new(surface.as_widget());
    let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0));
    let node = surface
        .as_widget_mut()
        .layout(&mut tree, &renderer, &limits);
    let mut messages = Vec::new();
    surface.as_widget_mut().update(
        &mut tree,
        &Event::Window(window::Event::FileDropped(root.join("source.txt"))),
        Layout::new(&node),
        mouse::Cursor::Available(Point::new(30.0, 12.0)),
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &node.bounds(),
    );
    let [Message::DesktopDrop(drop)] = messages.as_slice() else {
        panic!("expected exactly one local copy intent");
    };
    assert!(!drop.is_remote());
    assert_eq!(
        drop.destination.current_path(&state.local),
        Some(Location::Local(root.join("folder")))
    );
}

#[test]
fn pane_drag_keeps_the_ordinary_transfer_selection_and_destination() {
    let scratch = crate::snippets::tests::Profile::new();
    let local_path = scratch.0.canonicalize().unwrap();
    std::fs::write(local_path.join("first.txt"), b"first").unwrap();
    std::fs::write(local_path.join("second.txt"), b"second").unwrap();
    let mut local = FilePanel::default();
    local.load(Arc::new(
        super::super::files::list_local(&local_path).unwrap(),
    ));
    local.selected = local.entries().map(|entry| entry.name.clone()).collect();
    let listing = remote_listing();
    let mut remote = FilePanel::default();
    remote.load(listing);

    let mut surface = scope(|zone| {
        let source = Panel::new(Side::Local, &local, &remote).unwrap();
        let destination = Panel::new(Side::Remote, &remote, &local).unwrap();
        row![
            zone.item(
                button("Selected file")
                    .width(100)
                    .height(28)
                    .on_press(Message::Up(Side::Local)),
                Some(source.row(local.entries().next().unwrap())),
            ),
            zone.item(
                scrollable(
                    zone.item(
                        button("Folder")
                            .width(100)
                            .height(28)
                            .on_press(Message::Up(Side::Remote)),
                        Some(destination.row(remote.entries().next().unwrap())),
                    )
                )
                .height(56),
                Some(destination.background()),
            )
        ]
        .into()
    });
    let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
    let mut tree = Tree::new(surface.as_widget());
    let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0));
    let node = surface
        .as_widget_mut()
        .layout(&mut tree, &renderer, &limits);
    let start = Point::new(30.0, 12.0);
    let finish = Point::new(130.0, 12.0);
    let mut messages = Vec::new();
    for (event, position) in [
        (mouse::Event::CursorMoved { position: start }, start),
        (mouse::Event::ButtonPressed(mouse::Button::Left), start),
        (mouse::Event::CursorMoved { position: finish }, finish),
        (mouse::Event::ButtonReleased(mouse::Button::Left), finish),
    ] {
        surface.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(event),
            Layout::new(&node),
            mouse::Cursor::Available(position),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &node.bounds(),
        );
    }
    let [Message::DropTransfer(drop)] = messages.as_slice() else {
        panic!("expected exactly one pane transfer intent");
    };
    let transfers = drop.transfers(&local, &remote).unwrap();
    assert_eq!(transfers.len(), 2);
    assert!(transfers.iter().all(|transfer| {
        transfer.target_directory == Location::Remote(b"/upload/folder".to_vec())
    }));
}
