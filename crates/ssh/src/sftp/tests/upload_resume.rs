use super::*;

const OWNER: UploadOwner = UploadOwner {
    uid: 1000,
    gid: 1001,
};

fn attrs(replies: &mut Vec<u8>, id: u32, size: u64, uid: u32) {
    attrs_with_times(replies, id, size, uid, 0, 24);
}

fn attrs_with_times(
    replies: &mut Vec<u8>,
    id: u32,
    size: u64,
    uid: u32,
    accessed: u32,
    modified: u32,
) {
    upload_attrs(
        replies,
        id,
        size,
        UploadOwner {
            uid,
            gid: OWNER.gid,
        },
        0o600,
        accessed,
        modified,
    );
}

fn upload_attrs(
    replies: &mut Vec<u8>,
    id: u32,
    size: u64,
    owner: UploadOwner,
    mode: u32,
    accessed: u32,
    modified: u32,
) {
    let mut bytes = (1u32 | wire::ATTR_UID_GID | wire::ATTR_PERMISSIONS | wire::ATTR_TIMES)
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&owner.uid.to_be_bytes());
    bytes.extend_from_slice(&owner.gid.to_be_bytes());
    bytes.extend_from_slice(&(0o100000 | mode).to_be_bytes());
    bytes.extend_from_slice(&accessed.to_be_bytes());
    bytes.extend_from_slice(&modified.to_be_bytes());
    push_reply(replies, wire::ATTRS, id, &bytes);
}

#[test]
fn archive_metadata_uses_the_byte_verified_handle_and_keeps_the_last_verified_state() {
    const HANDLE: &[u8] = b"verified-archive";
    const NEW_OWNER: UploadOwner = UploadOwner {
        uid: 2000,
        gid: 2001,
    };
    for allow_mode in [true, false] {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[]);
        push_string_reply(&mut replies, wire::HANDLE, 1, HANDLE);
        for id in [2, 3, 5, 6] {
            if id == 5 {
                push_string_reply(&mut replies, wire::DATA, 4, b"abcdef");
            }
            attrs(&mut replies, id, 6, OWNER.uid);
        }
        push_status(&mut replies, 7, 0, "");
        for id in 8..=9 {
            upload_attrs(&mut replies, id, 6, NEW_OWNER, 0o600, 13, 24);
        }
        if allow_mode {
            push_status(&mut replies, 10, 0, "");
            for id in 11..=12 {
                upload_attrs(&mut replies, id, 6, NEW_OWNER, 0, 13, 24);
            }
        }
        push_status(&mut replies, if allow_mode { 13 } else { 10 }, 0, "");
        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        let mut verified = archive_stage_attributes();
        let permissions_checked = std::cell::Cell::new(0);
        let result = client.set_private_upload_metadata(
            b"/private/archive",
            &mut verified,
            Cursor::new(b"abcdef"),
            UploadMetadata {
                uid: Some(NEW_OWNER.uid),
                gid: Some(NEW_OWNER.gid),
                mode: Some(0),
            },
            || false,
            || {
                let count = permissions_checked.get() + 1;
                permissions_checked.set(count);
                if count == 2 && !allow_mode {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            },
        );
        assert_eq!(permissions_checked.get(), 2);
        if allow_mode {
            result.unwrap();
        } else {
            assert!(
                matches!(result, Err(Error::LocalIo(error)) if error.kind() == io::ErrorKind::PermissionDenied)
            );
        }
        assert_eq!(
            verified,
            Attributes {
                uid: Some(NEW_OWNER.uid),
                gid: Some(NEW_OWNER.gid),
                permissions: Some(if allow_mode { 0o100000 } else { 0o100600 }),
                ..archive_stage_attributes()
            }
        );
        let sent = requests(client.writer);
        let mut read = Decoder::new(&sent[4]);
        assert_eq!(read.byte().unwrap(), wire::READ);
        assert_eq!(read.u32().unwrap(), 4);
        assert_eq!(read.string().unwrap(), HANDLE);
        let mut set_owner = Decoder::new(&sent[7]);
        assert_eq!(set_owner.byte().unwrap(), wire::FSETSTAT);
        assert_eq!(set_owner.u32().unwrap(), 7);
        assert_eq!(set_owner.string().unwrap(), HANDLE);
        assert_eq!(set_owner.u32().unwrap(), wire::ATTR_UID_GID);
        assert_eq!(set_owner.u32().unwrap(), NEW_OWNER.uid);
        assert_eq!(set_owner.u32().unwrap(), NEW_OWNER.gid);
        set_owner.finish().unwrap();
        if allow_mode {
            let mut set_mode = Decoder::new(&sent[10]);
            assert_eq!(set_mode.byte().unwrap(), wire::FSETSTAT);
            assert_eq!(set_mode.u32().unwrap(), 10);
            assert_eq!(set_mode.string().unwrap(), HANDLE);
            assert_eq!(set_mode.u32().unwrap(), wire::ATTR_PERMISSIONS);
            assert_eq!(set_mode.u32().unwrap(), 0);
            set_mode.finish().unwrap();
        }
        assert!(!sent.iter().any(|packet| packet[0] == wire::SETSTAT));
        assert_eq!(sent.last().unwrap()[0], wire::CLOSE);
    }
}

#[test]
fn archive_metadata_rejects_changed_bytes_before_attempting_a_write() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_string_reply(&mut replies, wire::HANDLE, 1, b"changed-archive");
    attrs(&mut replies, 2, 6, OWNER.uid);
    attrs(&mut replies, 3, 6, OWNER.uid);
    push_string_reply(&mut replies, wire::DATA, 4, b"ABCDEF");
    push_status(&mut replies, 5, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let mut verified = archive_stage_attributes();
    assert!(matches!(
        client.set_private_upload_metadata(
            b"/private/archive",
            &mut verified,
            Cursor::new(b"abcdef"),
            UploadMetadata {
                uid: None,
                gid: None,
                mode: Some(0o644)
            },
            || false,
            || panic!("A stale stage must not reach the write guard"),
        ),
        Err(Error::SourceChanged)
    ));
    assert_eq!(verified, archive_stage_attributes());
    let sent = requests(client.writer);
    assert!(!sent.iter().any(|packet| packet[0] == wire::FSETSTAT));
    assert_eq!(sent.last().unwrap()[0], wire::CLOSE);
}

fn archive_stage_attributes() -> Attributes {
    Attributes {
        size: Some(6),
        uid: Some(OWNER.uid),
        gid: Some(OWNER.gid),
        permissions: Some(0o100600),
        accessed: Some(0),
        modified: Some(24),
    }
}

#[test]
fn replacement_metadata_guard_runs_after_read_only_preflight_and_before_first_mutation() {
    const HANDLE: &[u8] = b"replacement-stage";
    const MODIFIED: u32 = 31;
    for preflight_completes in [false, true] {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[]);
        push_string_reply(&mut replies, wire::HANDLE, 1, HANDLE);
        attrs(&mut replies, 2, 6, OWNER.uid);
        attrs(&mut replies, 3, 6, OWNER.uid);
        push_string_reply(&mut replies, wire::DATA, 4, b"abcdef");
        if preflight_completes {
            attrs(&mut replies, 5, 6, OWNER.uid);
            attrs(&mut replies, 6, 6, OWNER.uid);
            push_status(&mut replies, 7, 0, "");
            attrs_with_times(&mut replies, 8, 6, OWNER.uid, 0, MODIFIED);
            attrs_with_times(&mut replies, 9, 6, OWNER.uid, 0, MODIFIED);
            push_status(&mut replies, 10, 0, "");
        }
        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        let mut verified = archive_stage_attributes();
        let mut guard_calls = 0;
        let result = client.finalize_private_upload(
            b"/private/replacement",
            &mut verified,
            Cursor::new(b"abcdef"),
            (
                UploadMetadata {
                    uid: None,
                    gid: None,
                    mode: Some(0o600),
                },
                Some(MODIFIED),
            ),
            || false,
            || {
                guard_calls += 1;
                Ok(())
            },
        );
        let sent = requests(client.writer);
        if preflight_completes {
            result.unwrap();
            assert_eq!(guard_calls, 1);
            assert_eq!(verified.modified, Some(MODIFIED));
            let mut mutation = Decoder::new(&sent[7]);
            assert_eq!(mutation.byte().unwrap(), wire::FSETSTAT);
            assert_eq!(mutation.u32().unwrap(), 7);
            assert_eq!(mutation.string().unwrap(), HANDLE);
            assert_eq!(mutation.u32().unwrap(), wire::ATTR_TIMES);
            assert_eq!(mutation.u32().unwrap(), 0);
            assert_eq!(mutation.u32().unwrap(), MODIFIED);
            mutation.finish().unwrap();
        } else {
            assert!(matches!(result, Err(Error::Transport(_))));
            assert_eq!(guard_calls, 0);
            assert_eq!(verified, archive_stage_attributes());
            assert!(!sent.iter().any(|packet| packet[0] == wire::FSETSTAT));
        }
    }
}

#[test]
fn reviewed_upload_sets_mtime_on_the_same_handle_and_rejects_an_unverified_result() {
    const MODIFIED: u32 = 1_700_000_001;
    for case in 0..4 {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[]);
        push_string_reply(&mut replies, wire::HANDLE, 1, b"exact-write-handle");
        for id in 2..=5 {
            attrs_with_times(&mut replies, id, 0, OWNER.uid, 13, 24);
        }
        push_status(&mut replies, 6, 0, "");
        for id in 7..=8 {
            attrs_with_times(&mut replies, id, 6, OWNER.uid, 13, 24);
        }
        push_status(&mut replies, 9, if case == 1 { 3 } else { 0 }, "");
        let close_id = match case {
            1 => 10,
            2 => {
                attrs_with_times(&mut replies, 10, 6, OWNER.uid, 13, 24);
                11
            }
            _ => {
                attrs_with_times(&mut replies, 10, 6, OWNER.uid, 13, MODIFIED);
                attrs_with_times(
                    &mut replies,
                    11,
                    6,
                    OWNER.uid + u32::from(case == 3),
                    13,
                    MODIFIED,
                );
                12
            }
        };
        push_status(&mut replies, close_id, 0, "");
        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        let result = client.upload_private_with_modified(
            b"/stage",
            6,
            MODIFIED,
            Cursor::new(b"abcdef"),
            |owner, offset| {
                assert_eq!((owner, offset), (OWNER, 0));
                Ok(())
            },
            || false,
        );
        match case {
            0 => assert_eq!(result.unwrap(), 6),
            1 => assert!(matches!(result, Err(Error::Status(_)))),
            _ => assert!(matches!(result, Err(Error::SourceChanged))),
        }
        let sent = requests(client.writer);
        assert!(!sent.iter().any(|packet| packet[0] == wire::SETSTAT));
        let mut set = Decoder::new(&sent[9]);
        assert_eq!(set.byte().unwrap(), wire::FSETSTAT);
        assert_eq!(set.u32().unwrap(), 9);
        assert_eq!(set.string().unwrap(), b"exact-write-handle");
        assert_eq!(set.u32().unwrap(), wire::ATTR_TIMES);
        assert_eq!(set.u32().unwrap(), 13);
        assert_eq!(set.u32().unwrap(), MODIFIED);
        set.finish().unwrap();
        assert_eq!(sent.last().unwrap()[0], wire::CLOSE);
    }
}

#[test]
fn durable_upload_uses_the_actual_verified_prefix_and_same_write_handle() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    attrs(&mut replies, 1, 4, OWNER.uid);
    push_string_reply(&mut replies, wire::HANDLE, 2, b"verified-stage");
    attrs(&mut replies, 3, 4, OWNER.uid);
    attrs(&mut replies, 4, 4, OWNER.uid);
    push_string_reply(&mut replies, wire::DATA, 5, b"abcd");
    attrs(&mut replies, 6, 4, OWNER.uid);
    attrs(&mut replies, 7, 4, OWNER.uid);
    push_status(&mut replies, 8, 0, "");
    attrs(&mut replies, 9, 6, OWNER.uid);
    attrs(&mut replies, 10, 6, OWNER.uid);
    push_status(&mut replies, 11, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let mut checked = None;
    assert_eq!(
        client
            .upload_private_verified(
                b"/stage",
                UploadStage::Resume(OWNER),
                6,
                Cursor::new(b"abcdef"),
                |owner, offset| {
                    checked = Some((owner, offset));
                    Ok(())
                },
                || false,
            )
            .unwrap(),
        6
    );
    assert_eq!(checked, Some((OWNER, 4)));
    let sent = requests(client.writer);
    let mut open = Decoder::new(&sent[2]);
    assert_eq!(open.byte().unwrap(), wire::OPEN);
    assert_eq!(open.u32().unwrap(), 2);
    assert_eq!(open.string().unwrap(), b"/stage");
    assert_eq!(open.u32().unwrap(), OPEN_READ | OPEN_WRITE);
    assert_eq!(open.u32().unwrap(), 0);
    open.finish().unwrap();
    let mut read = Decoder::new(&sent[5]);
    assert_eq!(read.byte().unwrap(), wire::READ);
    assert_eq!(read.u32().unwrap(), 5);
    assert_eq!(read.string().unwrap(), b"verified-stage");
    assert_eq!(read.u64().unwrap(), 0);
    assert_eq!(read.u32().unwrap(), 4);
    let mut write = Decoder::new(&sent[8]);
    assert_eq!(write.byte().unwrap(), wire::WRITE);
    assert_eq!(write.u32().unwrap(), 8);
    assert_eq!(write.string().unwrap(), b"verified-stage");
    assert_eq!(write.u64().unwrap(), 4);
    assert_eq!(write.string().unwrap(), b"ef");
    write.finish().unwrap();
}

#[test]
fn durable_upload_checks_owner_prefix_and_receipt_before_writing() {
    for wrong_owner in [false, true] {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[]);
        attrs(&mut replies, 1, 4, OWNER.uid + u32::from(wrong_owner));
        if !wrong_owner {
            push_string_reply(&mut replies, wire::HANDLE, 2, b"verified-stage");
            attrs(&mut replies, 3, 4, OWNER.uid);
            attrs(&mut replies, 4, 4, OWNER.uid);
            push_string_reply(&mut replies, wire::DATA, 5, b"xbcd");
            push_status(&mut replies, 6, 0, "");
        }
        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        assert!(matches!(
            client.upload_private_verified(
                b"/stage",
                UploadStage::Resume(OWNER),
                6,
                Cursor::new(b"abcdef"),
                |_, _| panic!("invalid prefix or owner must not advance the receipt"),
                || false,
            ),
            Err(Error::SourceChanged)
        ));
        let sent = requests(client.writer);
        assert!(!sent.iter().any(|packet| packet[0] == wire::WRITE));
        assert_eq!(
            sent.iter().any(|packet| packet[0] == wire::OPEN),
            !wrong_owner
        );
    }

    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_string_reply(&mut replies, wire::HANDLE, 1, b"exclusive-stage");
    for id in 2..=5 {
        attrs(&mut replies, id, 0, OWNER.uid);
    }
    push_status(&mut replies, 6, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(matches!(client.upload_private_verified(
        b"/stage", UploadStage::Create, 6, Cursor::new(b"abcdef"),
        |owner, offset| { assert_eq!((owner, offset), (OWNER, 0)); Err(io::ErrorKind::PermissionDenied.into()) }, || false,
    ), Err(Error::LocalIo(error)) if error.kind() == io::ErrorKind::PermissionDenied));
    let sent = requests(client.writer);
    assert!(!sent.iter().any(|packet| packet[0] == wire::WRITE));
    let mut open = Decoder::new(&sent[1]);
    assert_eq!(open.byte().unwrap(), wire::OPEN);
    assert_eq!(open.u32().unwrap(), 1);
    assert_eq!(open.string().unwrap(), b"/stage");
    assert_eq!(
        open.u32().unwrap(),
        OPEN_READ | OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE
    );
}
