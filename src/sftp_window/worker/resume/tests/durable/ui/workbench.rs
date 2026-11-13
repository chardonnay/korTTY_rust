use crate::sftp_window::{ConflictChoice, ConflictDefault, Message as WindowMessage, Side};

use super::*;

fn check_copy(command: Command, title: &str) {
    let Operation::Copy {
        transfer,
        destination: Destination::Check,
        ..
    } = command.operation
    else {
        panic!("Expected a transfer with a fresh conflict check");
    };
    assert_eq!(transfer.title, title);
}

fn select_files(window: &mut SftpWindow, root: &Path, names: &[&str]) {
    window
        .local
        .load(Arc::new(files::list_local(root).unwrap()));
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(b"/remote".to_vec()),
        Vec::new(),
    )));
    for &name in names {
        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Local,
            EntryName::Local(name.into()),
            true,
        ));
    }
}

fn conflict(
    window: &mut SftpWindow,
    command: Command,
    title: &str,
    mode: u32,
    version: u32,
    can_replace: bool,
) -> u64 {
    let Operation::Copy {
        transfer,
        destination: Destination::Check,
        ..
    } = command.operation
    else {
        panic!("expected a fresh destination check");
    };
    assert_eq!(transfer.title, title);
    let ticket = window.busy.as_ref().unwrap().id;
    let existing = Stamp::Remote(Attributes {
        size: Some(4),
        uid: Some(1000),
        gid: Some(1001),
        permissions: Some(mode),
        modified: Some(version),
        ..Attributes::default()
    });
    let _ = window.update(WindowMessage::Done(
        ticket,
        Ok(Arc::new(Reply::Conflict(Conflict {
            transfer,
            existing,
            can_replace,
        }))),
    ));
    ticket
}

fn complete_replacement(window: &mut SftpWindow, command: Command, title: &str, version: u32) {
    let Operation::Copy {
        transfer,
        destination: Destination::Replace(Stamp::Remote(stamp)),
        ..
    } = command.operation
    else {
        panic!("expected replacement with the latest destination stamp");
    };
    assert_eq!(transfer.title, title);
    assert_eq!(stamp.modified, Some(version));
    let ticket = window.busy.as_ref().unwrap().id;
    let _ = window.update(WindowMessage::Done(
        ticket,
        Ok(Arc::new(Reply::Changed {
            directory: None,
            note: "Replaced file".into(),
        })),
    ));
}

#[test]
fn transfer_queue_keeps_waiting_items_and_rechecks_a_failed_item_when_retried() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let names = ["first.txt", "second.txt", "third.txt"];
    for name in names {
        fs::write(root.join(name), b"data").unwrap();
    }
    super::upload::window(&context, |window, incoming, _| {
        select_files(window, &root, &names);
        let _ = window.update(WindowMessage::Transfer(Side::Local));
        check_copy(incoming.try_recv().unwrap(), names[0]);
        let busy = window.busy.as_ref().unwrap();
        let ticket = busy.id;
        let row = busy.queued.unwrap();
        assert_eq!(window.workbench.waiting(), 2);
        let _ = window.update(WindowMessage::PauseQueue(true));
        let _ = window.update(WindowMessage::CancelQueued(row + 1));
        let _ = window.update(WindowMessage::Done(
            ticket,
            Err(io::Error::other("Permission denied").into()),
        ));
        assert_eq!(window.workbench.waiting(), 1);
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::RetryQueued(row));
        assert_eq!(window.workbench.waiting(), 2);
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::PauseQueue(false));
        check_copy(incoming.try_recv().unwrap(), names[2]);
        let ticket = window.busy.as_ref().unwrap().id;
        let _ = window.update(WindowMessage::Done(
            ticket,
            Ok(Arc::new(Reply::Changed {
                directory: None,
                note: "Copied third.txt".into(),
            })),
        ));
        check_copy(incoming.try_recv().unwrap(), names[0]);
        assert_eq!(window.busy.as_ref().unwrap().queued, Some(row));
    });
}

#[test]
fn transfer_queue_clears_private_rows_and_ignores_old_retry_messages_on_mode_change() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let names = ["private-first.txt", "private-second.txt"];
    for name in names {
        fs::write(root.join(name), b"private").unwrap();
    }
    super::upload::window(&context, |window, incoming, _| {
        window.set_incognito(true);
        select_files(window, &root, &names);
        let _ = window.update(WindowMessage::Transfer(Side::Local));
        check_copy(incoming.try_recv().unwrap(), "private-first.txt");
        let busy = window.busy.as_ref().unwrap();
        let ticket = busy.id;
        let row = busy.queued.unwrap();
        let _ = window.update(WindowMessage::Done(
            ticket,
            Err(io::Error::other("Private transfer error").into()),
        ));
        assert_eq!(window.workbench.waiting(), 1);
        assert_eq!(window.status(), Some("Private transfer error"));
        window.set_incognito(false);
        assert_eq!(window.workbench.waiting(), 0);
        assert_eq!(window.status(), None);
        let _ = window.update(WindowMessage::RetryQueued(row));
        let _ = window.update(WindowMessage::PauseQueue(false));
        assert!(incoming.try_recv().is_err());
    });
}

#[test]
fn apply_all_replaces_only_matching_supported_files_in_the_current_batch() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let names = ["a.txt", "b.txt", "c-link.txt", "d-unsupported.txt"];
    for name in names {
        fs::write(root.join(name), b"data").unwrap();
    }
    super::upload::window(&context, |window, incoming, _| {
        select_files(window, &root, &names);
        let _ = window.update(WindowMessage::Transfer(Side::Local));
        let first = conflict(
            window,
            incoming.try_recv().unwrap(),
            names[0],
            0o100600,
            1,
            true,
        );
        assert!(window.prompt.is_some());
        let _ = window.update(WindowMessage::ApplyConflictToBatch(first, true));
        let _ = window.update(WindowMessage::Resolve(first, ConflictChoice::Replace));
        complete_replacement(window, incoming.try_recv().unwrap(), names[0], 1);

        conflict(
            window,
            incoming.try_recv().unwrap(),
            names[1],
            0o100600,
            2,
            true,
        );
        assert!(window.prompt.is_none());
        complete_replacement(window, incoming.try_recv().unwrap(), names[1], 2);

        let link = conflict(
            window,
            incoming.try_recv().unwrap(),
            names[2],
            0o120777,
            3,
            true,
        );
        assert!(window.prompt.is_some());
        let _ = window.update(WindowMessage::Resolve(first, ConflictChoice::Skip));
        let _ = window.update(WindowMessage::Resolve(link, ConflictChoice::Replace));
        assert!(window.prompt.is_some());
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::Resolve(link, ConflictChoice::Skip));

        let last = conflict(
            window,
            incoming.try_recv().unwrap(),
            names[3],
            0o100600,
            4,
            false,
        );
        assert!(window.prompt.is_some());
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::CancelConflictBatch(last));
        assert!(window.prompt.is_none());
        assert_eq!(window.workbench.waiting(), 0);

        let _ = window.update(WindowMessage::Transfer(Side::Local));
        conflict(
            window,
            incoming.try_recv().unwrap(),
            names[0],
            0o100600,
            5,
            true,
        );
        assert!(window.prompt.is_some());
        assert!(incoming.try_recv().is_err());
    });
}

#[test]
fn private_window_defaults_stay_local_and_managed_defaults_control_new_batches() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    fs::write(root.join("file.txt"), b"data").unwrap();
    super::upload::window(&context, |window, incoming, _| {
        window.configure_conflict_default(ConflictDefault::Overwrite, None);
        window.set_incognito(true);
        assert_eq!(window.effective_conflict_default(), ConflictDefault::Ask);
        window.set_private_conflict_default(ConflictDefault::Skip);
        window.configure_conflict_default(ConflictDefault::Overwrite, None);
        assert_eq!(window.effective_conflict_default(), ConflictDefault::Skip);
        window.configure_conflict_default(ConflictDefault::Skip, Some(ConflictDefault::Overwrite));
        assert!(!window.can_set_conflict_default());
        window.set_private_conflict_default(ConflictDefault::Ask);

        select_files(window, &root, &["file.txt"]);
        let _ = window.update(WindowMessage::Transfer(Side::Local));
        conflict(
            window,
            incoming.try_recv().unwrap(),
            "file.txt",
            0o100600,
            1,
            true,
        );
        assert!(window.prompt.is_none());
        complete_replacement(window, incoming.try_recv().unwrap(), "file.txt", 1);

        let _ = window.update(WindowMessage::Transfer(Side::Local));
        conflict(
            window,
            incoming.try_recv().unwrap(),
            "file.txt",
            0o120777,
            2,
            false,
        );
        assert!(window.prompt.is_some());
        assert!(incoming.try_recv().is_err());
    });
}
