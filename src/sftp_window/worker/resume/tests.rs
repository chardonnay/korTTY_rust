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
