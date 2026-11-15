#![cfg(all(feature = "decoder", target_os = "linux"))]

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink,
};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use kortty_server_archive::rsync_tree::{
    DeleteScope, EXISTING_ARGUMENT, EXISTING_V2_ARGUMENT, EXISTING_V3_ARGUMENT,
    ExistingDecision as Decision, ExistingReply, ExistingRequest, ExistingRequestV2, ExistingTree,
    Inventory, Manifest, read_frame, valid_stage, write_existing_v2_request, write_frame,
};
use rustix::fs::{FlockOperation, XattrFlags, fgetxattr, flock, fsetxattr};
use sha2::{Digest, Sha256};

#[test]
fn merge_preserves_receiver_only_entries_permissions_and_recoverable_original() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    directory(&destination.join("a"));
    write(&destination.join("a/keep"), b"receiver-only child");
    write(&destination.join("a/replaced"), b"old contents");
    write(&destination.join("receiver-only"), b"untouched");
    set_mode(&destination, 0o750);
    set_mode(&destination.join("a"), 0o750);
    let original_attributes = b"opaque\0metadata";
    fsetxattr(
        File::open(destination.join("a/replaced")).unwrap(),
        "user.kortty-test",
        original_attributes,
        XattrFlags::empty(),
    )
    .unwrap();
    set_mode(&destination.join("a/replaced"), 0o440);
    set_mode(&destination.join("a/keep"), 0o644);
    let old_root = identity(&destination);
    let old_file = identity(&destination.join("a/replaced"));

    write(&source.join("a.ext"), b"new root entry");
    directory(&source.join("a"));
    write(&source.join("a/replaced"), b"incoming contents");
    write(&source.join("a/added"), b"new nested entry");

    let original = review(&destination);
    assert_eq!(original.inventory, inventory(&destination));
    let (mut helper, payload) = prepare(&destination, inventory(&source), original, false);
    populate(&source, &payload);
    let successor = helper.verify();
    assert_eq!(identity(&destination), old_root);
    assert_eq!(
        fs::read(destination.join("a/replaced")).unwrap(),
        b"old contents"
    );
    let recovery = publish(helper, &destination, successor);

    assert_eq!(inventory(&destination), successor);
    assert_eq!((successor.entries, successor.files), (6, 5));
    assert_eq!(
        fs::read(destination.join("a/replaced")).unwrap(),
        b"incoming contents"
    );
    assert_eq!(
        fs::read(destination.join("a/keep")).unwrap(),
        b"receiver-only child"
    );
    assert_eq!(
        fs::read(destination.join("receiver-only")).unwrap(),
        b"untouched"
    );
    assert_eq!(
        fs::read(destination.join("a/added")).unwrap(),
        b"new nested entry"
    );
    assert_eq!(
        fs::read(destination.join("a.ext")).unwrap(),
        b"new root entry"
    );
    assert_eq!(mode(&destination), 0o750);
    assert_eq!(mode(&destination.join("a")), 0o750);
    assert_eq!(mode(&destination.join("a/replaced")), 0o440);
    let mut attributes = [0; 32];
    let size = fgetxattr(
        File::open(destination.join("a/replaced")).unwrap(),
        "user.kortty-test",
        &mut attributes,
    )
    .unwrap();
    assert_eq!(&attributes[..size], original_attributes);
    assert_eq!(mode(&destination.join("a/keep")), 0o644);
    assert_eq!(mode(&destination.join("a/added")), 0o600);
    assert_ne!(identity(&destination), old_root);
    assert_eq!(identity(&recovery), old_root);
    assert_eq!(identity(&recovery.join("a/replaced")), old_file);
    assert_eq!(inventory(&recovery), original.inventory);
    assert_eq!(mode(recovery.parent().unwrap()), 0o700);
}

#[test]
fn mirror_deletes_only_inside_the_exact_receiver() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    let sibling = fixture.tree("sibling");
    write(&sibling.join("retain"), b"outside the receiver");
    let outside_file = identity(&sibling.join("retain"));
    directory(&destination.join("subtree"));
    write(&destination.join("subtree/prune"), b"remove nested");
    write(&destination.join("prune"), b"remove outer");
    directory(&source.join("subtree"));
    write(&source.join("subtree/incoming"), b"incoming");
    directory(&source.join("empty"));

    let original = review(&destination);
    let incoming = inventory(&source);
    let (mut helper, payload) = prepare(&destination, incoming, original, true);
    populate(&source, &payload);
    let successor = helper.verify();
    assert_eq!(successor, incoming);
    assert_eq!(inventory(&destination), original.inventory);
    let recovery = publish(helper, &destination, successor);

    assert_eq!(inventory(&destination), incoming);
    assert!(!destination.join("prune").exists());
    assert!(!destination.join("subtree/prune").exists());
    assert!(destination.join("empty").is_dir());
    assert_eq!(identity(&sibling.join("retain")), outside_file);
    assert_eq!(
        fs::read(sibling.join("retain")).unwrap(),
        b"outside the receiver"
    );
    assert_eq!(inventory(&recovery), original.inventory);
}

#[test]
fn mixed_rsync_mirror_prunes_only_checked_named_incoming_directories() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    let sibling = fixture.tree("sibling");
    write(&sibling.join("file"), b"outside receiver");
    for directory_name in ["logs", "logs-more", "also"] {
        directory(&destination.join(directory_name));
        write(
            &destination.join(directory_name).join("old"),
            b"receiver child",
        );
    }
    write(&destination.join("unrelated"), b"receiver-only entry");
    write(&destination.join("selected-file"), b"old file");
    set_mode(&destination.join("selected-file"), 0o640);
    let original = review(&destination);

    for directory_name in ["logs", "logs-more", "also"] {
        directory(&source.join(directory_name));
    }
    write(&source.join("logs/new"), b"checked log");
    write(
        &source.join("logs-more/new"),
        b"merge under a similar prefix",
    );
    write(&source.join("selected-file"), b"new file");

    let (mut helper, payload) = prepare_v2(
        &destination,
        inventory(&source),
        original,
        DeleteScope::Named(vec!["logs".into(), "also".into()]),
    );
    populate(&source, &payload);
    let successor = helper.verify();
    assert_eq!(review(&destination), original);
    let recovery = publish(helper, &destination, successor);

    assert_eq!(inventory(&destination), successor);
    assert!(!destination.join("logs/old").exists());
    assert!(!destination.join("also/old").exists());
    assert!(destination.join("also").is_dir());
    assert_eq!(
        fs::read(destination.join("logs-more/old")).unwrap(),
        b"receiver child"
    );
    assert_eq!(
        fs::read(destination.join("logs-more/new")).unwrap(),
        b"merge under a similar prefix"
    );
    assert_eq!(
        fs::read(destination.join("unrelated")).unwrap(),
        b"receiver-only entry"
    );
    assert_eq!(
        fs::read(destination.join("selected-file")).unwrap(),
        b"new file"
    );
    assert_eq!(mode(&destination.join("selected-file")), 0o640);
    assert_eq!(fs::read(sibling.join("file")).unwrap(), b"outside receiver");
    assert_eq!(inventory(&recovery), original.inventory);
}

#[test]
fn mixed_rsync_file_only_mirror_replaces_an_incoming_kind_without_pruning_other_paths() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    directory(&destination.join("collision"));
    write(&destination.join("collision/old"), b"recovered child");
    write(&destination.join("untouched"), b"outside incoming paths");
    write(&source.join("collision"), b"incoming file");
    let original = review(&destination);
    let (mut helper, payload) = prepare_v2(
        &destination,
        inventory(&source),
        original,
        DeleteScope::Named(Vec::new()),
    );
    populate(&source, &payload);
    let successor = helper.verify();
    let recovery = publish(helper, &destination, successor);
    assert_eq!(
        fs::read(destination.join("collision")).unwrap(),
        b"incoming file"
    );
    assert_eq!(
        fs::read(destination.join("untouched")).unwrap(),
        b"outside incoming paths"
    );
    assert_eq!(
        fs::read(recovery.join("collision/old")).unwrap(),
        b"recovered child"
    );
}

#[test]
fn mixed_rsync_refuses_scope_names_without_matching_incoming_directories() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    directory(&destination.join("root"));
    write(&destination.join("root/old"), b"untouched");
    write(&source.join("root"), b"not a directory");
    let original = review(&destination);
    let incoming = inventory(&source);
    for name in ["root", "missing"] {
        let (mut helper, payload) = prepare_v2(
            &destination,
            incoming,
            original,
            DeleteScope::Named(vec![name.into()]),
        );
        populate(&source, &payload);
        helper.send(&Decision::Verify);
        assert_refused(
            helper.finish(),
            "missing from the checked incoming directories",
        );
        assert_no_stage(&fixture.0);
        assert_eq!(review(&destination), original);
    }
    for names in [
        vec!["root".into(), "root".into()],
        vec!["root/child".into()],
    ] {
        let mut helper = Helper::start_with(EXISTING_V2_ARGUMENT);
        helper.send_v2(&prepare_request_v2(
            &destination,
            incoming,
            original,
            DeleteScope::Named(names),
        ));
        assert_refused(helper.finish(), "deletion scope is invalid");
        assert_no_stage(&fixture.0);
        assert_eq!(review(&destination), original);
    }
}

#[test]
fn recurring_rsync_attests_exact_removals_and_the_published_receiver_identity() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    directory(&destination.join("scope"));
    write(&destination.join("scope/deleted"), b"old");
    directory(&destination.join("kind-change"));
    write(&destination.join("kind-change/child"), b"old kind");
    write(&destination.join("overwritten"), b"old bytes");
    write(&destination.join("retained"), b"unchanged");
    directory(&source.join("scope"));
    write(&source.join("scope/added"), b"new");
    write(&source.join("kind-change"), b"new kind");
    write(&source.join("overwritten"), b"new bytes");
    let mut original = review(&destination);
    let prepare = |original| {
        let mut helper = Helper::start_with(EXISTING_V3_ARGUMENT);
        helper.send_v2(&prepare_request_v2(
            &destination,
            inventory(&source),
            original,
            DeleteScope::Named(vec!["scope".into()]),
        ));
        helper
    };
    for expected_removed in [3, 0] {
        let old_identity = identity(&destination);
        let (mut helper, payload) = prepared(prepare(original), &destination);
        populate(&source, &payload);
        helper.send(&Decision::Verify);
        let ExistingReply::VerifiedWithRemovals { successor, removed } = helper.receive() else {
            panic!("expected the v3 removal count");
        };
        assert_eq!(removed, expected_removed);
        assert_eq!(review(&destination), original);
        let original_path = helper.quarantine();
        assert_eq!(identity(&original_path), old_identity);
        helper.send(&Decision::Publish);
        let ExistingReply::PublishedWithReceiver {
            destination: published,
            receiver,
            recovery,
        } = helper.finish()
        else {
            panic!("expected the v3 published receiver");
        };
        assert_eq!(Path::new(&published), destination);
        assert_eq!(Path::new(&recovery), original_path);
        assert_eq!(receiver.inventory, successor);
        assert_eq!(review(&destination), receiver);
        assert_ne!(receiver, original);
        assert_ne!(identity(&destination), old_identity);
        assert_eq!(
            fs::read(destination.join("retained")).unwrap(),
            b"unchanged"
        );
        original = receiver;
    }

    let moved = fixture.0.join("moved");
    fs::rename(&destination, &moved).unwrap();
    directory(&destination);
    populate(&moved, &destination);
    assert_eq!(review(&destination).inventory, original.inventory);
    assert_refused(prepare(original).finish(), "changed");
    assert_eq!(inventory(&destination), original.inventory);
}

#[test]
fn abort_before_and_after_verify_keeps_original_and_discards_stages() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    write(&destination.join("file"), b"original");
    write(&source.join("file"), b"new contents");
    let original = review(&destination);
    let old_root = identity(&destination);
    for verify in [false, true] {
        let (mut helper, payload) = prepare(&destination, inventory(&source), original, true);
        populate(&source, &payload);
        if verify {
            helper.verify();
        }
        helper.send(&Decision::Abort);
        assert!(matches!(helper.finish(), ExistingReply::Aborted));
        assert_eq!(identity(&destination), old_root);
        assert_eq!(review(&destination), original);
        assert_no_stage(&fixture.0);
    }
}

#[test]
fn a_missing_publish_decision_restores_the_original_and_respects_a_late_occupant() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    write(&destination.join("file"), b"original");
    write(&source.join("file"), b"incoming");
    let original = identity(&destination);
    let incoming = inventory(&source);
    for send_abort in [true, false] {
        let (mut helper, payload) = prepare(&destination, incoming, review(&destination), true);
        populate(&source, &payload);
        helper.verify();
        let recovery = helper.quarantine();
        assert_eq!(identity(&recovery), original);
        if send_abort {
            helper.send(&Decision::Abort);
            assert!(matches!(helper.finish(), ExistingReply::Aborted));
        } else {
            assert!(matches!(
                helper.finish(),
                ExistingReply::Failed {
                    outcome_unknown: false,
                    ..
                }
            ));
        }
        assert_eq!(identity(&destination), original);
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"original");
        assert_no_stage(&fixture.0);
    }

    let (mut helper, payload) = prepare(&destination, incoming, review(&destination), true);
    populate(&source, &payload);
    helper.verify();
    let recovery = helper.quarantine();
    directory(&destination);
    write(&destination.join("file"), b"foreign");
    helper.send(&Decision::Publish);
    assert!(
        matches!(helper.finish(), ExistingReply::Failed { outcome_unknown: true, recovery: Some(path), .. } if Path::new(&path) == recovery)
    );
    assert_eq!(fs::read(destination.join("file")).unwrap(), b"foreign");
    assert_eq!(identity(&recovery), original);
    assert_eq!(fs::read(recovery.join("file")).unwrap(), b"original");
}

#[test]
fn a_displaced_parent_cannot_certify_a_stale_recovery_path() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let parent = fixture.tree("anchor");
    let destination = parent.join("receiver");
    directory(&destination);
    let source = fixture.tree("source");
    write(&source.join("file"), b"incoming");
    write(&destination.join("file"), b"original");
    let (mut helper, payload) =
        prepare(&destination, inventory(&source), review(&destination), true);
    populate(&source, &payload);
    helper.verify();
    let old_recovery = helper.quarantine();
    let moved = fixture.0.join("moved-parent");
    fs::rename(&parent, &moved).unwrap();
    directory(&parent);
    directory(&destination);
    write(&destination.join("file"), b"foreign");

    helper.send(&Decision::Publish);
    assert!(matches!(
        helper.finish(),
        ExistingReply::Failed {
            outcome_unknown: true,
            recovery: None,
            ..
        }
    ));
    assert_eq!(fs::read(destination.join("file")).unwrap(), b"foreign");
    let held = moved.join(old_recovery.strip_prefix(&parent).unwrap());
    assert_eq!(fs::read(held.join("file")).unwrap(), b"original");
}

#[test]
fn concurrent_publication_respects_the_held_receiver_lock() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    write(&destination.join("file"), b"original");
    write(&source.join("file"), b"incoming");
    let original = identity(&destination);
    let lock = File::open(&destination).unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();

    let (mut helper, payload) =
        prepare(&destination, inventory(&source), review(&destination), true);
    populate(&source, &payload);
    helper.verify();
    helper.send(&Decision::Quarantine);
    assert_refused(helper.finish(), "holds the reviewed receiver");
    assert_eq!(identity(&destination), original);
    assert_eq!(fs::read(destination.join("file")).unwrap(), b"original");
    assert_no_stage(&fixture.0);
}

#[test]
fn refuses_same_length_receiver_edits_at_prepare_verify_and_commit() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    let target = destination.join("file");
    write(&target, b"old");
    write(&source.join("file"), b"src");
    let incoming = inventory(&source);
    let old_root = identity(&destination);
    let old_file = identity(&target);

    let stale = review(&destination);
    fs::write(&target, b"one").unwrap();
    assert_refused(
        invoke(&prepare_request(&destination, incoming, stale, false)),
        "changed",
    );
    assert_eq!(fs::read(&target).unwrap(), b"one");
    assert_no_stage(&fixture.0);

    let (mut helper, payload) = prepare(&destination, incoming, review(&destination), false);
    populate(&source, &payload);
    fs::write(&target, b"two").unwrap();
    helper.send(&Decision::Verify);
    assert_refused(helper.finish(), "changed");
    assert_eq!(fs::read(&target).unwrap(), b"two");
    assert_no_stage(&fixture.0);

    let (mut helper, payload) = prepare(&destination, incoming, review(&destination), false);
    populate(&source, &payload);
    helper.verify();
    fs::write(&target, b"new").unwrap();
    helper.send(&Decision::Quarantine);
    assert_refused(helper.finish(), "changed");
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(identity(&destination), old_root);
    assert_eq!(identity(&target), old_file);
    assert_no_stage(&fixture.0);
}

#[test]
fn refuses_foreign_receiver_installed_after_verify() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    write(&destination.join("file"), b"original");
    write(&source.join("file"), b"incoming");
    let old_root = identity(&destination);
    let (mut helper, payload) =
        prepare(&destination, inventory(&source), review(&destination), true);
    populate(&source, &payload);
    helper.verify();

    let moved = fixture.0.join("moved");
    fs::rename(&destination, &moved).unwrap();
    directory(&destination);
    write(&destination.join("file"), b"foreign");
    let foreign = identity(&destination);
    helper.send(&Decision::Quarantine);
    assert_refused(helper.finish(), "changed");
    assert_eq!(identity(&destination), foreign);
    assert_eq!(fs::read(destination.join("file")).unwrap(), b"foreign");
    assert_eq!(identity(&moved), old_root);
    assert_eq!(fs::read(moved.join("file")).unwrap(), b"original");
    assert_no_stage(&fixture.0);
}

#[test]
fn refuses_untrusted_entries_and_merge_that_would_remove_a_nonempty_directory() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.tree("receiver");
    let source = fixture.tree("source");
    let outside = fixture.0.join("outside");
    write(&outside, b"protected contents");
    let suspect = destination.join("suspect");

    symlink(&outside, &suspect).unwrap();
    assert_refused(review_reply(&destination), "unsupported entry");
    fs::remove_file(&suspect).unwrap();
    fs::hard_link(&outside, &suspect).unwrap();
    assert_refused(review_reply(&destination), "invalid");
    fs::remove_file(&suspect).unwrap();
    write(&suspect, b"writable by group");
    set_mode(&suspect, 0o660);
    assert_refused(review_reply(&destination), "unprotected entry");
    fs::remove_file(&suspect).unwrap();

    let original = review(&destination);
    write(&source.join("collision"), b"incoming file");
    let (mut helper, payload) = prepare(&destination, inventory(&source), original, false);
    symlink(&outside, payload.join("collision")).unwrap();
    helper.send(&Decision::Verify);
    assert_refused(helper.finish(), "unsupported entry");
    assert_eq!(review(&destination), original);
    assert_no_stage(&fixture.0);

    let (mut helper, payload) = prepare(&destination, inventory(&source), original, false);
    write(&payload.join("collision"), b"incoming fake");
    helper.send(&Decision::Verify);
    assert_refused(helper.finish(), "differs from the reviewed inventory");
    assert_eq!(review(&destination), original);
    assert_no_stage(&fixture.0);

    directory(&destination.join("collision"));
    write(&destination.join("collision/retain"), b"receiver child");
    let original = review(&destination);
    let (mut helper, payload) = prepare(&destination, inventory(&source), original, false);
    populate(&source, &payload);
    helper.send(&Decision::Verify);
    assert_refused(helper.finish(), "nonempty destination directory");
    assert_eq!(review(&destination), original);
    assert_eq!(
        fs::read(destination.join("collision/retain")).unwrap(),
        b"receiver child"
    );
    assert_eq!(fs::read(outside).unwrap(), b"protected contents");
    assert_no_stage(&fixture.0);
}

fn review_reply(destination: &Path) -> ExistingReply {
    invoke(&ExistingRequest::Review {
        destination: destination.to_str().unwrap().to_owned(),
    })
}

fn review(destination: &Path) -> ExistingTree {
    match review_reply(destination) {
        ExistingReply::Reviewed { original } => original,
        reply => panic!("expected Reviewed, got {reply:?}"),
    }
}

fn prepare_request(
    destination: &Path,
    incoming: Inventory,
    original: ExistingTree,
    delete: bool,
) -> ExistingRequest {
    ExistingRequest::Prepare {
        destination: destination.to_str().unwrap().to_owned(),
        incoming,
        original,
        delete,
    }
}

fn prepare(
    destination: &Path,
    incoming: Inventory,
    original: ExistingTree,
    delete: bool,
) -> (Helper, PathBuf) {
    let mut helper = Helper::start();
    helper.send(&prepare_request(destination, incoming, original, delete));
    prepared(helper, destination)
}

fn prepare_request_v2(
    destination: &Path,
    incoming: Inventory,
    original: ExistingTree,
    delete: DeleteScope,
) -> ExistingRequestV2 {
    ExistingRequestV2::Prepare {
        destination: destination.to_str().unwrap().to_owned(),
        incoming,
        original,
        delete,
    }
}

fn prepare_v2(
    destination: &Path,
    incoming: Inventory,
    original: ExistingTree,
    delete: DeleteScope,
) -> (Helper, PathBuf) {
    let mut helper = Helper::start_with(EXISTING_V2_ARGUMENT);
    helper.send_v2(&prepare_request_v2(destination, incoming, original, delete));
    prepared(helper, destination)
}

fn prepared(mut helper: Helper, destination: &Path) -> (Helper, PathBuf) {
    match helper.receive() {
        ExistingReply::Prepared { stage } => {
            assert!(valid_stage(&stage));
            let stage = destination.parent().unwrap().join(stage);
            let payload = stage.join("payload");
            assert_eq!(mode(&stage), 0o700);
            assert_eq!(mode(&payload), 0o700);
            (helper, payload)
        }
        reply => panic!("expected Prepared, got {reply:?}"),
    }
}

fn publish(mut helper: Helper, expected_destination: &Path, expected: Inventory) -> PathBuf {
    let displaced = helper.quarantine();
    assert!(!expected_destination.exists());
    helper.send(&Decision::Publish);
    match helper.finish() {
        ExistingReply::Published {
            destination,
            successor,
            recovery,
        } => {
            assert_eq!(Path::new(&destination), expected_destination);
            assert_eq!(successor, expected);
            let recovery = PathBuf::from(recovery);
            assert_eq!(recovery, displaced);
            let stage = recovery.parent().unwrap();
            assert!(valid_stage(stage.file_name().unwrap().to_str().unwrap()));
            assert_eq!(stage.parent(), expected_destination.parent());
            assert_eq!(recovery.file_name().unwrap(), "original");
            recovery
        }
        reply => panic!("expected Published, got {reply:?}"),
    }
}

fn assert_refused(reply: ExistingReply, expected: &str) {
    match reply {
        ExistingReply::Failed {
            message,
            outcome_unknown: false,
            recovery: None,
        } => {
            assert!(
                message.contains(expected),
                "expected {expected:?} in {message:?}"
            );
        }
        reply => panic!("expected a refusal before publication, got {reply:?}"),
    }
}

fn assert_no_stage(parent: &Path) {
    assert!(fs::read_dir(parent).unwrap().all(|entry| {
        let name = entry.unwrap().file_name();
        !valid_stage(name.to_str().unwrap())
    }));
}

fn inventory(root: &Path) -> Inventory {
    fn visit(root: &Path, folder: &Path, manifest: &mut Manifest) {
        let mut entries: Vec<_> = fs::read_dir(folder).unwrap().map(Result::unwrap).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
            if entry.file_type().unwrap().is_dir() {
                manifest.add(relative, None).unwrap();
                visit(root, &path, manifest);
            } else {
                let bytes = fs::read(&path).unwrap();
                manifest
                    .add(
                        relative,
                        Some((bytes.len() as u64, Sha256::digest(&bytes).into())),
                    )
                    .unwrap();
            }
        }
    }

    let mut manifest = Manifest::default();
    visit(root, root, &mut manifest);
    manifest.finish()
}

fn populate(source: &Path, payload: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = payload.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            directory(&path);
            populate(&entry.path(), &path);
        } else {
            write(&path, &fs::read(entry.path()).unwrap());
        }
    }
}

fn directory(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn write(path: &Path, data: &[u8]) {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(data)
        .unwrap();
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o7777
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn invoke(request: &ExistingRequest) -> ExistingReply {
    let mut helper = Helper::start();
    helper.send(request);
    helper.finish()
}

struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Helper {
    fn start() -> Self {
        Self::start_with(EXISTING_ARGUMENT)
    }

    fn start_with(argument: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kortty-server-archive"))
            .arg(argument)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        Self {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, request: &impl serde::Serialize) {
        write_frame(self.input.as_mut().unwrap(), request).unwrap();
    }

    fn send_v2(&mut self, request: &ExistingRequestV2) {
        write_existing_v2_request(self.input.as_mut().unwrap(), request).unwrap();
    }

    fn receive(&mut self) -> ExistingReply {
        read_frame(&mut self.output).unwrap()
    }

    fn verify(&mut self) -> Inventory {
        self.send(&Decision::Verify);
        match self.receive() {
            ExistingReply::Verified { successor } => successor,
            reply => panic!("expected Verified, got {reply:?}"),
        }
    }

    fn quarantine(&mut self) -> PathBuf {
        self.send(&Decision::Quarantine);
        match self.receive() {
            ExistingReply::Quarantined { recovery } => recovery.into(),
            reply => panic!("expected Quarantined, got {reply:?}"),
        }
    }

    fn finish(mut self) -> ExistingReply {
        drop(self.input.take());
        let reply = self.receive();
        assert!(self.child.wait().unwrap().success());
        reply
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-rsync-existing-{}", hex::encode(nonce)));
        directory(&path);
        Self(path)
    }

    fn tree(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        directory(&path);
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
