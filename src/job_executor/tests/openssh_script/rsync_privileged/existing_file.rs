use rustix::fs::{XattrFlags, fchown, fgetxattr, fsetxattr};
use rustix::process::Gid;

use crate::scheduler_window::rsync::standing::{Budget, Receiver};

use super::mixed::recurring::Recurring;
use super::*;

const ROOT: &str = "/test-rsync";
const ORIGINAL: &[u8] = b"reviewed old bytes\n";
const CHANGED: &[u8] = b"changed  new bytes\n";
const INCOMING: &[u8] = b"new approved local file\n";
const ATTRIBUTE_NAME: &str = "user.kortty-rsync-fixture";
const ATTRIBUTE_VALUE: &[u8] = b"receiver metadata\0";
const RECEIVER_GID: u32 = 4321;
const FIRST_CREATION: &[u8] = b"first protected upload\n";
const SECOND_CREATION: &[u8] = b"later protected upload\n";

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

    let recovery = checked_recovery(&completed, &destination, original);
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

#[test]
#[ignore = "requires the isolated native OpenSSH and recurring passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_creates_vacant_server_files() {
    require_isolated_root();
    let fixture = Fixture::new();
    let input = fixture.profile.0.join("recurring-vacant-file-input");
    fs::write(&input, INCOMING).unwrap();
    assert_eq!(FIRST_CREATION.len(), SECOND_CREATION.len());

    // A legacy one-use creation needs only the create scope. A first firing
    // that will replace this path later needs both scopes during review.
    let create_only = Path::new(ROOT).join("write/vacant-file-create-only");
    assert!(
        preview_vacancy(&fixture, &replace_spec(&create_only)).is_err(),
        "a first recurring review must require replacement access"
    );
    let legacy = fixture.action(&Case {
        local: &input,
        path: create_only.to_str().unwrap(),
        upload: true,
        directory: false,
        replace: false,
    });
    let completed = fixture.execute(legacy, true);
    assert_eq!(completed.outcome, RunOutcome::Success, "{completed:?}");
    assert_eq!(fs::read(&create_only).unwrap(), INCOMING);
    assert_empty_handoff();

    for (job, chain) in [
        ("protected-vacant-file-calendar", false),
        ("protected-vacant-file-chain", true),
    ] {
        let parent = Path::new(ROOT).join("replace").join(job);
        private_dir(&parent);
        let destination = parent.join("receiver");
        fs::write(&input, INCOMING).unwrap();
        let action = approve_vacancy(&fixture, &input, &destination);
        let recurring = Recurring::with_budget(
            &fixture,
            job,
            action,
            chain,
            Budget {
                bytes: FIRST_CREATION.len() as u64,
                entries: 1,
                deletes: 0,
            },
        );
        assert!(!destination.exists());

        // A valid vacancy review cannot quarantine or overwrite an occupant
        // that appears before the helper opens the destination for a firing.
        fs::write(&input, FIRST_CREATION).unwrap();
        seed_receiver(&destination);
        let outsider = identity(&destination);
        assert_rejected(recurring.fire(0, true));
        assert_eq!(identity(&destination), outsider);
        assert_eq!(fs::read(&destination).unwrap(), ORIGINAL);
        assert_metadata(&destination);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        assert_empty_handoff();
        fs::remove_file(&destination).unwrap();

        fs::write(&input, vec![b'x'; FIRST_CREATION.len() + 1]).unwrap();
        let over_budget = recurring.fire(1, true);
        assert_eq!(over_budget.outcome, RunOutcome::Blocked, "{over_budget:?}");
        assert!(over_budget.detail.unwrap().contains("firing limits"));
        assert!(fs::read_dir(&parent).unwrap().next().is_none());
        assert_empty_handoff();

        fs::write(&input, FIRST_CREATION).unwrap();
        let first = recurring.fire(2, true);
        assert_eq!(first.outcome, RunOutcome::Success, "{first:?}");
        assert!(!first.detail.unwrap().contains("original is retained at"));
        assert_eq!(fs::read(&destination).unwrap(), FIRST_CREATION);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        let first_identity = identity(&destination);
        let first_metadata = fs::metadata(&destination).unwrap();
        assert_eq!(first_metadata.uid(), 0);
        assert_eq!(first_metadata.mode() & 0o7777, 0o600);
        assert_eq!(first_metadata.nlink(), 1);
        assert_saved_successor(&fixture, job, &destination);
        assert_empty_handoff();

        // Recurring::fire reopens the Scheduler and claims a fresh firing.
        // This run must use the first publication's exact persisted proof.
        fs::write(&input, SECOND_CREATION).unwrap();
        let replaced = recurring.fire(3, true);
        assert_eq!(replaced.outcome, RunOutcome::Success, "{replaced:?}");
        let original = checked_recovery(&replaced, &destination, first_identity);
        assert_eq!(fs::read(original).unwrap(), FIRST_CREATION);
        assert_eq!(fs::read(&destination).unwrap(), SECOND_CREATION);
        assert_ne!(identity(&destination), first_identity);
        let metadata = fs::metadata(&destination).unwrap();
        assert_eq!(metadata.uid(), first_metadata.uid());
        assert_eq!(metadata.gid(), first_metadata.gid());
        assert_eq!(metadata.mode() & 0o7777, first_metadata.mode() & 0o7777);
        assert_saved_successor(&fixture, job, &destination);
        assert_empty_handoff();
    }

    let parent = Path::new(ROOT).join("replace/vacant-file-reviewed-parent");
    private_dir(&parent);
    let destination = parent.join("receiver");
    let action = approve_vacancy(&fixture, &input, &destination);
    let recurring = Recurring::new(&fixture, "protected-vacant-file-parent", action, false, 0);
    let displaced = parent.with_file_name("vacant-file-displaced-parent");
    let original_parent = identity(&parent);
    fs::rename(&parent, &displaced).unwrap();
    private_dir(&parent);
    let replacement_parent = identity(&parent);
    assert_ne!(original_parent, replacement_parent);
    assert_rejected(recurring.fire(0, true));
    assert_eq!(identity(&parent), replacement_parent);
    assert_eq!(identity(&displaced), original_parent);
    assert!(fs::read_dir(&parent).unwrap().next().is_none());
    assert!(fs::read_dir(&displaced).unwrap().next().is_none());
    assert_empty_handoff();
}

fn replace_spec(destination: &Path) -> Spec {
    Spec {
        path: destination.to_str().unwrap().to_owned(),
        operation: Operation::Upload {
            directory: false,
            replace: true,
            delete: DeleteScope::Retain,
        },
    }
}

fn preview_vacancy(
    fixture: &Fixture,
    spec: &Spec,
) -> Result<kortty_server_archive::privileged_rsync::Review, String> {
    let credentials = Credentials::resolve_directory_browser(
        &fixture.reference,
        fixture.connections.get(fixture.reference.id).unwrap(),
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    rsync::preview_privileged(
        credentials,
        &fixture.reference,
        spec,
        rsync::PrivilegedPreviewMode::VacantFile,
        &fixture.trust,
        Arc::new(AtomicBool::new(false)),
        || Ok(()),
    )
}

fn approve_vacancy(fixture: &Fixture, input: &Path, destination: &Path) -> SshCommand {
    let spec = replace_spec(destination);
    let review = preview_vacancy(fixture, &spec).unwrap();
    assert_eq!(review.spec, spec);
    assert!(review.first_file && review.inventory.is_none());
    let digest = rsync::program_digest(&fixture.program).unwrap();
    let pins = sync::fingerprints(&sync::pins(&fixture.reference, &fixture.trust).unwrap());
    let (bytes, source_digest) = rsync::upload_digest(input).unwrap();
    serde_json::from_value(json!({
        "connection": fixture.reference,
        "remote": {
            "kind": "rsync_upload", "program": fixture.program, "local": input,
            "remote": destination, "elevation": "passwordless",
            "existing": {"include_source_name": false, "delete": false},
            "allow_vacant_server_file_upload": true,
            "approval": {
                "program": fixture.program, "digest": digest, "reference": fixture.reference,
                "pins": pins, "privileged": review,
                "upload": {"source": input, "destination": destination, "bytes": bytes, "digest": source_digest},
            }
        }
    }))
    .unwrap()
}

fn assert_saved_successor(fixture: &Fixture, job: &str, destination: &Path) {
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.profile.0.join("job-rsync-standing-reviews.json")).unwrap(),
    )
    .unwrap();
    let route = &ledger[job]["grant"]["routes"][0];
    assert_eq!(route["reference"], json!(fixture.reference));
    let Receiver::Remote {
        destination: saved,
        snapshot,
    } = serde_json::from_value(route["receiver"].clone()).unwrap()
    else {
        panic!("expected a confirmed server file receiver");
    };
    assert_eq!(saved.as_str(), destination.to_str().unwrap());
    let spec = replace_spec(destination);
    let existing = fixture.preview(&spec).unwrap();
    assert_eq!(preview_vacancy(fixture, &spec).unwrap(), existing);
    assert!(!existing.first_file);
    assert_eq!(snapshot.fingerprint, existing.fingerprint);
    assert_eq!(snapshot.inventory, existing.inventory.unwrap());
}

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn checked_recovery<'a>(
    completion: &'a Completion,
    destination: &Path,
    expected: (u64, u64),
) -> &'a Path {
    let detail = completion.detail.as_deref().unwrap();
    let (_, recovery) = detail.split_once("original is retained at ").unwrap();
    let recovery = Path::new(recovery.split(". ").next().unwrap().trim_end_matches('.'));
    let stage = recovery.parent().unwrap();
    assert_eq!(stage.parent(), destination.parent());
    assert!(kortty_server_archive::rsync_tree::valid_stage(
        stage.file_name().unwrap().to_str().unwrap()
    ));
    assert_eq!(recovery.file_name().unwrap(), "original");
    assert_eq!(fs::metadata(stage).unwrap().mode() & 0o777, 0o700);
    assert_eq!(identity(recovery), expected);
    recovery
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
