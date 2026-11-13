use std::fs;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use kortty_ssh::sftp::Sftp;

use super::*;

#[test]
fn local_destination_creates_merges_and_preserves_collisions() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("selected");
    let created = LocalDestination::prepare(&target, false).unwrap();
    created.verify().unwrap();
    assert!(target.is_dir());
    fs::write(target.join("kept"), b"existing").unwrap();
    let merged = LocalDestination::prepare(&target, true).unwrap();
    assert!(merged.directory.same_as(&created.directory).unwrap());
    assert!(LocalDestination::prepare(&target, false).is_err());
    assert_eq!(fs::read(target.join("kept")).unwrap(), b"existing");
    let collision = root.join("file");
    fs::write(&collision, b"preserved").unwrap();
    assert!(LocalDestination::prepare(&collision, true).is_err());
    assert_eq!(fs::read(collision).unwrap(), b"preserved");
    assert!(LocalDestination::prepare(&root.join("SELECTED"), true).is_err());
}

#[cfg(unix)]
#[test]
fn local_merge_refuses_links_and_rechecks_parent_identity() {
    use std::os::unix::fs::symlink;
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir(root.join("outside")).unwrap();
    symlink(root.join("outside"), root.join("link")).unwrap();
    assert!(LocalDestination::prepare(&root.join("link"), true).is_err());
    fs::create_dir(root.join("parent")).unwrap();
    let target = LocalDestination::prepare(&root.join("parent/selected"), true).unwrap();
    fs::rename(root.join("parent"), root.join("moved")).unwrap();
    fs::create_dir_all(root.join("parent/selected")).unwrap();
    assert!(target.verify().is_err());
    assert!(root.join("moved/selected").is_dir());
}

#[test]
fn names_reject_aliases_and_enforce_both_listing_budgets() {
    let mut names = Names::default();
    names.insert(b"Report").unwrap();
    assert!(names.insert(b"report").is_err());
    assert!(names.require_exact_alias(b"REPORT").is_err());
    names.require_exact_alias(b"Report").unwrap();
    let mut names = Names::default();
    names.insert("é".as_bytes()).unwrap();
    assert!(names.insert("e\u{301}".as_bytes()).is_err());
    assert!(Names::default().insert(b"../escape").is_err());
    let mut names = Names::default();
    for index in 0..MAX_ENTRIES {
        names.insert(index.to_string().as_bytes()).unwrap();
    }
    assert!(names.insert(b"extra").is_err());
    let mut names = Names::default();
    names.insert(&vec![b'a'; MAX_LISTING_BYTES]).unwrap();
    assert!(names.insert(b"b").is_err());
}

#[test]
fn upload_creates_an_ordinary_folder_and_queues_only_direct_children() {
    let fixture = crate::snippets::tests::Profile::new();
    let source = fixture.0.canonicalize().unwrap();
    fs::write(source.join("file"), b"payload").unwrap();
    fs::create_dir(source.join("nested")).unwrap();
    fs::write(source.join("nested/unlisted"), b"not enumerated").unwrap();
    let mut peer = Peer::new();
    peer.upload_destination(false, false);
    let (mut client, capture) = peer.connect();
    let expansion = upload(
        &mut client,
        &source,
        b"/remote/selected",
        &Control::default(),
        false,
    )
    .unwrap();
    assert_eq!(expansion.children.len(), 2);
    assert_eq!(expansion.children[0].kind, EntryKind::File);
    assert_eq!(expansion.children[0].size, Some(7));
    assert_eq!(expansion.children[1].kind, EntryKind::Directory);
    assert_eq!(expansion.children[1].size, None);
    assert!(
        expansion
            .children
            .iter()
            .all(|child| child.target_directory == Location::Remote(b"/remote/selected".to_vec()))
    );
    let requests = capture.requests();
    assert_eq!(requests.iter().filter(|packet| packet[0] == 14).count(), 1);
    let mkdir = requests.iter().find(|packet| packet[0] == 14).unwrap();
    // SFTP MKDIR has an empty attribute record: the server supplies umask/default ACLs.
    assert_eq!(&mkdir[mkdir.len() - 4..], &0_u32.to_be_bytes());
    assert!(
        requests
            .iter()
            .all(|packet| !matches!(packet[0], 3 | 5 | 6))
    );
}

#[test]
fn upload_merges_a_verified_directory_that_arrives_during_mkdir() {
    let fixture = crate::snippets::tests::Profile::new();
    let source = fixture.0.canonicalize().unwrap();
    let mut peer = Peer::new();
    peer.upload_destination(false, true);
    let (mut client, _) = peer.connect();
    let expansion = upload(
        &mut client,
        &source,
        b"/remote/selected",
        &Control::default(),
        true,
    )
    .unwrap();
    assert!(expansion.children.is_empty());
    let mut peer = Peer::new();
    peer.upload_destination(false, true);
    let (mut client, capture) = peer.connect();
    assert!(
        upload(
            &mut client,
            &source,
            b"/remote/selected",
            &Control::default(),
            false
        )
        .is_err()
    );
    assert_eq!(capture.requests().last().unwrap()[0], 14);
}

#[test]
fn download_queues_regular_file_links_and_preserves_existing_contents() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("selected");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("kept"), b"preserved").unwrap();
    let mut peer = Peer::new();
    peer.directory(b"/remote/selected");
    peer.list(&[
        (b"file", 0o100640),
        (b"empty", 0o040755),
        (b"file-link", 0o120777),
        (b"directory-link", 0o120777),
        (b"broken-link", 0o120777),
        (b"loop", 0o120777),
        (b"special-link", 0o120777),
    ]);
    peer.attributes(0o100640);
    peer.attributes(0o040755);
    let canonical_file = b"/outside/bytes-\xff";
    peer.attributes(0o120777);
    peer.canonical(canonical_file);
    peer.attributes_with_size(0o100640, 23);
    peer.attributes_with_size(0o100640, 23);
    peer.canonical(canonical_file);
    peer.attributes(0o120777);
    peer.canonical(canonical_file);
    peer.attributes_with_size(0o100640, 23);
    peer.attributes(0o120777);
    peer.canonical(b"/outside/folder");
    peer.attributes(0o040755);
    peer.attributes(0o120777);
    peer.status(2);
    peer.attributes(0o120777);
    peer.status(4);
    peer.attributes(0o120777);
    peer.canonical(b"/outside/pipe");
    peer.attributes(0o010640);
    peer.directory(b"/remote/selected");
    let (mut client, capture) = peer.connect();
    let result = download(
        &mut client,
        b"/remote/selected",
        &target,
        &Control::default(),
        true,
    )
    .unwrap();
    assert_eq!(result.children.len(), 3);
    assert_eq!(result.skipped, 4);
    assert_eq!(
        result.children[0].source,
        Location::Remote(b"/remote/selected/file".to_vec())
    );
    assert_eq!(result.children[1].kind, EntryKind::Directory);
    let link = &result.children[2];
    assert_eq!(link.kind, EntryKind::File);
    assert_eq!(link.name, EntryName::Remote(b"file-link".to_vec()));
    assert_eq!(link.title, "file-link");
    assert_eq!(link.source, Location::Remote(canonical_file.to_vec()));
    assert_eq!(link.size, Some(23));
    assert!(
        result
            .children
            .iter()
            .all(|child| child.target_directory == Location::Local(target.clone()))
    );
    assert_eq!(fs::read(target.join("kept")).unwrap(), b"preserved");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
    let requests = capture.requests();
    assert!(
        requests
            .iter()
            .all(|packet| !matches!(packet[0], 3 | 5 | 6 | 14))
    );
    let opened: Vec<_> = requests.iter().filter(|packet| packet[0] == 11).collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(&opened[0][9..], b"/remote/selected");
}

#[cfg(unix)]
#[test]
fn upload_pins_file_link_target_and_preserves_its_original_name() {
    use std::os::unix::fs::symlink;

    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::create_dir(root.join("outside")).unwrap();
    let payload = root.join("outside/payload.dat");
    fs::write(&payload, b"file behind the link").unwrap();
    symlink("outside/payload.dat", root.join("chain")).unwrap();
    symlink("../chain", source.join("file-link")).unwrap();
    symlink("../outside", source.join("directory-link")).unwrap();
    symlink("missing", source.join("broken-link")).unwrap();
    symlink("loop", source.join("loop")).unwrap();

    let mut peer = Peer::new();
    peer.upload_destination(false, false);
    let (mut client, capture) = peer.connect();
    let result = upload(
        &mut client,
        &source,
        b"/remote/selected",
        &Control::default(),
        false,
    )
    .unwrap();
    assert_eq!(result.children.len(), 1);
    assert_eq!(result.skipped, 3);
    let link = &result.children[0];
    assert_eq!(link.kind, EntryKind::File);
    assert_eq!(link.name, EntryName::Local("file-link".into()));
    assert_eq!(link.title, "file-link");
    assert_eq!(link.source, Location::Local(payload));
    assert_eq!(
        link.target_directory,
        Location::Remote(b"/remote/selected".to_vec())
    );
    assert_eq!(link.size, Some(b"file behind the link".len() as u64));
    assert!(
        capture
            .requests()
            .iter()
            .all(|packet| !matches!(packet[0], 3 | 5 | 6))
    );
}

#[test]
fn download_rejects_a_changed_source_path_and_preserves_an_empty_public_folder() {
    let fixture = crate::snippets::tests::Profile::new();
    let target = fixture.0.canonicalize().unwrap().join("selected");
    let mut peer = Peer::new();
    peer.directory(b"/remote/selected");
    peer.list(&[]);
    peer.attributes(0o040755);
    peer.canonical(b"/elsewhere");
    let (mut client, _) = peer.connect();
    assert!(
        download(
            &mut client,
            b"/remote/selected",
            &target,
            &Control::default(),
            true
        )
        .is_err()
    );
    assert!(target.is_dir());
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

struct Peer {
    bytes: Vec<u8>,
    id: u32,
}

impl Peer {
    fn new() -> Self {
        let mut bytes = Vec::new();
        packet(&mut bytes, &[2, 0, 0, 0, 3]);
        Self { bytes, id: 0 }
    }

    fn response(&mut self, kind: u8, content: &[u8]) {
        self.id += 1;
        let mut body = vec![kind];
        body.extend(self.id.to_be_bytes());
        body.extend(content);
        packet(&mut self.bytes, &body);
    }

    fn status(&mut self, status: u32) {
        let mut body = status.to_be_bytes().to_vec();
        body.extend([0; 8]);
        self.response(101, &body);
    }

    fn attributes(&mut self, mode: u32) {
        self.response(105, &attributes(mode));
    }

    fn attributes_with_size(&mut self, mode: u32, size: u64) {
        let mut body = attributes(mode);
        body[4..12].copy_from_slice(&size.to_be_bytes());
        self.response(105, &body);
    }

    fn canonical(&mut self, path: &[u8]) {
        let mut body = 1_u32.to_be_bytes().to_vec();
        packet(&mut body, path);
        packet(&mut body, b"");
        body.extend(0_u32.to_be_bytes());
        self.response(104, &body);
    }

    fn directory(&mut self, path: &[u8]) {
        self.attributes(0o040755);
        self.canonical(path);
    }

    fn list(&mut self, entries: &[(&[u8], u32)]) {
        let mut handle = Vec::new();
        packet(&mut handle, b"directory");
        self.response(102, &handle);
        if !entries.is_empty() {
            let mut body = (entries.len() as u32).to_be_bytes().to_vec();
            for (name, mode) in entries {
                packet(&mut body, name);
                packet(&mut body, b"");
                body.extend(attributes(*mode));
            }
            self.response(104, &body);
        }
        self.status(1);
        self.status(0);
    }

    fn upload_destination(&mut self, existing: bool, raced: bool) {
        self.directory(b"/remote");
        self.list(&[]);
        if existing {
            self.attributes(0o040755);
        } else {
            self.status(2);
            self.status(if raced { 4 } else { 0 });
            if raced {
                self.attributes(0o040755);
            }
        }
        self.directory(b"/remote/selected");
        self.list(&[]);
        self.directory(b"/remote");
        self.directory(b"/remote/selected");
    }

    fn connect(self) -> (Client, Capture) {
        let capture = Capture::default();
        let client = Sftp::new(
            Box::new(Cursor::new(self.bytes)) as _,
            Box::new(capture.clone()) as _,
        )
        .unwrap();
        (client, capture)
    }
}

fn attributes(mode: u32) -> Vec<u8> {
    let mut body = 13_u32.to_be_bytes().to_vec();
    body.extend(7_u64.to_be_bytes());
    body.extend(mode.to_be_bytes());
    body.extend(0_u32.to_be_bytes());
    body.extend(24_u32.to_be_bytes());
    body
}

fn packet(target: &mut Vec<u8>, body: &[u8]) {
    target.extend((body.len() as u32).to_be_bytes());
    target.extend(body);
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn requests(&self) -> Vec<Vec<u8>> {
        let bytes = self.0.lock().unwrap();
        let mut rest = bytes.as_slice();
        let mut packets = Vec::new();
        while !rest.is_empty() {
            let count = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            packets.push(rest[4..4 + count].to_vec());
            rest = &rest[4 + count..];
        }
        packets
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
