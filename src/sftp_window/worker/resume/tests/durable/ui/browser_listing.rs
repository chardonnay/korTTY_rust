use crate::sftp_window::{Message as WindowMessage, Side, SortColumn};

use super::*;

fn listing(bytes: &mut Vec<u8>) {
    super::upload::realpath(bytes, 1);
    string(bytes, HANDLE, 2, b"directory");
    let rows = [
        (b"a", Some((10, 4))),
        (b"b", Some((2, 1))),
        (b"c", None),
        (b"d", Some((2, 5))),
    ];
    let mut body = (rows.len() as u32).to_be_bytes().to_vec();
    for (name, ids) in rows {
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(name);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(4u32 | if ids.is_some() { 2 } else { 0 }).to_be_bytes());
        if let Some((uid, gid)) = ids {
            body.extend_from_slice(&u32::to_be_bytes(uid));
            body.extend_from_slice(&u32::to_be_bytes(gid));
        }
        body.extend_from_slice(&0o100600u32.to_be_bytes());
    }
    packet(bytes, 104, 3, &body);
    status(bytes, 4, 1);
    status(bytes, 5, 0);
}

fn names(window: &SftpWindow) -> Vec<&str> {
    window
        .remote
        .entries()
        .map(|entry| entry.display.as_str())
        .collect()
}

#[test]
fn sftp_browser_sorts_numeric_owner_and_group_without_losing_the_selection() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    super::upload::window(&context, |window, incoming, control| {
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::Refresh(Side::Remote));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut client(listing),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        let _ = window.update(WindowMessage::Done(ticket, result));
        assert_eq!(names(window), ["a", "b", "c", "d"]);
        let a = window.remote.entries().next().unwrap();
        assert_eq!((a.owner, a.group), (Some(10), Some(4)));
        let _ = window.update(WindowMessage::Select(
            Side::Remote,
            EntryName::Remote(b"a".to_vec()),
        ));
        window.remote.page = 3;
        let _ = window.update(WindowMessage::Sort(Side::Remote, SortColumn::Owner));
        assert_eq!(names(window), ["b", "d", "a", "c"]);
        assert_eq!(window.remote.page, 0);
        let _ = window.update(WindowMessage::Sort(Side::Remote, SortColumn::Owner));
        assert_eq!(names(window), ["a", "b", "d", "c"]);
        let _ = window.update(WindowMessage::Sort(Side::Remote, SortColumn::Group));
        assert_eq!(names(window), ["b", "a", "d", "c"]);
        let _ = window.update(WindowMessage::Sort(Side::Remote, SortColumn::Group));
        assert_eq!(names(window), ["d", "a", "b", "c"]);
        assert_eq!(window.remote.selected().unwrap().1.display, "a");
        let _ = window.update(WindowMessage::Sort(Side::Remote, SortColumn::Name));
        assert_eq!(names(window), ["a", "b", "c", "d"]);
        let _ = window.update(WindowMessage::Filter(Side::Remote, "[Ab]".into()));
        assert_eq!(
            window
                .remote
                .entries()
                .filter(|entry| window.remote.visible(entry))
                .map(|entry| entry.display.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        let _ = window.update(WindowMessage::Filter(Side::Remote, "D".into()));
        assert_eq!(
            window
                .remote
                .entries()
                .filter(|entry| window.remote.visible(entry))
                .count(),
            1
        );
        let _ = window.update(WindowMessage::Filter(Side::Remote, "[".into()));
        assert!(
            !window
                .remote
                .entries()
                .any(|entry| window.remote.visible(entry))
        );
    });
}

#[test]
fn sftp_browser_opens_remote_image_under_transfer_denial_and_revokes_its_view() {
    use iced::futures::{StreamExt as _, executor::block_on};

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    let path = root.join("fixture.png");
    ::image::RgbaImage::new(3, 2).save(&path).unwrap();
    let image = fs::read(path).unwrap();
    let name = EntryName::Remote(b"preview-\xff.png".to_vec());
    super::upload::window_with_policy(
        &context,
        RemoteEditorPolicy::ReadOnly,
        |window, incoming, control| {
            window.revalidate_file_transfers(false);
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                vec![Entry {
                    name: name.clone(),
                    display: "preview.png".into(),
                    hidden: false,
                    kind: EntryKind::File,
                    size: Some(image.len() as u64),
                    modified: Some(8),
                    owner: None,
                    group: None,
                    mode: Some(0o600),
                }],
            )));
            let _ = window.update(WindowMessage::Select(Side::Remote, name));
            let task = window.update(WindowMessage::OpenImage(Side::Remote));
            let command = incoming.try_recv().expect("remote image command");
            assert!(matches!(
                &command.operation,
                Operation::OpenProjectImage(path) if path == b"/remote/preview-\xff.png"
            ));
            assert!(window.transport_busy());
            let mut attributes = 13u32.to_be_bytes().to_vec();
            attributes.extend_from_slice(&(image.len() as u64).to_be_bytes());
            attributes.extend_from_slice(&0o100600u32.to_be_bytes());
            attributes.extend_from_slice(&0u32.to_be_bytes());
            attributes.extend_from_slice(&8u32.to_be_bytes());
            let mut client = client(|bytes| {
                super::upload::realpath(bytes, 1);
                for id in [2, 3] {
                    packet(bytes, ATTRS, id, &attributes);
                }
                string(bytes, HANDLE, 4, b"regular");
                for id in [5, 6] {
                    packet(bytes, ATTRS, id, &attributes);
                }
                string(bytes, DATA, 7, &image);
                status(bytes, 8, 1);
                packet(bytes, ATTRS, 9, &attributes);
                status(bytes, 10, 0);
                packet(bytes, ATTRS, 11, &attributes);
                super::upload::realpath(bytes, 12);
            });
            let reply = execute(
                &mut client,
                command.operation,
                control,
                RemoteEditorPolicy::ReadOnly,
            )
            .map(Arc::new);
            assert!(matches!(reply.as_deref(), Ok(Reply::ProjectImageOpened(_))));
            command.response.try_send(reply).unwrap();
            let mut task = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(message @ WindowMessage::ImageLoaded(..))) =
                block_on(task.next())
            else {
                panic!("image load returned no result");
            };
            let _ = window.update(message);
            assert!(window.showing_image());
            assert!(!window.transport_busy());
            window.revalidate_remote_images(false);
            assert!(!window.showing_image());
            assert!(!window.can_open_image(Side::Remote));
        },
    );
}
