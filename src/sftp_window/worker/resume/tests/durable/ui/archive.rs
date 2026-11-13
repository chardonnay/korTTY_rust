use std::time::Instant;

use crate::sftp_window::{
    Message as WindowMessage, Prompt, Side, archive::Message as ArchiveMessage,
};

use super::*;

#[test]
fn server_archive_metadata_draft_validates_fields_and_checks_the_live_route() {
    let fixture = crate::snippets::tests::Profile::new();
    let context = context(&fixture.0.canonicalize().unwrap());
    super::upload::window(&context, |window, incoming, control| {
        open_server_archive(window, control);
        for message in [
            ArchiveMessage::Owner("4294967295".into()),
            ArchiveMessage::Group("1001".into()),
            ArchiveMessage::Permissions("640".into()),
        ] {
            edit(window, message);
        }
        assert_review_unqueued(window, incoming);

        edit(window, ArchiveMessage::Owner("1000".into()));
        edit(window, ArchiveMessage::Permissions("0640".into()));
        assert_review_unqueued(window, incoming);

        edit(window, ArchiveMessage::Permissions("640".into()));
        edit(window, ArchiveMessage::Group("archive-group".into()));
        assert_review_unqueued(window, incoming);

        edit(window, ArchiveMessage::Group("1001".into()));
        edit(window, ArchiveMessage::Review);
        let command = incoming
            .try_recv()
            .expect("queue valid owner, group and mode");
        assert!(matches!(
            command.operation,
            Operation::ReviewServerArchive(..)
        ));
        assert_eq!(window.busy.as_ref().unwrap().archive, Some(Side::Remote));
    });

    super::upload::window(&context, |window, incoming, control| {
        open_server_archive(window, control);
        edit(window, ArchiveMessage::Owner("archive-owner".into()));
        control.abort();
        assert_review_unqueued(window, incoming);
    });
}

#[test]
fn local_archive_review_ignores_server_metadata_messages() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::write(root.join("fixture.txt"), b"local archive").unwrap();
    let mut window = SftpWindow::new(context(&root).route, None, true, RemoteEditorPolicy::Deny);
    window
        .local
        .load(Arc::new(files::list_local(&root).unwrap()));
    let _ = window.update(WindowMessage::Select(
        Side::Local,
        EntryName::Local("fixture.txt".into()),
    ));
    let _ = window.update(WindowMessage::CreateLocalArchive);
    for message in [
        ArchiveMessage::Owner("invalid account".into()),
        ArchiveMessage::Group("4294967295".into()),
        ArchiveMessage::Permissions("999".into()),
        ArchiveMessage::Review,
    ] {
        edit(&mut window, message);
    }
    assert_eq!(window.busy.as_ref().unwrap().archive, Some(Side::Local));
    let deadline = Instant::now() + Duration::from_secs(5);
    let message = loop {
        if let Some(message) = window.poll_worker_reply_for_test() {
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "local archive review did not finish"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert!(
        matches!(&message, WindowMessage::Done(_, Ok(reply)) if matches!(reply.as_ref(), Reply::LocalArchiveReview(_)))
    );
    let _ = window.update(message);
    assert!(matches!(window.prompt, Some(Prompt::Archive(_))));
    assert!(!root.join("archive.zip").exists());
}

fn open_server_archive(window: &mut SftpWindow, control: &Control) {
    control.archive_supported.store(true, Ordering::Release);
    let name = EntryName::Remote(b"fixture.txt".to_vec());
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(b"/remote".to_vec()),
        vec![Entry {
            name: name.clone(),
            display: "fixture.txt".into(),
            hidden: false,
            kind: EntryKind::File,
            size: Some(7),
            modified: Some(24),
            owner: Some(1000),
            group: Some(1001),
            mode: Some(0o100600),
        }],
    )));
    let _ = window.update(WindowMessage::Select(Side::Remote, name));
    let _ = window.update(WindowMessage::CreateServerArchive);
    assert!(matches!(window.prompt, Some(Prompt::Archive(_))));
}

fn assert_review_unqueued(window: &mut SftpWindow, incoming: &mpsc::Receiver<Command>) {
    edit(window, ArchiveMessage::Review);
    assert!(matches!(
        incoming.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(window.busy.is_none());
    assert!(matches!(window.prompt, Some(Prompt::Archive(_))));
}

fn edit(window: &mut SftpWindow, message: ArchiveMessage) {
    let _ = window.update(WindowMessage::ServerArchive(message));
}
