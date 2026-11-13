use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::process::{Child, ChildStdin, Command as Process, Stdio};

use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct InjectTarget {
    input: ChildStdin,
    target: PathBuf,
    armed: Arc<AtomicBool>,
    mkdir: Option<(PathBuf, Arc<AtomicBool>)>,
}

impl Write for InjectTarget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some((path, armed)) = &self.mkdir
            && bytes.first() == Some(&14)
            && armed.swap(false, Ordering::AcqRel)
        {
            fs::DirBuilder::new().mode(0o700).create(path)?;
            fs::write(path.join("preexisting"), b"concurrent directory")?;
        }
        if bytes.first() == Some(&200)
            && bytes
                .get(9..)
                .is_some_and(|body| body.starts_with(b"hardlink@openssh.com"))
            && self.armed.swap(false, Ordering::AcqRel)
        {
            fs::write(&self.target, b"concurrent file")?;
        }
        self.input.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
    }
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with hardlink"]
fn openssh_browser_server_draft_publishes_exact_bytes_without_replacing() {
    use crate::sftp_window::worker::draft_file;

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    let [published, raced] = ["published", "raced"].map(|name| root.join(name));
    for directory in [&published, &raced] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    let name = "generated.conf";
    let draft: Arc<str> = "name=café\r\nvalue=1\r\n".into();
    let armed = Arc::new(AtomicBool::new(false));
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let input = InjectTarget {
        input: server.0.stdin.take().unwrap(),
        target: raced.join(name),
        armed: Arc::clone(&armed),
        mkdir: None,
    };
    let mut client: Client = Sftp::new(
        Box::new(server.0.stdout.take().unwrap()) as _,
        Box::new(input) as _,
    )
    .unwrap();
    assert!(client.supports_hardlink());
    super::upload::window_with_policy(&context, RemoteEditorPolicy::Allow, |window, _, control| {
        let worker = window.worker.as_ref().unwrap();
        for destination in [&published, &raced] {
            let request = draft_file::Request::new(
                destination.as_os_str().as_bytes(),
                name,
                Arc::clone(&draft),
                &context.route,
                worker,
                Arc::new(|| Ok(())),
            )
            .unwrap();
            let Reply::DraftFileReviewed(plan) = execute(
                &mut client,
                Operation::ReviewDraftFile(Box::new(request), sync::Cancel::default()),
                control,
                RemoteEditorPolicy::Allow,
            )
            .unwrap() else {
                panic!("expected a native SFTP draft review");
            };
            assert!(plan.matches(destination.as_os_str().as_bytes(), name, &draft));
            assert_eq!(plan.bytes(), draft.len());
            assert_eq!(
                fs::read_dir(destination).unwrap().count(),
                0,
                "review must be read-only"
            );
            let denied = execute(
                &mut client,
                Operation::PublishDraftFile(Arc::clone(&plan), sync::Cancel::default()),
                control,
                RemoteEditorPolicy::ReadOnly,
            )
            .unwrap_err();
            assert!(denied.message.contains("policy forbids"));
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
            let collision = destination == &raced;
            armed.store(collision, Ordering::Release);
            let result = execute(
                &mut client,
                Operation::PublishDraftFile(Arc::clone(&plan), sync::Cancel::default()),
                control,
                RemoteEditorPolicy::Allow,
            );
            if collision {
                assert!(
                    result.is_err(),
                    "late-arriving files must never be replaced"
                );
                assert!(
                    !armed.load(Ordering::Acquire),
                    "fixture must race the hard-link request"
                );
                assert_eq!(
                    fs::read(destination.join(name)).unwrap(),
                    b"concurrent file"
                );
            } else {
                assert!(
                    matches!(&result, Ok(Reply::DraftFilePublished(note, None)) if note.starts_with("Created new server file")),
                    "{result:?}"
                );
                assert_eq!(fs::read(destination.join(name)).unwrap(), draft.as_bytes());
                assert_eq!(
                    fs::metadata(destination.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            assert_eq!(
                fs::read_dir(destination).unwrap().count(),
                1,
                "private stage must be cleaned"
            );
            let repeated = execute(
                &mut client,
                Operation::PublishDraftFile(plan, sync::Cancel::default()),
                control,
                RemoteEditorPolicy::Allow,
            )
            .unwrap_err();
            assert!(repeated.message.contains("already used"), "{repeated:?}");
        }
    });
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with copy-data and hardlink"]
fn openssh_browser_server_copy_reviews_one_file_and_never_replaces() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    let [source, copied, raced, cancelled, stale] =
        ["source", "copied", "raced", "cancelled", "stale"].map(|name| root.join(name));
    for directory in [&source, &copied, &raced, &cancelled, &stale] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    let body: Vec<_> = (0..132_219).map(|index| (index % 251) as u8).collect();
    fs::write(source.join("payload"), &body).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(&source)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let input = InjectTarget {
        input: server.0.stdin.take().unwrap(),
        target: raced.join("payload"),
        armed: Arc::clone(&armed),
        mkdir: None,
    };
    let mut client: Client = Sftp::new(
        Box::new(server.0.stdout.take().unwrap()) as _,
        Box::new(input) as _,
    )
    .unwrap();
    assert!(client.supports_copy_data() && client.supports_hardlink());
    super::upload::window(&context, |window, incoming, control| {
        let remote = Location::Remote(source.as_os_str().as_bytes().to_vec());
        let Reply::Listed(listing) = execute(
            &mut client,
            Operation::Browse(remote),
            control,
            RemoteEditorPolicy::Deny,
        )
        .unwrap() else {
            panic!("expected real remote listing");
        };
        let entry = listing
            .entries
            .iter()
            .find(|entry| entry.display == "payload")
            .unwrap()
            .name
            .clone();
        window.remote.load(listing);
        let _ = window.update(WindowMessage::Select(Side::Remote, entry));

        for destination in [&copied, &raced, &cancelled, &stale] {
            let _ = window.update(WindowMessage::CopyOnServer);
            let _ = window.update(WindowMessage::ServerCopyDirectory(
                destination.to_str().unwrap().into(),
            ));
            let _ = window.update(WindowMessage::ReviewServerCopy);
            let command = incoming.try_recv().unwrap();
            assert!(matches!(
                &command.operation,
                Operation::ReviewServerCopy(..)
            ));
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut client,
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            assert!(
                matches!(result.as_deref(), Ok(Reply::ServerCopyReview(_))),
                "{result:?}"
            );
            let _ = window.update(WindowMessage::Done(ticket, result));
            window.revalidate_file_transfers(false);
            assert!(!destination.join("payload").exists(), "review is read-only");
            let _ = window.update(WindowMessage::ApproveServerCopy);
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::ApplyServerCopy(..)));
            if destination == &raced {
                armed.store(true, Ordering::Release);
            } else if destination == &cancelled {
                let _ = window.update(WindowMessage::CancelTransfer);
            } else if destination == &stale {
                fs::write(source.join("payload"), b"changed source after review").unwrap();
            }
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut client,
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let status = window.status().unwrap();
            if destination == &copied {
                assert!(status.starts_with("Copied "), "{status}");
            } else if destination == &cancelled {
                assert!(status.starts_with("Server copy cancelled"), "{status}");
            } else {
                assert!(status.starts_with("Server copy stopped"), "{status}");
            }
        }
    });
    assert!(
        !armed.load(Ordering::Acquire),
        "late target must be inserted at publication"
    );
    assert_eq!(fs::read(copied.join("payload")).unwrap(), body);
    assert_eq!(fs::read(raced.join("payload")).unwrap(), b"concurrent file");
    assert_eq!(fs::read_dir(&copied).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&raced).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&cancelled).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&stale).unwrap().count(), 0);
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with copy-data and hardlink"]
fn openssh_browser_server_copy_reviews_exact_folder_and_preserves_racing_paths() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    let [source, copied, raced, occupied, cancelled, stale, rejected] = [
        "tree",
        "copied",
        "raced",
        "occupied",
        "cancelled",
        "stale",
        "rejected",
    ]
    .map(|name| root.join(name));
    let private_dir = |path: &Path| fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    for directory in [
        &source, &copied, &raced, &occupied, &cancelled, &stale, &rejected,
    ] {
        private_dir(directory);
    }
    private_dir(&source.join("empty"));
    private_dir(&source.join("nested"));
    let body: Vec<_> = (0..132_219).map(|index| (index % 251) as u8).collect();
    fs::write(source.join("nested/payload"), &body).unwrap();
    fs::write(source.join("second"), b"second file").unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let mkdir_armed = Arc::new(AtomicBool::new(false));
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let input = InjectTarget {
        input: server.0.stdin.take().unwrap(),
        target: raced.join("tree/nested/payload"),
        armed: Arc::clone(&armed),
        mkdir: Some((occupied.join("tree"), Arc::clone(&mkdir_armed))),
    };
    let mut client: Client = Sftp::new(
        Box::new(server.0.stdout.take().unwrap()) as _,
        Box::new(input) as _,
    )
    .unwrap();
    assert!(client.supports_copy_data() && client.supports_hardlink());
    super::upload::window(&context, |window, incoming, control| {
        let remote = Location::Remote(root.as_os_str().as_bytes().to_vec());
        let Reply::Listed(listing) = execute(
            &mut client,
            Operation::Browse(remote),
            control,
            RemoteEditorPolicy::Deny,
        )
        .unwrap() else {
            panic!("expected real remote listing");
        };
        let entry = listing
            .entries
            .iter()
            .find(|entry| entry.display == "tree")
            .unwrap()
            .name
            .clone();
        window.remote.load(listing);
        let _ = window.update(WindowMessage::Select(Side::Remote, entry));

        for destination in [
            &copied, &raced, &occupied, &cancelled, &stale, &rejected, &source,
        ] {
            if destination == &rejected {
                symlink("second", source.join("link")).unwrap();
            }
            let _ = window.update(WindowMessage::CopyOnServer);
            let _ = window.update(WindowMessage::ServerCopyDirectory(
                destination.to_str().unwrap().into(),
            ));
            let _ = window.update(WindowMessage::ReviewServerCopy);
            let command = incoming.try_recv().unwrap();
            assert!(matches!(
                &command.operation,
                Operation::ReviewServerCopy(..)
            ));
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut client,
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            assert!(!destination.join("tree").exists(), "review is read-only");
            if destination == &rejected || destination == &source {
                assert!(result.is_err(), "unsafe tree review succeeded: {result:?}");
                let _ = window.update(WindowMessage::Done(ticket, result));
                continue;
            }
            let Ok(Reply::ServerCopyReview(plan)) = result.as_deref() else {
                panic!("{result:?}");
            };
            let (files, entries) = plan.folder_review().unwrap();
            assert_eq!(files, 2);
            assert_eq!(
                entries,
                &[
                    (String::from("empty"), true),
                    (String::from("nested"), true),
                    (String::from("nested/payload"), false),
                    (String::from("second"), false),
                ]
            );
            let _ = window.update(WindowMessage::Done(ticket, result));
            let _ = window.update(WindowMessage::ApproveServerCopy);
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::ApplyServerCopy(..)));
            if destination == &raced {
                armed.store(true, Ordering::Release);
            } else if destination == &occupied {
                mkdir_armed.store(true, Ordering::Release);
            } else if destination == &cancelled {
                let _ = window.update(WindowMessage::CancelTransfer);
            } else if destination == &stale {
                fs::write(source.join("second"), b"changed after review").unwrap();
            }
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut client,
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let status = window.status().unwrap();
            if destination == &copied {
                assert!(status.starts_with("Copied 3 folders · 2 files"), "{status}");
            } else if destination == &raced {
                assert!(
                    status.contains("Confirmed 3 folders · 0 files")
                        && status.contains("Partial destination"),
                    "{status}"
                );
            } else if destination == &cancelled {
                assert!(status.starts_with("Server copy cancelled"), "{status}");
            } else {
                assert!(
                    status.starts_with("Server copy stopped")
                        && !status.contains("Partial destination"),
                    "{status}"
                );
            }
        }
    });
    assert!(!armed.load(Ordering::Acquire) && !mkdir_armed.load(Ordering::Acquire));
    assert_eq!(fs::read(copied.join("tree/nested/payload")).unwrap(), body);
    assert_eq!(
        fs::read(copied.join("tree/second")).unwrap(),
        b"second file"
    );
    assert_eq!(fs::read_dir(copied.join("tree/empty")).unwrap().count(), 0);
    assert_eq!(
        fs::metadata(copied.join("tree"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(copied.join("tree/nested/payload"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(
        fs::read(raced.join("tree/nested/payload")).unwrap(),
        b"concurrent file"
    );
    assert!(!raced.join("tree/second").exists());
    assert_eq!(
        fs::read(occupied.join("tree/preexisting")).unwrap(),
        b"concurrent directory"
    );
    for directory in [&copied, &raced, &occupied] {
        assert_eq!(
            fs::read_dir(directory).unwrap().count(),
            1,
            "private staging was cleaned"
        );
    }
    for directory in [&cancelled, &stale, &rejected] {
        assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
    }
}
