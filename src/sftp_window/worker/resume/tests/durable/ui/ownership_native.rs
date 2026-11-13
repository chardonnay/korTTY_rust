use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
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

struct RecordOwnership {
    input: ChildStdin,
    writes: OwnershipWrites,
}

type OwnershipWrites = Arc<Mutex<Vec<OwnershipWrite>>>;

#[derive(Debug, Eq, PartialEq)]
struct OwnershipWrite {
    path: Vec<u8>,
    fields: Vec<u8>,
}

impl OwnershipWrite {
    fn expected(path: &Path, fields: &[u32]) -> Self {
        Self {
            path: path.as_os_str().as_bytes().to_vec(),
            fields: fields.iter().flat_map(|word| word.to_be_bytes()).collect(),
        }
    }
}

impl Write for RecordOwnership {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.write_all(bytes)?;
        if let Some((path, fields)) = lsetstat(bytes) {
            self.writes.lock().unwrap().push(OwnershipWrite {
                path: path.to_vec(),
                fields: fields.to_vec(),
            });
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
    }
}

fn lsetstat(packet: &[u8]) -> Option<(&[u8], &[u8])> {
    let [200, _, _, _, _, fields @ ..] = packet else {
        return None;
    };
    let mut fields = fields;
    if wire_string(&mut fields)? != b"lsetstat@openssh.com" {
        return None;
    }
    let path = wire_string(&mut fields)?;
    Some((path, fields))
}

fn wire_string<'a>(fields: &mut &'a [u8]) -> Option<&'a [u8]> {
    let size = u32::from_be_bytes(*fields.first_chunk::<4>()?) as usize;
    let (value, remaining) = fields.get(4..)?.split_at_checked(size)?;
    *fields = remaining;
    Some(value)
}

fn recorded_server(root: &Path, writes: &OwnershipWrites) -> (Server, Client) {
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let input = RecordOwnership {
        input: server.0.stdin.take().unwrap(),
        writes: Arc::clone(writes),
    };
    let client: Client = Sftp::new(
        Box::new(server.0.stdout.take().unwrap()) as _,
        Box::new(input) as _,
    )
    .unwrap();
    assert!(client.supports_lsetstat());
    (server, client)
}

#[test]
fn local_ownership_browser_waits_for_the_exact_descriptor_review() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let file = root.join("payload");
    fs::write(&file, b"local ownership").unwrap();
    let original = fs::metadata(&file).unwrap();
    let mut client = super::client(|_| {});

    super::upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        let _ = window.update(WindowMessage::Select(
            Side::Local,
            EntryName::Local("payload".into()),
        ));
        let _ = window.update(WindowMessage::ChangeLocalOwnership);
        let _ = window.update(WindowMessage::OwnerId(original.uid().to_string()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            command.operation,
            Operation::ReviewLocalOwnership { .. }
        ));
        let reply = finish(window, &mut client, control, command).unwrap();
        let Reply::OwnershipReview(review) = reply.as_ref() else {
            panic!("expected a read-only local review");
        };
        assert_eq!(review.rows().len(), 1);
        assert_eq!(review.rows()[0].path, "payload");
        assert_eq!(review.uids(), (Some(original.uid()), None));
        assert!(incoming.try_recv().is_err());

        let _ = window.update(WindowMessage::ApplyOwnership);
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            command.operation,
            Operation::ApplyLocalOwnership(..)
        ));
        let reply = finish(window, &mut client, control, command).unwrap();
        assert!(matches!(
            reply.as_ref(),
            Reply::Changed {
                directory: Some(_),
                ..
            }
        ));
        assert!(window.status().unwrap().contains("1 already matched"));
    });
    let after = fs::metadata(&file).unwrap();
    assert_eq!(
        (after.ino(), after.uid(), after.gid()),
        (original.ino(), original.uid(), original.gid())
    );
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with lsetstat support"]
fn openssh_browser_changes_numeric_ownership_and_rejects_stale_targets() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let file = root.join("payload");
    let second = root.join("second");
    let target = root.join("other");
    let moved = root.join("previous");
    fs::write(&file, b"reviewed file").unwrap();
    fs::write(&second, b"second reviewed file").unwrap();
    fs::write(&target, b"another file").unwrap();
    let metadata = fs::metadata(&file).unwrap();
    let uid = metadata.uid();
    let original_gid = metadata.gid();
    let target_gid = fs::metadata(&target).unwrap().gid();
    let gid = supplementary_group(original_gid).unwrap_or(original_gid);
    eprintln!("Ownership fixture GID {original_gid} -> {gid}");

    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .arg("-d")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut client: Client = Sftp::new(
        Box::new(server.0.stdout.take().unwrap()) as _,
        Box::new(server.0.stdin.take().unwrap()) as _,
    )
    .unwrap();
    assert!(client.supports_lsetstat());

    super::upload::window(&context, |window, incoming, control| {
        list(window, &mut client, control, &root);
        select(window, b"payload");
        let _ = window.update(WindowMessage::ChangeOwnership);
        let _ = window.update(WindowMessage::GroupId(gid.to_string()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        assert!(
            incoming.try_recv().is_err(),
            "ownership requires the advertised extension"
        );
        control.ownership_supported.store(true, Ordering::Release);

        select(window, b"payload");
        let _ = window.update(WindowMessage::ChangeOwnership);
        let _ = window.update(WindowMessage::GroupId("test-group".into()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        assert!(
            incoming.try_recv().is_err(),
            "names require a command route"
        );
        let _ = window.update(WindowMessage::GroupId(original_gid.to_string()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        let numeric = incoming
            .try_recv()
            .expect("the same form accepts a numeric ID");
        assert!(matches!(
            numeric.operation,
            Operation::ChangeOwnership { .. }
        ));
        let reply = finish(window, &mut client, control, numeric).unwrap();
        assert!(matches!(reply.as_ref(), Reply::Changed { .. }));

        select(window, b"payload");
        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"second".to_vec()),
            true,
        ));
        let names = ["payload", "second"];
        let command = queue(window, incoming, &names, None, Some(gid));
        let reply = finish(window, &mut client, control, command);
        assert!(
            matches!(reply.as_deref(), Ok(Reply::Changed { .. })),
            "{reply:?}"
        );
        assert_ownership(window, &mut client, &names, uid, gid);

        select(window, b"payload");
        let command = queue(window, incoming, &["payload"], Some(uid), None);
        let reply = finish(window, &mut client, control, command);
        assert!(
            matches!(reply.as_deref(), Ok(Reply::Changed { .. })),
            "{reply:?}"
        );
        assert_ownership(window, &mut client, &["payload"], uid, gid);

        select(window, b"payload");
        let _ = window.update(WindowMessage::ChangeOwnership);
        let _ = window.update(WindowMessage::GroupId(original_gid.to_string()));
        assert!(!window.incognito());
        window.set_incognito(true);
        let _ = window.update(WindowMessage::ApplyOwnership);
        assert!(
            incoming.try_recv().is_err(),
            "a privacy mode change invalidates the review"
        );

        select(window, b"payload");
        let _ = window.update(WindowMessage::ChangeOwnership);
        let _ = window.update(WindowMessage::GroupId(original_gid.to_string()));
        window.remote.selected.clear();
        window
            .remote
            .selected
            .insert(EntryName::Remote(b"other".to_vec()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        assert!(
            incoming.try_recv().is_err(),
            "selection change invalidates the review"
        );
        let _ = window.update(WindowMessage::Dismiss);

        select(window, b"payload");
        let command = queue(window, incoming, &["payload"], None, Some(gid));
        fs::rename(&file, &moved).unwrap();
        symlink("other", &file).unwrap();
        let link_gid = fs::symlink_metadata(&file).unwrap().gid();
        let reply = finish(window, &mut client, control, command);
        assert!(
            reply.is_err(),
            "replacement symlink invalidates the review: {reply:?}"
        );

        list(window, &mut client, control, &root);
        select(window, b"payload");
        let _ = window.update(WindowMessage::ChangeOwnership);
        let _ = window.update(WindowMessage::GroupId(gid.to_string()));
        let _ = window.update(WindowMessage::ApplyOwnership);
        assert!(
            incoming.try_recv().is_err(),
            "a listed symlink cannot be reviewed"
        );
        assert_eq!(fs::symlink_metadata(&file).unwrap().gid(), link_gid);
    });

    assert_eq!(fs::read(&moved).unwrap(), b"reviewed file");
    assert_eq!(fs::metadata(&moved).unwrap().gid(), gid);
    assert_eq!(fs::read(&second).unwrap(), b"second reviewed file");
    assert_eq!(fs::metadata(&second).unwrap().gid(), gid);
    assert_eq!(fs::read_link(&file).unwrap(), Path::new("other"));
    assert_eq!(fs::read(&target).unwrap(), b"another file");
    assert_eq!(fs::metadata(&target).unwrap().gid(), target_gid);
}

#[test]
#[ignore = "requires OpenSSH sftp-server with lsetstat and KORTTY_TEST_OWNERSHIP_GID"]
fn openssh_browser_reviews_permissions_and_group_on_sftp_only_route() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let gid: u32 = std::env::var("KORTTY_TEST_OWNERSHIP_GID")
        .expect("set KORTTY_TEST_OWNERSHIP_GID to a distinct supplementary group")
        .parse()
        .unwrap();
    let cases = [
        (
            "mixed",
            b"mixed ownership and permissions".as_slice(),
            0o750,
            Some(gid),
            0o640,
        ),
        ("mode-only", b"keep ownership", 0o644, None, 0o600),
    ];
    let original = cases.map(|(name, contents, old_mode, _, _)| {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(old_mode)).unwrap();
        fs::metadata(&path).unwrap()
    });
    assert_ne!(
        gid,
        original[0].gid(),
        "the fixture must change a real group"
    );
    eprintln!(
        "Combined ownership fixture GID {} -> {gid}",
        original[0].gid()
    );

    let writes = Arc::new(Mutex::new(Vec::new()));
    let (_server, mut client) = recorded_server(&root, &writes);
    super::upload::window(&context, |window, incoming, control| {
        assert!(window.worker.as_ref().unwrap().exec_opener().is_none());
        control.ownership_supported.store(true, Ordering::Release);
        list(window, &mut client, control, &root);
        for ((name, contents, old_mode, requested_gid, mode), before) in
            cases.into_iter().zip(original.iter())
        {
            select(window, name.as_bytes());
            let _ = window.update(WindowMessage::ChangeOwnership);
            if let Some(gid) = requested_gid {
                let _ = window.update(WindowMessage::GroupId(gid.to_string()));
            }
            let _ = window.update(WindowMessage::OwnershipMode(format!("{mode:03o}")));
            let _ = window.update(WindowMessage::ApplyOwnership);
            let command = incoming
                .try_recv()
                .expect("the read-only permissions review");
            assert!(matches!(
                &command.operation,
                Operation::ReviewRemoteOwnership(..)
            ));
            let writes_before_review = writes.lock().unwrap().len();
            let reply = finish(window, &mut client, control, command).unwrap();
            let Reply::OwnershipReview(review) = reply.as_ref() else {
                panic!("expected a read-only combined review: {reply:?}");
            };
            assert_eq!(review.uids(), (None, requested_gid));
            assert_eq!(review.mode(), Some(mode));
            assert_eq!(review.rows().len(), 1);
            assert_eq!(review.rows()[0].mode & 0o7777, old_mode);
            assert_eq!(writes.lock().unwrap().len(), writes_before_review);

            let reply = apply_review(window, incoming, &mut client, control).unwrap();
            let Reply::Changed {
                directory: Some(directory),
                note,
            } = reply.as_ref()
            else {
                panic!("expected a verified ownership and permissions result: {reply:?}");
            };
            assert!(note.contains("1 changed, 0 already matched"), "{note}");
            let expected_gid = requested_gid.unwrap_or(before.gid());
            let attrs = client.lstat(name).unwrap();
            assert_eq!(
                (attrs.uid, attrs.gid),
                (Some(before.uid()), Some(expected_gid))
            );
            assert_eq!(attrs.permissions.unwrap() & 0o7777, mode);
            let row = directory
                .entries
                .iter()
                .find(|entry| entry.display == name)
                .unwrap();
            assert_eq!(
                (row.owner, row.group, row.mode),
                (attrs.uid, attrs.gid, attrs.permissions)
            );

            let path = root.join(name);
            let after = fs::metadata(&path).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            assert_eq!(
                (after.uid(), after.gid(), after.mode() & 0o7777),
                (before.uid(), expected_gid, mode)
            );
            assert_eq!(fs::read(path).unwrap(), contents);
        }
    });

    let expected = [
        OwnershipWrite::expected(&root.join("mixed"), &[2, u32::MAX, gid]),
        OwnershipWrite::expected(&root.join("mixed"), &[4, 0o640]),
        OwnershipWrite::expected(&root.join("mode-only"), &[4, 0o600]),
    ];
    assert_eq!(writes.lock().unwrap().as_slice(), expected.as_slice());
}

#[test]
#[ignore = "requires OpenSSH sftp-server with lsetstat and a distinct supplementary group"]
fn openssh_browser_reviews_recursive_ownership_and_applies_children_before_parents() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let targets = ["tree/branch/payload", "tree/branch", "tree"];
    let stale = ["stale/branch/payload", "stale/branch", "stale"];
    for [file, directory, _] in [targets, stale] {
        fs::create_dir_all(root.join(directory)).unwrap();
        fs::write(root.join(file), b"reviewed file").unwrap();
    }
    let outside = root.join("outside");
    let link = root.join("tree/branch/link");
    fs::write(&outside, b"outside selection").unwrap();
    symlink("../../outside", &link).unwrap();
    let metadata = fs::metadata(&outside).unwrap();
    let (uid, original_gid) = (metadata.uid(), metadata.gid());
    let link_gid = fs::symlink_metadata(&link).unwrap().gid();
    let gid = supplementary_group(original_gid).expect("run with a distinct supplementary group");
    eprintln!("Recursive ownership fixture GID {original_gid} -> {gid}");

    let writes = Arc::new(Mutex::new(Vec::new()));
    let (_server, mut client) = recorded_server(&root, &writes);

    super::upload::window(&context, |window, incoming, control| {
        control.ownership_supported.store(true, Ordering::Release);
        list(window, &mut client, control, &root);
        select(window, b"tree");
        let _ = window.update(WindowMessage::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"stale".to_vec()),
            true,
        ));
        let reply = review_recursive(window, incoming, &mut client, control, gid);
        let Ok(Reply::OwnershipReview(plan)) = reply.as_deref() else {
            panic!("expected the two-root review: {reply:?}");
        };
        assert_eq!(plan.rows().len(), targets.len() + stale.len());
        assert_eq!(plan.skipped(), 1);
        assert!(writes.lock().unwrap().is_empty(), "review is read-only");

        fs::write(root.join("stale/branch/late"), b"created after review").unwrap();
        let reply = apply_review(window, incoming, &mut client, control);
        assert!(reply.is_err(), "the recursive manifest changed: {reply:?}");
        assert!(
            writes.lock().unwrap().is_empty(),
            "no root may change after a stale review"
        );
        for name in targets
            .into_iter()
            .chain(stale)
            .chain(["stale/branch/late"])
        {
            let attrs = client.lstat(name).unwrap();
            assert_eq!(
                (attrs.uid, attrs.gid),
                (Some(uid), Some(original_gid)),
                "{name}"
            );
        }

        list(window, &mut client, control, &root);
        select(window, b"tree");
        let reply = review_recursive(window, incoming, &mut client, control, gid);
        let Ok(Reply::OwnershipReview(plan)) = reply.as_deref() else {
            panic!("expected a fresh recursive review: {reply:?}");
        };
        assert_eq!(plan.rows().len(), targets.len());
        assert_eq!(plan.skipped(), 1);
        assert!(
            writes.lock().unwrap().is_empty(),
            "fresh review is read-only"
        );

        let reply = apply_review(window, incoming, &mut client, control);
        assert!(
            matches!(reply.as_deref(), Ok(Reply::Changed { .. })),
            "{reply:?}"
        );
        let expected =
            targets.map(|path| OwnershipWrite::expected(&root.join(path), &[2, u32::MAX, gid]));
        assert_eq!(writes.lock().unwrap().as_slice(), expected.as_slice());
        assert_ownership(window, &mut client, &["tree"], uid, gid);
        for path in targets {
            let attrs = client.lstat(path).unwrap();
            assert_eq!((attrs.uid, attrs.gid), (Some(uid), Some(gid)), "{path}");
        }
    });

    assert_eq!(fs::read_link(&link).unwrap(), Path::new("../../outside"));
    assert_eq!(fs::symlink_metadata(&link).unwrap().gid(), link_gid);
    assert_eq!(fs::read(&outside).unwrap(), b"outside selection");
    assert_eq!(fs::metadata(&outside).unwrap().gid(), original_gid);
}

fn review_recursive(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    client: &mut Client,
    control: &Control,
    gid: u32,
) -> Result<Arc<Reply>> {
    let _ = window.update(WindowMessage::ChangeOwnership);
    let _ = window.update(WindowMessage::GroupId(gid.to_string()));
    let _ = window.update(WindowMessage::OwnershipRecursive(true));
    let _ = window.update(WindowMessage::ApplyOwnership);
    let command = incoming.try_recv().expect("the recursive ownership review");
    assert!(matches!(
        &command.operation,
        Operation::ReviewRemoteOwnership(..)
    ));
    finish(window, client, control, command)
}

fn apply_review(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    client: &mut Client,
    control: &Control,
) -> Result<Arc<Reply>> {
    let _ = window.update(WindowMessage::ApplyOwnership);
    let command = incoming
        .try_recv()
        .expect("the reviewed server ownership operation");
    assert!(matches!(
        &command.operation,
        Operation::ApplyRemoteOwnership(..)
    ));
    finish(window, client, control, command)
}

fn supplementary_group(original: u32) -> Option<u32> {
    let groups = Process::new("id").arg("-G").output().unwrap();
    assert!(groups.status.success());
    std::str::from_utf8(&groups.stdout)
        .unwrap()
        .split_ascii_whitespace()
        .map(|group| group.parse::<u32>().unwrap())
        .find(|group| *group != original)
}

fn list(window: &mut SftpWindow, client: &mut Client, control: &Control, root: &Path) {
    let remote = Location::Remote(root.as_os_str().as_bytes().to_vec());
    let Reply::Listed(listing) = execute(
        client,
        Operation::Browse(remote),
        control,
        RemoteEditorPolicy::Deny,
    )
    .unwrap() else {
        panic!("expected a live remote listing");
    };
    window.remote.load(listing);
}

fn select(window: &mut SftpWindow, name: &[u8]) {
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(name.to_vec()),
    ));
}

fn queue(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    names: &[&str],
    uid: Option<u32>,
    gid: Option<u32>,
) -> Command {
    let _ = window.update(WindowMessage::ChangeOwnership);
    let _ = window.update(WindowMessage::OwnerId(
        uid.map_or_else(String::new, |id| id.to_string()),
    ));
    let _ = window.update(WindowMessage::GroupId(
        gid.map_or_else(String::new, |id| id.to_string()),
    ));
    let _ = window.update(WindowMessage::ApplyOwnership);
    let command = incoming
        .try_recv()
        .expect("the reviewed ownership operation");
    assert!(matches!(
        &command.operation,
        Operation::ChangeOwnership { selected, uid: new_uid, gid: new_gid, .. }
            if selected.len() == names.len()
                && selected.iter().all(|entry| names.contains(&entry.display.as_str()))
                && *new_uid == uid && *new_gid == gid
    ));
    command
}

fn assert_ownership(window: &SftpWindow, client: &mut Client, names: &[&str], uid: u32, gid: u32) {
    let listing = window.remote.directory.as_ref().unwrap();
    for &name in names {
        let attrs = client.lstat(name).unwrap();
        assert_eq!((attrs.uid, attrs.gid), (Some(uid), Some(gid)));
        let row = listing
            .entries
            .iter()
            .find(|entry| entry.display == name)
            .unwrap();
        assert_eq!((row.owner, row.group), (Some(uid), Some(gid)));
    }
}

fn finish(
    window: &mut SftpWindow,
    client: &mut Client,
    control: &Control,
    command: Command,
) -> Result<Arc<Reply>> {
    let ticket = window.busy.as_ref().unwrap().id;
    let result =
        execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, result.clone()));
    result
}
