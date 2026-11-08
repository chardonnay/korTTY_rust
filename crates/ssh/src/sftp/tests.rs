use std::io::Cursor;

use super::*;

#[test]
fn negotiates_v3_and_checks_the_exact_replace_extension_version() {
    let mut replies = Vec::new();
    push_version(&mut replies, 4, &[]);
    assert!(matches!(
        Sftp::new(Cursor::new(replies), Vec::new()),
        Err(Error::Version(4))
    ));

    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[(POSIX_RENAME, b"2")]);
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(!client.supports_atomic_replace());
    assert!(matches!(
        client.replace("part", "target"),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(requests(client.writer), vec![vec![1, 0, 0, 0, 3]]);

    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[(POSIX_RENAME, b"1")]);
    push_status(&mut replies, 1, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(client.supports_atomic_replace());
    client.replace("part", "target").unwrap();
    let requests = requests(client.writer);
    let mut fields = Decoder::new(&requests[1]);
    assert_eq!(fields.byte().unwrap(), wire::EXTENDED);
    assert_eq!(fields.u32().unwrap(), 1);
    assert_eq!(fields.string().unwrap(), POSIX_RENAME);
    assert_eq!(fields.string().unwrap(), b"part");
    assert_eq!(fields.string().unwrap(), b"target");
    fields.finish().unwrap();
}

#[test]
fn checks_the_exact_server_copy_and_owner_extension_versions() {
    let mut replies = Vec::new();
    push_version(
        &mut replies,
        3,
        &[(LSETSTAT, b"2"), (COPY_DATA, b"2"), (HARDLINK, b"2")],
    );
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(!client.supports_lsetstat());
    assert!(!client.supports_copy_data());
    assert!(!client.supports_hardlink());
    assert!(matches!(
        client.set_owner_no_follow("file", Some(12), None),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        client.copy_regular_new(b"source", Attributes::default(), b"stage", || false),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        client.hardlink("stage", "target"),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(requests(client.writer).len(), 1);
}

#[test]
fn no_follow_owner_encodes_the_omitted_id() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[(LSETSTAT, b"1")]);
    push_status(&mut replies, 1, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(matches!(
        client.set_owner_no_follow("file", Some(u32::MAX), None),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        client.set_owner_no_follow("file", None, None),
        Err(Error::InvalidInput(_))
    ));
    client.set_owner_no_follow("file", None, Some(17)).unwrap();
    let packets = requests(client.writer);
    assert_eq!(packets.len(), 2);
    let mut fields = Decoder::new(&packets[1]);
    assert_eq!(fields.byte().unwrap(), wire::EXTENDED);
    assert_eq!(fields.u32().unwrap(), 1);
    assert_eq!(fields.string().unwrap(), LSETSTAT);
    assert_eq!(fields.string().unwrap(), b"file");
    assert_eq!(fields.u32().unwrap(), wire::ATTR_UID_GID);
    assert_eq!(fields.u32().unwrap(), u32::MAX);
    assert_eq!(fields.u32().unwrap(), 17);
    fields.finish().unwrap();
}

#[test]
fn server_copy_uses_bounded_handle_requests_and_never_an_unbounded_zero_length() {
    for size in [0, COPY_CHUNK + 5] {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[(COPY_DATA, b"1")]);
        let mode = 0o100600u32;
        let mut attrs = (1u32 | wire::ATTR_PERMISSIONS).to_be_bytes().to_vec();
        attrs.extend_from_slice(&size.to_be_bytes());
        attrs.extend_from_slice(&mode.to_be_bytes());
        push_reply(&mut replies, wire::ATTRS, 1, &attrs);
        push_string_reply(&mut replies, wire::HANDLE, 2, b"read handle");
        push_reply(&mut replies, wire::ATTRS, 3, &attrs);
        push_reply(&mut replies, wire::ATTRS, 4, &attrs);
        push_string_reply(&mut replies, wire::HANDLE, 5, b"write handle");
        let chunks = if size == 0 {
            &[][..]
        } else {
            &[(0, COPY_CHUNK), (COPY_CHUNK, 5)][..]
        };
        let mut next_id = 6;
        for _ in chunks {
            push_status(&mut replies, next_id, 0, "");
            next_id += 1;
        }
        push_reply(&mut replies, wire::ATTRS, next_id, &attrs);
        push_status(&mut replies, next_id + 1, 0, "");
        push_reply(&mut replies, wire::ATTRS, next_id + 2, &attrs);
        push_status(&mut replies, next_id + 3, 0, "");
        push_reply(&mut replies, wire::ATTRS, next_id + 4, &attrs);

        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        let expected = Attributes {
            size: Some(size),
            permissions: Some(mode),
            ..Attributes::default()
        };
        assert_eq!(
            client
                .copy_regular_new(b"source", expected, b"stage", || false)
                .unwrap(),
            size
        );
        let packets = requests(client.writer);
        assert!(
            !packets
                .iter()
                .any(|packet| matches!(packet[0], wire::READ | wire::WRITE))
        );
        let copies = packets
            .iter()
            .filter(|packet| packet[0] == wire::EXTENDED)
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), chunks.len());
        for (packet, &(offset, count)) in copies.into_iter().zip(chunks) {
            let mut fields = Decoder::new(packet);
            assert_eq!(fields.byte().unwrap(), wire::EXTENDED);
            fields.u32().unwrap();
            assert_eq!(fields.string().unwrap(), COPY_DATA);
            assert_eq!(fields.string().unwrap(), b"read handle");
            assert_eq!(fields.u64().unwrap(), offset);
            assert_eq!(fields.u64().unwrap(), count);
            assert_eq!(fields.string().unwrap(), b"write handle");
            assert_eq!(fields.u64().unwrap(), offset);
            fields.finish().unwrap();
        }
    }
}

#[test]
fn refuses_oversized_frames_and_disables_a_desynchronized_channel() {
    let oversized = ((wire::MAX_PACKET as u32) + 1).to_be_bytes();
    assert!(matches!(
        Sftp::new(Cursor::new(oversized), Vec::new()),
        Err(Error::Protocol(_))
    ));

    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_reply(&mut replies, wire::ATTRS, 22, &[0; 4]);
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let error = client.stat("target").unwrap_err();
    assert!(matches!(error, Error::Protocol(_)));
    assert!(error.connection_lost());
    assert!(matches!(client.stat("again"), Err(Error::Closed)));
    assert_eq!(requests(client.writer).len(), 2);
}

#[test]
fn returns_remote_errors_without_losing_synchronization() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_status(&mut replies, 1, 2, "missing\x1b[31m\n");
    push_status(&mut replies, 2, 3, "permission denied");
    push_reply(&mut replies, wire::ATTRS, 3, &[0; 4]);
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let missing = client.stat("missing").unwrap_err();
    assert!(missing.is_not_found());
    assert!(!missing.connection_lost());
    assert!(missing.to_string().contains("missing�[31m�"));
    let denied = client.stat("private").unwrap_err();
    assert!(matches!(
        denied,
        Error::Status(RemoteStatus {
            code: StatusCode::PermissionDenied,
            ..
        })
    ));
    assert!(!denied.connection_lost());
    assert_eq!(client.lstat("known").unwrap(), Attributes::default());
}

#[test]
fn decodes_directory_metadata_and_preserves_filename_bytes() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_string_reply(&mut replies, wire::HANDLE, 1, b"dir");
    let mut fields = 3u32.to_be_bytes().to_vec();
    push_entry(&mut fields, b".", &[0; 4]);
    push_entry(&mut fields, b"..", &[0; 4]);
    let mut attrs = 0x8000_000fu32.to_be_bytes().to_vec();
    attrs.extend_from_slice(&0x1000_0001u64.to_be_bytes());
    for value in [1001u32, 1002, 0o120777, 10, 20, 1] {
        attrs.extend_from_slice(&value.to_be_bytes());
    }
    wire::string(&mut attrs, b"vendor-attr").unwrap();
    wire::string(&mut attrs, b"ignored").unwrap();
    push_entry(&mut fields, b"link-\xff", &attrs);
    push_reply(&mut replies, wire::NAME, 2, &fields);
    push_status(&mut replies, 3, 1, "EOF");
    push_status(&mut replies, 4, 0, "");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();

    let entries = client.read_dir(".").unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.name, b"link-\xff");
    assert_eq!(entry.attrs.kind(), FileKind::Symlink);
    assert_eq!(entry.attrs.size, Some(0x1000_0001));
    assert_eq!(entry.attrs.uid, Some(1001));
    assert_eq!(entry.attrs.gid, Some(1002));
    assert_eq!(entry.attrs.accessed, Some(10));
    assert_eq!(entry.attrs.modified, Some(20));
    let requests = requests(client.writer);
    assert_eq!(requests[4][0], wire::CLOSE);
}

#[test]
fn rejects_non_leaf_names_and_truncated_entry_counts() {
    for (name, count) in [(b"../escape".as_slice(), 1u32), (b"valid", u32::MAX)] {
        let mut replies = Vec::new();
        push_version(&mut replies, 3, &[]);
        push_string_reply(&mut replies, wire::HANDLE, 1, b"dir");
        let mut fields = count.to_be_bytes().to_vec();
        push_entry(&mut fields, name, &[0; 4]);
        push_reply(&mut replies, wire::NAME, 2, &fields);
        let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
        assert!(matches!(client.read_dir("."), Err(Error::Protocol(_))));
        assert!(matches!(client.stat("."), Err(Error::Closed)));
    }
}

#[test]
fn advances_download_by_actual_byte_count_and_checks_remote_close() {
    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_string_reply(&mut replies, wire::HANDLE, 1, b"read");
    push_string_reply(&mut replies, wire::DATA, 2, b"abc");
    push_string_reply(&mut replies, wire::DATA, 3, b"de");
    push_status(&mut replies, 4, 1, "EOF");
    push_status(&mut replies, 5, 4, "close failed");
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    let mut local = Vec::new();
    assert!(matches!(
        client.download("data", &mut local),
        Err(Error::Status(_))
    ));
    assert_eq!(local, b"abcde");

    let requests = requests(client.writer);
    for (request, expected_offset) in requests[2..5].iter().zip([0u64, 3, 5]) {
        let mut fields = Decoder::new(request);
        assert_eq!(fields.byte().unwrap(), wire::READ);
        fields.u32().unwrap();
        assert_eq!(fields.string().unwrap(), b"read");
        assert_eq!(fields.u64().unwrap(), expected_offset);
        assert_eq!(fields.u32().unwrap(), CHUNK as u32);
        fields.finish().unwrap();
    }
    assert_eq!(requests[5][0], wire::CLOSE);
}

#[test]
fn uploads_exclusively_and_closes_the_handle_after_a_local_failure() {
    // Test double for a local file whose next read fails.
    struct FailingSource;
    impl Read for FailingSource {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("test source failed"))
        }
    }

    let mut replies = Vec::new();
    push_version(&mut replies, 3, &[]);
    push_string_reply(&mut replies, wire::HANDLE, 1, b"write");
    push_status(&mut replies, 2, 0, "");
    push_reply(&mut replies, wire::ATTRS, 3, &[0; 4]);
    let mut client = Sftp::new(Cursor::new(replies), Vec::new()).unwrap();
    assert!(matches!(
        client.upload_new("part", FailingSource),
        Err(Error::LocalIo(_))
    ));
    client.stat(".").unwrap();
    let requests = requests(client.writer);
    let mut fields = Decoder::new(&requests[1]);
    assert_eq!(fields.byte().unwrap(), wire::OPEN);
    assert_eq!(fields.u32().unwrap(), 1);
    assert_eq!(fields.string().unwrap(), b"part");
    assert_eq!(
        fields.u32().unwrap(),
        OPEN_CREATE | OPEN_EXCLUSIVE | OPEN_WRITE
    );
    assert_eq!(fields.u32().unwrap(), wire::ATTR_PERMISSIONS);
    assert_eq!(fields.u32().unwrap(), 0o600);
    fields.finish().unwrap();
    assert_eq!(requests[2][0], wire::CLOSE);
    assert_eq!(requests[3][0], wire::STAT);
}

// Scripted protocol peer used only by the focused wire tests.
fn push_version(replies: &mut Vec<u8>, version: u32, extensions: &[(&[u8], &[u8])]) {
    let mut packet = vec![wire::VERSION];
    packet.extend_from_slice(&version.to_be_bytes());
    for (name, data) in extensions {
        wire::string(&mut packet, name).unwrap();
        wire::string(&mut packet, data).unwrap();
    }
    wire::send(replies, &packet).unwrap();
}

fn push_reply(replies: &mut Vec<u8>, kind: u8, id: u32, fields: &[u8]) {
    let mut packet = vec![kind];
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(fields);
    wire::send(replies, &packet).unwrap();
}

fn push_string_reply(replies: &mut Vec<u8>, kind: u8, id: u32, value: &[u8]) {
    let mut fields = Vec::new();
    wire::string(&mut fields, value).unwrap();
    push_reply(replies, kind, id, &fields);
}

fn push_status(replies: &mut Vec<u8>, id: u32, status: u32, message: &str) {
    let mut fields = status.to_be_bytes().to_vec();
    wire::string(&mut fields, message.as_bytes()).unwrap();
    wire::string(&mut fields, b"").unwrap();
    push_reply(replies, wire::STATUS, id, &fields);
}

fn push_entry(fields: &mut Vec<u8>, name: &[u8], attrs: &[u8]) {
    wire::string(fields, name).unwrap();
    wire::string(fields, b"display only").unwrap();
    fields.extend_from_slice(attrs);
}

fn requests(writes: Vec<u8>) -> Vec<Vec<u8>> {
    let end = writes.len() as u64;
    let mut cursor = Cursor::new(writes);
    let mut packets = Vec::new();
    while cursor.position() < end {
        packets.push(wire::receive(&mut cursor).unwrap());
    }
    packets
}
