use std::cell::RefCell;

use super::*;

const PATH: &[u8] = b"/reviewed/foreign";
const HANDLE: &[u8] = b"foreign-handle";
const ORIGINAL: &[u8] = b"abcdef";
const REVIEWED: Attributes = Attributes {
    size: Some(6),
    uid: Some(2002),
    gid: Some(2003),
    permissions: Some(0o100660),
    accessed: Some(1),
    modified: Some(10),
};

fn attrs(replies: &mut Vec<u8>, id: u32, size: u64, modified: u32) {
    let mut fields =
        (wire::ATTR_SIZE | wire::ATTR_UID_GID | wire::ATTR_PERMISSIONS | wire::ATTR_TIMES)
            .to_be_bytes()
            .to_vec();
    fields.extend_from_slice(&size.to_be_bytes());
    for value in [
        REVIEWED.uid.unwrap(),
        REVIEWED.gid.unwrap(),
        REVIEWED.permissions.unwrap(),
        REVIEWED.accessed.unwrap(),
        modified,
    ] {
        fields.extend_from_slice(&value.to_be_bytes());
    }
    push_reply(replies, wire::ATTRS, id, &fields);
}

fn canonical(replies: &mut Vec<u8>, id: u32) {
    let mut fields = 1_u32.to_be_bytes().to_vec();
    push_entry(&mut fields, PATH, &0_u32.to_be_bytes());
    push_reply(replies, wire::NAME, id, &fields);
}

fn checked(replies: &mut Vec<u8>, id: u32, size: u64, modified: u32) {
    attrs(replies, id, size, modified);
    attrs(replies, id + 1, size, modified);
    canonical(replies, id + 2);
}

fn prefix() -> Vec<u8> {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    attrs(&mut replies, 1, 6, 10);
    canonical(&mut replies, 2);
    push_string_reply(&mut replies, wire::HANDLE, 3, HANDLE);
    checked(&mut replies, 4, 6, 10);
    push_string_reply(&mut replies, wire::DATA, 7, ORIGINAL);
    replies
}

fn upload(size: usize) -> InPlaceUpload {
    InPlaceUpload {
        original: REVIEWED,
        size: size as u64,
        modified: 12,
    }
}

#[test]
fn in_place_upload_uses_one_checked_handle_and_shrinks_after_acknowledged_writes() {
    for (replacement, preserve_modified) in [
        (b"new".as_slice(), false),
        (b"".as_slice(), true),
        (b"larger!".as_slice(), true),
    ] {
        let size = replacement.len() as u64;
        let mut replies = prefix();
        checked(&mut replies, 8, 6, 10);
        let mut id = 11;
        if size != 0 {
            push_status(&mut replies, id, 0, "");
            id += 1;
        }
        let after_write = if size == 0 { 10 } else { 11 };
        attrs(&mut replies, id, size.max(6), after_write);
        checked(&mut replies, id + 1, size.max(6), after_write);
        id += 4;
        if size < 6 {
            push_status(&mut replies, id, 0, "");
            id += 1;
        }
        attrs(&mut replies, id, size, 11);
        checked(&mut replies, id + 1, size, 11);
        id += 4;
        if size != 0 {
            push_string_reply(&mut replies, wire::DATA, id, replacement);
            id += 1;
        }
        checked(&mut replies, id, size, 11);
        push_status(
            &mut replies,
            id + 3,
            if preserve_modified { 0 } else { 3 },
            "",
        );
        let modified = if preserve_modified { 12 } else { 11 };
        checked(&mut replies, id + 4, size, modified);
        push_status(&mut replies, id + 7, 0, "");
        attrs(&mut replies, id + 8, size, modified);
        canonical(&mut replies, id + 9);

        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        let acknowledged = RefCell::new(Vec::new());
        let preserved = client
            .upload_in_place(
                PATH,
                upload(size as usize),
                Cursor::new(ORIGINAL),
                Cursor::new(replacement),
                || false,
                |offset| {
                    acknowledged.borrow_mut().push(offset);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(preserved, preserve_modified);
        let packets = requests(client.writer);
        let mut open = Decoder::new(&packets[3][5..]);
        assert_eq!(packets[3][0], wire::OPEN);
        assert_eq!(open.string().unwrap(), PATH);
        assert_eq!(open.u32().unwrap(), OPEN_READ | OPEN_WRITE);
        assert_eq!(open.u32().unwrap(), 0);
        open.finish().unwrap();
        let mut changes = Vec::new();
        for request in &packets[4..] {
            if matches!(
                request[0],
                wire::READ | wire::WRITE | wire::FSTAT | wire::FSETSTAT | wire::CLOSE
            ) {
                let mut fields = Decoder::new(&request[5..]);
                assert_eq!(fields.string().unwrap(), HANDLE);
                match request[0] {
                    wire::WRITE => {
                        changes.push(wire::WRITE);
                        assert_eq!(fields.u64().unwrap(), 0);
                        assert_eq!(fields.string().unwrap(), replacement);
                        fields.finish().unwrap();
                    }
                    wire::FSETSTAT => {
                        let flags = fields.u32().unwrap();
                        if flags == wire::ATTR_SIZE {
                            changes.push(wire::ATTR_SIZE as u8);
                            assert_eq!(fields.u64().unwrap(), size);
                        } else {
                            assert_eq!(flags, wire::ATTR_TIMES);
                            changes.push(wire::ATTR_TIMES as u8);
                            assert_eq!(fields.u32().unwrap(), 1);
                            assert_eq!(fields.u32().unwrap(), 12);
                        }
                        fields.finish().unwrap();
                    }
                    _ => {}
                }
            } else {
                assert!(matches!(request[0], wire::LSTAT | wire::REALPATH));
            }
        }
        let mut expected = Vec::new();
        let mut progress = Vec::new();
        if size != 0 {
            expected.push(wire::WRITE);
            progress.push(0);
        }
        if size < 6 {
            expected.push(wire::ATTR_SIZE as u8);
            progress.push(size);
        }
        expected.push(wire::ATTR_TIMES as u8);
        progress.push(size);
        assert_eq!(changes, expected);
        assert_eq!(acknowledged.into_inner(), progress);
    }
}

#[test]
fn in_place_upload_rejects_a_changed_original_before_any_mutation() {
    let mut replies = prefix();
    push_status(&mut replies, 8, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let result = client.upload_in_place(
        PATH,
        upload(3),
        Cursor::new(b"ABCDEF"),
        Cursor::new(b"new"),
        || false,
        |_| panic!("changed bytes must prevent mutation"),
    );
    assert!(matches!(result, Err(Error::SourceChanged)));
    let packets = requests(client.writer);
    assert_eq!(packets.last().unwrap()[0], wire::CLOSE);
    assert!(
        !packets
            .iter()
            .any(|request| matches!(request[0], wire::WRITE | wire::FSETSTAT))
    );
}

#[test]
fn in_place_upload_does_not_retry_a_write_with_a_lost_reply() {
    let mut replies = prefix();
    checked(&mut replies, 8, 6, 10);
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let result = client.upload_in_place(
        PATH,
        upload(3),
        Cursor::new(ORIGINAL),
        Cursor::new(b"new"),
        || false,
        |acknowledged| {
            assert_eq!(acknowledged, 0);
            Ok(())
        },
    );
    assert!(result.unwrap_err().connection_lost());
    let packets = requests(client.writer);
    assert_eq!(packets.last().unwrap()[0], wire::WRITE);
    assert_eq!(
        packets
            .iter()
            .filter(|request| request[0] == wire::WRITE)
            .count(),
        1
    );
    assert!(!packets.iter().any(|request| request[0] == wire::FSETSTAT));
}
