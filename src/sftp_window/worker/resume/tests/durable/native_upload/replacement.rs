//! Reviewed changed-file batches through the desktop workflow and real OpenSSH.

use std::time::UNIX_EPOCH;

use crate::sftp_window::{Message as WindowMessage, Side, compare};

use super::*;

mod foreign;
mod resume;

const NAMES: [&str; 2] = ["first.bin", "second.bin"];
const REMOTE_NAMES: &[&[u8]] = &[NAMES[0].as_bytes(), NAMES[1].as_bytes()];
const OLD: [&[u8]; 2] = [b"first server original", b"second server original"];
const UP: [&[u8]; 2] = [b"first local edit", b"second local edit"];
const DOWN: [&[u8]; 2] = [
    b"first server edit after upload",
    b"second server edit after upload",
];
const NEXT: [&[u8]; 2] = [
    b"first local edit after download",
    b"second local edit after download",
];
const LATE: &[u8] = b"a concurrent server writer owns this name";
const MODIFIED: u32 = 1_760_000_000;
const MIXED_NAMES: &[&[u8]] = &[b"first.bin", b"new-upload.bin", b"new-download.bin"];

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and REPLACEMENT_SCRATCH"]
fn live_sftp_reviewed_changed_batch_replaces_both_directions_without_clobbering() {
    let fixture = SshFixture::read("REPLACEMENT_SCRATCH", b"kortty-replacement-live-");
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let local = root.join("files");
    fs::create_dir(&local).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&local, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let context = fixture.context(root.join("profile"));
    let mut scratch = RemoteScratch::create(&fixture, &context, REMOTE_NAMES);
    let trace = Arc::new(Trace::new(scratch.path.clone()));
    scratch.replacement = Some(Arc::clone(&trace));
    let paths = NAMES.map(|name| files::remote_join(&scratch.path, name.as_bytes()));
    let local_paths = NAMES.map(|name| local.join(name));
    for (path, bytes) in local_paths.iter().zip(UP) {
        write_local(path, bytes, MODIFIED + 10);
    }

    let (mut setup_session, mut setup_client) =
        fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
    let modes: [u32; 2] = std::array::from_fn(|index| {
        seed_remote(&mut setup_client, &paths[index], OLD[index], MODIFIED);
        let mode = if index == 0 { 0o640 } else { 0o751 };
        setup_client.set_permissions(&paths[index], mode).unwrap();
        if index == 0
            && let Err(error) = setup_client.set_permissions(&paths[index], mode | 0o4000)
        {
            use kortty_ssh::sftp::StatusCode;
            assert!(
                matches!(error, Error::Status(status) if matches!(status.code,
                StatusCode::PermissionDenied | StatusCode::Unsupported | StatusCode::Failure))
            );
        }
        let stored = setup_client
            .lstat(&paths[index])
            .unwrap()
            .permissions
            .unwrap()
            & 0o7777;
        assert!(stored == mode || index == 0 && stored == (mode | 0o4000));
        stored
    });
    drop(setup_client);
    setup_session.close().unwrap();

    let (mut session, mut client) = traced_client(&fixture, &trace);
    assert!(client.supports_atomic_replace() && client.supports_hardlink());
    let uploaded = batch(&context, &mut client, &local, &scratch.path, Side::Remote);
    require_completed(&uploaded);
    let writes = trace.writes.lock().unwrap().clone();
    assert_eq!(writes.stages.len(), 4);
    assert_eq!(writes.moves.len(), 2);
    assert_eq!(writes.links.len(), 2);
    assert_eq!(writes.removals.len(), 2);
    for (index, path) in paths.iter().enumerate() {
        let backup = &writes
            .moves
            .iter()
            .find(|(from, _)| from == path)
            .unwrap()
            .1;
        let stage = &writes.links.iter().find(|(_, to)| to == path).unwrap().0;
        assert_eq!(
            read_remote(&mut client, path),
            (UP[index].to_vec(), modes[index], MODIFIED + 10)
        );
        assert_eq!(
            read_remote(&mut client, backup),
            (OLD[index].to_vec(), modes[index], MODIFIED)
        );
        assert!(
            uploaded.contains(&backup.escape_ascii().to_string()),
            "{uploaded}"
        );
        assert!(writes.removals.contains(stage));
        let (directory, _) = remote_components(stage).unwrap();
        assert!(client.lstat(directory).unwrap_err().is_not_found());
    }
    drop(client);
    session.close().unwrap();

    let (mut session, mut client) = fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
    for (path, bytes) in paths.iter().zip(DOWN) {
        client.remove_file(path).unwrap();
        seed_remote(&mut client, path, bytes, MODIFIED + 20);
    }
    let downloaded = batch(&context, &mut client, &local, &scratch.path, Side::Local);
    require_completed(&downloaded);
    let local_backups: Vec<_> = fs::read_dir(&local)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| stage_name(path.file_name().unwrap().as_encoded_bytes()))
        .collect();
    assert_eq!(local_backups.len(), 2);
    for (index, path) in local_paths.iter().enumerate() {
        let backup = local_backups
            .iter()
            .map(|directory| directory.join(NAMES[index]))
            .find(|candidate| candidate.is_file())
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), DOWN[index]);
        assert_eq!(modified_local(path), MODIFIED + 20);
        assert_eq!(fs::read(&backup).unwrap(), UP[index]);
        assert_eq!(modified_local(&backup), MODIFIED + 10);
        assert!(
            downloaded.contains(
                &backup
                    .as_os_str()
                    .as_encoded_bytes()
                    .escape_ascii()
                    .to_string()
            ),
            "{downloaded}"
        );
        write_local(path, NEXT[index], MODIFIED + 30);
    }
    drop(client);
    session.close().unwrap();

    // A second pinned connection creates a late destination after the original
    // has moved into quarantine, just before the actual hard-link publication.
    let (mut competing_session, competing_client) =
        fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
    *trace.contender.lock().unwrap() = Some((paths[0].clone(), competing_client));
    let (mut session, mut client) = traced_client(&fixture, &trace);
    let collision = batch(&context, &mut client, &local, &scratch.path, Side::Remote);
    assert!(
        collision.contains("Replaced 0 of 2 reviewed files · 1 failed · 1 not attempted"),
        "{collision}"
    );
    assert!(trace.contender.lock().unwrap().is_none());
    assert_eq!(read_remote(&mut client, &paths[0]).0, LATE);
    assert_eq!(read_remote(&mut client, &paths[1]).0, DOWN[1]);
    let writes = trace.writes.lock().unwrap().clone();
    assert_eq!(writes.stages.len(), 6);
    assert_eq!(writes.moves.len(), 3);
    assert_eq!(
        writes.links.len(),
        4,
        "publish and rollback both use no-replace links"
    );
    assert_eq!(writes.removals.len(), 2);
    let recovery = &writes.moves.last().unwrap().1;
    assert_eq!(read_remote(&mut client, recovery).0, DOWN[0]);
    assert!(
        collision.contains(&recovery.escape_ascii().to_string()),
        "{collision}"
    );
    competing_session.close().unwrap();

    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();
    println!(
        "OpenSSH verified two-file batches in each direction, remote modes {:04o}/{:04o}, and preserved a late destination and its original",
        modes[0], modes[1]
    );
}

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and COMBINED_SYNC_SCRATCH"]
fn live_sftp_reviewed_combined_sync_copies_and_replaces_with_explicit_removals() {
    let fixture = SshFixture::read("COMBINED_SYNC_SCRATCH", b"kortty-sync-live-");
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let local = root.join("files");
    fs::create_dir(&local).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&local, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let context = fixture.context(root.join("profile"));
    let mut scratch = RemoteScratch::create(&fixture, &context, MIXED_NAMES);
    let trace = Arc::new(Trace::new(scratch.path.clone()));
    scratch.replacement = Some(Arc::clone(&trace));
    let changed = files::remote_join(&scratch.path, MIXED_NAMES[0]);
    let uploaded = files::remote_join(&scratch.path, MIXED_NAMES[1]);
    let downloaded = files::remote_join(&scratch.path, MIXED_NAMES[2]);
    let local_changed = local.join("first.bin");
    let local_upload = local.join("new-upload.bin");
    let local_download = local.join("new-download.bin");
    let local_extra = local.join("local-extra.bin");
    write_local(&local_changed, UP[0], MODIFIED + 10);
    write_local(&local_upload, b"new file for the server", MODIFIED + 11);

    let (mut session, mut client) = traced_client(&fixture, &trace);
    seed_remote(&mut client, &changed, OLD[0], MODIFIED);
    seed_remote(
        &mut client,
        &downloaded,
        b"new file for this computer",
        MODIFIED + 20,
    );
    client.set_permissions(&changed, 0o751).unwrap();

    let report = run_review(
        &context,
        &mut client,
        &local,
        &scratch.path,
        Side::Remote,
        ReviewAction::Combined { remove: false },
    );
    require_combined_completed(&report);
    assert_eq!(
        read_remote(&mut client, &uploaded),
        (b"new file for the server".to_vec(), 0o600, MODIFIED + 11)
    );
    assert_eq!(
        read_remote(&mut client, &changed),
        (UP[0].to_vec(), 0o751, MODIFIED + 10)
    );
    assert_eq!(
        read_remote(&mut client, &downloaded).0,
        b"new file for this computer"
    );
    let writes = trace.writes.lock().unwrap().clone();
    assert_eq!(writes.moves.len(), 1);
    assert_eq!(writes.moves[0].0, changed);
    assert_eq!(read_remote(&mut client, &writes.moves[0].1).0, OLD[0]);
    assert_eq!(writes.links.len(), 2);
    assert_eq!(writes.links[0].1, uploaded);
    assert_eq!(writes.links[1].1, changed);
    assert!(
        report.contains(&writes.moves[0].1.escape_ascii().to_string()),
        "{report}"
    );

    client.remove_file(&changed).unwrap();
    seed_remote(&mut client, &changed, DOWN[0], MODIFIED + 21);
    write_local(&local_extra, b"destination-only file", MODIFIED + 12);
    let report = run_review(
        &context,
        &mut client,
        &local,
        &scratch.path,
        Side::Local,
        ReviewAction::Combined { remove: true },
    );
    require_combined_completed(&report);
    assert!(
        report.contains("Removed 1 of 1 reviewed entries"),
        "{report}"
    );
    assert!(!local_extra.exists());
    assert_eq!(fs::read(&local_changed).unwrap(), DOWN[0]);
    assert_eq!(modified_local(&local_changed), MODIFIED + 21);
    assert_eq!(
        fs::read(&local_download).unwrap(),
        b"new file for this computer"
    );
    assert_eq!(modified_local(&local_download), MODIFIED + 20);
    let backup = fs::read_dir(&local)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("first.bin"))
        .find(|path| path.is_file())
        .unwrap();
    assert_eq!(fs::read(&backup).unwrap(), UP[0]);

    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();
    println!(
        "OpenSSH verified mixed same-directory sync in both directions and separate removal consent"
    );
}

fn write_local(path: &Path, bytes: &[u8], modified: u32) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(bytes).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(modified)))
        .unwrap();
}

fn modified_local(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

fn seed_remote(client: &mut Client, path: &[u8], bytes: &[u8], modified: u32) {
    client
        .upload_private_with_modified(
            path,
            bytes.len() as u64,
            modified,
            io::Cursor::new(bytes),
            |_, _| Ok(()),
            || false,
        )
        .unwrap();
}

fn read_remote(client: &mut Client, path: &[u8]) -> (Vec<u8>, u32, u32) {
    let stamp = client.lstat(path).unwrap();
    let mut bytes = Vec::new();
    client.download_regular(path, stamp, &mut bytes).unwrap();
    (
        bytes,
        stamp.permissions.unwrap() & 0o7777,
        stamp.modified.unwrap(),
    )
}

fn traced_client(fixture: &SshFixture, trace: &Arc<Trace>) -> (SshSession, Client) {
    let mut probe = Probe::new(Cut::None);
    probe.replacement = Some(Arc::clone(trace));
    fixture.open(Arc::new(probe)).unwrap()
}

fn require_completed(report: &str) {
    assert!(
        report.contains("Replaced 2 of 2 reviewed files · 0 failed · 0 not attempted"),
        "{report}"
    );
}

fn require_combined_completed(report: &str) {
    assert!(
        report.starts_with(
            "Copied 1 of 1 new files · Created 0 of 0 folders · Replaced 1 of 1 reviewed files"
        ),
        "{report}"
    );
}

fn batch(context: &Context, client: &mut Client, local: &Path, remote: &[u8], to: Side) -> String {
    run_review(context, client, local, remote, to, ReviewAction::Changed)
}

#[derive(Clone, Copy)]
enum ReviewAction {
    Changed,
    Combined { remove: bool },
    Foreign,
    StageResume { disconnected: bool },
}

fn run_review(
    context: &Context,
    client: &mut Client,
    local: &Path,
    remote: &[u8],
    to: Side,
    action: ReviewAction,
) -> String {
    let (commands, incoming) = mpsc::sync_channel(1);
    let (_finished, done) = bounded(1);
    let control = Arc::new(control(context));
    let mut window = SftpWindow::new(context.route.clone(), None, true, RemoteEditorPolicy::Deny);
    window.worker = Some(Worker {
        commands,
        control: Arc::clone(&control),
        done,
        editor_policy: RemoteEditorPolicy::Deny,
        exec: None,
        sftp: None,
    });
    window
        .local
        .load(Arc::new(files::list_local(local).unwrap()));
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(remote.to_vec()),
        Vec::new(),
    )));
    let _ = window.update(WindowMessage::CompareFolders(None));
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::StartFolders(window.counter),
    ));
    let command = incoming.try_recv().unwrap();
    assert!(matches!(command.operation, Operation::CompareFolders(_)));
    let reply = finish(&mut window, client, &control, command);
    let Reply::ComparedFolders(review) = reply.as_ref() else {
        panic!("{reply:?}")
    };
    let changed = if matches!(action, ReviewAction::Changed) {
        2
    } else {
        1
    };
    assert_eq!(review.replacement_candidates(to).count(), changed);
    let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
        to,
    )));
    assert!(matches!(
        window.comparison,
        Some(compare::View::CopySetup(_))
    ));
    let setup = window.counter;
    let prepare = match action {
        ReviewAction::Changed => compare::Message::ReviewReplacementBatch(setup),
        ReviewAction::Foreign | ReviewAction::StageResume { .. } => {
            compare::Message::ReviewReplacement(setup, 0)
        }
        ReviewAction::Combined { .. } => {
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::IncludeRemovals(setup, true),
            ));
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ReviewCombined(setup - 1),
            ));
            assert!(incoming.try_recv().is_err());
            compare::Message::ReviewCombined(setup)
        }
    };
    let _ = window.update(WindowMessage::ComparisonView(prepare));
    let command = incoming.try_recv().unwrap();
    assert!(matches!(
        command.operation,
        Operation::ReviewSyncReplacement(..)
    ));
    let reply = finish(&mut window, client, &control, command);
    let Reply::SyncReplacementReview(plan) = reply.as_ref() else {
        panic!("{reply:?}")
    };
    assert_eq!(
        plan.is_batch(),
        matches!(
            action,
            ReviewAction::Changed | ReviewAction::Combined { .. }
        )
    );
    assert_eq!(plan.direction(), to);
    assert_eq!(plan.entries().len(), changed);
    assert!(plan.omitted().is_empty());
    assert_eq!(
        plan.entries().iter().any(|entry| entry.is_in_place()),
        matches!(action, ReviewAction::Foreign)
    );
    assert!(
        plan.entries()
            .iter()
            .all(|entry| entry.digests()[0] != entry.digests()[1])
    );
    let review = window.counter;
    assert_ne!(setup, review);
    let _ = window.update(WindowMessage::ComparisonView(
        compare::Message::ConfirmReplacement(setup),
    ));
    assert!(incoming.try_recv().is_err());
    let (confirmation, removes) = match action {
        ReviewAction::Changed | ReviewAction::Foreign | ReviewAction::StageResume { .. } => {
            assert!(plan.combined().is_none());
            (compare::Message::ConfirmReplacement(review), false)
        }
        ReviewAction::Combined { remove } => {
            let combined = plan.combined().unwrap();
            assert_eq!(combined.copy_counts(), (1, 0));
            assert_eq!(combined.removal_count(), 1);
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCombinedRemovals(review),
            ));
            assert!(incoming.try_recv().is_err());
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ReviewCombinedRemovals(review),
            ));
            let removal = window.counter;
            assert_ne!(removal, review);
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCombinedRemovals(review),
            ));
            assert!(incoming.try_recv().is_err());
            let confirmation = if remove {
                compare::Message::ConfirmCombinedRemovals(removal)
            } else {
                compare::Message::ConfirmReplacement(removal)
            };
            (confirmation, remove)
        }
    };
    let _ = window.update(WindowMessage::ComparisonView(confirmation));
    let command = incoming.try_recv().unwrap();
    assert!(matches!(
        command.operation,
        Operation::ApplySyncReplacement(.., remove) if remove == removes
    ));
    let reply = finish(&mut window, client, &control, command);
    let Reply::SyncFinished(report) = reply.as_ref() else {
        panic!("{reply:?}")
    };
    assert_eq!(
        report.disconnected,
        matches!(action, ReviewAction::StageResume { disconnected: true }),
        "{}",
        report.note
    );
    report.note.clone()
}

fn finish(
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

#[derive(Clone, Default)]
struct Writes {
    stages: Vec<Vec<u8>>,
    moves: Vec<(Vec<u8>, Vec<u8>)>,
    links: Vec<(Vec<u8>, Vec<u8>)>,
    removals: Vec<Vec<u8>>,
}

pub(super) struct Trace {
    root: Vec<u8>,
    writes: Mutex<Writes>,
    contender: Mutex<Option<(Vec<u8>, Client)>>,
}

impl Trace {
    fn new(root: Vec<u8>) -> Self {
        Self {
            root,
            writes: Mutex::default(),
            contender: Mutex::default(),
        }
    }

    pub(super) fn before_request(&self, packet: &[u8]) -> io::Result<()> {
        const REMOVE: u8 = 13;
        const MKDIR: u8 = 14;
        const RENAME: u8 = 18;
        const EXTENDED: u8 = 200;

        let [kind, _, _, _, _, fields @ ..] = packet else {
            return Ok(());
        };
        let mut fields = fields;
        let mut writes = self.writes.lock().unwrap();
        match *kind {
            REMOVE => writes.removals.push(wire_string(&mut fields).to_vec()),
            MKDIR => {
                let path = wire_string(&mut fields);
                let (parent, name) = remote_components(path).unwrap();
                assert_eq!(parent, self.root);
                assert!(stage_name(name) && writes.stages.len() < 8);
                writes.stages.push(path.to_vec());
            }
            RENAME => panic!("reviewed replacement must not use ordinary SFTP rename"),
            EXTENDED => match wire_string(&mut fields) {
                b"posix-rename@openssh.com" => {
                    let from = wire_string(&mut fields);
                    let to = wire_string(&mut fields);
                    let (directory, name) = remote_components(to).unwrap();
                    assert_eq!(name, b"payload");
                    assert!(writes.stages.iter().any(|stage| stage == directory));
                    writes.moves.push((from.to_vec(), to.to_vec()));
                }
                b"hardlink@openssh.com" => {
                    let from = wire_string(&mut fields);
                    let to = wire_string(&mut fields);
                    writes.links.push((from.to_vec(), to.to_vec()));
                    let mut contender = self.contender.lock().unwrap();
                    if contender.as_ref().is_some_and(|(target, _)| target == to) {
                        let (_, mut client) = contender.take().unwrap();
                        assert!(client.lstat(to).unwrap_err().is_not_found());
                        client.upload_new(to, LATE).map_err(io::Error::other)?;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    pub(super) fn cleanup(
        &self,
        client: &mut Client,
        owner: (u32, u32),
    ) -> std::result::Result<(), String> {
        let writes = self
            .writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for directory in writes.stages {
            let (parent, name) = remote_components(&directory)
                .ok_or_else(|| "invalid observed staging path".to_owned())?;
            if parent != self.root || !stage_name(name) {
                return Err("unexpected replacement test stage name".into());
            }
            match client.lstat(&directory) {
                Ok(stamp)
                    if stamp.kind() == FileKind::Directory
                        && stamp.permissions.map(|mode| mode & 0o7777) == Some(0o700)
                        && stamp.uid.zip(stamp.gid) == Some(owner)
                        && client
                            .canonicalize(&directory)
                            .map_err(|error| error.to_string())?
                            == directory => {}
                Err(error) if error.is_not_found() => continue,
                _ => return Err("unexpected replacement test stage identity".into()),
            }
            let payload = files::remote_join(&directory, b"payload");
            match client.lstat(&payload) {
                Ok(stamp)
                    if stamp.kind() == FileKind::File
                        && stamp.uid == Some(owner.0)
                        && client
                            .canonicalize(&payload)
                            .map_err(|error| error.to_string())?
                            == payload =>
                {
                    client
                        .remove_file(&payload)
                        .map_err(|error| error.to_string())?;
                }
                Err(error) if error.is_not_found() => {}
                _ => return Err("unexpected replacement test stage payload".into()),
            }
            client
                .remove_dir(&directory)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn wire_string<'a>(fields: &mut &'a [u8]) -> &'a [u8] {
    let length = u32::from_be_bytes(*fields.first_chunk::<4>().unwrap()) as usize;
    let (value, remaining) = fields[4..].split_at(length);
    *fields = remaining;
    value
}

fn stage_name(name: &[u8]) -> bool {
    files::replacement_stage_id(name).is_some()
        || name
            .strip_prefix(b".kortty-")
            .and_then(|name| name.strip_suffix(b".kortty-part"))
            .is_some_and(|nonce| {
                nonce.len() == 32
                    && nonce
                        .iter()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
}
