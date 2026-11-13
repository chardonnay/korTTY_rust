use crate::sftp_window::{Message as WindowMessage, Side, search};

use super::*;

const RAW_FILE: &[u8] = b"result-\xff.log";
const ROOT: &[u8] = b"/remote";
const NESTED: &[u8] = b"/remote/nested";
const FOLDER: u32 = 0o40700;
const FILE: u32 = 0o100600;

fn push_string(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value);
}

fn name(bytes: &mut Vec<u8>, id: u32, entries: &[(&[u8], u32)]) {
    let mut body = (entries.len() as u32).to_be_bytes().to_vec();
    for (name, mode) in entries {
        push_string(&mut body, name);
        push_string(&mut body, b"");
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(&mode.to_be_bytes());
    }
    packet(bytes, 104, id, &body);
}

fn realpath(bytes: &mut Vec<u8>, id: u32, path: &[u8]) {
    let mut body = 1u32.to_be_bytes().to_vec();
    push_string(&mut body, path);
    push_string(&mut body, b"");
    body.extend_from_slice(&0u32.to_be_bytes());
    packet(bytes, 104, id, &body);
}

fn attrs(bytes: &mut Vec<u8>, id: u32, mode: u32, modified: u32) {
    let mut body = 13u32.to_be_bytes().to_vec();
    body.extend_from_slice(&4u64.to_be_bytes());
    body.extend_from_slice(&mode.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&modified.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn checked(bytes: &mut Vec<u8>, id: u32, path: &[u8]) {
    attrs(bytes, id, FOLDER, 8);
    realpath(bytes, id + 1, path);
}

fn listing(bytes: &mut Vec<u8>, id: u32, entries: &[(&[u8], u32)]) {
    string(bytes, HANDLE, id, b"directory");
    name(bytes, id + 1, entries);
    status(bytes, id + 2, 1);
    status(bytes, id + 3, 0);
}

fn search_snapshot(bytes: &mut Vec<u8>) {
    checked(bytes, 1, ROOT);
    attrs(bytes, 3, FOLDER, 8);
    checked(bytes, 4, ROOT);
    listing(bytes, 6, &[(b".hidden", FOLDER), (b"nested", FOLDER)]);
    // The hidden subtree is excluded before any LSTAT or OPENDIR.
    checked(bytes, 10, ROOT);
    attrs(bytes, 12, FOLDER, 8);
    checked(bytes, 13, NESTED);
    listing(bytes, 15, &[(RAW_FILE, FILE)]);
    checked(bytes, 19, NESTED);
    attrs(bytes, 21, FILE, 8);
    checked(bytes, 22, NESTED);
    attrs(bytes, 24, FOLDER, 8);
    checked(bytes, 25, ROOT);
    checked(bytes, 27, ROOT);
    attrs(bytes, 29, FOLDER, 8);
}

fn reveal(bytes: &mut Vec<u8>, stale: bool) {
    checked(bytes, 1, ROOT);
    attrs(bytes, 3, FOLDER, 8);
    checked(bytes, 4, NESTED);
    attrs(bytes, 6, FOLDER, 8);
    attrs(bytes, 7, FILE, if stale { 9 } else { 8 });
    if stale {
        return;
    }
    realpath(bytes, 8, NESTED);
    listing(bytes, 9, &[(RAW_FILE, FILE)]);
    checked(bytes, 13, ROOT);
    attrs(bytes, 15, FOLDER, 8);
    checked(bytes, 16, NESTED);
    attrs(bytes, 18, FOLDER, 8);
    attrs(bytes, 19, FILE, 8);
}

#[derive(Clone, Copy)]
enum Outcome {
    Opened,
    Stale,
    Cancelled,
}

#[test]
fn sftp_recursive_search_navigates_exact_raw_hits_and_rejects_stale_or_cancelled_results() {
    for outcome in [Outcome::Opened, Outcome::Stale, Outcome::Cancelled] {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let context = context(&root);
        fs::write(root.join("selected.txt"), b"local").unwrap();
        super::upload::window(&context, |window, incoming, control| {
            window.revalidate_file_transfers(false);
            if matches!(outcome, Outcome::Opened) {
                let error = execute(
                    &mut client(|_| {}),
                    Operation::UploadLocal {
                        path: root.join("selected.txt"),
                        target: ROOT.to_vec(),
                        progress: Progress::new(None),
                    },
                    control,
                    RemoteEditorPolicy::Deny,
                )
                .unwrap_err();
                assert_eq!(error.message, "Application policy disables file transfer");
            }
            window
                .local
                .load(Arc::new(files::list_local(&root).unwrap()));
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(ROOT.to_vec()),
                Vec::new(),
            )));
            let _ = window.update(WindowMessage::Select(
                Side::Local,
                EntryName::Local("selected.txt".into()),
            ));
            let _ = window.update(WindowMessage::Hidden(Side::Remote));
            let _ = window.update(WindowMessage::Filter(Side::Remote, "*.LOG".into()));
            let _ = window.update(WindowMessage::SearchRemote);
            let setup_ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::Search(
                search::Message::Start(setup_ticket - 1),
            )));
            assert!(incoming.try_recv().is_err());
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::Search(
                search::Message::Start(setup_ticket),
            )));
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::SearchTree(_)));
            let search_ticket = window.busy.as_ref().unwrap().id;
            let reply = execute(
                &mut client(search_snapshot),
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            assert!(
                matches!(reply.as_deref(), Ok(Reply::Searched(_))),
                "{reply:?}"
            );
            if matches!(outcome, Outcome::Cancelled) {
                let _ = window.update(WindowMessage::CancelComparison);
            }
            let _ = window.update(WindowMessage::Done(search_ticket, reply));
            if matches!(outcome, Outcome::Cancelled) {
                assert!(window.comparison.is_none());
                assert!(window.status().unwrap().contains("Search cancelled"));
                return;
            }
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::Search(
                search::Message::Open(setup_ticket, 0),
            )));
            assert!(incoming.try_recv().is_err());
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::Search(
                search::Message::Open(search_ticket, 0),
            )));
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::RevealSearchHit(_)));
            let ticket = window.busy.as_ref().unwrap().id;
            let reply = execute(
                &mut client(|bytes| reveal(bytes, matches!(outcome, Outcome::Stale))),
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, reply));
            assert_eq!(window.local.selected().unwrap().1.display, "selected.txt");
            if matches!(outcome, Outcome::Stale) {
                assert!(window.status().unwrap().contains("search result changed"));
                assert_eq!(
                    window.remote.directory.as_ref().unwrap().path,
                    Location::Remote(ROOT.to_vec())
                );
            } else {
                let (directory, entry) = window.remote.selected().unwrap();
                assert_eq!(directory.path, Location::Remote(NESTED.to_vec()));
                assert_eq!(entry.name, EntryName::Remote(RAW_FILE.to_vec()));
            }
        });
    }
}
