use rustix::fs::{XattrFlags, fchown, fgetxattr, fsetxattr};
use rustix::process::Gid;

use super::*;

const ROOT: &str = "/test-rsync";
const ORIGINAL: &[u8] = b"reviewed old bytes\n";
const CHANGED: &[u8] = b"changed  new bytes\n";
const INCOMING: &[u8] = b"new approved local file\n";
const ATTRIBUTE_NAME: &str = "user.kortty-rsync-fixture";
const ATTRIBUTE_VALUE: &[u8] = b"receiver metadata\0";
const RECEIVER_GID: u32 = 4321;

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync file replacement fixture"]
fn openssh_passwordless_rsync_replaces_exact_file_and_retains_metadata_and_original() {
    require_isolated_root();
    let fixture = Fixture::new();
    let input = fixture.profile.0.join("approved-source-file");
    fs::write(&input, INCOMING).unwrap();
    let destination = Path::new(ROOT).join("replace/existing-file");
    seed_receiver(&destination);
    let original = identity(&destination);
    let approved = fixture.action(&replace(&input, &destination));

    assert_eq!(
        fixture.execute(approved.clone(), false).outcome,
        RunOutcome::Blocked
    );
    assert_eq!(identity(&destination), original);
    assert_eq!(fs::read(&destination).unwrap(), ORIGINAL);

    let completed = fixture.execute(approved, true);
    assert_eq!(completed.outcome, RunOutcome::Success, "{completed:?}");
    assert_ne!(identity(&destination), original);
    assert_eq!(fs::read(&destination).unwrap(), INCOMING);
    assert_metadata(&destination);

    let detail = completed.detail.as_deref().unwrap();
    let (_, recovery) = detail.split_once("original is retained at ").unwrap();
    let recovery = Path::new(recovery.trim_end_matches('.'));
    let stage = recovery.parent().unwrap();
    assert_eq!(stage.parent(), destination.parent());
    assert!(kortty_server_archive::rsync_tree::valid_stage(
        stage.file_name().unwrap().to_str().unwrap()
    ));
    assert_eq!(recovery.file_name().unwrap(), "original");
    assert_eq!(fs::metadata(stage).unwrap().mode() & 0o777, 0o700);
    assert_eq!(identity(recovery), original);
    assert_eq!(fs::read(recovery).unwrap(), ORIGINAL);
    assert_metadata(recovery);
    assert_empty_handoff();
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync file replacement fixture"]
fn openssh_passwordless_rsync_existing_file_refuses_edited_or_substituted_receiver() {
    require_isolated_root();
    assert_eq!(CHANGED.len(), ORIGINAL.len());
    let fixture = Fixture::new();
    let input = fixture.profile.0.join("approved-stale-input");
    fs::write(&input, INCOMING).unwrap();

    let edited = Path::new(ROOT).join("replace/edited-file");
    seed_receiver(&edited);
    let original = identity(&edited);
    let reviewed = fixture.action(&replace(&input, &edited));
    fs::write(&edited, CHANGED).unwrap();
    assert_rejected(fixture.execute(reviewed, true));
    assert_eq!(identity(&edited), original);
    assert_eq!(fs::read(&edited).unwrap(), CHANGED);
    assert_metadata(&edited);
    assert_empty_handoff();

    let substituted = Path::new(ROOT).join("replace/substituted-file");
    let displaced = Path::new(ROOT).join("replace/displaced-file");
    seed_receiver(&substituted);
    let original = identity(&substituted);
    let reviewed = fixture.action(&replace(&input, &substituted));
    fs::rename(&substituted, &displaced).unwrap();
    seed_receiver(&substituted);
    let replacement = identity(&substituted);
    assert_ne!(original, replacement);
    assert_rejected(fixture.execute(reviewed, true));
    assert_eq!(identity(&substituted), replacement);
    assert_eq!(fs::read(&substituted).unwrap(), ORIGINAL);
    assert_eq!(identity(&displaced), original);
    assert_eq!(fs::read(&displaced).unwrap(), ORIGINAL);
    assert_metadata(&substituted);
    assert_metadata(&displaced);
    assert_empty_handoff();
}

fn require_isolated_root() {
    assert!(rustix::process::geteuid().is_root());
    assert_eq!(
        fs::read(Path::new(ROOT).join(".file-rsync-fixture")).unwrap(),
        b"kortty-file-rsync-fixture-v1\n"
    );
}

fn replace<'a>(source: &'a Path, destination: &'a Path) -> Case<'a> {
    Case {
        local: source,
        path: destination.to_str().unwrap(),
        upload: true,
        directory: false,
        replace: true,
    }
}

fn seed_receiver(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut file, ORIGINAL).unwrap();
    fchown(&file, None, Some(Gid::from_raw(RECEIVER_GID))).unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o640))
        .unwrap();
    fsetxattr(&file, ATTRIBUTE_NAME, ATTRIBUTE_VALUE, XattrFlags::empty()).unwrap();
    file.sync_all().unwrap();
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn assert_metadata(path: &Path) {
    let file = fs::File::open(path).unwrap();
    let metadata = file.metadata().unwrap();
    assert_eq!(
        (metadata.uid(), metadata.gid(), metadata.mode() & 0o7777),
        (0, RECEIVER_GID, 0o640)
    );
    let mut attribute = [0; 64];
    let length = fgetxattr(&file, ATTRIBUTE_NAME, &mut attribute).unwrap();
    assert_eq!(&attribute[..length], ATTRIBUTE_VALUE);
}

fn assert_rejected(completion: Completion) {
    assert_eq!(completion.outcome, RunOutcome::Blocked, "{completion:?}");
    assert!(
        !completion
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("original is retained at")
    );
}

fn assert_empty_handoff() {
    assert!(
        fs::read_dir(Path::new(ROOT).join("handoff/probe"))
            .unwrap()
            .next()
            .is_none()
    );
}
