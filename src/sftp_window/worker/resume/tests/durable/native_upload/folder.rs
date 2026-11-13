//! Live folder retry through the ordinary transfer queue.

use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

const NAMES: [&str; 2] = ["first.bin", "second.bin"];
const REMOTE_NAMES: &[&[u8]] = &[NAMES[0].as_bytes(), NAMES[1].as_bytes()];

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and FOLDER_SCRATCH"]
fn live_sftp_folder_retry_resumes_only_the_interrupted_child() {
    let fixture = SshFixture::read("FOLDER_SCRATCH", b"kortty-folder-live-");
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = fixture.context(root.join("profile"));
    let store = Store::new(&context.profile);
    let mut scratch = RemoteScratch::create(&fixture, &context, REMOTE_NAMES);
    let (parent, name) = remote_components(&scratch.path).unwrap();
    let destination = root.join(std::str::from_utf8(name).unwrap());
    let payload: Vec<_> = (0..512 * 1024).map(|index| (index % 251) as u8).collect();
    let size = payload.len() as u64;

    {
        let (mut session, mut client) = fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
        for name in REMOTE_NAMES {
            let path = files::remote_join(&scratch.path, name);
            client
                .upload_new_with_permissions(path, payload.as_slice(), 0o751)
                .unwrap();
        }
        drop(client);
        session.close().unwrap();
    }

    let mut window = SftpWindow::new(context.route.clone(), None, true, RemoteEditorPolicy::Deny);
    window.configure_saved_downloads(Some(context.profile.clone()));
    // SFTP servers can return either order. Complete the first child and
    // interrupt the second occurrence of this read offset.
    let probe = Arc::new(Probe::new(Cut::ReadDataAt(CUT_AT_READ, 2)));
    let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
    let (incoming, worker, _done) = window_worker(&context);
    let _ = window.initialize_at(worker, Some(root.clone()), Some(scratch.path.clone()));
    drain(&mut window, &incoming, &mut client);

    // Present the fixture entry directly to keep the listing scoped to the test.
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(parent.to_vec()),
        vec![Entry {
            name: EntryName::Remote(name.to_vec()),
            display: std::str::from_utf8(name).unwrap().to_owned(),
            hidden: false,
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            owner: None,
            group: None,
            mode: Some(0o40700),
        }],
    )));
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(name.to_vec()),
    ));
    let _ = window.update(WindowMessage::Transfer(Side::Remote));
    let parent_row = window.busy.as_ref().unwrap().queued.unwrap();
    drain(&mut window, &incoming, &mut client);
    assert!(probe.cut.load(Ordering::Acquire));
    assert!(window.worker.is_none());
    let child_row = window.partial_row.unwrap();
    assert_ne!(child_row, parent_row);
    assert_eq!(window.partial.as_ref().unwrap().offset, CUT_AT_READ);
    let records = store.list().unwrap();
    let [receipt] = records.as_slice() else {
        panic!("expected the interrupted child's only receipt");
    };
    let interrupted_name = NAMES
        .iter()
        .copied()
        .find(|name| receipt.remote == files::remote_join(&scratch.path, name.as_bytes()))
        .expect("the receipt belongs to a child of the fixture folder");
    let completed_name = NAMES
        .into_iter()
        .find(|name| *name != interrupted_name)
        .unwrap();
    let interrupted = destination.join(interrupted_name);
    let complete = destination.join(completed_name);
    let expected_digest = hash(&payload);
    assert_eq!(hash(&fs::read(&complete).unwrap()), expected_digest);
    let completed_stamp = fs::metadata(&complete).unwrap().modified().unwrap();
    assert!(!interrupted.exists());
    assert_eq!(receipt.payload.offset, CUT_AT_READ);
    let stage = destination.join(&receipt.payload.stage_name);
    assert_eq!(
        fs::metadata(stage.join("payload")).unwrap().len(),
        CUT_AT_READ
    );
    drop(client);
    session.close().unwrap();

    let probe = Arc::new(Probe::new(Cut::None));
    let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
    let (incoming, worker, _done) = window_worker(&context);
    let _ = window.initialize_at(worker, Some(root.clone()), Some(scratch.path.clone()));
    drain(&mut window, &incoming, &mut client);
    assert_eq!(window.partial_row, Some(child_row));

    let _ = window.update(WindowMessage::PauseQueue(true));
    let _ = window.update(WindowMessage::RetryQueued(parent_row));
    assert!(incoming.try_recv().is_err());
    let _ = window.update(WindowMessage::PauseQueue(false));
    let command = incoming
        .try_recv()
        .expect("retry should submit the saved child");
    assert!(matches!(
        &command.operation,
        Operation::ResumeDownload { .. }
    ));
    assert_eq!(window.busy.as_ref().unwrap().queued, Some(child_row));
    finish(&mut window, &mut client, command);
    drain(&mut window, &incoming, &mut client);

    assert_eq!(*probe.reads.lock().unwrap(), read_offsets(size));
    assert!(window.partial.is_none());
    assert!(window.prompt.is_none());
    assert!(window.workbench.active_ids().is_empty());
    assert_eq!(window.workbench.waiting(), 0);
    let _ = window.update(WindowMessage::RetryQueued(parent_row));
    assert!(incoming.try_recv().is_err());
    assert_eq!(hash(&fs::read(&complete).unwrap()), expected_digest);
    assert_eq!(
        fs::metadata(complete).unwrap().modified().unwrap(),
        completed_stamp
    );
    let digest = hash(&fs::read(&interrupted).unwrap());
    assert_eq!(digest, expected_digest);
    super::super::ui::download::require_final_permissions(&interrupted, 0o100751);
    assert!(store.list().unwrap().is_empty());
    assert!(!stage.exists());
    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();
    println!(
        "OpenSSH folder retry preserved completed sibling and resumed child at {CUT_AT_READ} bytes"
    );
}

fn window_worker(
    context: &Context,
) -> (mpsc::Receiver<Command>, Worker, async_channel::Sender<()>) {
    let (commands, incoming) = mpsc::sync_channel(1);
    let (finished, done) = bounded(1);
    let worker = Worker {
        commands,
        control: Arc::new(control(context)),
        done,
        editor_policy: RemoteEditorPolicy::Deny,
        exec: None,
        sftp: None,
    };
    (incoming, worker, finished)
}

fn finish(window: &mut SftpWindow, client: &mut Client, command: Command) {
    let ticket = window.busy.as_ref().unwrap().id;
    let control = &window.worker.as_ref().unwrap().control;
    let reply = execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, reply));
}

fn drain(window: &mut SftpWindow, incoming: &mpsc::Receiver<Command>, client: &mut Client) {
    for _ in 0..16 {
        let Ok(command) = incoming.try_recv() else {
            return;
        };
        finish(window, client, command);
    }
    panic!("The folder fixture did not settle");
}
