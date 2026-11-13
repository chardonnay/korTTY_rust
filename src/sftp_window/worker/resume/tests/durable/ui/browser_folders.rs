//! A protocol test double verifies metadata scans without an SSH exec channel.

use crate::sftp_window::{Message as WindowMessage, Side, SortColumn, browser};

use super::*;

const ROOT: &[u8] = b"/remote/selected";
const FOLDER: u32 = 0o40700;
const FILE: u32 = 0o100600;

struct Responses<'a> {
    bytes: &'a mut Vec<u8>,
    id: u32,
}

impl Responses<'_> {
    fn attrs(&mut self, mode: u32, length: u64) {
        let mut body = 13u32.to_be_bytes().to_vec();
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&mode.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&8u32.to_be_bytes());
        packet(self.bytes, ATTRS, self.id, &body);
        self.id += 1;
    }

    fn checked(&mut self) {
        self.attrs(FOLDER, 4);
        let mut body = 1u32.to_be_bytes().to_vec();
        push(&mut body, ROOT);
        push(&mut body, b"");
        body.extend_from_slice(&0u32.to_be_bytes());
        packet(self.bytes, 104, self.id, &body);
        self.id += 1;
    }

    fn scan(&mut self, length: u64) {
        self.checked();
        self.attrs(FOLDER, 4);
        self.checked();
        string(self.bytes, HANDLE, self.id, b"folder");
        self.id += 1;
        let mut body = 2u32.to_be_bytes().to_vec();
        for (name, mode) in [(b"data.bin".as_slice(), FILE), (b"link", 0o120777)] {
            push(&mut body, name);
            push(&mut body, b"");
            body.extend_from_slice(&4u32.to_be_bytes());
            body.extend_from_slice(&mode.to_be_bytes());
        }
        packet(self.bytes, 104, self.id, &body);
        self.id += 1;
        status(self.bytes, self.id, 1);
        self.id += 1;
        status(self.bytes, self.id, 0);
        self.id += 1;
        self.checked();
        self.attrs(FILE, length);
        self.checked();
        self.attrs(0o120777, 7);
        self.checked();
        self.checked();
        self.attrs(FOLDER, 4);
        self.checked();
        self.attrs(FOLDER, 4);
    }
}

fn push(bytes: &mut Vec<u8>, text: &[u8]) {
    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
    bytes.extend_from_slice(text);
}

#[test]
fn sftp_folder_sizes_are_metadata_only_and_reject_changed_cancelled_and_stale_results() {
    for mode in 0..4 {
        let fixture = crate::snippets::tests::Profile::new();
        let context = context(&fixture.0.canonicalize().unwrap());
        super::upload::window(&context, |window, incoming, control| {
            let selected = EntryName::Remote(b"selected".to_vec());
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                vec![Entry {
                    name: selected.clone(),
                    display: "selected".into(),
                    hidden: false,
                    kind: EntryKind::Directory,
                    size: Some(4),
                    modified: Some(8),
                    owner: None,
                    group: None,
                    mode: Some(FOLDER),
                }],
            )));
            let _ = window.update(WindowMessage::Select(Side::Remote, selected.clone()));
            let _ = window.update(WindowMessage::MeasureFolders(Side::Remote));
            let ticket = window.busy.as_ref().unwrap().id;
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::MeasureFolders(_)));
            let reply = execute(
                &mut client(|bytes| {
                    let mut response = Responses { bytes, id: 1 };
                    response.attrs(FOLDER, 4);
                    response.scan(1024);
                    response.scan(if mode == 1 { 1025 } else { 1024 });
                }),
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            if mode == 2 {
                let _ = window.update(WindowMessage::CancelMeasurement);
            } else if mode == 3 {
                // Same path and entries, but a newer listing must invalidate the result.
                let current = window.remote.directory.as_ref().unwrap().as_ref().clone();
                window.remote.load(Arc::new(current));
            }
            let _ = window.update(WindowMessage::Done(ticket, reply));
            if mode == 0 {
                assert_eq!(window.remote.folder_sizes.get(&selected), Some(&1024));
                assert!(
                    window
                        .status()
                        .unwrap()
                        .contains("1 links or special entries skipped")
                );
                assert_eq!(window.remote.selected().unwrap().1.size, Some(4));
            } else {
                assert!(window.remote.folder_sizes.is_empty());
                if mode == 1 {
                    assert!(window.status().unwrap().contains("contents changed"));
                }
                if mode == 2 {
                    assert!(window.status().unwrap().contains("calculation cancelled"));
                }
            }
        });
    }
}

#[test]
fn sftp_local_folder_sizes_sort_without_changing_transfer_metadata_and_refresh_invalidates_them() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    fs::create_dir(root.join("a-small")).unwrap();
    fs::create_dir_all(root.join("b-large/nested")).unwrap();
    fs::write(root.join("a-small/file"), b"one").unwrap();
    fs::write(root.join("b-large/nested/file"), b"larger file").unwrap();
    super::upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        for name in ["a-small", "b-large"] {
            let _ = window.update(WindowMessage::ToggleSelection(
                Side::Local,
                EntryName::Local(name.into()),
                true,
            ));
        }
        let _ = window.update(WindowMessage::MeasureFolders(Side::Local));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        // Any remote request would fail: the fixture has only its version greeting.
        let reply = execute(
            &mut client(|_| {}),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        let _ = window.update(WindowMessage::Done(ticket, reply));
        assert_eq!(window.local.folder_sizes.len(), 2);
        let _ = window.update(WindowMessage::Sort(Side::Local, SortColumn::Size));
        assert_eq!(window.local.entries().next().unwrap().display, "a-small");
        let _ = window.update(WindowMessage::Sort(Side::Local, SortColumn::Size));
        assert_eq!(window.local.entries().next().unwrap().display, "b-large");
        assert!(
            window
                .local
                .entries()
                .filter(|entry| entry.kind == EntryKind::Directory)
                .all(|entry| entry.size.is_none())
        );
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        assert!(window.local.folder_sizes.is_empty());
    });
}

#[test]
fn sftp_breadcrumb_navigation_uses_raw_targets_and_rejects_old_listing_links() {
    let fixture = crate::snippets::tests::Profile::new();
    let context = context(&fixture.0.canonicalize().unwrap());
    super::upload::window(&context, |window, incoming, _| {
        let directory = Arc::new(Directory::sorted(
            Location::Remote(b"/remote/raw-\xff/child".to_vec()),
            Vec::new(),
        ));
        window.remote.load(Arc::clone(&directory));
        let navigation = browser::Navigation {
            source: Arc::clone(&directory),
            target: Location::Remote(b"/remote/raw-\xff".to_vec()),
        };
        let _ = window.update(WindowMessage::Breadcrumb(navigation.clone()));
        let command = incoming.try_recv().unwrap();
        assert!(
            matches!(command.operation, Operation::Browse(Location::Remote(path)) if path == b"/remote/raw-\xff")
        );
        window.busy = None;
        window.remote.load(Arc::new(directory.as_ref().clone()));
        let _ = window.update(WindowMessage::Breadcrumb(navigation));
        assert!(incoming.try_recv().is_err());
    });
}
