use std::fs;
use std::io::Write as _;

use crate::snippets::tests::Profile;

use super::*;

struct Fixture {
    incoming: Directory,
    root: PathBuf,
    target: PathBuf,
    _profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new();
        let profile_root = profile.0.canonicalize().unwrap();
        let root = profile_root.join("workspace");
        #[cfg(unix)]
        fs::create_dir(&root).unwrap();
        #[cfg(windows)]
        {
            let profile_directory = Directory::open_path(&profile_root).unwrap();
            let _private_workspace =
                local_tree::create_reviewed_tree_stage(&profile_directory, OsStr::new("workspace"))
                    .unwrap();
        }
        let parent = local_tree::open_reviewed_tree_parent(&root).unwrap();
        let target = root.join("receiver");
        let original =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("receiver")).unwrap();
        write(&original, "file", b"original");
        write(&original, "retained", b"retained");
        drop(original);
        let incoming =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("incoming")).unwrap();
        write(&incoming, "file", b"incoming");
        write(&incoming, "new", b"new");
        Self {
            incoming,
            root,
            target,
            _profile: profile,
        }
    }

    fn snapshot(&self) -> LocalExistingSnapshot {
        review(&self.target, &AtomicBool::new(false)).unwrap()
    }

    fn inventory(&self) -> Inventory {
        let volume = local_tree::reviewed_tree_volume(&self.incoming).unwrap();
        Tree::capture(&self.incoming, volume, &|| false).unwrap().1
    }

    fn recovery(&self) -> Option<PathBuf> {
        fs::read_dir(&self.root).unwrap().find_map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name()?.to_str()?;
            kortty_server_archive::rsync_tree::valid_stage(name).then(|| path.join(ORIGINAL))
        })
    }

    fn apply(
        &self,
        receiver: Receiver,
        snapshot: Inventory,
        delete: bool,
        authorize: &impl Fn() -> Result<(), Failure>,
    ) -> Result<Published, Failure> {
        self.apply_armed(receiver, snapshot, delete, authorize, |_| Ok(()))
    }

    fn apply_armed(
        &self,
        receiver: Receiver,
        snapshot: Inventory,
        delete: bool,
        authorize: &impl Fn() -> Result<(), Failure>,
        arm: impl FnOnce(u32) -> Result<(), Failure>,
    ) -> Result<Published, Failure> {
        receiver.publish(
            Incoming {
                directory: &self.incoming,
                inventory: snapshot,
                delete: &if delete {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                },
            },
            &|| false,
            authorize,
            serde_json::from_str("42").unwrap(),
            arm,
        )
    }
}

fn write(directory: &Directory, name: &str, content: &[u8]) {
    let mut file = local_tree::create_reviewed_tree_file(directory, OsStr::new(name)).unwrap();
    file.write_all(content).unwrap();
}

#[test]
fn existing_local_rsync_merge_and_mirror_keep_the_exact_original_in_private_recovery() {
    for delete in [false, true] {
        let fixture = Fixture::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                fixture.target.join("file"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
        }
        let snapshot = fixture.snapshot();
        let receiver = Receiver::prepare(&fixture.target, snapshot, &|| false).unwrap();
        assert!(review(&fixture.target, &AtomicBool::new(false)).is_err());
        let published = fixture
            .apply(receiver, fixture.inventory(), delete, &|| Ok(()))
            .unwrap_or_else(|error| panic!("{}", error.detail));
        assert_eq!(published.successor, Some(fixture.snapshot()));
        let completion = published.completion;
        assert_eq!(
            completion.outcome,
            RunOutcome::Success,
            "{:?}",
            completion.detail
        );
        let detail = completion.detail.unwrap();
        let recovery = fixture.recovery().expect("private recovery");
        assert!(detail.contains(recovery.to_str().unwrap()));
        assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"incoming");
        assert_eq!(fs::read(fixture.target.join("new")).unwrap(), b"new");
        assert_eq!(fixture.target.join("retained").exists(), !delete);
        assert_eq!(fs::read(recovery.join("file")).unwrap(), b"original");
        assert_eq!(fs::read(recovery.join("retained")).unwrap(), b"retained");
        assert!(snapshot.aliases(&review(&recovery, &AtomicBool::new(false)).unwrap()));
        assert!(!snapshot.aliases(&fixture.snapshot()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(fixture.target.join("file"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o640
            );
        }
    }
}

#[test]
fn existing_local_rsync_cancellation_restores_but_never_replaces_a_new_occupant() {
    for occupied in [false, true] {
        let fixture = Fixture::new();
        let snapshot = fixture.snapshot();
        let receiver = Receiver::prepare(&fixture.target, snapshot, &|| false).unwrap();
        let requested = Cell::new(false);
        let published = fixture
            .apply(receiver, fixture.inventory(), true, &|| {
                if !fixture.target.exists() && !requested.replace(true) && occupied {
                    fs::create_dir(&fixture.target).unwrap();
                    fs::write(fixture.target.join("file"), b"other occupant").unwrap();
                }
                if requested.get() {
                    Err(Failure {
                        outcome: RunOutcome::Cancelled,
                        detail: "Fixture cancellation",
                    })
                } else {
                    Ok(())
                }
            })
            .unwrap_or_else(|error| panic!("{}", error.detail));
        assert!(published.successor.is_none());
        let completion = published.completion;
        assert!(requested.get());
        let detail = completion.detail.unwrap();
        if occupied {
            assert!(matches!(completion.outcome, RunOutcome::Failed { .. }));
            let recovery = fixture.recovery().expect("retained displaced original");
            assert!(detail.contains(recovery.to_str().unwrap()));
            assert_eq!(
                fs::read(fixture.target.join("file")).unwrap(),
                b"other occupant"
            );
            assert_eq!(fs::read(recovery.join("file")).unwrap(), b"original");
        } else {
            assert_eq!(completion.outcome, RunOutcome::Cancelled);
            assert!(detail.contains("restored"));
            assert!(fixture.recovery().is_none());
            assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"original");
            assert!(snapshot.aliases(&fixture.snapshot()));
        }
    }
}

#[test]
fn existing_local_rsync_successor_reopens_and_rejects_an_identical_replacement() {
    let fixture = Fixture::new();
    let original = fixture.snapshot();
    let receiver = Receiver::prepare(&fixture.target, original, &|| false).unwrap();
    let first = fixture
        .apply(receiver, fixture.inventory(), true, &|| Ok(()))
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(first.completion.outcome, RunOutcome::Success);
    let successor = first.successor.unwrap();
    assert_eq!(successor, fixture.snapshot());
    assert!(Receiver::prepare(&fixture.target, original, &|| false).is_err());

    let receiver = Receiver::prepare(&fixture.target, successor, &|| false).unwrap();
    let next = fixture
        .apply(receiver, fixture.inventory(), true, &|| Ok(()))
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(next.completion.outcome, RunOutcome::Success);
    let next = next.successor.unwrap();
    assert_eq!(next, fixture.snapshot());
    assert_eq!(next.inventory, successor.inventory);
    assert!(!next.aliases(&successor));

    fs::rename(&fixture.target, fixture.root.join("different location")).unwrap();
    let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
    let replacement =
        local_tree::create_reviewed_tree_stage(&parent, OsStr::new("receiver")).unwrap();
    write(&replacement, "file", b"incoming");
    write(&replacement, "new", b"new");
    drop(replacement);
    assert_eq!(fixture.snapshot().inventory, next.inventory);
    assert!(Receiver::prepare(&fixture.target, next, &|| false).is_err());
}

#[test]
fn existing_local_rsync_arms_exact_removed_paths_before_the_first_move() {
    let fixture = Fixture::new();
    let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
    let original = local_tree::open_reviewed_tree_child(&parent, OsStr::new("receiver")).unwrap();
    let removed = local_tree::create_reviewed_tree_stage(&original, OsStr::new("removed")).unwrap();
    let nested = local_tree::create_reviewed_tree_stage(&removed, OsStr::new("nested")).unwrap();
    write(&removed, "a", b"old");
    write(&nested, "b", b"old nested");
    write(&original, "changed-kind", b"old file");
    drop((nested, removed, original, parent));
    write(&fixture.incoming, "removed", b"new file");
    drop(
        local_tree::create_reviewed_tree_stage(&fixture.incoming, OsStr::new("changed-kind"))
            .unwrap(),
    );

    // "removed" and its three descendants, "changed-kind", and "retained"
    // disappear by path or kind. The same-kind overwrite of "file" costs zero.
    let initial = fixture.snapshot();
    let receiver = Receiver::prepare(&fixture.target, initial, &|| false).unwrap();
    let blocked = fixture.apply_armed(receiver, fixture.inventory(), true, &|| Ok(()), |removed| {
        assert_eq!(removed, 6);
        assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"original");
        Err(Failure::blocked("Fixture receipt was not stored"))
    });
    assert!(matches!(
        blocked,
        Err(Failure {
            outcome: RunOutcome::Blocked,
            detail: "Fixture receipt was not stored",
        })
    ));
    assert_eq!(fixture.snapshot(), initial);
    assert!(fixture.recovery().is_none());

    let receiver = Receiver::prepare(&fixture.target, initial, &|| false).unwrap();
    let armed = Cell::new(false);
    let revoked = fixture.apply_armed(
        receiver,
        fixture.inventory(),
        true,
        &|| {
            if armed.get() {
                Err(Failure::blocked("Fixture authority revoked"))
            } else {
                Ok(())
            }
        },
        |removed| {
            assert_eq!(removed, 6);
            armed.set(true);
            Ok(())
        },
    );
    assert!(armed.get());
    assert!(matches!(
        revoked,
        Err(Failure {
            outcome: RunOutcome::Blocked,
            ..
        })
    ));
    assert_eq!(fixture.snapshot(), initial);
    assert!(fixture.recovery().is_none());
}

#[test]
fn existing_local_rsync_refuses_stale_bytes_and_merge_that_would_remove_a_folder() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    fs::write(fixture.target.join("file"), b"modified").unwrap();
    assert!(Receiver::prepare(&fixture.target, snapshot, &|| false).is_err());
    assert!(snapshot.aliases(&fixture.snapshot()));
    let snapshot = fixture.snapshot();
    let receiver = Receiver::prepare(&fixture.target, snapshot, &|| false).unwrap();
    let mut wrong_manifest = fixture.inventory();
    wrong_manifest.digest[0] ^= 1;
    assert!(
        fixture
            .apply(receiver, wrong_manifest, true, &|| Ok(()))
            .is_err()
    );
    assert!(fixture.recovery().is_none());

    fs::remove_file(fixture.target.join("file")).unwrap();
    fs::create_dir(fixture.target.join("file")).unwrap();
    fs::write(fixture.target.join("file/keep"), b"kept").unwrap();
    let snapshot = fixture.snapshot();
    let receiver = Receiver::prepare(&fixture.target, snapshot, &|| false).unwrap();
    let outcome = fixture.apply(receiver, fixture.inventory(), false, &|| Ok(()));
    assert!(outcome.is_err());
    assert!(fixture.recovery().is_none());
    assert_eq!(fs::read(fixture.target.join("file/keep")).unwrap(), b"kept");
}

#[test]
fn existing_local_rsync_cleanup_leaves_an_entry_not_created_in_the_private_stage() {
    let fixture = Fixture::new();
    let receiver = Receiver::prepare(&fixture.target, fixture.snapshot(), &|| false).unwrap();
    let mut stage = Stage::create(&receiver.parent).unwrap();
    receiver
        .compose(
            &mut stage,
            &fixture.incoming,
            fixture.inventory(),
            &DeleteScope::All,
            &|| false,
        )
        .unwrap();
    let payload = stage.path(&fixture.root).join(PAYLOAD);
    let foreign = fixture.root.join("unrelated-file");
    fs::write(&foreign, b"untouched").unwrap();
    fs::rename(
        payload.join("file"),
        fixture.root.join("old-candidate-file"),
    )
    .unwrap();
    fs::rename(&foreign, payload.join("file")).unwrap();
    assert!(stage.cleanup().is_err());
    assert_eq!(fs::read(payload.join("file")).unwrap(), b"untouched");
    assert_eq!(fs::read(payload.join("new")).unwrap(), b"new");
    assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn existing_local_rsync_never_reports_success_under_a_swapped_parent() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let receiver = Receiver::prepare(&fixture.target, snapshot, &|| false).unwrap();
    let displaced = fixture.root.with_file_name("displaced-workspace");
    let swapped = Cell::new(false);
    let published = fixture
        .apply(receiver, fixture.inventory(), true, &|| {
            if !fixture.target.exists() && !swapped.replace(true) {
                fs::rename(&fixture.root, &displaced).unwrap();
                fs::create_dir(&fixture.root).unwrap();
                fs::create_dir(&fixture.target).unwrap();
                fs::write(fixture.target.join("file"), b"foreign parent").unwrap();
            }
            Ok(())
        })
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert!(published.successor.is_none());
    let completion = published.completion;
    assert!(swapped.get());
    assert!(matches!(completion.outcome, RunOutcome::Failed { .. }));
    assert!(
        completion
            .detail
            .unwrap()
            .contains("could not be confirmed")
    );
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"foreign parent"
    );
    let recovery = fs::read_dir(displaced)
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name()?.to_str()?;
            kortty_server_archive::rsync_tree::valid_stage(name).then(|| path.join(ORIGINAL))
        })
        .unwrap();
    assert_eq!(fs::read(recovery.join("file")).unwrap(), b"original");
}
