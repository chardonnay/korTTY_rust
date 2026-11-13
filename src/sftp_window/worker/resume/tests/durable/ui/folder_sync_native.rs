use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server"]
fn openssh_reviewed_folder_copy_publishes_without_replacement() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    let local = root.join("local");
    let remote = root.join("remote");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&remote).unwrap();
    fs::set_permissions(&remote, fs::Permissions::from_mode(0o700)).unwrap();
    let payload: Vec<_> = (0..132_219).map(|index| (index % 251) as u8).collect();
    fs::write(local.join("copy.bin"), &payload).unwrap();
    fs::create_dir_all(local.join("nested/leaf")).unwrap();
    fs::write(local.join("nested/leaf/child.txt"), b"reviewed nested file").unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_001);
    for relative in ["copy.bin", "nested/leaf/child.txt"] {
        fs::File::options()
            .write(true)
            .open(local.join(relative))
            .unwrap()
            .set_modified(modified + Duration::from_millis(573))
            .unwrap();
    }
    fs::create_dir(local.join("empty")).unwrap();
    fs::create_dir(local.join("excluded")).unwrap();
    fs::write(local.join("excluded/kept.txt"), b"excluded").unwrap();
    fs::write(local.join("kept.txt"), b"local version").unwrap();
    fs::write(remote.join("kept.txt"), b"server version").unwrap();
    let server_path = std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER");
    let mut server = Server(
        Process::new(server_path)
            .arg("-d")
            .arg(&remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let reader = Box::new(server.0.stdout.take().unwrap()) as _;
    let writer = Box::new(server.0.stdin.take().unwrap()) as _;
    let mut client: Client = Sftp::new(reader, writer).unwrap();
    assert!(client.supports_hardlink());
    super::upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&local).unwrap()));
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(remote.as_os_str().as_bytes().to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::CompareFolders(None));
        let ticket = window.counter;
        use iced::widget::text_editor::{Action, Edit};
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::FolderExcludes(
                ticket,
                Action::Edit(Edit::Paste(Arc::new("excluded".into()))),
            ),
        ));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(ticket),
        ));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut client,
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(
            matches!(result.as_deref(), Ok(Reply::ComparedFolders(_))),
            "{result:?}"
        );
        let _ = window.update(WindowMessage::Done(ticket, result));

        let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
            Side::Remote,
        )));
        assert!(matches!(
            &window.comparison,
            Some(compare::View::CopySetup(_))
        ));
        let ticket = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmCopy(ticket),
        ));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(&command.operation, Operation::ApplySync(..)));
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
        assert!(
            status.contains("Copied 2 of 2 reviewed files · 3 of 3 folders created"),
            "{status}"
        );
    });
    assert_eq!(fs::read(remote.join("copy.bin")).unwrap(), payload);
    for relative in ["copy.bin", "nested/leaf/child.txt"] {
        assert_eq!(
            fs::metadata(remote.join(relative))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }
    assert_eq!(
        fs::read(remote.join("kept.txt")).unwrap(),
        b"server version"
    );
    assert_eq!(
        fs::read(remote.join("nested/leaf/child.txt")).unwrap(),
        b"reviewed nested file"
    );
    assert_eq!(fs::read_dir(remote.join("nested/leaf")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(remote.join("empty")).unwrap().count(), 0);
    assert!(!remote.join("excluded").exists());
    assert_eq!(fs::read_dir(&remote).unwrap().count(), 4);
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server"]
fn openssh_reviewed_folder_download_uses_file_modes_and_ordinary_directories() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (local, remote) = roots(&root);
    let context = context(&root);
    let reference = root.join("ordinary-directory");
    fs::create_dir(&reference).unwrap();
    let directory_mode = fs::metadata(&reference).unwrap().permissions().mode() & 0o7777;
    fs::remove_dir(reference).unwrap();
    fs::create_dir(remote.join("nested")).unwrap();
    fs::create_dir(remote.join("empty")).unwrap();
    private_dir(&remote.join("excluded"));
    private_file(&remote.join("excluded/kept.txt"), b"excluded source").unwrap();
    private_file(&remote.join("kept.txt"), b"server existing file").unwrap();
    private_file(&local.join("kept.txt"), b"preserved local file").unwrap();
    let sources: [(&str, &[u8], u32); 3] = [
        ("ordinary.txt", b"ordinary download", 0o644),
        (
            "nested/execute.sh",
            b"#!/bin/sh\nprintf executable\\n",
            0o751,
        ),
        ("privileged", b"privileged bits must be removed", 0o6777),
    ];
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_001);
    for (relative, payload, mode) in sources {
        let source = remote.join(relative);
        private_file(&source, payload).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(modified + Duration::from_millis(573))
            .unwrap();
        assert_eq!(
            fs::metadata(source).unwrap().permissions().mode() & 0o7777,
            mode
        );
    }
    let (mut server, reader) = open_server(&remote);
    let mut client: Client =
        Sftp::new(reader, Box::new(server.0.stdin.take().unwrap()) as _).unwrap();
    super::upload::window(&context, |window, incoming, control| {
        compare_pair(
            window,
            incoming,
            &mut client,
            control,
            &local,
            &remote,
            "excluded",
        );
        let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
            Side::Local,
        )));
        let ticket = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmCopy(ticket),
        ));
        let command = incoming
            .try_recv()
            .expect("queue the reviewed browser download");
        assert!(matches!(command.operation, Operation::ApplySync(..)));
        finish_command(window, &mut client, control, command);
        let status = window.status().unwrap();
        assert!(
            status.contains("Copied 3 of 3 reviewed files · 2 of 2 folders created"),
            "{status}"
        );
    });
    for (relative, payload, mode) in sources {
        let target = local.join(relative);
        let metadata = fs::metadata(&target).unwrap();
        assert_eq!(fs::read(target).unwrap(), payload);
        assert_eq!(metadata.permissions().mode() & 0o7777, mode & 0o755);
        assert_eq!(metadata.modified().unwrap(), modified);
        assert_eq!(fs::read(remote.join(relative)).unwrap(), payload);
        assert_eq!(
            fs::metadata(remote.join(relative))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            mode
        );
    }
    for relative in ["nested", "empty"] {
        assert_eq!(
            fs::metadata(local.join(relative))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            directory_mode
        );
    }
    assert_eq!(fs::read_dir(local.join("empty")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(local.join("nested")).unwrap().count(), 1);
    assert_eq!(
        fs::read(local.join("kept.txt")).unwrap(),
        b"preserved local file"
    );
    assert_eq!(
        fs::read(remote.join("kept.txt")).unwrap(),
        b"server existing file"
    );
    assert_eq!(
        fs::read(remote.join("excluded/kept.txt")).unwrap(),
        b"excluded source"
    );
    assert!(!local.join("excluded").exists());
    assert_eq!(fs::read_dir(&local).unwrap().count(), 5);
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with POSIX rename and hardlink"]
fn openssh_reviewed_sync_removals_are_opt_in_and_directional() {
    for to in [Side::Local, Side::Remote] {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (local, remote) = roots(&root);
        let context = context(&root);
        let (source, target) = match to {
            Side::Local => (&remote, &local),
            Side::Remote => (&local, &remote),
        };
        let nested = target.join("nested");
        let leaf = nested.join("leaf");
        let excluded = target.join("excluded");
        let type_conflict = target.join("type-conflict");
        for directory in [
            &target.join("gone"),
            &nested,
            &leaf,
            &excluded,
            &type_conflict,
        ] {
            private_dir(directory);
        }
        private_file(&source.join("copy-only"), b"source-only copy").unwrap();
        private_file(&source.join("common"), b"source version").unwrap();
        private_file(&source.join("type-conflict"), b"source is a file").unwrap();
        private_file(&target.join("common"), b"target version preserved").unwrap();
        private_file(&target.join("only-file"), b"approved target extra").unwrap();
        private_file(&leaf.join("payload"), b"approved nested target extra").unwrap();
        private_file(&excluded.join("retained.tmp"), b"excluded from review").unwrap();
        private_file(&excluded.join("removed"), b"approved file beside exclusion").unwrap();
        private_file(&type_conflict.join("child"), b"blocked by type conflict").unwrap();
        let (mut server, reader) = open_server(&remote);
        let mut client: Client =
            Sftp::new(reader, Box::new(server.0.stdin.take().unwrap()) as _).unwrap();
        assert!(client.supports_atomic_replace() && client.supports_hardlink());

        super::upload::window(&context, |window, incoming, control| {
            compare_pair(
                window,
                incoming,
                &mut client,
                control,
                &local,
                &remote,
                "*.tmp",
            );
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
                to,
            )));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCopy(ticket),
            ));
            let status = finish_sync(window, incoming, &mut client, control);
            assert!(status.contains("Copied 1 of 1 reviewed files"), "{status}");
            assert_eq!(
                fs::read(target.join("copy-only")).unwrap(),
                b"source-only copy"
            );
            assert_eq!(
                fs::read(target.join("only-file")).unwrap(),
                b"approved target extra"
            );
            assert_eq!(
                fs::read(leaf.join("payload")).unwrap(),
                b"approved nested target extra"
            );
            assert!(target.join("gone").is_dir());
            assert_eq!(
                fs::read(excluded.join("removed")).unwrap(),
                b"approved file beside exclusion"
            );

            let review = compare_pair(
                window,
                incoming,
                &mut client,
                control,
                &local,
                &remote,
                "*.tmp",
            );
            let extras: Vec<_> = review
                .destination_only(to)
                .map(|path| path.review_label())
                .collect();
            assert_eq!(
                extras,
                [
                    "excluded",
                    "excluded/removed",
                    "gone",
                    "nested",
                    "nested/leaf",
                    "nested/leaf/payload",
                    "only-file"
                ]
            );
            let confirmation = review_removals(window, incoming, to);
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmRemovals(confirmation),
            ));
            let status = finish_sync(window, incoming, &mut client, control);
            assert!(
                status.contains("Removed 6 of 7 reviewed entries"),
                "{status}"
            );
            assert!(
                status
                    .to_ascii_lowercase()
                    .contains("kept 1 nonempty folder"),
                "{status}"
            );
        });

        assert_eq!(
            fs::read(source.join("copy-only")).unwrap(),
            b"source-only copy"
        );
        assert_eq!(fs::read(source.join("common")).unwrap(), b"source version");
        assert_eq!(
            fs::read(source.join("type-conflict")).unwrap(),
            b"source is a file"
        );
        assert_eq!(fs::read_dir(source).unwrap().count(), 3);
        assert_eq!(
            fs::read(target.join("copy-only")).unwrap(),
            b"source-only copy"
        );
        assert_eq!(
            fs::read(target.join("common")).unwrap(),
            b"target version preserved"
        );
        assert_eq!(
            fs::read(type_conflict.join("child")).unwrap(),
            b"blocked by type conflict"
        );
        assert_eq!(
            fs::read(excluded.join("retained.tmp")).unwrap(),
            b"excluded from review"
        );
        assert_eq!(fs::read_dir(&excluded).unwrap().count(), 1);
        assert!(!target.join("gone").exists());
        assert!(!target.join("only-file").exists());
        assert!(!nested.exists());
        assert_eq!(
            fs::read_dir(target).unwrap().count(),
            4,
            "no private quarantine remains"
        );
    }
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with POSIX rename"]
fn openssh_reviewed_sync_removals_keep_late_children_and_obey_policy() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (local, remote) = roots(&root);
    let context = context(&root);
    let extra = remote.join("late-folder");
    private_dir(&extra);
    for folder in [&local, &remote] {
        private_file(&folder.join("common"), b"same common bytes").unwrap();
    }
    let armed = Arc::new(AtomicBool::new(false));
    let trace = Arc::new(Mutex::new(RemovalTrace::default()));
    let (mut server, reader) = open_server(&remote);
    let writer = LateChild {
        input: server.0.stdin.take().unwrap(),
        directory: extra.clone(),
        armed: Arc::clone(&armed),
        trace: Arc::clone(&trace),
    };
    let mut client: Client = Sftp::new(reader, Box::new(writer) as _).unwrap();
    assert!(client.supports_atomic_replace());

    super::upload::window(&context, |window, incoming, control| {
        compare_pair(window, incoming, &mut client, control, &local, &remote, "");
        let ticket = review_removals(window, incoming, Side::Remote);
        window.revalidate_file_transfers(false);
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmRemovals(ticket),
        ));
        assert!(
            incoming.try_recv().is_err(),
            "revoked transfer policy prevents approval"
        );
        assert!(extra.is_dir());
        window.revalidate_file_transfers(true);

        compare_pair(window, incoming, &mut client, control, &local, &remote, "");
        let ticket = review_removals(window, incoming, Side::Remote);
        let appeared = extra.join("appeared");
        private_file(&appeared, b"new after review").unwrap();
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmRemovals(ticket),
        ));
        let status = finish_sync(window, incoming, &mut client, control);
        assert!(
            status.contains("Removed 0 of 1 reviewed entries"),
            "{status}"
        );
        assert_eq!(fs::read(&appeared).unwrap(), b"new after review");
        assert!(
            trace.lock().unwrap().moves.is_empty(),
            "stale review cannot move a target"
        );
        fs::remove_file(&appeared).unwrap();

        compare_pair(window, incoming, &mut client, control, &local, &remote, "");
        let ticket = review_removals(window, incoming, Side::Remote);
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmRemovals(ticket),
        ));
        armed.store(true, Ordering::Release);
        let status = finish_sync(window, incoming, &mut client, control);
        assert!(
            !armed.load(Ordering::Acquire),
            "the move reached posix-rename@openssh.com"
        );
        assert!(
            status.contains("Removed 0 of 1 reviewed entries"),
            "{status}"
        );
        let trace = trace.lock().unwrap();
        let [(source, quarantined)] = trace.moves.as_slice() else {
            panic!("one atomic quarantine move was expected: {:?}", trace.moves);
        };
        assert_eq!(source, &extra);
        assert!(
            !extra.exists(),
            "a populated quarantine is never restored by overwriting a live path"
        );
        assert_eq!(
            fs::read(quarantined.join("late-child")).unwrap(),
            b"new at atomic move"
        );
        let stage = quarantined.parent().unwrap();
        assert_eq!(stage.parent(), Some(remote.as_path()));
        assert_eq!(
            fs::metadata(stage).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let stage_label = stage.as_os_str().as_bytes().escape_ascii().to_string();
        assert!(
            status.contains(&stage_label),
            "the recovery path must be shown: {status}"
        );
        assert!(
            !trace.removals.contains(&extra),
            "the live path must not receive REMOVE/RMDIR"
        );
    });

    assert_eq!(
        fs::read(local.join("common")).unwrap(),
        b"same common bytes"
    );
    assert_eq!(
        fs::read(remote.join("common")).unwrap(),
        b"same common bytes"
    );
    assert_eq!(fs::read_dir(&local).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(&remote).unwrap().count(),
        2,
        "only the recovery quarantine remains alongside common"
    );
}

#[derive(Default)]
struct RemovalTrace {
    moves: Vec<(PathBuf, PathBuf)>,
    removals: Vec<PathBuf>,
}

struct LateChild {
    input: ChildStdin,
    directory: PathBuf,
    armed: Arc<AtomicBool>,
    trace: Arc<Mutex<RemovalTrace>>,
}

impl Write for LateChild {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        const REMOVE: u8 = 13;
        const RMDIR: u8 = 15;
        const EXTENDED: u8 = 200;
        if let [kind, _, _, _, _, fields @ ..] = bytes {
            let mut fields = fields;
            match *kind {
                REMOVE | RMDIR => {
                    let path = wire_string(&mut fields);
                    self.trace
                        .lock()
                        .unwrap()
                        .removals
                        .push(PathBuf::from(OsStr::from_bytes(path)));
                }
                EXTENDED if wire_string(&mut fields) == b"posix-rename@openssh.com" => {
                    let source = PathBuf::from(OsStr::from_bytes(wire_string(&mut fields)));
                    let destination = PathBuf::from(OsStr::from_bytes(wire_string(&mut fields)));
                    if source == self.directory && self.armed.swap(false, Ordering::AcqRel) {
                        private_file(&self.directory.join("late-child"), b"new at atomic move")?;
                    }
                    self.trace.lock().unwrap().moves.push((source, destination));
                }
                _ => {}
            }
        }
        self.input.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
    }
}

fn wire_string<'a>(fields: &mut &'a [u8]) -> &'a [u8] {
    let size = u32::from_be_bytes(*fields.first_chunk::<4>().unwrap()) as usize;
    let (value, rest) = fields[4..].split_at(size);
    *fields = rest;
    value
}

fn roots(root: &Path) -> (PathBuf, PathBuf) {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let local = root.join("local");
    let remote = root.join("remote");
    private_dir(&local);
    private_dir(&remote);
    (local, remote)
}

fn private_dir(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

fn open_server(remote: &Path) -> (Server, Box<dyn Read + Send>) {
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let reader = Box::new(server.0.stdout.take().unwrap());
    (server, reader)
}

fn compare_pair(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    client: &mut Client,
    control: &Control,
    local: &Path,
    remote: &Path,
    excludes: &str,
) -> Arc<compare::folders::Review> {
    window
        .local
        .load(Arc::new(files::list_local(local).unwrap()));
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(remote.as_os_str().as_bytes().to_vec()),
        Vec::new(),
    )));
    let _ = window.update(WindowMessage::CompareFolders(None));
    let ticket = window.counter;
    if !excludes.is_empty() {
        use iced::widget::text_editor::{Action, Edit};
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::FolderExcludes(
                ticket,
                Action::Edit(Edit::Paste(Arc::new(excludes.into()))),
            ),
        ));
    }
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::StartFolders(ticket),
    ));
    let command = incoming.try_recv().expect("queue the folder comparison");
    assert!(matches!(command.operation, Operation::CompareFolders(_)));
    let reply = finish_command(window, client, control, command);
    let Reply::ComparedFolders(review) = reply.as_ref() else {
        panic!("expected folder review: {reply:?}");
    };
    Arc::clone(review)
}

fn review_removals(window: &mut SftpWindow, incoming: &mpsc::Receiver<Command>, to: Side) -> u64 {
    let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
        to,
    )));
    assert!(matches!(
        window.comparison,
        Some(compare::View::CopySetup(_))
    ));
    let initial = window.counter;
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::IncludeRemovals(initial, true),
    ));
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::ConfirmCopy(initial),
    ));
    assert!(
        incoming.try_recv().is_err(),
        "removals require their separate review"
    );
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::ReviewRemovals(initial),
    ));
    assert!(matches!(
        window.comparison,
        Some(compare::View::RemovalReview(_))
    ));
    let confirmation = window.counter;
    assert_ne!(initial, confirmation);
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::ConfirmRemovals(initial),
    ));
    assert!(incoming.try_recv().is_err(), "stale approval ticket");
    confirmation
}

fn finish_sync(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    client: &mut Client,
    control: &Control,
) -> String {
    let command = incoming
        .try_recv()
        .expect("queue the approved synchronization");
    assert!(matches!(command.operation, Operation::ApplySync(..)));
    let reply = finish_command(window, client, control, command);
    assert!(
        matches!(reply.as_ref(), Reply::SyncFinished(_)),
        "{reply:?}"
    );
    window.status().unwrap().to_owned()
}

fn finish_command(
    window: &mut SftpWindow,
    client: &mut Client,
    control: &Control,
    command: Command,
) -> Arc<Reply> {
    let ticket = window.busy.as_ref().unwrap().id;
    let reply = execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, reply.clone()));
    reply.unwrap()
}
