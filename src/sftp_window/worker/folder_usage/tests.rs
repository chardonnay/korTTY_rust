use super::*;
use crate::clipboard_policy::RemoteEditorPolicy;
use crate::sftp_window::{Message, SftpWindow, Side, archive::Message as ArchiveMessage};
use crate::ssh_connection::Connected;
use crate::workspace::SshTarget;

#[test]
fn allocated_folder_rows_bind_exact_paths_and_reject_duplicates_and_overflow() {
    let paths = ["/a ' Grüße".as_bytes().to_vec(), b"/b".to_vec()];
    let names = [
        EntryName::Remote(b"a".to_vec()),
        EntryName::Remote(b"b".to_vec()),
    ];
    let output = "  128\t/a ' Grüße/\n9\t/unrequested\n-7\t/b\nerror\n";
    let sizes = parse(output.as_bytes(), &paths, &names).unwrap();
    assert_eq!(sizes.get(&names[0]), Some(&(128 * 1024)));
    assert!(!sizes.contains_key(&names[1]));
    for output in [
        b"1\t/b\n2\t/b/\n".as_slice(),
        b"18014398509481984\t/b\n",
        b"18446744073709551616\t/b\n",
    ] {
        assert!(parse(output, &paths, &names).is_err());
    }
    let mut result = Output::default();
    result
        .push(0, b"4\t/path with sftp connections only\n")
        .unwrap();
    assert!(!result.sftp_only());
    result
        .push(1, b"This service allows sftp connections only.\r\n")
        .unwrap();
    assert!(result.sftp_only());
    assert!(result.push(1, &vec![0; OUTPUT_LIMIT]).is_err());
}

#[test]
fn allocated_folder_cancel_terminates_polling_io() {
    let owner = Arc::new(Control::default());
    let cancel = Cancel::default();
    let gate = Gate {
        owner: Arc::downgrade(&owner),
        cancel: cancel.clone(),
        deadline: Arc::new(Mutex::new(Instant::now() + DEADLINE)),
    };
    let narrowed = gate.clone();
    gate.restrict(Duration::ZERO);
    assert_eq!(
        narrowed.interval().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    let mut stream = Polling {
        io: std::io::Cursor::new(vec![0]),
        gate,
    };
    cancel.stop_local();
    assert_eq!(
        stream.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::Other
    );
    assert_eq!(stream.write(&[0]).unwrap_err().kind(), io::ErrorKind::Other);
}

pub(crate) fn exercise_live_folder_sizes(
    target: SshTarget,
    connect: impl Fn() -> Connected,
    root: &[u8],
    expected: &[(&str, u64)],
    expected_status: &[&str],
) {
    let profile = crate::snippets::tests::Profile::new();
    let connection = connect();
    let archive_race_ready = if matches!(&connection, Connected::Worker(_)) {
        2
    } else {
        1
    };
    let worker = Worker::start(
        super::super::Connection::Standalone(connection),
        RemoteEditorPolicy::Deny,
    )
    .unwrap();
    let mut window = SftpWindow::new(target, None, true, RemoteEditorPolicy::Deny);
    let _ = window.initialize_at(worker, Some(profile.0.clone()), Some(root.to_vec()));
    finish(&mut window);
    assert_eq!(
        window.remote.directory.as_ref().unwrap().path,
        Location::Remote(root.to_vec())
    );

    for &(name, _) in expected {
        let _ = window.update(Message::ToggleSelection(
            Side::Remote,
            EntryName::Remote(name.as_bytes().to_vec()),
            true,
        ));
    }
    exercise_archive_estimate(&mut window, expected, expected_status, archive_race_ready);
    let _ = window.update(Message::MeasureFolders(Side::Remote));
    assert!(window.busy.as_ref().unwrap().sync_cancel.is_some());
    finish(&mut window);
    for &(name, bytes) in expected {
        assert_eq!(
            window
                .remote
                .folder_sizes
                .get(&EntryName::Remote(name.as_bytes().to_vec())),
            Some(&bytes)
        );
    }
    let status = window.status().unwrap();
    for text in expected_status {
        assert!(
            status.contains(text),
            "unexpected measurement status: {status}"
        );
    }

    let _ = window.update(Message::MeasureFolders(Side::Remote));
    let _ = window.update(Message::CancelMeasurement);
    finish(&mut window);
    match window.status() {
        Some("Folder size calculation cancelled.") => {}
        Some("Folder size calculation cancelled. Reconnect to browse the server.") => {
            // This cancel can race a request on the primary channel. In
            // particular, a one-session server runs the fallback there.
            assert!(window.worker.is_none());
            let worker = Worker::start(
                super::super::Connection::Standalone(connect()),
                RemoteEditorPolicy::Deny,
            )
            .unwrap();
            let _ = window.initialize_at(worker, Some(profile.0.clone()), Some(root.to_vec()));
            finish(&mut window);
        }
        status => panic!("unexpected cancellation status: {status:?}"),
    }
    let _ = window.update(Message::Refresh(Side::Remote));
    finish(&mut window);
    assert_eq!(
        window.remote.directory.as_ref().unwrap().path,
        Location::Remote(root.to_vec())
    );
    assert!(window.remote.folder_sizes.is_empty());
    for done in window.stop() {
        let started = Instant::now();
        while !done.is_closed() {
            assert!(
                started.elapsed() < Duration::from_secs(12),
                "the browser worker did not stop"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn exercise_archive_estimate(
    window: &mut SftpWindow,
    expected: &[(&str, u64)],
    expected_status: &[&str],
    archive_race_ready: u64,
) {
    use iced_runtime::{Action, task};
    use std::task::{Context, Poll, Waker};

    let name = EntryName::Remote(b"estimate-input.txt".to_vec());
    let _ = window.update(Message::ToggleSelection(Side::Remote, name.clone(), true));
    let mut stream = task::into_stream(window.update(Message::CreateServerArchive)).unwrap();
    assert!(
        window
            .archive_estimate_label()
            .unwrap()
            .contains("Measuring")
    );
    let started = Instant::now();
    let mut context = Context::from_waker(Waker::noop());
    let completed = loop {
        match stream.as_mut().poll_next(&mut context) {
            Poll::Ready(Some(Action::Output(
                message @ Message::ServerArchive(ArchiveMessage::Estimated(_, _)),
            ))) => break message,
            Poll::Pending => {
                assert!(started.elapsed() < Duration::from_secs(12));
                thread::sleep(Duration::from_millis(10));
            }
            _ => panic!("expected a completed archive estimate"),
        }
    };
    let _ = window.update(completed);
    let label = window.archive_estimate_label().unwrap();
    assert!(label.contains("7 file bytes"), "{label}");
    if expected_status.contains(&"allocated bytes") {
        let total: u64 = expected.iter().map(|(_, bytes)| bytes).sum();
        assert!(
            label.contains(&format!("{total} allocated bytes")),
            "{label}"
        );
        assert!(label.contains("(partial)"), "{label}");
    } else {
        assert!(label.contains("Allocated size unavailable"), "{label}");
    }
    let _ = window.update(Message::Dismiss);
    assert!(window.archive_estimate_label().is_none());

    let _ = window.update(Message::Select(Side::Remote, name.clone()));
    assert!(task::into_stream(window.update(Message::CreateServerArchive)).is_none());
    assert_eq!(
        window.archive_estimate_label().as_deref(),
        Some("7 file bytes")
    );
    let _ = window.update(Message::ServerArchive(ArchiveMessage::Review));
    finish(window);
    assert_eq!(window.archive_review_bytes(), Some(7));
    let _ = window.update(Message::Dismiss);
    if std::env::var_os("KORTTY_SSH_EXEC_NAMED_ARCHIVE_RACE").is_some() {
        exercise_named_archive_while_measuring(window, archive_race_ready);
    } else {
        let _ = window.update(Message::ToggleSelection(Side::Remote, name, false));
    }
    for &(name, _) in expected {
        let _ = window.update(Message::ToggleSelection(
            Side::Remote,
            EntryName::Remote(name.as_bytes().to_vec()),
            true,
        ));
    }
}

fn exercise_named_archive_while_measuring(window: &mut SftpWindow, expected_ready: u64) {
    use crate::sftp_window::worker::{Operation, Reply};

    let folder = EntryName::Remote(b"named-review".to_vec());
    let _ = window.update(Message::Select(Side::Remote, folder.clone()));
    let _estimate = window.update(Message::CreateServerArchive);
    assert!(
        window
            .archive_estimate_label()
            .unwrap()
            .contains("Measuring")
    );
    // Observe the fixture through the original browser channel. The marker
    // proves du occupies the other session slot when review starts.
    let signal = window
        .remote
        .directory
        .as_ref()
        .unwrap()
        .path
        .with(&EntryName::Remote(b"loopback".to_vec()))
        .unwrap();
    let worker = window.worker.as_ref().unwrap();
    let started = Instant::now();
    loop {
        let reply = worker.submit(Operation::Browse(signal.clone())).unwrap();
        let reply = wait_for(&reply).unwrap();
        let Reply::Listed(listing) = reply.as_ref() else {
            panic!("expected the private race signals directory");
        };
        if listing.entries.iter().any(|entry| {
            entry.name == EntryName::Remote(b"named-ready".to_vec())
                && entry.size == Some(expected_ready)
        }) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    }
    let owner = std::env::var("KORTTY_SSH_EXEC_USER").unwrap();
    let _ = window.update(Message::ServerArchive(ArchiveMessage::Owner(owner)));
    let _ = window.update(Message::ServerArchive(ArchiveMessage::Review));
    finish(window);
    assert_eq!(window.archive_review_bytes(), Some(7));
    let _ = window.update(Message::Dismiss);
    let _ = window.update(Message::ToggleSelection(Side::Remote, folder, false));
}

fn finish(window: &mut SftpWindow) {
    let busy = window
        .busy
        .as_ref()
        .expect("the normal browser operation is running");
    let id = busy.id;
    let reply = wait_for(&busy.reply);
    if let Err(error) = &reply {
        assert!(busy.cancelled, "browser request failed: {error:?}");
    }
    let _ = window.update(Message::Done(id, reply));
}

fn wait_for<T>(receiver: &async_channel::Receiver<T>) -> T {
    let started = Instant::now();
    loop {
        match receiver.try_recv() {
            Ok(reply) => return reply,
            Err(async_channel::TryRecvError::Closed) => {
                panic!("the browser response channel closed")
            }
            Err(async_channel::TryRecvError::Empty) => {}
        }
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "the browser fixture timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
