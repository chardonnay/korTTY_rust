use crate::sftp_window::compare::Message as DiffMessage;
use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

const LOCAL_NAME: &str = "local.txt";
const REMOTE_NAME: &[u8] = b"remote.txt";
const REMOTE_TEXT: &[u8] = b"caf\xe9\n";

fn stamp(bytes: &mut Vec<u8>, id: u32, version: u32, size: usize) {
    let mut body = 15u32.to_be_bytes().to_vec();
    body.extend_from_slice(&(size as u64).to_be_bytes());
    body.extend_from_slice(&1000u32.to_be_bytes());
    body.extend_from_slice(&1000u32.to_be_bytes());
    body.extend_from_slice(&0o100600u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&version.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn remote_snapshot(changed: bool) -> Client {
    remote_snapshot_bytes(changed, REMOTE_TEXT)
}

fn remote_snapshot_bytes(changed: bool, text: &[u8]) -> Client {
    client(|bytes| {
        super::upload::realpath(bytes, 1);
        stamp(bytes, 2, 8, text.len());
        stamp(bytes, 3, 8, text.len());
        string(bytes, HANDLE, 4, b"regular");
        stamp(bytes, 5, 8, text.len());
        stamp(bytes, 6, 8, text.len());
        string(bytes, DATA, 7, text);
        status(bytes, 8, 1);
        stamp(bytes, 9, 8, text.len());
        status(bytes, 10, 0);
        stamp(bytes, 11, 8, text.len());
        super::upload::realpath(bytes, 12);
        stamp(bytes, 13, 8, text.len());
        super::upload::realpath(bytes, 14);
        stamp(bytes, 15, if changed { 9 } else { 8 }, text.len());
    })
}

fn select_cross_pair(window: &mut SftpWindow, root: &Path) {
    window
        .local
        .load(Arc::new(files::list_local(root).unwrap()));
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(b"/remote".to_vec()),
        vec![Entry {
            name: EntryName::Remote(REMOTE_NAME.to_vec()),
            display: "remote.txt".into(),
            hidden: false,
            kind: EntryKind::File,
            size: Some(REMOTE_TEXT.len() as u64),
            modified: Some(8),
            owner: None,
            group: None,
            mode: Some(0o600),
        }],
    )));
    let _ = window.update(WindowMessage::Select(
        Side::Local,
        EntryName::Local(LOCAL_NAME.into()),
    ));
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(REMOTE_NAME.to_vec()),
    ));
}

#[test]
fn sftp_file_comparison_reviews_exact_local_and_remote_snapshots() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let original = b"\xef\xbb\xbfcaf\xc3\xa9\r\n";
    fs::write(root.join(LOCAL_NAME), original).unwrap();
    super::upload::window_with_policy(
        &context,
        RemoteEditorPolicy::ReadOnly,
        |window, incoming, control| {
            select_cross_pair(window, &root);
            let _ = window.update(WindowMessage::CompareFiles(Side::Remote));
            let command = incoming.try_recv().unwrap();
            assert!(
                matches!(&command.operation, Operation::CompareFiles([Location::Local(_), Location::Remote(path)]) if path == b"/remote/remote.txt")
            );
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut remote_snapshot(false),
                command.operation,
                control,
                RemoteEditorPolicy::ReadOnly,
            )
            .map(Arc::new);
            assert!(matches!(result.as_deref(), Ok(Reply::Compared(review)) if !review.same_bytes));
            let _ = window.update(WindowMessage::Done(ticket, result));
            assert!(window.comparison.is_some());
            let _ = window.update(WindowMessage::ComparisonView(DiffMessage::Close));
            assert!(window.comparison.is_none());

            let _ = window.update(WindowMessage::CompareFiles(Side::Local));
            let ticket = window.busy.as_ref().unwrap().id;
            let command = incoming.try_recv().unwrap();
            let result = execute(
                &mut remote_snapshot(true),
                command.operation,
                control,
                RemoteEditorPolicy::ReadOnly,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            assert!(window.comparison.is_none());
            assert!(
                window
                    .status()
                    .unwrap()
                    .contains("changed during comparison")
            );
        },
    );
    assert_eq!(fs::read(root.join(LOCAL_NAME)).unwrap(), original);
}

#[test]
fn sftp_file_comparison_rejects_remote_reads_under_policy_and_ignores_cancelled_results() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let other = "second.txt";
    for name in [LOCAL_NAME, other] {
        fs::write(root.join(name), b"one\n").unwrap();
    }
    super::upload::window(&context, |window, incoming, control| {
        select_cross_pair(window, &root);
        let _ = window.update(WindowMessage::CompareFiles(Side::Remote));
        assert!(incoming.try_recv().is_err());
        let forged = Operation::CompareFiles([
            Location::Local(root.join(LOCAL_NAME)),
            Location::Remote(b"/remote/remote.txt".to_vec()),
        ]);
        let blocked = execute(
            &mut client(|_| {}),
            forged,
            control,
            RemoteEditorPolicy::Deny,
        )
        .unwrap_err();
        assert!(blocked.message.contains("policy forbids"));

        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Local,
            EntryName::Local(other.into()),
            true,
        ));
        let _ = window.update(WindowMessage::CompareFiles(Side::Local));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::CompareFiles([Location::Local(_), Location::Local(_)])
        ));
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut client(|_| {}),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(matches!(result.as_deref(), Ok(Reply::Compared(review)) if review.same_bytes));
        let _ = window.update(WindowMessage::CancelComparison);
        let _ = window.update(WindowMessage::Done(ticket, result));
        assert!(window.comparison.is_none());
        assert!(window.status().unwrap().contains("cancelled"));
    });
}

#[test]
fn sftp_file_comparison_rejects_binary_and_oversized_inputs() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let left = root.join("left");
    let right = root.join("right");
    fs::write(&left, b"first\0last").unwrap();
    fs::write(&right, b"text").unwrap();
    let pair = || {
        Operation::CompareFiles([
            Location::Local(left.clone()),
            Location::Local(right.clone()),
        ])
    };
    let run = || {
        execute(
            &mut client(|_| {}),
            pair(),
            &control(&context),
            RemoteEditorPolicy::ReadOnly,
        )
        .unwrap_err()
    };
    assert!(run().message.contains("Binary files"));
    fs::OpenOptions::new()
        .write(true)
        .open(&left)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    assert!(run().message.contains("4 MiB comparison limit"));
}

#[test]
fn sftp_ai_right_local_draft_refresh_requires_matching_bytes_and_connection() {
    use crate::clipboard_policy::ClipboardPolicy;
    use crate::sftp_window::compare::{View, ai};
    use pgp::crypto::hash::HashAlgorithm;

    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let left = root.join("left.txt");
    let right = root.join("right.txt");
    fs::write(&left, "left\n").unwrap();
    fs::write(&right, "right\n").unwrap();
    let files = [Location::Local(left), Location::Local(right.clone())];
    let policy = ClipboardPolicy::default();
    let scope = || ai::Scope {
        config: None,
        policy: &policy,
        vault: None,
        terminal_secrets: None,
    };
    super::upload::window(&context, |window, incoming, control| {
        let run = |operation| {
            let Reply::Compared(review) = execute(
                &mut client(|_| {}),
                operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .unwrap() else {
                panic!("expected a local comparison");
            };
            review
        };
        let initial = run(Operation::CompareFiles(files.clone()));
        window.comparison = Some(View::files(83, &initial));
        let snapshot = |window: &SftpWindow| {
            let Some(View::Files(view)) = window.comparison.as_ref() else {
                panic!("expected a file comparison");
            };
            let (sources, notice, stale) = view.refresh_test_snapshot();
            (sources[1].1.to_owned(), notice.map(str::to_owned), stale)
        };
        let digest = |bytes: &[u8]| {
            HashAlgorithm::Sha256
                .digest(bytes)
                .unwrap()
                .try_into()
                .unwrap()
        };

        fs::write(&right, "draft\n").unwrap();
        let (view, refresh) = window
            .start_test_comparison_refresh(Location::Local(right.clone()), digest(b"draft\n"))
            .unwrap();
        let request = incoming.try_recv().unwrap();
        assert!(matches!(&request.operation, Operation::CompareFiles(pair) if pair == &files));
        let refreshed = Arc::new(run(request.operation));
        let _ = window.update_comparison_ai(
            view,
            ai::Event::Refreshed(refresh.wrapping_add(1), Ok(Arc::clone(&refreshed))),
            scope(),
        );
        assert_eq!(snapshot(window).0, "right\n");
        let _ = window.update_comparison_ai(
            view,
            ai::Event::Refreshed(refresh, Ok(refreshed)),
            scope(),
        );
        let (right_text, notice, stale) = snapshot(window);
        assert_eq!(right_text, "draft\n");
        assert!(
            notice
                .unwrap()
                .contains("Saved fixture · Comparison refreshed")
        );
        assert!(!stale);

        fs::write(&right, "changed after save\n").unwrap();
        let (view, refresh) = window
            .start_test_comparison_refresh(Location::Local(right.clone()), digest(b"draft\n"))
            .unwrap();
        let refreshed = Arc::new(run(incoming.try_recv().unwrap().operation));
        let _ = window.update_comparison_ai(
            view,
            ai::Event::Refreshed(refresh, Ok(refreshed)),
            scope(),
        );
        let (right_text, notice, stale) = snapshot(window);
        assert_eq!(right_text, "draft\n");
        assert!(notice.unwrap().contains("The saved file changed"));
        assert!(stale);

        let (view, refresh) = window
            .start_test_comparison_refresh(
                Location::Local(right.clone()),
                digest(b"changed after save\n"),
            )
            .unwrap();
        let refreshed = Arc::new(run(incoming.try_recv().unwrap().operation));
        window.worker.take();
        let _ = window.update_comparison_ai(
            view,
            ai::Event::Refreshed(refresh, Ok(refreshed)),
            scope(),
        );
        let (right_text, notice, stale) = snapshot(window);
        assert_eq!(right_text, "draft\n");
        assert!(notice.unwrap().contains("The original SFTP connection"));
        assert!(stale);
    });
}

#[test]
fn sftp_ai_right_server_draft_refresh_checks_the_exact_raw_bytes() {
    use crate::clipboard_policy::ClipboardPolicy;
    use crate::sftp_window::compare::{View, ai};

    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let left = root.join(LOCAL_NAME);
    fs::write(&left, b"left\n").unwrap();
    let right = Location::Remote(b"/remote/remote.txt".to_vec());
    let pair = [Location::Local(left), right.clone()];
    let policy = ClipboardPolicy::default();
    super::upload::window_with_policy(
        &context,
        RemoteEditorPolicy::Allow,
        |window, incoming, control| {
            let run = |operation, text| {
                let Reply::Compared(review) = execute(
                    &mut remote_snapshot_bytes(false, text),
                    operation,
                    control,
                    RemoteEditorPolicy::Allow,
                )
                .unwrap() else {
                    panic!("expected a cross-pane comparison");
                };
                review
            };
            let initial = run(Operation::CompareFiles(pair.clone()), REMOTE_TEXT);
            window.comparison = Some(View::files(54, &initial));
            let digest = crate::sftp_window::worker::draft_file::digest(b"draft\r\n").unwrap();
            for (actual, expected_stale) in [
                (b"draft\r\n".as_slice(), false),
                (b"draft\n".as_slice(), true),
            ] {
                let (view, ticket) = window
                    .start_test_comparison_refresh(right.clone(), digest)
                    .unwrap();
                let operation = incoming.try_recv().unwrap().operation;
                assert!(matches!(&operation, Operation::CompareFiles(files) if files == &pair));
                let refreshed = Arc::new(run(operation, actual));
                let _ = window.update_comparison_ai(
                    view,
                    ai::Event::Refreshed(ticket, Ok(refreshed)),
                    ai::Scope {
                        config: None,
                        policy: &policy,
                        vault: None,
                        terminal_secrets: None,
                    },
                );
                let Some(View::Files(view)) = &window.comparison else {
                    panic!("expected the original comparison");
                };
                let (sources, notice, stale) = view.refresh_test_snapshot();
                assert_eq!(sources[1].1, "draft\r\n");
                assert_eq!(stale, expected_stale);
                assert!(notice.unwrap().contains(if stale {
                    "The saved file changed"
                } else {
                    "Comparison refreshed"
                }));
            }
        },
    );
}
