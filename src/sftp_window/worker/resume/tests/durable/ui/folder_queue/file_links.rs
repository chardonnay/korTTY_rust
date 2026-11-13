use super::*;

const ALIAS: &[u8] = b"/remote/alias.txt";
const TARGET: &[u8] = b"/shared/regular.txt";
const TARGET_BYTES: &[u8] = b"regular source contents";

fn link_fixture() -> (Client, Arc<Mutex<Server>>) {
    let (client, server) = Server::fixture(false);
    let mut state = server.lock().unwrap();
    state.nodes.insert(b"/shared".to_vec(), Node::Directory);
    state
        .nodes
        .insert(TARGET.to_vec(), Node::File(TARGET_BYTES.to_vec()));
    state
        .nodes
        .insert(b"/shared/chain".to_vec(), Node::Link(TARGET.to_vec()));
    state
        .nodes
        .insert(ALIAS.to_vec(), Node::Link(b"/shared/chain".to_vec()));
    drop(state);
    (client, server)
}

fn browse_root(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    control: &Control,
    client: &mut Client,
    local: &Path,
) {
    let worker = window.worker.take().unwrap();
    let _ = window.initialize_at(worker, Some(local.to_path_buf()), Some(b"/remote".to_vec()));
    drain(window, incoming, control, client);
}

fn assert_regular_copy(path: &Path) {
    assert!(fs::symlink_metadata(path).unwrap().is_file());
    assert_eq!(fs::read(path).unwrap(), TARGET_BYTES);
}

fn assert_missing(path: &Path) {
    assert_eq!(
        fs::symlink_metadata(path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn sftp_selected_file_link_download_preserves_the_alias_and_skips_a_broken_link() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let (mut client, server) = link_fixture();
    server.lock().unwrap().nodes.insert(
        b"/remote/0-broken".to_vec(),
        Node::Link(b"/shared/missing".to_vec()),
    );
    super::super::upload::window(&context, |window, incoming, control| {
        browse_root(window, incoming, control, &mut client, &root);
        let _ = window.update(WindowMessage::Select(
            Side::Remote,
            EntryName::Remote(b"alias.txt".to_vec()),
        ));
        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"0-broken".to_vec()),
            true,
        ));
        assert_eq!(window.remote.selected.len(), 2);
        let _ = window.update(WindowMessage::Transfer(Side::Remote));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::CopyLink { transfer, .. } if transfer.title == "0-broken"
        ));
        run(window, control, &mut client, command);
        assert!(window.status().unwrap().contains("0-broken"));
        assert!(window.prompt.is_none());
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::CopyLink { transfer, .. } if transfer.title == "alias.txt"
        ));
        run(window, control, &mut client, command);
        drain(window, incoming, control, &mut client);
        assert!(window.busy.is_none());
        assert_eq!(window.workbench.waiting(), 0);
    });
    assert_regular_copy(&root.join("alias.txt"));
    assert_missing(&root.join("0-broken"));
    assert_missing(&root.join("regular.txt"));
    assert_eq!(server.lock().unwrap().opened_files, [TARGET.to_vec()]);
}

#[test]
fn sftp_folder_download_follows_regular_file_links_without_traversing_directory_links() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let (mut client, server) = link_fixture();
    {
        let mut state = server.lock().unwrap();
        for (name, target) in [
            (b"file-link".as_slice(), b"/shared/chain".as_slice()),
            (b"directory-link", b"/shared"),
            (b"broken-link", b"/shared/missing"),
            (b"loop-link", b"/remote/selected/loop-link"),
        ] {
            let path = [REMOTE, b"/", name].concat();
            state.nodes.insert(path, Node::Link(target.to_vec()));
        }
    }
    super::super::upload::window(&context, |window, incoming, control| {
        queue_folder(window, &root);
        drain(window, incoming, control, &mut client);
        assert!(window.busy.is_none());
        assert!(window.prompt.is_none());
        assert_eq!(window.workbench.waiting(), 0);
    });
    let target = root.join("selected");
    assert_regular_copy(&target.join("file-link"));
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), A_BYTES);
    assert_eq!(fs::read(target.join("b.txt")).unwrap(), B_BYTES);
    for name in ["directory-link", "broken-link", "loop-link"] {
        assert_missing(&target.join(name));
    }
    let state = server.lock().unwrap();
    assert_eq!(state.file_opens(TARGET), 1);
    assert!(
        !state
            .opened_folders
            .iter()
            .any(|path| path.as_slice() == b"/shared")
    );
}

fn file_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(target, link) {
        if error.raw_os_error() == Some(1314) {
            eprintln!(
                "Skipping local link fixture because creating symbolic links requires Windows privilege."
            );
            return false;
        }
        panic!("Could not create local link fixture: {error}");
    }
    true
}

fn keep_both_after_retarget(destination_is_link: bool) {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("alias.txt");
    let existing = if destination_is_link {
        let existing = root.join("existing.txt");
        fs::write(&existing, b"keep existing contents").unwrap();
        if !file_link(Path::new("existing.txt"), &destination) {
            return;
        }
        existing
    } else {
        fs::write(&destination, b"keep existing contents").unwrap();
        destination.clone()
    };
    let context = context(&root);
    let (mut client, server) = link_fixture();
    super::super::upload::window(&context, |window, incoming, control| {
        browse_root(window, incoming, control, &mut client, &root);
        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"alias.txt".to_vec()),
            true,
        ));
        let _ = window.update(WindowMessage::Transfer(Side::Remote));
        run(window, control, &mut client, incoming.try_recv().unwrap());
        let Some(crate::sftp_window::Prompt::Conflict {
            ticket, conflict, ..
        }) = window.prompt.as_ref()
        else {
            panic!("Expected the alias name to conflict with the existing destination");
        };
        assert!(
            matches!(&conflict.transfer.source, Location::Remote(path) if path.as_slice() == TARGET)
        );
        let ticket = *ticket;
        if destination_is_link {
            assert!(!conflict.can_replace());
            let _ = window.update(WindowMessage::Resolve(ticket, ConflictChoice::Replace));
            assert!(incoming.try_recv().is_err());
            assert!(window.prompt.is_some());
        }
        {
            let mut state = server.lock().unwrap();
            assert!(state.opened_files.is_empty());
            state.nodes.insert(ALIAS.to_vec(), Node::Link(A.to_vec()));
        }
        let _ = window.update(WindowMessage::Resolve(ticket, ConflictChoice::KeepBoth));
        drain(window, incoming, control, &mut client);
        assert!(window.busy.is_none());
        assert!(window.prompt.is_none());
    });
    assert_eq!(fs::read(existing).unwrap(), b"keep existing contents");
    assert_eq!(
        fs::symlink_metadata(destination).unwrap().is_symlink(),
        destination_is_link
    );
    assert_regular_copy(&root.join("alias (1).txt"));
    assert_eq!(server.lock().unwrap().opened_files, [TARGET.to_vec()]);
}

#[test]
fn sftp_file_link_conflict_keeps_the_resolved_source_after_retargeting() {
    keep_both_after_retarget(false);
}

#[test]
fn sftp_file_link_keep_both_preserves_a_destination_symlink() {
    keep_both_after_retarget(true);
}

#[test]
fn sftp_file_link_resume_reopens_the_saved_canonical_source() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let store = Store::new(&context.profile);
    let destination = root.join("alias.txt");
    let (mut client, server) = link_fixture();
    server.lock().unwrap().disconnect_read = Some(TARGET.to_vec());
    super::super::upload::window(&context, |window, incoming, control| {
        browse_root(window, incoming, control, &mut client, &root);
        let _ = window.update(WindowMessage::Select(
            Side::Remote,
            EntryName::Remote(b"alias.txt".to_vec()),
        ));
        let _ = window.update(WindowMessage::Transfer(Side::Remote));
        drain(window, incoming, control, &mut client);
        assert_eq!(window.partial.as_ref().unwrap().offset, 5);
    });
    assert_missing(&destination);
    let records = store.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].remote, TARGET);
    assert_eq!(records[0].payload.destination.path().unwrap(), destination);
    {
        let mut state = server.lock().unwrap();
        assert!(state.requests.is_empty() && state.replies.is_empty());
        state.nodes.insert(ALIAS.to_vec(), Node::Link(A.to_vec()));
        state.disconnect_read = None;
        state.disconnected = false;
        state.handles.clear();
    }
    client = Server::connect(&server);
    super::super::upload::window(&context, |window, incoming, control| {
        let _ = window.update(WindowMessage::FindInterruptedDownload);
        drain(window, incoming, control, &mut client);
        assert_eq!(window.partial.as_ref().unwrap().offset, 5);
        let _ = window.update(WindowMessage::ResumeDownload);
        drain(window, incoming, control, &mut client);
        assert!(window.partial.is_none());
        assert!(window.status().unwrap().contains("Resumed alias.txt"));
    });
    assert_regular_copy(&destination);
    assert!(store.list().unwrap().is_empty());
    assert_eq!(
        server.lock().unwrap().opened_files,
        [TARGET.to_vec(), TARGET.to_vec()]
    );
}

#[test]
fn sftp_selected_local_file_link_uploads_regular_bytes_under_its_alias() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let local = root.join("selected");
    fs::create_dir(&local).unwrap();
    let source = root.join("data.txt");
    fs::write(&source, TARGET_BYTES).unwrap();
    if !file_link(
        Path::new("..").join("data.txt").as_path(),
        &local.join("local-link.txt"),
    ) {
        return;
    }
    let context = context(&root);
    let (mut client, server) = Server::fixture(false);
    super::super::upload::window_without_verified_host(&context, |window, incoming, control| {
        browse_root(window, incoming, control, &mut client, &local);
        let _ = window.update(WindowMessage::Select(
            Side::Local,
            EntryName::Local("local-link.txt".into()),
        ));
        let _ = window.update(WindowMessage::Transfer(Side::Local));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(&command.operation, Operation::CopyLink { .. }));
        run(window, control, &mut client, command);
        drain(window, incoming, control, &mut client);
        assert!(window.busy.is_none());
        assert!(window.prompt.is_none());
        assert!(window.status().unwrap().contains("Copied local-link.txt"));
    });
    let state = server.lock().unwrap();
    assert!(matches!(
        state.nodes.get(b"/remote/local-link.txt".as_slice()),
        Some(Node::File(bytes)) if bytes == TARGET_BYTES
    ));
    assert!(!state.nodes.contains_key(b"/remote/data.txt".as_slice()));
    assert_eq!(fs::read(source).unwrap(), TARGET_BYTES);
}
