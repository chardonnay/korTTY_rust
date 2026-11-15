use std::collections::BTreeSet;

use rustix::fs::{XattrFlags, fchown, fgetxattr, fsetxattr};
use rustix::process::Gid;

use crate::sftp_window::ReviewedFileReceiver;

use super::mixed::recurring::Recurring;
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
    let profile = Profile::new();
    let (_, seed, connections) = scheduled_batch(&profile.0, &[&server], "true", vec![]);
    let fixture = Fixture {
        connections,
        reference: seed.action.connection.clone().unwrap(),
        trust: TrustStore::from_pins(vec![server.pin()]).unwrap(),
        program: PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
            .canonicalize()
            .unwrap(),
        profile,
    };
    assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
    let source = fixture.profile.0.join("ordinary-download-source");
    exercise(&fixture, &source, Peer::Ordinary(&server));
}

#[test]
#[ignore = "requires native OpenSSH and the isolated passwordless rsync fixture"]
fn openssh_passwordless_rsync_download_replaces_only_the_reviewed_local_file() {
    assert!(rustix::process::geteuid().is_root());
    assert_eq!(
        fs::read("/test-rsync/.file-rsync-fixture").unwrap(),
        b"kortty-file-rsync-fixture-v1\n"
    );
    let fixture = Fixture::new();
    exercise(
        &fixture,
        Path::new("/test-rsync/read/protected-local-file-source"),
        Peer::Privileged,
    );
    assert!(
        fs::read_dir("/test-rsync/handoff/probe")
            .unwrap()
            .next()
            .is_none()
    );
}

#[derive(Clone, Copy)]
enum Peer<'a> {
    Ordinary(&'a Sshd),
    Privileged,
}

impl Peer<'_> {
    fn run(self, execute: impl FnOnce() -> Completion) -> Completion {
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

    fn review(self, fixture: &Fixture, source: &Path, destination: &Path) -> SshCommand {
        let remote = source.to_str().unwrap();
        let mut action = match self {
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
        };
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
}

fn exercise(fixture: &Fixture, source: &Path, peer: Peer<'_>) {
    let mut input = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(source)
        .unwrap();
    input
        .set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    std::io::Write::write_all(&mut input, FIRST).unwrap();
    drop(input);
    let gid = if matches!(peer, Peer::Privileged) {
        Gid::from_raw(4321)
    } else {
        rustix::process::getegid()
    };
    one_use(fixture, source, peer, gid);
    recurring(fixture, source, peer, gid);
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
