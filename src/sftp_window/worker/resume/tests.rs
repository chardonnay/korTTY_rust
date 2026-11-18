use std::io::Cursor;

use crate::session_security::IsolationLevel;
use crate::sftp_window::{ClosedDownload, SftpWindow};
use crate::ssh_dialog::LoginTarget;
use crate::workspace::SshTarget;

use super::*;

mod durable;

const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const ATTRS: u8 = 105;
const HOST: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX";
const ROTATED: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo";

fn peer(key: &str) -> Control {
    Control {
        host_key: Some(HostKey::from_openssh(key).unwrap()),
        ..Control::default()
    }
}

fn packet(bytes: &mut Vec<u8>, kind: u8, id: u32, body: &[u8]) {
    bytes.extend_from_slice(&((5 + body.len()) as u32).to_be_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes.extend_from_slice(body);
}

fn string(bytes: &mut Vec<u8>, kind: u8, id: u32, value: &[u8]) {
    let mut body = (value.len() as u32).to_be_bytes().to_vec();
    body.extend_from_slice(value);
    packet(bytes, kind, id, &body);
}

fn attributes(bytes: &mut Vec<u8>, id: u32, modified: u32) {
    let mut body = 13u32.to_be_bytes().to_vec();
    body.extend_from_slice(&10u64.to_be_bytes());
    body.extend_from_slice(&0o100600u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&modified.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn status(bytes: &mut Vec<u8>, id: u32, code: u32) {
    let mut body = code.to_be_bytes().to_vec();
    body.extend_from_slice(&[0; 8]);
    packet(bytes, STATUS, id, &body);
}

fn client(replies: impl FnOnce(&mut Vec<u8>)) -> Client {
    let mut bytes = Vec::new();
    packet(&mut bytes, 2, 3, &[]);
    replies(&mut bytes);
    Sftp::new(
        Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>,
        Box::new(io::sink()) as Box<dyn Write + Send>,
    )
    .unwrap()
}

#[test]
fn delete_selection_preflights_all_entries_then_removes_only_selected_children() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    let retained = root.path().join("retained.txt");
    for path in [&first, &second, &retained] {
        std::fs::write(path, b"fixture").unwrap();
    }
    let snapshot = || crate::sftp_window::files::list_local(root.path()).unwrap();
    let selected = |directory: &crate::sftp_window::Directory| {
        directory
            .entries
            .iter()
            .filter(|entry| entry.display != "retained.txt")
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut client = client(|_| {});
    let original = snapshot();
    std::fs::write(&second, b"changed fixture").unwrap();
    assert!(
        super::super::delete_selection(&mut client, &original.path, &selected(&original), || false)
            .is_err()
    );
    assert!(first.exists() && second.exists() && retained.exists());
    let reviewed = snapshot();
    let result =
        super::super::delete_selection(&mut client, &reviewed.path, &selected(&reviewed), || false)
            .unwrap();
    assert!(
        matches!(result, crate::sftp_window::worker::Reply::Changed { note, .. }
        if note == "Deleted 2 entries")
    );
    assert!(!first.exists() && !second.exists() && retained.exists());
}

#[test]
fn deletion_cancellation_reports_confirmed_removals_and_keeps_remaining_entries() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    std::fs::write(&first, b"fixture").unwrap();
    std::fs::write(&second, b"fixture").unwrap();
    let directory = crate::sftp_window::files::list_local(root.path()).unwrap();
    let mut client = client(|_| {});
    let run = |client: &mut Client, stop: &dyn Fn() -> bool| {
        super::super::delete_selection(client, &directory.path, &directory.entries, stop).unwrap()
    };
    assert!(matches!(run(&mut client, &|| true),
        crate::sftp_window::worker::Reply::Changed { note, .. }
        if note == "Deletion stopped · Deleted 0 of 2 entries"));
    assert!(first.exists() && second.exists());
    assert!(matches!(run(&mut client, &|| !first.exists()),
        crate::sftp_window::worker::Reply::Changed { note, .. }
        if note == "Deletion stopped · Deleted 1 of 2 entries"));
    assert!(!first.exists() && second.exists());
}

#[test]
fn server_delete_reports_an_unconfirmed_removal_after_disconnect() {
    let entries = ["first.txt", "second.txt"].map(|name| crate::sftp_window::Entry {
        name: EntryName::Remote(name.as_bytes().to_vec()),
        display: name.to_owned(),
        hidden: false,
        kind: EntryKind::File,
        size: Some(10),
        modified: None,
        owner: None,
        group: None,
        mode: None,
    });
    let mut client = client(|bytes| {
        for id in [1, 2, 3] {
            attributes(bytes, id, 24);
        }
        status(bytes, 4, 0);
        attributes(bytes, 5, 24);
    });
    let error = super::super::delete_selection(
        &mut client,
        &Location::Remote(b"/fixture".to_vec()),
        &entries,
        || false,
    )
    .unwrap_err();
    assert!(error.disconnected);
    assert!(error.message.contains("Deleted 1 of 2 entries"));
    assert!(error.message.contains("second.txt"));
    assert!(
        error
            .message
            .contains("The last server removal could not be confirmed")
    );
}

#[test]
fn recursive_deletion_reviews_descendants_and_refuses_changed_or_replayed_plans() {
    use super::super::recursive_delete::{apply, review};

    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("chosen");
    let child = selected.join("nested");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("file.txt"), b"fixture").unwrap();
    let retained = root.path().join("unselected.txt");
    std::fs::write(&retained, b"retained").unwrap();
    let mut client = client(|_| {});
    let control = Arc::new(Control::default());
    let listing = crate::sftp_window::files::list_local(root.path()).unwrap();
    let entries = listing
        .entries
        .into_iter()
        .filter(|entry| entry.display == "chosen")
        .collect::<Vec<_>>();
    let plan = |client: &mut Client| {
        review(
            client,
            listing.path.clone(),
            &entries,
            Arc::downgrade(&control),
            &|| false,
        )
        .unwrap()
    };
    let first = plan(&mut client);
    assert_eq!(
        first.paths(),
        ["chosen", "chosen/nested", "chosen/nested/file.txt"]
    );
    assert!(selected.exists() && retained.exists());
    std::fs::write(child.join("late.txt"), b"changed").unwrap();
    assert!(apply(&mut client, &first, &control, &|| false).is_err());
    assert!(child.join("file.txt").exists() && child.join("late.txt").exists());
    let second = plan(&mut client);
    let result = apply(&mut client, &second, &control, &|| false).unwrap();
    assert!(
        matches!(result, crate::sftp_window::worker::Reply::Changed { note, .. }
        if note == "Deleted 4 of 4 reviewed entries")
    );
    assert!(!selected.exists() && retained.exists());
    assert!(apply(&mut client, &second, &control, &|| false).is_err());
}

#[cfg(unix)]
#[test]
fn recursive_local_deletion_reviews_links_and_special_entries_without_following_them() {
    use std::os::unix::fs::{FileTypeExt as _, symlink};
    use std::os::unix::net::UnixListener;
    use std::process::Command;

    use super::super::recursive_delete::{apply, review};

    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("chosen");
    let outside = root.path().join("retained");
    let link = selected.join("directory-link");
    let socket_path = selected.join("socket");
    let fifo = root.path().join("selected-fifo");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), b"retained").unwrap();
    symlink("../retained", &link).unwrap();
    symlink("missing", selected.join("broken-link")).unwrap();
    let socket = UnixListener::bind(&socket_path).unwrap();
    assert!(
        Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let listing = files::list_local(root.path()).unwrap();
    let entries = listing
        .entries
        .into_iter()
        .filter(|entry| matches!(entry.display.as_str(), "chosen" | "selected-fifo"))
        .collect::<Vec<_>>();
    assert!(entries.iter().all(Entry::batch_selectable));
    let mut client = client(|_| {});
    let control = Arc::new(Control::default());
    let plan = |client: &mut Client| {
        review(
            client,
            listing.path.clone(),
            &entries,
            Arc::downgrade(&control),
            &|| false,
        )
        .unwrap()
    };
    let first = plan(&mut client);
    assert_eq!(
        first.paths(),
        [
            "chosen",
            "chosen/broken-link",
            "chosen/directory-link",
            "chosen/socket",
            "selected-fifo"
        ]
    );
    std::fs::rename(&link, outside.join("old-link")).unwrap();
    symlink("../retained", &link).unwrap();
    assert!(apply(&mut client, &first, &control, &|| false).is_err());
    assert!(
        std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert!(
        std::fs::symlink_metadata(&socket_path)
            .unwrap()
            .file_type()
            .is_socket()
    );
    let second = plan(&mut client);
    let result = apply(&mut client, &second, &control, &|| false).unwrap();
    assert!(matches!(
        result, crate::sftp_window::worker::Reply::Changed { note, .. }
        if note == "Deleted 5 of 5 reviewed entries"
    ));
    drop(socket);
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"retained");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn recursive_local_deletion_windows_reviews_junctions_and_preserves_the_targets() {
    use std::os::windows::fs::symlink_file;
    use std::process::Command;

    use super::super::recursive_delete::{apply, review};

    let junction = |path: &Path, target: &Path| {
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "New-Item -ItemType Junction -Path $env:KORTTY_FIXTURE_JUNCTION -Target $env:KORTTY_FIXTURE_TARGET -ErrorAction Stop | Out-Null"])
            .env("KORTTY_FIXTURE_JUNCTION", path)
            .env("KORTTY_FIXTURE_TARGET", target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("chosen");
    let outside = root.path().join("retained");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), b"retained").unwrap();
    let link = selected.join("junction");
    let direct_link = root.path().join("selected-junction");
    junction(&link, &outside);
    junction(&direct_link, &outside);
    let symlink = selected.join("file-link");
    let has_symlink = match symlink_file(outside.join("file"), &symlink) {
        Ok(()) => true,
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!("Windows symbolic link creation is unavailable in this test account");
            false
        }
        Err(error) => panic!("Could not create the symbolic link fixture: {error}"),
    };

    let listing = files::list_local(root.path()).unwrap();
    let entries = listing
        .entries
        .into_iter()
        .filter(|entry| matches!(entry.display.as_str(), "chosen" | "selected-junction"))
        .collect::<Vec<_>>();
    assert!(entries.iter().any(|entry| entry.kind == EntryKind::Link));
    let mut client = client(|_| {});
    let control = Arc::new(Control::default());
    let plan = |client: &mut Client| {
        review(
            client,
            listing.path.clone(),
            &entries,
            Arc::downgrade(&control),
            &|| false,
        )
        .unwrap()
    };
    let first = plan(&mut client);
    let mut expected = vec!["chosen", "chosen/junction", "selected-junction"];
    if has_symlink {
        expected.insert(1, "chosen/file-link");
    }
    assert_eq!(first.paths(), expected);
    std::fs::rename(&link, outside.join("old-junction")).unwrap();
    junction(&link, &outside);
    assert!(apply(&mut client, &first, &control, &|| false).is_err());
    assert!(link.exists() && direct_link.exists());
    let second = plan(&mut client);
    let result = apply(&mut client, &second, &control, &|| false).unwrap();
    assert!(
        matches!(
            &result, crate::sftp_window::worker::Reply::Changed { note, .. }
            if *note == format!("Deleted {} of {} reviewed entries", expected.len(), expected.len())
        ),
        "{result:?}"
    );
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"retained");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

fn interrupt(target: &Path) -> Offer {
    let mut first = client(|bytes| {
        attributes(bytes, 1, 24);
        attributes(bytes, 2, 24);
        string(bytes, HANDLE, 3, b"source");
        attributes(bytes, 4, 24);
        attributes(bytes, 5, 24);
        string(bytes, DATA, 6, b"first");
        // The channel closes before the server replies to the next READ.
    });
    let source = b"/remote/file.txt";
    let transfer = Transfer {
        source: Location::Remote(source.to_vec()),
        target_directory: Location::Local(target.parent().unwrap().to_path_buf()),
        name: EntryName::Remote(b"file.txt".to_vec()),
        title: "file.txt".into(),
        kind: EntryKind::File,
        size: Some(10),
    };
    let error = download(
        &mut first,
        &transfer,
        source,
        target,
        &Progress::new(transfer.size),
        &peer(HOST),
    )
    .unwrap_err();
    assert!(error.disconnected);
    error
        .partial
        .expect("kept a verified nonempty private prefix")
}

fn route(username: &str) -> SshTarget {
    SshTarget::from_imported_route(
        LoginTarget {
            host: "sftp.example.test".into(),
            port: 22,
            username: username.into(),
        },
        None,
        IsolationLevel::None,
    )
    .unwrap()
}

#[test]
fn explicit_download_cancellation_keeps_the_prefix_only_when_both_settings_allow_it() {
    struct CancelAfterPrefix {
        cursor: Cursor<Vec<u8>>,
        boundary: u64,
        cancel: crate::sftp_window::worker::sync::Cancel,
    }

    impl Read for CancelAfterPrefix {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.cursor.position() >= self.boundary {
                self.cancel.stop_local();
            }
            self.cursor.read(bytes)
        }
    }

    for (enabled, keep, expect_partial) in [
        (true, true, true),
        (true, false, false),
        (false, true, false),
    ] {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let target = root.join("file.txt");
        let source = b"/remote/file.txt";
        let transfer = Transfer {
            source: Location::Remote(source.to_vec()),
            target_directory: Location::Local(root.clone()),
            name: EntryName::Remote(b"file.txt".to_vec()),
            title: "file.txt".into(),
            kind: EntryKind::File,
            size: Some(10),
        };
        let progress = Progress::new(transfer.size);
        let control = peer(HOST);
        *control.resume_preferences.lock().unwrap() = ResumePreferences {
            enabled,
            keep_on_cancel: keep,
        };
        let mut bytes = Vec::new();
        packet(&mut bytes, 2, 3, &[]);
        attributes(&mut bytes, 1, 24);
        attributes(&mut bytes, 2, 24);
        string(&mut bytes, HANDLE, 3, b"source");
        attributes(&mut bytes, 4, 24);
        attributes(&mut bytes, 5, 24);
        string(&mut bytes, DATA, 6, b"first");
        let boundary = bytes.len() as u64;
        string(&mut bytes, DATA, 7, b" last");
        status(&mut bytes, 8, 0);
        let reader = CancelAfterPrefix {
            cursor: Cursor::new(bytes),
            boundary,
            cancel: progress.cancel.clone(),
        };
        let mut client = Sftp::new(
            Box::new(reader) as Box<dyn Read + Send>,
            Box::new(io::sink()) as Box<dyn Write + Send>,
        )
        .unwrap();
        let error =
            download(&mut client, &transfer, source, &target, &progress, &control).unwrap_err();
        assert!(!error.disconnected);
        assert_eq!(
            error.partial.as_ref().map(|offer| offer.offset),
            expect_partial.then_some(5)
        );
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            usize::from(expect_partial)
        );
        assert!(!target.exists());
    }
}

#[test]
fn disabling_sftp_resume_releases_the_partial_and_changes_a_waiting_retry_to_fresh_copy() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("file.txt");
    let mut window = SftpWindow::new(route("user"), None, true, RemoteEditorPolicy::Deny);
    let transfer = Transfer {
        source: Location::Remote(b"/remote/file.txt".to_vec()),
        target_directory: Location::Local(root.clone()),
        name: EntryName::Remote(b"file.txt".to_vec()),
        title: "file.txt".into(),
        kind: EntryKind::File,
        size: Some(10),
    };
    use crate::sftp_window::{
        ConflictDefault,
        workbench::{Outcome, Pending},
    };
    window
        .workbench
        .append([transfer].into(), ConflictDefault::Ask);
    let (row, _) = window.workbench.next_at(1).unwrap();
    window.workbench.finish(
        row,
        Outcome::Failed {
            message: "Interrupted".into(),
            partial: true,
        },
    );
    window.partial = Some(interrupt(&target));
    window.partial_row = Some(row);
    let offers = window.retry_offers();
    assert!(window.workbench.retry(row, &offers, ConflictDefault::Ask));
    assert!(window.workbench.waiting_to_resume(row));
    drop(offers);
    window.configure_resume_preferences(ResumePreferences {
        enabled: false,
        keep_on_cancel: true,
    });
    assert!(window.partial.is_none());
    assert!(!window.workbench.waiting_to_resume(row));
    window.workbench.set_paused(false);
    assert!(matches!(window.workbench.next_at(1), Some((id, Pending::Copy(_))) if id == row));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn sftp_download_can_resume_after_closing_the_window_for_the_same_authenticated_route() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let target = route("user");
    let mut window = SftpWindow::new(target.clone(), None, true, RemoteEditorPolicy::Deny);
    window.partial = Some(interrupt(&destination));
    let mut retained = window.retain_partial();
    drop(window);

    let control = peer(HOST);
    let key = control.host_key.as_ref();
    assert!(ClosedDownload::take_for(&mut retained, &route("other"), key).is_none());
    let partial = ClosedDownload::take_for(&mut retained, &target, key).unwrap();
    assert!(retained.is_none());
    assert!(partial.available());
    assert_eq!(partial.offset, 5);
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    let mut reconnected = client(|bytes| {
        attributes(bytes, 1, 24);
        string(bytes, HANDLE, 2, b"source");
        attributes(bytes, 3, 24);
        attributes(bytes, 4, 24);
        string(bytes, DATA, 5, b"first");
        string(bytes, DATA, 6, b" last");
        status(bytes, 7, 1);
        attributes(bytes, 8, 24);
        status(bytes, 9, 0);
        attributes(bytes, 10, 24);
    });
    let progress = partial.progress();
    let result = partial
        .clone()
        .run(&mut reconnected, &progress, &control)
        .unwrap();
    assert!(matches!(result, Reply::Changed { .. }));
    assert!(!partial.available());
    assert_eq!(progress.label(), "10/10 bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"first last");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn sftp_closed_download_rejects_unverified_and_incognito_windows() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let target = route("user");
    let mut window = SftpWindow::new(target.clone(), None, true, RemoteEditorPolicy::Deny);
    window.partial = Some(interrupt(&destination));
    let mut retained = window.retain_partial();

    window.set_incognito(true);
    window.restore_partial(&mut retained);
    assert!(window.partial.is_none());
    assert!(retained.is_some());
    let rotated = peer(ROTATED);
    assert!(ClosedDownload::take_for(&mut retained, &target, rotated.host_key.as_ref()).is_none());
    assert!(retained.is_none());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

    window.partial = Some(interrupt(&destination));
    assert!(window.retain_partial().is_none());
    drop(window);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn sftp_resume_rejects_remote_changes_and_never_replaces_a_new_destination() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let offer = interrupt(&destination);
    let progress = offer.progress();
    let mut reconnected = client(|bytes| attributes(bytes, 1, 25));
    let error = offer
        .run(&mut reconnected, &progress, &peer(HOST))
        .unwrap_err();
    assert!(error.message.contains("changed"));
    assert!(error.partial.is_none());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

    let offer = interrupt(&destination);
    let progress = offer.progress();
    fs::write(&destination, b"unrelated new file").unwrap();
    let error = offer
        .run(&mut client(|_| {}), &progress, &peer(HOST))
        .unwrap_err();
    assert!(error.message.contains("already exists"));
    assert_eq!(fs::read(&destination).unwrap(), b"unrelated new file");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn sftp_resume_discards_the_partial_when_the_authenticated_peer_changes() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    for control in [peer(ROTATED), Control::default()] {
        let offer = interrupt(&destination);
        let progress = offer.progress();
        assert!(!offer.matches_peer(control.host_key.as_ref()));
        let error = offer
            .run(&mut client(|_| {}), &progress, &control)
            .unwrap_err();
        assert!(error.message.contains("host key changed"));
        assert!(!error.disconnected);
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
