//! Resume a replacement suffix only through a newly confirmed desktop review.

use super::*;

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and REPLACEMENT_RESUME_SCRATCH"]
fn live_sftp_reviewed_replacement_reuses_verified_stage_after_reconnect() {
    let fixture = SshFixture::read(
        "REPLACEMENT_RESUME_SCRATCH",
        b"kortty-replacement-resume-live-",
    );
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = fixture.context(root.join("profile"));
    let local = root.join("files");
    fs::create_dir(&local).unwrap();
    let source = local.join(NAMES[0]);
    let payload: Vec<_> = (0..512 * 1024).map(|index| (index % 251) as u8).collect();
    write_local(&source, &payload, MODIFIED + 10);
    const TARGETS: &[&[u8]] = &[NAMES[0].as_bytes()];
    let mut scratch = RemoteScratch::create(&fixture, &context, TARGETS);
    let target = scratch.target();
    let trace = Arc::new(Trace::new(scratch.path.clone()));
    scratch.replacement = Some(Arc::clone(&trace));
    let (mut session, mut client) = fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
    seed_remote(&mut client, &target, OLD[0], MODIFIED);
    client.set_permissions(&target, 0o640).unwrap();
    drop(client);
    session.close().unwrap();

    let mut interrupted = Probe::new(Cut::WriteReply(CUT_AT_WRITE));
    interrupted.replacement = Some(Arc::clone(&trace));
    let interrupted = Arc::new(interrupted);
    let (mut session, mut client) = fixture.open(Arc::clone(&interrupted)).unwrap();
    let note = run_review(
        &context,
        &mut client,
        &local,
        &scratch.path,
        Side::Remote,
        ReviewAction::StageResume { disconnected: true },
    );
    assert!(
        note.contains("Private replacement upload saved at"),
        "{note}"
    );
    assert!(interrupted.cut.load(Ordering::Acquire));
    assert!(trace.writes.lock().unwrap().moves.is_empty());
    drop(client);
    session.close().unwrap();

    let store = Store::new(&context.profile);
    let receipts = store.replacements().unwrap();
    assert_eq!(receipts.len(), 1);
    let directory = receipts[0].directory().unwrap();
    assert_eq!(
        trace.writes.lock().unwrap().stages.as_slice(),
        std::slice::from_ref(&directory)
    );
    let stage = files::remote_join(&directory, b"payload");
    let mut resumed = Probe::new(Cut::None);
    resumed.replacement = Some(Arc::clone(&trace));
    let resumed = Arc::new(resumed);
    let (mut session, mut client) = fixture.open(Arc::clone(&resumed)).unwrap();
    assert_eq!(
        read_remote(&mut client, &target),
        (OLD[0].to_vec(), 0o640, MODIFIED)
    );
    let (prefix, mode, _) = read_remote(&mut client, &stage);
    assert_eq!(mode, 0o600);
    let offset = prefix.len();
    assert!(offset > kortty_ssh::sftp::DOWNLOAD_OVERLAP_BYTES && offset < payload.len());
    assert_eq!(prefix, payload[..offset]);
    assert!(matches!(
        storage::find_transfer(&mut client, &control(&context)).unwrap(),
        Reply::InterruptedDownload { offer: None, note } if note.contains("fresh synchronization review")
    ));
    assert_eq!(resumed.first_write.load(Ordering::Acquire), u64::MAX);

    let note = run_review(
        &context,
        &mut client,
        &local,
        &scratch.path,
        Side::Remote,
        ReviewAction::StageResume {
            disconnected: false,
        },
    );
    assert!(note.contains("Replaced the reviewed server file"), "{note}");
    assert_eq!(resumed.first_write.load(Ordering::Acquire), offset as u64);
    let writes = trace.writes.lock().unwrap().clone();
    assert_eq!(writes.stages.len(), 2);
    assert_eq!(writes.moves.len(), 1);
    assert_eq!(writes.links, [(stage, target.clone())]);
    assert_eq!(
        read_remote(&mut client, &writes.moves[0].1),
        (OLD[0].to_vec(), 0o640, MODIFIED)
    );
    assert_eq!(
        read_remote(&mut client, &target),
        (payload, 0o640, MODIFIED + 10)
    );
    assert!(store.replacements().unwrap().is_empty());
    assert!(client.lstat(&directory).unwrap_err().is_not_found());
    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();
    println!("OpenSSH replacement resumed at verified byte {offset} after a new review");
}
