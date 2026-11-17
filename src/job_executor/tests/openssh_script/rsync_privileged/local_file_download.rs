use std::collections::BTreeSet;

use rustix::fs::{XattrFlags, fchown, fgetxattr, fsetxattr};
use rustix::process::Gid;

use crate::scheduler_window::rsync::standing::Budget;
use crate::sftp_window::{ReviewedFileReceiver, VacantLocalFileReceiver};

use super::mixed::recurring::{FIRING_BYTES, Recurring};
use super::*;

const ORIGINAL: &[u8] = b"reviewed old bytes\n";
const EDITED: &[u8] = b"changed  new bytes\n";
const FIRST: &[u8] = b"verified first source\n";
const SECOND: &[u8] = b"verified second source\n";
const ATTRIBUTE: &str = "user.kortty-local-rsync";
const VALUE: &[u8] = b"original local metadata\0";

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_rsync_download_replaces_only_the_reviewed_local_file() {
    let server = Sshd::new();
    let (fixture, _) = ordinary_fixture(&[&server]);
    let source = fixture.profile.0.join("ordinary-download-source");
    exercise(&fixture, &source, Peer::Ordinary(&server));
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_rsync_group_retains_each_reviewed_local_file() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    let source = fixture.profile.0.join("grouped-download-source");
    exercise_group(
        &fixture,
        &source,
        &additional[0],
        [Peer::Ordinary(&first), Peer::Ordinary(&second)],
    );
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_rsync_file_downloads_into_reviewed_missing_parents() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    let source = fixture.profile.0.join("file-with-missing-parents-source");
    exercise_missing_parents(
        &fixture,
        &source,
        &additional[0],
        [Peer::Ordinary(&first), Peer::Ordinary(&second)],
    );
}

pub(super) fn ordinary_fixture(servers: &[&Sshd]) -> (Fixture, Vec<ConnectionRef>) {
    let profile = Profile::new();
    let (_, seed, connections) = scheduled_batch(&profile.0, servers, "true", vec![]);
    let fixture = Fixture {
        connections,
        reference: seed.action.connection.clone().unwrap(),
        trust: TrustStore::from_pins(servers.iter().map(|server| server.pin()).collect()).unwrap(),
        program: PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
            .canonicalize()
            .unwrap(),
        profile,
    };
    assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
    (fixture, seed.action.additional_connections.clone())
}

#[test]
#[ignore = "requires native OpenSSH and the isolated passwordless rsync fixture"]
fn openssh_passwordless_rsync_download_replaces_only_the_reviewed_local_file() {
    require_file_fixture();
    let fixture = Fixture::new();
    exercise(
        &fixture,
        Path::new("/test-rsync/read/protected-local-file-source"),
        Peer::Privileged,
    );
    assert_empty_handoff();
}

#[test]
#[ignore = "requires native OpenSSH and the isolated passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_group_retains_each_reviewed_local_file() {
    require_file_fixture();
    let mut fixture = Fixture::new();
    let additional = fixture.second_pinned_reference();
    exercise_group(
        &fixture,
        Path::new("/test-rsync/read/grouped-local-file-source"),
        &additional,
        [Peer::Privileged; 2],
    );
    assert_empty_handoff();
}

#[test]
#[ignore = "requires native OpenSSH and the isolated passwordless rsync fixture"]
fn openssh_passwordless_rsync_file_downloads_into_reviewed_missing_parents() {
    require_file_fixture();
    let mut fixture = Fixture::new();
    let additional = fixture.second_pinned_reference();
    exercise_missing_parents(
        &fixture,
        Path::new("/test-rsync/read/file-with-missing-parents-source"),
        &additional,
        [Peer::Privileged; 2],
    );
    assert_empty_handoff();
}

pub(super) fn require_file_fixture() {
    assert!(rustix::process::geteuid().is_root());
    assert_eq!(
        fs::read("/test-rsync/.file-rsync-fixture").unwrap(),
        b"kortty-file-rsync-fixture-v1\n"
    );
}

fn assert_empty_handoff() {
    assert!(
        fs::read_dir("/test-rsync/handoff/probe")
            .unwrap()
            .next()
            .is_none()
    );
}

#[derive(Clone, Copy)]
pub(super) enum Peer<'a> {
    Ordinary(&'a Sshd),
    Privileged,
}

impl Peer<'_> {
    pub(super) fn run(self, execute: impl FnOnce() -> Completion) -> Completion {
        let Self::Ordinary(server) = self else {
            return execute();
        };
        let finished = AtomicBool::new(false);
        thread::scope(|scope| {
            let daemon = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match server.listener.accept() {
                        Ok((stream, _)) => {
                            return Some(Sshd::serve_stream(&server.config, stream));
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            if finished.load(std::sync::atomic::Ordering::Acquire) {
                                return None;
                            }
                            assert!(Instant::now() < deadline, "no rsync SSH connection");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("rsync SSH fixture accept failed: {error}"),
                    }
                }
            });
            let completion = execute();
            finished.store(true, std::sync::atomic::Ordering::Release);
            if let Some(trace) = daemon.join().unwrap() {
                let trace = String::from_utf8_lossy(&trace.stderr);
                assert!(
                    trace.contains("Accepted publickey"),
                    "{completion:?}\n{trace}"
                );
            }
            completion
        })
    }

    fn draft(self, fixture: &Fixture, source: &Path, destination: &Path) -> serde_json::Value {
        let remote = source.to_str().unwrap();
        match self {
            Self::Privileged => serde_json::to_value(fixture.action(&Case {
                local: destination,
                path: remote,
                upload: false,
                directory: false,
                replace: false,
            }))
            .unwrap(),
            Self::Ordinary(_) => {
                let pins =
                    sync::fingerprints(&sync::pins(&fixture.reference, &fixture.trust).unwrap());
                let digest = rsync::program_digest(&fixture.program).unwrap();
                json!({
                    "connection": fixture.reference,
                    "remote": {
                        "kind": "rsync_download", "directory": false, "program": fixture.program,
                        "local": destination, "remote": remote,
                        "approval": {
                            "program": fixture.program, "digest": digest,
                            "reference": fixture.reference, "pins": pins,
                            "download": {"source": remote, "destination": destination},
                        }
                    }
                })
            }
        }
    }

    fn review(self, fixture: &Fixture, source: &Path, destination: &Path) -> SshCommand {
        let mut action = self.draft(fixture, source, destination);
        let snapshot = ReviewedFileReceiver::inspect(destination, &|| false).unwrap();
        action["remote"]["existing_download"] =
            json!({"delete": false, "include_source_name": false});
        action["remote"]["approval"]["existing_file_download"] =
            json!({"destination": destination, "snapshot": snapshot});
        serde_json::from_value::<SshCommand>(action)
            .unwrap()
            .validated()
            .unwrap()
    }

    fn review_vacancy(self, fixture: &Fixture, source: &Path, destination: &Path) -> SshCommand {
        let mut action = self.draft(fixture, source, destination);
        let snapshot = VacantLocalFileReceiver::inspect(destination, &|| false).unwrap();
        action["remote"]["approval"]["vacant_file_download"] =
            json!({"destination": destination, "snapshot": snapshot});
        serde_json::from_value(action).unwrap()
    }

    fn review_missing_parents(
        self,
        fixture: &Fixture,
        source: &Path,
        destination: &Path,
    ) -> SshCommand {
        let mut action = self.draft(fixture, source, destination);
        action["remote"]["allow_vacant_file_download"] = json!(true);
        action["remote"]["approval"]["vacant_file_download"] = hierarchy_approval(destination);
        serde_json::from_value(action).unwrap()
    }
}

fn exercise(fixture: &Fixture, source: &Path, peer: Peer<'_>) {
    write_new_source(source);
    let gid = if matches!(peer, Peer::Privileged) {
        Gid::from_raw(4321)
    } else {
        rustix::process::getegid()
    };
    one_use(fixture, source, peer, gid);
    recurring(fixture, source, peer, gid);
    create_then_replace(fixture, source, peer);
}

fn write_new_source(source: &Path) {
    let mut input = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(source)
        .unwrap();
    input
        .set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    std::io::Write::write_all(&mut input, FIRST).unwrap();
}

fn one_use(fixture: &Fixture, source: &Path, peer: Peer<'_>, gid: Gid) {
    let destination = fixture.profile.0.join("reviewed-manual-file");
    seed_receiver(&destination, gid, ORIGINAL);
    let original = identity(&destination);
    let approval = peer.review(fixture, source, &destination);

    let cancelled = fixture.execute_manual(approval.clone(), true, true);
    assert_eq!(cancelled.outcome, RunOutcome::Cancelled, "{cancelled:?}");
    assert_eq!(identity(&destination), original);
    assert_eq!(fs::read(&destination).unwrap(), ORIGINAL);
    assert!(recoveries(&destination).is_empty());

    if matches!(peer, Peer::Privileged) {
        let denied = fixture.execute(approval.clone(), false);
        assert_blocked(&denied);
        assert_eq!(identity(&destination), original);
        assert_eq!(fs::read(&destination).unwrap(), ORIGINAL);
        assert!(recoveries(&destination).is_empty());
    }

    let completion = peer.run(|| fixture.execute(approval, true));
    assert_publication(
        &completion,
        &destination,
        original,
        ORIGINAL,
        FIRST,
        gid,
        None,
    );

    let edited = fixture.profile.0.join("reviewed-edited-file");
    seed_receiver(&edited, gid, ORIGINAL);
    let original = identity(&edited);
    let approval = peer.review(fixture, source, &edited);
    assert_eq!(EDITED.len(), ORIGINAL.len());
    fs::write(&edited, EDITED).unwrap();
    assert_blocked(&peer.run(|| fixture.execute(approval, true)));
    assert_eq!(identity(&edited), original);
    assert_eq!(fs::read(&edited).unwrap(), EDITED);
    assert_metadata(&edited, gid);
    assert!(recoveries(&edited).is_empty());

    let substituted = fixture.profile.0.join("reviewed-substituted-file");
    let displaced = fixture.profile.0.join("displaced-manual-file");
    seed_receiver(&substituted, gid, ORIGINAL);
    let original = identity(&substituted);
    let approval = peer.review(fixture, source, &substituted);
    fs::rename(&substituted, &displaced).unwrap();
    seed_receiver(&substituted, gid, ORIGINAL);
    let replacement = identity(&substituted);
    assert_ne!(replacement, original);
    assert_blocked(&peer.run(|| fixture.execute(approval, true)));
    assert_eq!(identity(&substituted), replacement);
    assert_eq!(identity(&displaced), original);
    assert_eq!(fs::read(&substituted).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&displaced).unwrap(), ORIGINAL);
    assert_metadata(&substituted, gid);
    assert!(recoveries(&substituted).is_empty());
}

fn recurring(fixture: &Fixture, source: &Path, peer: Peer<'_>, gid: Gid) {
    let destination = fixture.profile.0.join("reviewed-recurring-file");
    seed_receiver(&destination, gid, ORIGINAL);
    let approval = peer.review(fixture, source, &destination);
    let recurring = Recurring::new(fixture, "existing-file-download", approval, false, 0);
    let mut recovery = None;
    let mut previous = ORIGINAL;
    for (round, bytes) in [FIRST, SECOND].into_iter().enumerate() {
        fs::write(source, bytes).unwrap();
        let original = identity(&destination);
        let completion = peer.run(|| recurring.fire(round, true));
        recovery = Some(assert_publication(
            &completion,
            &destination,
            original,
            previous,
            bytes,
            gid,
            recovery.as_deref(),
        ));
        previous = bytes;
    }
    let before = recoveries(&destination);
    let displaced = fixture.profile.0.join("displaced-recurring-file");
    fs::rename(&destination, &displaced).unwrap();
    seed_receiver(&destination, gid, SECOND);
    let replacement = identity(&destination);
    let completion = peer.run(|| recurring.fire(2, true));
    assert_blocked(&completion);
    assert_eq!(recoveries(&destination), before);
    assert_eq!(identity(&destination), replacement);
    assert_eq!(fs::read(&destination).unwrap(), SECOND);
    assert_eq!(fs::read(&displaced).unwrap(), SECOND);
    assert_metadata(&destination, gid);
}

fn create_then_replace(fixture: &Fixture, source: &Path, peer: Peer<'_>) {
    let chain = matches!(peer, Peer::Privileged);
    let destination = fixture.profile.0.join("initially-vacant-file");
    fs::write(source, FIRST).unwrap();
    let approval = peer.review_vacancy(fixture, source, &destination);
    let recurring = Recurring::new(fixture, "vacant-file-download", approval, chain, 0);

    let first = peer.run(|| recurring.fire(0, true));
    assert_eq!(first.outcome, RunOutcome::Success, "{first:?}");
    assert_eq!(fs::read(&destination).unwrap(), FIRST);
    assert!(recoveries(&destination).is_empty());
    let created = identity(&destination);
    let original = fs::metadata(&destination).unwrap();
    assert_eq!(original.mode() & 0o777, 0o600);

    fs::write(source, SECOND).unwrap();
    let second = peer.run(|| recurring.fire(1, true));
    assert_eq!(second.outcome, RunOutcome::Success, "{second:?}");
    assert_eq!(fs::read(&destination).unwrap(), SECOND);
    assert_ne!(identity(&destination), created);
    let current = fs::metadata(&destination).unwrap();
    assert_eq!(current.mode() & 0o7777, original.mode() & 0o7777);
    assert_eq!(
        (current.uid(), current.gid()),
        (original.uid(), original.gid())
    );
    let recoveries_after_replace = recoveries(&destination);
    assert_eq!(recoveries_after_replace.len(), 1);
    let retained = recoveries_after_replace.first().unwrap();
    assert_eq!(identity(retained), created);
    assert_eq!(fs::read(retained).unwrap(), FIRST);

    let occupied = fixture.profile.0.join("occupied-after-review");
    let approval = peer.review_vacancy(fixture, source, &occupied);
    let occupied_run = Recurring::new(fixture, "occupied-file-download", approval, chain, 0);
    fs::write(&occupied, ORIGINAL).unwrap();
    let occupant = identity(&occupied);
    assert_blocked(&peer.run(|| occupied_run.fire(0, true)));
    assert_eq!(identity(&occupied), occupant);
    assert_eq!(fs::read(&occupied).unwrap(), ORIGINAL);
    assert!(recoveries(&occupied).is_empty());
}

fn exercise_group(
    fixture: &Fixture,
    source: &Path,
    additional: &ConnectionRef,
    peers: [Peer<'_>; 2],
) {
    write_new_source(source);
    let first = fixture.profile.0.join("grouped-first");
    let second = fixture.profile.0.join("grouped-second");
    let review = || {
        grouped_review(
            fixture, source, &first, &second, additional, peers[0], false,
        )
    };
    let chain = matches!(peers[0], Peer::Privileged);
    let recurring = Recurring::new(fixture, "grouped-file-download", review(), chain, 0);
    let fire = |recurring: &Recurring<'_>, round| {
        peers[0].run(|| peers[1].run(|| recurring.fire(round, true)))
    };

    fs::write(&second, ORIGINAL).unwrap();
    let occupant = identity(&second);
    let partial = fire(&recurring, 0);
    assert_blocked(&partial);
    assert!(
        partial
            .detail
            .as_deref()
            .unwrap()
            .contains("1 of 2 targets succeeded")
    );
    assert_eq!(fs::read(&first).unwrap(), FIRST);
    let first_receipt = identity(&first);
    assert_eq!(identity(&second), occupant);
    assert_eq!(fs::read(&second).unwrap(), ORIGINAL);
    assert!(recoveries(&first).is_empty() && recoveries(&second).is_empty());

    fs::remove_file(&second).unwrap();
    fs::write(source, SECOND).unwrap();
    let both = fire(&recurring, 1);
    assert_eq!(both.outcome, RunOutcome::Success, "{both:?}");
    assert_eq!(fs::read(&first).unwrap(), SECOND);
    assert_eq!(fs::read(&second).unwrap(), SECOND);
    assert_retained(&first, first_receipt, FIRST);
    assert!(recoveries(&second).is_empty());

    let before = [identity(&first), identity(&second)];
    fs::write(source, EDITED).unwrap();
    let both = fire(&recurring, 2);
    assert_eq!(both.outcome, RunOutcome::Success, "{both:?}");
    for (path, previous) in [&first, &second].into_iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), EDITED);
        assert_retained(path, previous, SECOND);
    }

    let before = [identity(&first), identity(&second)];
    let oversized_combined = vec![b'x'; FIRING_BYTES as usize / 2 + 1];
    fs::write(source, &oversized_combined).unwrap();
    let bounded = fire(&recurring, 3);
    assert_blocked(&bounded);
    assert!(
        bounded
            .detail
            .as_deref()
            .unwrap()
            .contains("1 of 2 targets succeeded")
    );
    assert_eq!(fs::read(&first).unwrap(), oversized_combined);
    assert_retained(&first, before[0], EDITED);
    assert_eq!(identity(&second), before[1]);
    assert_eq!(fs::read(&second).unwrap(), EDITED);

    fs::rename(&second, fixture.profile.0.join("displaced-grouped-second")).unwrap();
    fs::write(source, FIRST).unwrap();
    let renewed = Recurring::new(fixture, "renewed-grouped-file-download", review(), chain, 0);
    let previous = identity(&first);
    let retained_second = recoveries(&second);
    let both = fire(&renewed, 0);
    assert_eq!(both.outcome, RunOutcome::Success, "{both:?}");
    assert_eq!(fs::read(&first).unwrap(), FIRST);
    assert_eq!(fs::read(&second).unwrap(), FIRST);
    assert_retained(&first, previous, &oversized_combined);
    assert_eq!(recoveries(&second), retained_second);
}

fn grouped_review(
    fixture: &Fixture,
    source: &Path,
    first: &Path,
    second: &Path,
    additional: &ConnectionRef,
    peer: Peer<'_>,
    create_missing_parents: bool,
) -> SshCommand {
    let mut action = peer.draft(fixture, source, first);
    let mut second_review = action["remote"]["approval"].clone();
    let pins = sync::fingerprints(&sync::pins(additional, &fixture.trust).unwrap());
    second_review["reference"] = json!(additional);
    second_review["pins"] = json!(pins);
    second_review["download"]["destination"] = json!(second);
    if create_missing_parents {
        action["remote"]["allow_vacant_file_download"] = json!(true);
    }
    for (destination, approval) in [
        (first, &mut action["remote"]["approval"]),
        (second, &mut second_review),
    ] {
        if destination.exists() {
            let snapshot = ReviewedFileReceiver::inspect(destination, &|| false).unwrap();
            approval["existing_file_download"] =
                json!({"destination": destination, "snapshot": snapshot});
        } else if create_missing_parents {
            approval["vacant_file_download"] = hierarchy_approval(destination);
        } else {
            let snapshot = VacantLocalFileReceiver::inspect(destination, &|| false).unwrap();
            approval["vacant_file_download"] =
                json!({"destination": destination, "snapshot": snapshot});
        }
    }
    action["additional_connections"] = json!([additional]);
    action["remote"]["routes"] =
        json!([{"profile": additional.id, "destination": second, "approval": second_review}]);
    serde_json::from_value(action).unwrap()
}

fn hierarchy_approval(destination: &Path) -> serde_json::Value {
    let snapshot = rsync::review_local_file_vacancy(destination, &AtomicBool::new(false)).unwrap();
    assert!(matches!(snapshot, rsync::VacantFileProof::Hierarchy(_)));
    json!({"destination": destination, "snapshot": snapshot})
}

fn exercise_missing_parents(
    fixture: &Fixture,
    source: &Path,
    additional: &ConnectionRef,
    peers: [Peer<'_>; 2],
) {
    write_new_source(source);
    let shared = fixture.profile.0.join("manual-file-parent");
    let first = shared.join("first route").join("received file");
    let second = shared.join("second route").join("received file");
    let review = grouped_review(fixture, source, &first, &second, additional, peers[0], true);
    let manual = || peers[0].run(|| peers[1].run(|| fixture.execute(review.clone(), true)));
    assert!(!shared.exists(), "review created the missing directory");

    let cancelled = fixture.execute_manual(review.clone(), true, true);
    assert_eq!(cancelled.outcome, RunOutcome::Cancelled, "{cancelled:?}");
    assert!(!shared.exists());

    let chain = matches!(peers[0], Peer::Privileged);
    if chain {
        assert_blocked(&fixture.execute(review.clone(), false));
        assert!(!shared.exists());
    }

    fs::create_dir(&shared).unwrap();
    let marker = shared.join("independent-arrival");
    fs::write(&marker, ORIGINAL).unwrap();
    let occupied = identity(&shared);
    assert_blocked(&manual());
    assert_eq!(identity(&shared), occupied);
    assert_eq!(fs::read(&marker).unwrap(), ORIGINAL);
    assert!(!first.exists() && !second.exists());
    fs::remove_file(marker).unwrap();
    fs::remove_dir(&shared).unwrap();

    let created = manual();
    assert_eq!(created.outcome, RunOutcome::Success, "{created:?}");
    let original = [identity(&first), identity(&second)];
    assert_blocked(&manual());
    for (path, expected) in [&first, &second].into_iter().zip(original) {
        assert_eq!(identity(path), expected);
        assert_eq!(fs::read(path).unwrap(), FIRST);
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        assert!(recoveries(path).is_empty());
    }

    let entry_budget = |entries| Budget {
        bytes: 1024,
        entries,
        deletes: 0,
    };
    let unaffordable = fixture.profile.0.join("too-many-new-file-parents");
    let command =
        peers[0].review_missing_parents(fixture, source, &unaffordable.join("nested").join("file"));
    let denied = Recurring::with_budget(
        fixture,
        "reject-unaffordable-file-hierarchy",
        command,
        chain,
        entry_budget(2),
    );
    assert_blocked(&peers[0].run(|| denied.fire(0, true)));
    assert!(!unaffordable.exists());

    let shared = fixture.profile.0.join("recurring-file-parent");
    let first = shared.join("first route").join("result");
    let second = shared.join("second route").join("result");
    let command = grouped_review(fixture, source, &first, &second, additional, peers[0], true);
    let job = Recurring::with_budget(
        fixture,
        "resume-file-hierarchy-after-reopen",
        command,
        chain,
        entry_budget(3),
    );
    let fire = |round| peers[0].run(|| peers[1].run(|| job.fire(round, true)));
    let partial = fire(0);
    assert_blocked(&partial);
    assert!(partial.detail.unwrap().contains("1 of 2 targets succeeded"));
    let original = identity(&first);
    assert_eq!(fs::read(&first).unwrap(), FIRST);
    assert!(!second.exists());

    let held = shared.with_file_name("displaced-file-parent");
    fs::rename(&shared, &held).unwrap();
    fs::create_dir(&shared).unwrap();
    let marker = shared.join("foreign-replacement");
    fs::write(&marker, ORIGINAL).unwrap();
    assert_blocked(&fire(1));
    assert_eq!(fs::read(&marker).unwrap(), ORIGINAL);
    assert!(!first.exists() && !second.exists());
    fs::remove_file(marker).unwrap();
    fs::remove_dir(&shared).unwrap();
    fs::rename(&held, &shared).unwrap();

    // Each fire reopens the Scheduler. The second route needs the saved
    // common-parent proof to fit file + one new parent in the two remaining entries.
    fs::write(source, SECOND).unwrap();
    let resumed = fire(2);
    assert_eq!(resumed.outcome, RunOutcome::Success, "{resumed:?}");
    for path in [&first, &second] {
        assert_eq!(fs::read(path).unwrap(), SECOND);
    }
    assert_retained(&first, original, FIRST);
    assert!(recoveries(&second).is_empty());
}

fn assert_retained(destination: &Path, previous: (u64, u64), bytes: &[u8]) {
    assert_ne!(identity(destination), previous);
    let recovery = recoveries(destination)
        .into_iter()
        .find(|path| identity(path) == previous)
        .expect("original file kept for recovery");
    assert_eq!(fs::read(recovery).unwrap(), bytes);
}

fn seed_receiver(path: &Path, gid: Gid, content: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap();
    if gid != rustix::process::getegid() {
        fchown(&file, None, Some(gid)).unwrap();
    }
    file.set_permissions(fs::Permissions::from_mode(0o640))
        .unwrap();
    fsetxattr(&file, ATTRIBUTE, VALUE, XattrFlags::empty()).unwrap();
    std::io::Write::write_all(&mut file, content).unwrap();
    file.sync_all().unwrap();
}

fn identity(path: &Path) -> (u64, u64) {
    let info = fs::metadata(path).unwrap();
    (info.dev(), info.ino())
}

fn assert_metadata(path: &Path, gid: Gid) {
    let file = fs::File::open(path).unwrap();
    let info = file.metadata().unwrap();
    assert_eq!(
        (info.uid(), info.gid(), info.mode() & 0o7777),
        (rustix::process::geteuid().as_raw(), gid.as_raw(), 0o640)
    );
    let mut attribute = [0; 64];
    let length = fgetxattr(&file, ATTRIBUTE, &mut attribute).unwrap();
    assert_eq!(&attribute[..length], VALUE);
}

fn recoveries(destination: &Path) -> BTreeSet<PathBuf> {
    fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(kortty_server_archive::rsync_tree::valid_stage)
        })
        .map(|path| path.join(destination.file_name().unwrap()))
        .filter(|path| path.is_file())
        .collect()
}

fn assert_publication(
    completion: &Completion,
    destination: &Path,
    original: (u64, u64),
    previous: &[u8],
    current: &[u8],
    gid: Gid,
    prior: Option<&Path>,
) -> PathBuf {
    assert_eq!(completion.outcome, RunOutcome::Success, "{completion:?}");
    assert_ne!(identity(destination), original);
    assert_eq!(fs::read(destination).unwrap(), current);
    assert_metadata(destination, gid);
    let recovered: Vec<_> = recoveries(destination)
        .into_iter()
        .filter(|path| Some(path.as_path()) != prior)
        .collect();
    assert_eq!(recovered.len(), 1);
    let recovery = &recovered[0];
    assert_eq!(identity(recovery), original);
    assert_eq!(fs::read(recovery).unwrap(), previous);
    assert_eq!(
        fs::metadata(recovery.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
    assert_metadata(recovery, gid);
    assert!(
        completion
            .detail
            .as_deref()
            .unwrap()
            .contains(recovery.to_str().unwrap())
    );
    recovery.clone()
}

fn assert_blocked(completion: &Completion) {
    assert_eq!(completion.outcome, RunOutcome::Blocked, "{completion:?}");
}
