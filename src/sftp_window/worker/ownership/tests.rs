use std::cell::Cell;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use kortty_ssh::sftp::Sftp;

use super::*;
use crate::sftp_window::EntryKind;

const FOLDER: &[u8] = b"/review";
const FILE_MODE: u32 = 0o100640;
const DIR_MODE: u32 = 0o040700;
const OWNER: u32 = 1000;
const GROUP: u32 = 1001;
const NEW_GROUP: u32 = 1002;
const EXTENSION: &[u8] = b"lsetstat@openssh.com";

struct Peer {
    inbound: Vec<u8>,
    id: u32,
}

impl Peer {
    fn new(supports_ownership: bool) -> Self {
        let mut version = vec![2];
        version.extend(3u32.to_be_bytes());
        if supports_ownership {
            string(&mut version, EXTENSION);
            string(&mut version, b"1");
        }
        let mut inbound = Vec::new();
        packet(&mut inbound, &version);
        Self { inbound, id: 0 }
    }

    fn response(&mut self, kind: u8, bytes: &[u8]) {
        self.id += 1;
        let mut body = vec![kind];
        body.extend(self.id.to_be_bytes());
        body.extend(bytes);
        packet(&mut self.inbound, &body);
    }

    fn canonical(&mut self, path: &[u8]) {
        let mut canonical = 1u32.to_be_bytes().to_vec();
        string(&mut canonical, path);
        string(&mut canonical, b"");
        canonical.extend(0u32.to_be_bytes());
        self.response(104, &canonical);
    }

    fn entry(&mut self, path: &[u8], mode: u32, uid: u32, gid: u32) {
        self.canonical(path);
        let mut attributes = 15u32.to_be_bytes().to_vec();
        attributes.extend(9u64.to_be_bytes());
        for field in [uid, gid, mode, 0, 24] {
            attributes.extend(field.to_be_bytes());
        }
        self.response(105, &attributes);
    }

    fn status(&mut self, code: u32) {
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(&code.to_be_bytes());
        self.response(101, &bytes);
    }

    fn success(&mut self) {
        self.status(0);
    }

    fn empty_listing(&mut self, canonical: &[u8]) {
        self.canonical(canonical);
        let mut handle = Vec::new();
        string(&mut handle, b"folder");
        self.response(102, &handle);
        self.status(1);
        self.success();
    }

    fn connect(self) -> (Client, Capture) {
        let capture = Capture::default();
        let client = Sftp::new(
            Box::new(Cursor::new(self.inbound)) as _,
            Box::new(capture.clone()) as _,
        )
        .unwrap();
        (client, capture)
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn changes(&self) -> Vec<Vec<u8>> {
        let bytes = self.0.lock().unwrap();
        let mut remaining = bytes.as_slice();
        let mut result = Vec::new();
        while !remaining.is_empty() {
            let size = u32::from_be_bytes(remaining[..4].try_into().unwrap()) as usize;
            let (packet, rest) = remaining[4..].split_at(size);
            if packet[0] == 200 {
                result.push(packet[5..].to_vec());
            }
            remaining = rest;
        }
        result
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn packet(target: &mut Vec<u8>, body: &[u8]) {
    target.extend((body.len() as u32).to_be_bytes());
    target.extend(body);
}

fn string(target: &mut Vec<u8>, value: &[u8]) {
    packet(target, value);
}

fn metadata_change(path: &[u8], fields: &[u32]) -> Vec<u8> {
    let mut expected = Vec::new();
    string(&mut expected, EXTENSION);
    string(&mut expected, path);
    for field in fields {
        expected.extend(field.to_be_bytes());
    }
    expected
}

fn selected(name: &str) -> Entry {
    Entry {
        name: EntryName::Remote(name.as_bytes().to_vec()),
        display: name.into(),
        hidden: false,
        kind: EntryKind::File,
        size: Some(9),
        modified: Some(24),
        owner: Some(OWNER),
        group: Some(GROUP),
        mode: Some(FILE_MODE),
    }
}

#[test]
fn ownership_checks_every_selected_entry_before_the_first_write() {
    for (mode, gid) in [(FILE_MODE, NEW_GROUP), (0o120777, GROUP)] {
        let mut peer = Peer::new(true);
        peer.entry(FOLDER, DIR_MODE, OWNER, GROUP);
        peer.entry(b"/review/first", FILE_MODE, OWNER, GROUP);
        peer.entry(b"/review/second", mode, OWNER, gid);
        let (mut client, capture) = peer.connect();
        let entries = [selected("first"), selected("second")];
        let error = change(
            &mut client,
            FOLDER.to_vec(),
            &entries,
            None,
            Some(NEW_GROUP),
            || Ok(()),
        )
        .unwrap_err();
        assert!(error.message.contains("cannot be verified"));
        assert!(capture.changes().is_empty());
    }
}

#[test]
fn ownership_reports_confirmed_and_uncertain_entries_on_a_lost_write_reply() {
    let mut peer = Peer::new(true);
    peer.entry(FOLDER, DIR_MODE, OWNER, GROUP);
    for name in [b"/review/first".as_slice(), b"/review/second"] {
        peer.entry(name, FILE_MODE, OWNER, GROUP);
    }
    peer.entry(b"/review/first", FILE_MODE, OWNER, GROUP);
    peer.success();
    peer.entry(b"/review/first", FILE_MODE, OWNER, NEW_GROUP);
    peer.entry(b"/review/second", FILE_MODE, OWNER, GROUP);
    // The peer closes after receiving the second mutation, before its acknowledgement.
    let (mut client, capture) = peer.connect();
    let entries = [selected("first"), selected("second")];
    let error = change(
        &mut client,
        FOLDER.to_vec(),
        &entries,
        None,
        Some(NEW_GROUP),
        || Ok(()),
    )
    .unwrap_err();
    assert!(error.disconnected);
    assert!(error.message.contains("1 changed, 0 already matched"));
    assert!(
        error
            .message
            .contains("last attempted change could not be verified")
    );
    assert_eq!(capture.changes().len(), 2);
    assert_eq!(
        capture.changes()[0],
        metadata_change(b"/review/first", &[2, u32::MAX, NEW_GROUP])
    );
    assert!(
        timed_out(Err(error))
            .message
            .contains("1 changed, 0 already matched")
    );
}

#[test]
fn ownership_rechecks_cancellation_for_no_op_and_requires_the_extension() {
    let mut peer = Peer::new(true);
    peer.entry(FOLDER, DIR_MODE, OWNER, GROUP);
    peer.entry(b"/review/file", FILE_MODE, OWNER, GROUP);
    peer.entry(b"/review/file", FILE_MODE, OWNER, GROUP);
    let (mut client, capture) = peer.connect();
    let calls = Cell::new(0);
    let error = change(
        &mut client,
        FOLDER.to_vec(),
        &[selected("file")],
        None,
        Some(GROUP),
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 4 {
                return Err(io::Error::other("Test permission revoked").into());
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.message.contains("Test permission revoked"));
    assert!(capture.changes().is_empty());

    let (mut client, capture) = Peer::new(false).connect();
    let error = change(
        &mut client,
        FOLDER.to_vec(),
        &[selected("file")],
        Some(OWNER),
        None,
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("does not support no-follow ownership changes")
    );
    assert!(capture.changes().is_empty());
}

#[test]
fn ownership_does_not_confirm_a_write_when_the_server_readback_differs() {
    let mut peer = Peer::new(true);
    peer.entry(b"/review/file", FILE_MODE, OWNER, GROUP);
    peer.success();
    peer.entry(b"/review/file", FILE_MODE, OWNER, GROUP);
    let (mut client, capture) = peer.connect();
    let target = Target {
        path: b"/review/file".to_vec(),
        entry: &selected("file"),
    };
    let error = apply_target(
        &mut client,
        &target,
        Owners {
            uid: None,
            gid: Some(NEW_GROUP),
        },
        None,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.uncertain);
    assert_eq!(capture.changes().len(), 1);
}

#[test]
fn ownership_sets_mode_after_verified_chown_and_accepts_cleared_set_id_bits() {
    const PATH: &[u8] = b"/review/file";
    let mut peer = Peer::new(true);
    peer.entry(PATH, FILE_MODE | 0o6000, OWNER, GROUP);
    peer.success();
    peer.entry(PATH, FILE_MODE, OWNER, NEW_GROUP);
    peer.success();
    peer.entry(PATH, 0o100750, OWNER, NEW_GROUP);
    let (mut client, capture) = peer.connect();
    let mut entry = selected("file");
    entry.mode = Some(FILE_MODE | 0o6000);
    let target = Target {
        path: PATH.to_vec(),
        entry: &entry,
    };
    let result = apply_target(
        &mut client,
        &target,
        Owners {
            uid: Some(OWNER),
            gid: Some(NEW_GROUP),
        },
        Some(0o750),
        &|| Ok(()),
    );
    assert!(matches!(result, Ok(true)));
    assert_eq!(
        capture.changes(),
        [
            metadata_change(PATH, &[2, u32::MAX, NEW_GROUP]),
            metadata_change(PATH, &[4, 0o750]),
        ]
    );
}

#[test]
fn ownership_mode_only_clears_special_bits_and_skips_an_exact_match() {
    const PATH: &[u8] = b"/review/file";
    for extra in [0, 0o1000] {
        let mut peer = Peer::new(true);
        peer.entry(PATH, FILE_MODE | extra, OWNER, GROUP);
        if extra != 0 {
            peer.success();
            peer.entry(PATH, FILE_MODE, OWNER, GROUP);
        }
        let (mut client, capture) = peer.connect();
        let mut entry = selected("file");
        entry.mode = Some(FILE_MODE | extra);
        let target = Target {
            path: PATH.to_vec(),
            entry: &entry,
        };
        let result = apply_target(
            &mut client,
            &target,
            Owners {
                uid: None,
                gid: None,
            },
            Some(0o640),
            &|| Ok(()),
        );
        assert!(matches!(result, Ok(changed) if changed == (extra != 0)));
        if extra == 0 {
            assert!(capture.changes().is_empty());
        } else {
            assert_eq!(capture.changes(), [metadata_change(PATH, &[4, 0o640])]);
        }
    }
}

#[test]
fn ownership_mode_readback_requires_exact_bits_and_unchanged_ids() {
    const PATH: &[u8] = b"/review/file";
    for (after_mode, after_gid) in [(0o100750 | 0o2000, GROUP), (0o100750, NEW_GROUP)] {
        let mut peer = Peer::new(true);
        peer.entry(PATH, FILE_MODE, OWNER, GROUP);
        peer.success();
        peer.entry(PATH, after_mode, OWNER, after_gid);
        let (mut client, capture) = peer.connect();
        let target = Target {
            path: PATH.to_vec(),
            entry: &selected("file"),
        };
        let failure = apply_target(
            &mut client,
            &target,
            Owners {
                uid: None,
                gid: None,
            },
            Some(0o750),
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.uncertain);
        assert!(!failure.ownership_verified);
        assert!(failure.error.message.contains("requested permissions"));
        assert_eq!(capture.changes(), [metadata_change(PATH, &[4, 0o750])]);
    }
}

#[test]
fn ownership_reports_verified_chown_when_permissions_fail_or_lose_acknowledgement() {
    const PATH: &[u8] = b"/review/file";
    for denied in [true, false] {
        let mut peer = Peer::new(true);
        peer.entry(PATH, FILE_MODE, OWNER, GROUP);
        peer.success();
        peer.entry(PATH, FILE_MODE, OWNER, NEW_GROUP);
        if denied {
            peer.status(3);
            peer.empty_listing(FOLDER);
        }
        let (mut client, capture) = peer.connect();
        let target = Target {
            path: PATH.to_vec(),
            entry: &selected("file"),
        };
        let failure = apply_target(
            &mut client,
            &target,
            Owners {
                uid: None,
                gid: Some(NEW_GROUP),
            },
            Some(0o750),
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.ownership_verified);
        assert_eq!(failure.uncertain, !denied);
        let counts = Counts {
            uncertain: failure.uncertain,
            ownership_only: failure.ownership_verified,
            ..Counts::default()
        };
        let result = report_failure(
            &mut client,
            &Location::Remote(FOLDER.to_vec()),
            1,
            &counts,
            true,
            failure.error,
            &|| Ok(()),
        );
        let note = match result {
            Ok(Reply::Changed { note, .. }) if denied => note,
            Err(error) if !denied && error.disconnected => error.message,
            _ => panic!("expected the ownership result to survive the permission failure"),
        };
        assert!(note.contains("Ownership and permissions verified for 0 of 1 entries"));
        assert!(note.contains("Ownership was verified for the last entry"));
        assert_eq!(
            note.contains("last attempted change could not be verified"),
            !denied
        );
        assert_eq!(capture.changes().len(), 2);
    }
}

#[test]
fn ownership_rechecks_authority_between_chown_and_mode_write() {
    const PATH: &[u8] = b"/review/file";
    let mut peer = Peer::new(true);
    peer.entry(PATH, FILE_MODE, OWNER, GROUP);
    peer.success();
    peer.entry(PATH, FILE_MODE, OWNER, NEW_GROUP);
    let (mut client, capture) = peer.connect();
    let target = Target {
        path: PATH.to_vec(),
        entry: &selected("file"),
    };
    let calls = Cell::new(0);
    let failure = apply_target(
        &mut client,
        &target,
        Owners {
            uid: None,
            gid: Some(NEW_GROUP),
        },
        Some(0o750),
        &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 4 {
                return Err(io::Error::other("Test permission revoked").into());
            }
            Ok(())
        },
    )
    .err()
    .unwrap();
    assert!(failure.ownership_verified);
    assert!(!failure.uncertain);
    assert!(failure.error.message.contains("Test permission revoked"));
    assert_eq!(
        capture.changes(),
        [metadata_change(PATH, &[2, u32::MAX, NEW_GROUP])]
    );
}

#[test]
fn ownership_refresh_preserves_the_reviewed_directory_and_verified_counts() {
    const DRIVE: &[u8] = b"/C:/review";
    for (directory, refresh) in [
        (FOLDER, Some(FOLDER)),
        (DRIVE, Some(DRIVE)),
        (FOLDER, Some(b"/redirected".as_slice())),
        (FOLDER, None),
    ] {
        let mut peer = Peer::new(true);
        let path = files::remote_join(directory, b"file");
        peer.entry(directory, DIR_MODE, OWNER, GROUP);
        peer.entry(&path, FILE_MODE, OWNER, GROUP);
        peer.entry(&path, FILE_MODE, OWNER, GROUP);
        peer.success();
        peer.entry(&path, FILE_MODE, OWNER, NEW_GROUP);
        if let Some(canonical) = refresh {
            peer.empty_listing(canonical);
        }
        let (mut client, capture) = peer.connect();
        let result = change(
            &mut client,
            directory.to_vec(),
            &[selected("file")],
            None,
            Some(NEW_GROUP),
            || Ok(()),
        );
        assert_eq!(capture.changes().len(), 1);
        let Some(canonical) = refresh else {
            let error = result.unwrap_err();
            assert!(error.disconnected);
            assert!(error.message.contains("1 changed, 0 already matched"));
            continue;
        };
        let Reply::Changed {
            directory: refreshed,
            note,
        } = result.unwrap()
        else {
            panic!("expected a verified ownership result");
        };
        assert!(note.contains("1 changed, 0 already matched"));
        if canonical == directory {
            assert_eq!(
                refreshed.unwrap().path,
                Location::Remote(directory.to_vec())
            );
        } else {
            assert!(refreshed.is_none());
            assert!(note.contains("refresh refused because the server folder changed"));
        }
    }
}
