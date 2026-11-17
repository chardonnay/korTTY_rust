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

    fn publish_first(
        &self,
        receiver: VacantLocalTreeReceiver,
        inventory: Inventory,
        delete: &DeleteScope,
        arm: impl FnOnce(u32) -> Result<(), Failure>,
    ) -> Result<Published, Failure> {
        receiver.publish(
            Incoming {
                directory: &self.incoming,
                inventory,
                delete,
            },
            &|| false,
            &|| Ok(()),
            serde_json::from_str("42").unwrap(),
            arm,
        )
    }
}

fn write(directory: &Directory, name: &str, content: &[u8]) {
    let mut file = local_tree::create_reviewed_tree_file(directory, OsStr::new(name)).unwrap();
    file.write_all(content).unwrap();
}

fn reviewed_missing_file(path: &Path) -> VacantLocalTreeSnapshot {
    match review_file_vacancy(path, &|| false).unwrap() {
        crate::job_executor::rsync::VacantFileProof::Hierarchy(proof) => proof,
        crate::job_executor::rsync::VacantFileProof::File(_) => {
            panic!("expected a vacant file beneath missing parents")
        }
    }
}

fn stage_missing_file(
    receiver: &MissingLocalFileReceiver,
    content: &[u8],
) -> (MissingLocalFileStage, Inventory) {
    let stage = receiver.stage().unwrap();
    let mut file = stage.writer().unwrap();
    file.write_all(content).unwrap();
    let witness = crate::sftp_window::FileWitness::capture(file, &|| false).unwrap();
    (stage, witness.inventory)
}

#[test]
fn vacant_local_rsync_file_publishes_missing_parents_and_exact_successor() {
    use crate::job_executor::rsync::VacantFileProof;
    use crate::sftp_window::ReviewedFileReceiver;

    let fixture = Fixture::new();
    let prefix = fixture.root.join("missing");
    let first = prefix.join("nested/first.txt");
    let vacancy = reviewed_missing_file(&first);
    let receiver = MissingLocalFileReceiver::prepare(&first, vacancy, &[], &|| false).unwrap();
    assert_eq!(receiver.missing_parent_count(), 2);
    let (stage, incoming) = stage_missing_file(&receiver, b"first contents");
    assert!(!prefix.exists());
    let outcome = receiver
        .publish(stage, incoming, &|| false, &|| Ok(()), |removed| {
            assert_eq!(removed, 0);
            assert!(!prefix.exists());
            Ok(())
        })
        .unwrap();
    let MissingLocalFilePublication::Published {
        successor,
        parents,
        remaining_stage,
    } = outcome
    else {
        panic!("checked file publication did not produce a receipt");
    };
    assert!(remaining_stage.is_none());
    assert_eq!(fs::read(&first).unwrap(), b"first contents");
    assert_eq!(successor.inventory, incoming);
    assert!(VacantFileProof::Hierarchy(vacancy).allows_successor(&successor));
    assert!(ReviewedFileReceiver::prepare(&first, *successor, &|| false).is_ok());
    assert!(parents.is_empty() || parents[0].matches_path(&prefix));
    assert!(parents.iter().all(|receipt| !receipt.matches_path(&first)));
    assert!(fixture.recovery().is_none());
}

#[test]
fn vacant_local_rsync_file_requires_shared_receipts_even_with_no_parents_left_to_create() {
    use crate::job_executor::rsync::VacantFileProof;

    let fixture = Fixture::new();
    let prefix = fixture.root.join("missing/nested");
    let first = prefix.join("first.txt");
    let sibling = prefix.join("second.txt");
    let first_proof = reviewed_missing_file(&first);
    let sibling_proof = reviewed_missing_file(&sibling);
    let receiver = MissingLocalFileReceiver::prepare(&first, first_proof, &[], &|| false).unwrap();
    let (stage, incoming) = stage_missing_file(&receiver, b"first contents");
    let outcome = receiver
        .publish(stage, incoming, &|| false, &|| Ok(()), |_| Ok(()))
        .unwrap();
    let MissingLocalFilePublication::Published { parents, .. } = outcome else {
        panic!("expected confirmed shared parent creation");
    };
    assert!(MissingLocalFileReceiver::prepare(&sibling, sibling_proof, &[], &|| false).is_err());
    if !first_proof.same_publication_prefix(&sibling_proof) {
        assert!(parents.is_empty());
        return;
    }
    assert_eq!(parents.len(), 2);
    let receiver =
        MissingLocalFileReceiver::prepare(&sibling, sibling_proof, &parents, &|| false).unwrap();
    assert_eq!(receiver.missing_parent_count(), 0);
    let (stage, incoming) = stage_missing_file(&receiver, b"second contents");
    let outcome = receiver
        .publish(stage, incoming, &|| false, &|| Ok(()), |_| Ok(()))
        .unwrap();
    let MissingLocalFilePublication::Published {
        successor,
        parents,
        remaining_stage,
    } = outcome
    else {
        panic!("expected direct file publication in the confirmed parent");
    };
    assert!(remaining_stage.is_none());
    assert!(parents.is_empty());
    assert!(VacantFileProof::Hierarchy(sibling_proof).allows_successor(&successor));
    assert_eq!(fs::read(first).unwrap(), b"first contents");
    assert_eq!(fs::read(sibling).unwrap(), b"second contents");
    assert!(fixture.recovery().is_none());
}

#[test]
fn vacant_local_rsync_file_leaves_a_late_first_parent_untouched() {
    let fixture = Fixture::new();
    let prefix = fixture.root.join("missing");
    let file = prefix.join("nested/target.txt");
    let proof = reviewed_missing_file(&file);
    let receiver = MissingLocalFileReceiver::prepare(&file, proof, &[], &|| false).unwrap();
    let (stage, incoming) = stage_missing_file(&receiver, b"received");
    let result = receiver.publish(stage, incoming, &|| false, &|| Ok(()), |_| {
        let root = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
        let foreign = local_tree::create_reviewed_tree_stage(&root, OsStr::new("missing")).unwrap();
        write(&foreign, "sentinel", b"foreign occupant");
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(prefix.join("sentinel")).unwrap(),
        b"foreign occupant"
    );
    assert!(!file.exists());
    assert!(fixture.recovery().is_none());
}

#[test]
fn vacant_local_rsync_tree_returns_the_exact_successor_for_the_next_firing() {
    let mut fixture = Fixture::new();
    fixture.target = fixture.root.join("vacant-receiver");
    let named =
        local_tree::create_reviewed_tree_stage(&fixture.incoming, OsStr::new("named")).unwrap();
    write(&named, "child", b"inside");
    drop(local_tree::create_reviewed_tree_stage(&named, OsStr::new("empty")).unwrap());
    drop(named);
    let vacancy = VacantLocalTreeReceiver::inspect(&fixture.target, &|| false).unwrap();
    let receiver = VacantLocalTreeReceiver::prepare(&fixture.target, vacancy, &|| false).unwrap();
    let inventory = fixture.inventory();
    let published = fixture
        .publish_first(
            receiver,
            inventory,
            &DeleteScope::Named(vec!["named".into()]),
            |removed| {
                assert_eq!(removed, 0);
                assert!(!fixture.target.exists());
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(
        published.completion.outcome,
        RunOutcome::Success,
        "{:?}",
        published.completion.detail
    );
    let successor = published.successor.unwrap();
    assert_eq!(successor, fixture.snapshot());
    assert_eq!(successor.inventory, inventory);
    assert_eq!(successor.location(), Some(vacancy.location()));
    assert!(fixture.recovery().is_none());
    assert!(fixture.target.join("named/empty").is_dir());
    assert_eq!(
        fs::read(fixture.target.join("named/child")).unwrap(),
        b"inside"
    );

    let receiver = Receiver::prepare(&fixture.target, successor, &|| false).unwrap();
    fs::write(fixture.root.join("incoming/named/child"), b"updated").unwrap();
    fs::remove_file(fixture.root.join("incoming/new")).unwrap();
    let next = fixture
        .apply_armed(receiver, fixture.inventory(), true, &|| Ok(()), |removed| {
            assert_eq!(removed, 1);
            Ok(())
        })
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(
        next.completion.outcome,
        RunOutcome::Success,
        "{:?}",
        next.completion.detail
    );
    assert_eq!(next.successor, Some(fixture.snapshot()));
    let recovery = fixture.recovery().unwrap();
    assert!(successor.aliases(&inspect(&recovery, &|| false).unwrap()));
    assert_eq!(fs::read(recovery.join("named/child")).unwrap(), b"inside");
    assert_eq!(
        fs::read(fixture.target.join("named/child")).unwrap(),
        b"updated"
    );
    assert!(!fixture.target.join("new").exists());
}

#[test]
fn vacant_local_rsync_tree_checks_the_candidate_and_preserves_an_arrival_during_arm() {
    let mut fixture = Fixture::new();
    fixture.target = fixture.root.join("vacant-receiver");
    let vacancy = VacantLocalTreeReceiver::inspect(&fixture.target, &|| false).unwrap();
    let receiver =
        || VacantLocalTreeReceiver::prepare(&fixture.target, vacancy, &|| false).unwrap();
    let mut corrupt = fixture.inventory();
    corrupt.digest[0] ^= 1;
    let failed = fixture.publish_first(receiver(), corrupt, &DeleteScope::All, |_| {
        panic!("an unverified incoming tree must not arm publication");
    });
    assert!(matches!(
        failed,
        Err(Failure {
            outcome: RunOutcome::Blocked,
            ..
        })
    ));
    assert!(fixture.recovery().is_none());
    assert!(!fixture.target.exists());

    let armed = Cell::new(false);
    let failed = fixture.publish_first(
        receiver(),
        fixture.inventory(),
        &DeleteScope::Retain,
        |removed| {
            assert_eq!(removed, 0);
            assert!(!armed.replace(true));
            let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
            let occupant =
                local_tree::create_reviewed_tree_stage(&parent, OsStr::new("vacant-receiver"))
                    .unwrap();
            write(&occupant, "other", b"arrived before publication");
            Ok(())
        },
    );
    assert!(armed.get());
    assert!(matches!(
        failed,
        Err(Failure {
            outcome: RunOutcome::Blocked,
            ..
        })
    ));
    assert!(fixture.recovery().is_none());
    assert_eq!(
        fs::read(fixture.target.join("other")).unwrap(),
        b"arrived before publication"
    );
    assert!(!fixture.target.join("file").exists());
}

#[test]
fn vacant_local_rsync_tree_binds_the_parent_and_reads_legacy_existing_receipts() {
    let fixture = Fixture::new();
    let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
    let reviewed = OsStr::new("reviewed-parent");
    drop(local_tree::create_reviewed_tree_stage(&parent, reviewed).unwrap());
    let path = fixture.root.join(reviewed).join("vacant");
    let vacancy = VacantLocalTreeReceiver::inspect(&path, &|| false).unwrap();
    assert!(
        VacantLocalTreeReceiver::prepare(&path.with_file_name("other"), vacancy, &|| false)
            .is_err()
    );
    fs::rename(
        path.parent().unwrap(),
        fixture.root.join("displaced-parent"),
    )
    .unwrap();
    drop(local_tree::create_reviewed_tree_stage(&parent, reviewed).unwrap());
    assert!(VacantLocalTreeReceiver::prepare(&path, vacancy, &|| false).is_err());

    let existing = fixture.snapshot();
    assert!(existing.location().is_some());
    let mut old = serde_json::to_value(existing).unwrap();
    old.as_object_mut().unwrap().remove("location");
    let old: LocalExistingSnapshot = serde_json::from_value(old).unwrap();
    assert!(old.location().is_none());
    assert!(Receiver::prepare(&fixture.target, old, &|| false).is_ok());
}

#[test]
fn vacant_local_rsync_tree_publishes_missing_parents_with_an_exact_leaf_receipt() {
    let mut fixture = Fixture::new();
    let prefix = fixture.root.join("first-missing");
    fixture.target = prefix.join("nested-parent/receiver");
    let vacancy = VacantLocalTreeReceiver::inspect(&fixture.target, &|| false).unwrap();
    assert_eq!(vacancy.missing_parent_count(), 2);
    assert_eq!(
        vacancy.first_missing(&fixture.target),
        Some(prefix.as_path())
    );
    let sibling = VacantLocalTreeReceiver::inspect(&prefix.join("other/leaf"), &|| false).unwrap();
    assert!(vacancy.location().overlaps(sibling.location()));
    assert!(
        VacantLocalTreeReceiver::prepare(&prefix.join("other/leaf"), vacancy, &|| false).is_err()
    );
    let receiver = VacantLocalTreeReceiver::prepare(&fixture.target, vacancy, &|| false).unwrap();
    assert_eq!(receiver.missing_parent_count(), 2);
    let published = fixture
        .publish_first(
            receiver,
            fixture.inventory(),
            &DeleteScope::All,
            |removed| {
                assert_eq!(removed, 0);
                assert!(!prefix.exists());
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(
        published.completion.outcome,
        RunOutcome::Success,
        "{:?}",
        published.completion.detail
    );
    let successor = published.successor.unwrap();
    assert!(vacancy.allows_successor(&successor));
    let on_disk = fixture.snapshot();
    assert!(!vacancy.allows_successor(&on_disk));
    assert_eq!(successor.inventory, on_disk.inventory);
    assert_eq!(successor.location(), on_disk.location());
    assert!(successor.aliases(&on_disk));
    assert!(fixture.recovery().is_none());
    assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"incoming");

    let receiver = Receiver::prepare(&fixture.target, successor, &|| false).unwrap();
    fs::write(fixture.root.join("incoming/file"), b"later revision").unwrap();
    let replaced = fixture
        .apply(receiver, fixture.inventory(), true, &|| Ok(()))
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(
        replaced.completion.outcome,
        RunOutcome::Success,
        "{:?}",
        replaced.completion.detail
    );
    let next = replaced.successor.unwrap();
    assert_eq!(next, fixture.snapshot());
    assert!(!next.aliases(&on_disk));
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"later revision"
    );
}

#[test]
fn vacant_local_rsync_tree_shares_only_confirmed_parents_and_counts_each_once() {
    let fixture = Fixture::new();
    let shared = fixture.root.join("shared");
    let common = shared.join("common");
    let first = common.join("first");
    let second = common.join("second");
    let branch = shared.join("branch");
    let third = branch.join("third");
    let approve = |path: &Path| VacantLocalTreeReceiver::inspect(path, &|| false).unwrap();
    let (first_review, second_review, third_review) =
        (approve(&first), approve(&second), approve(&third));
    let first_receiver = VacantLocalTreeReceiver::prepare(&first, first_review, &|| false).unwrap();
    assert_eq!(first_receiver.missing_parent_count(), 2);
    let publish = |receiver| {
        let result = fixture
            .publish_first(
                receiver,
                fixture.inventory(),
                &DeleteScope::All,
                |removed| {
                    assert_eq!(removed, 0);
                    Ok(())
                },
            )
            .unwrap_or_else(|error| panic!("{}", error.detail));
        assert_eq!(
            result.completion.outcome,
            RunOutcome::Success,
            "{:?}",
            result.completion.detail
        );
        result
    };
    let receipt = publish(first_receiver);
    let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
    let stamp = local_tree::reviewed_tree_parent_stamp(&parent).unwrap();
    if local_tree::reviewed_tree_parent_birth(&parent, &stamp).is_none() {
        assert!(!first_review.same_publication_prefix(&second_review));
        assert!(
            VacantLocalTreeReceiver::prepare_with_parents(
                &second,
                second_review,
                &receipt.parents,
                &|| false
            )
            .is_err()
        );
        return;
    }

    assert!(first_review.same_publication_prefix(&second_review));
    assert!(first_review.same_publication_prefix(&third_review));
    assert_eq!(receipt.parents.len(), 2);
    assert!(receipt.parents[0].matches_path(&shared));
    assert!(receipt.parents[1].matches_path(&common));
    assert!(VacantLocalTreeReceiver::prepare(&second, second_review, &|| false).is_err());
    assert!(
        VacantLocalTreeReceiver::prepare_with_parents(
            &second,
            second_review,
            &receipt.parents[1..],
            &|| false,
        )
        .is_err()
    );

    let second_receiver = VacantLocalTreeReceiver::prepare_with_parents(
        &second,
        second_review,
        &receipt.parents,
        &|| false,
    )
    .unwrap();
    assert_eq!(second_receiver.missing_parent_count(), 0);
    let second_receipt = publish(second_receiver);
    assert!(second_review.allows_successor(&second_receipt.successor.unwrap()));
    assert!(second_receipt.parents.is_empty());

    let third_receiver = VacantLocalTreeReceiver::prepare_with_parents(
        &third,
        third_review,
        &receipt.parents,
        &|| false,
    )
    .unwrap();
    assert_eq!(third_receiver.missing_parent_count(), 1);
    let third_receipt = publish(third_receiver);
    assert!(third_review.allows_successor(&third_receipt.successor.unwrap()));
    assert_eq!(third_receipt.parents.len(), 1);
    assert!(third_receipt.parents[0].matches_path(&branch));

    for receiver in [first, second, third] {
        assert_eq!(fs::read(receiver.join("file")).unwrap(), b"incoming");
    }
}

#[test]
fn vacant_local_rsync_tree_refuses_unrecorded_removed_and_replaced_shared_parents() {
    let fixture = Fixture::new();
    let shared = fixture.root.join("shared");
    let first = shared.join("first");
    let second = shared.join("second/leaf");
    let first_review = VacantLocalTreeReceiver::inspect(&first, &|| false).unwrap();
    let second_review = VacantLocalTreeReceiver::inspect(&second, &|| false).unwrap();
    let first_receiver = VacantLocalTreeReceiver::prepare(&first, first_review, &|| false).unwrap();
    let published = fixture
        .publish_first(
            first_receiver,
            fixture.inventory(),
            &DeleteScope::All,
            |_| Ok(()),
        )
        .unwrap_or_else(|error| panic!("{}", error.detail));
    assert_eq!(
        published.completion.outcome,
        RunOutcome::Success,
        "{:?}",
        published.completion.detail
    );
    if !first_review.same_publication_prefix(&second_review) {
        assert!(published.parents.is_empty());
        return;
    }

    let prepare = || {
        VacantLocalTreeReceiver::prepare_with_parents(
            &second,
            second_review,
            &published.parents,
            &|| false,
        )
    };
    assert!(prepare().is_ok());
    {
        let parent = local_tree::open_reviewed_tree_parent(&shared).unwrap();
        let foreign =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("second")).unwrap();
        write(&foreign, "sentinel", b"unrecorded");
    }
    assert!(prepare().is_err());
    assert_eq!(
        fs::read(shared.join("second/sentinel")).unwrap(),
        b"unrecorded"
    );

    let displaced = fixture.root.join("displaced");
    fs::rename(&shared, &displaced).unwrap();
    assert!(prepare().is_err());
    assert!(!shared.exists());
    {
        let parent = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
        let replacement =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("shared")).unwrap();
        write(&replacement, "sentinel", b"replacement");
    }
    assert!(prepare().is_err());
    assert_eq!(fs::read(shared.join("sentinel")).unwrap(), b"replacement");
    assert_eq!(fs::read(displaced.join("first/file")).unwrap(), b"incoming");
}

#[test]
fn vacant_local_rsync_tree_preserves_a_prefix_occupied_during_arm() {
    let mut fixture = Fixture::new();
    let prefix = fixture.root.join("first-missing");
    fixture.target = prefix.join("nested-parent/receiver");
    let vacancy = VacantLocalTreeReceiver::inspect(&fixture.target, &|| false).unwrap();
    let receiver = VacantLocalTreeReceiver::prepare(&fixture.target, vacancy, &|| false).unwrap();
    let armed = Cell::new(false);
    let result = fixture.publish_first(
        receiver,
        fixture.inventory(),
        &DeleteScope::All,
        |removed| {
            assert_eq!(removed, 0);
            assert!(!armed.replace(true));
            let anchor = local_tree::open_reviewed_tree_parent(&fixture.root).unwrap();
            let occupant =
                local_tree::create_reviewed_tree_stage(&anchor, OsStr::new("first-missing"))
                    .unwrap();
            write(&occupant, "independent", b"unrelated prefix");
            Ok(())
        },
    );
    assert!(armed.get());
    assert!(matches!(
        result,
        Err(Failure {
            outcome: RunOutcome::Blocked,
            ..
        })
    ));
    assert!(fixture.recovery().is_none());
    assert!(!prefix.join("nested-parent").exists());
    assert_eq!(
        fs::read(prefix.join("independent")).unwrap(),
        b"unrelated prefix"
    );
    assert!(VacantLocalTreeReceiver::prepare(&fixture.target, vacancy, &|| false).is_err());
}

#[test]
fn vacant_local_rsync_tree_bounds_the_missing_parent_suffix() {
    let fixture = Fixture::new();
    let path = (0..33).fold(fixture.root.clone(), |path, _| path.join("new"));
    assert_eq!(
        VacantLocalTreeReceiver::inspect(&path, &|| false)
            .unwrap()
            .missing_parent_count(),
        32
    );
    assert!(VacantLocalTreeReceiver::inspect(&path.join("leaf"), &|| false).is_err());
    let path = fixture.root.join("vacant-root");
    let vacancy = VacantLocalTreeReceiver::inspect(&path, &|| false).unwrap();
    let serialized = serde_json::to_value(vacancy).unwrap();
    assert!(serialized.get("missing_parents").is_none());
    assert_eq!(
        serde_json::from_value::<VacantLocalTreeSnapshot>(serialized).unwrap(),
        vacancy
    );
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
    let parent = local_tree::open_reviewed_tree_parent(&fixture.target).unwrap();
    let folder = local_tree::create_reviewed_tree_stage(&parent, OsStr::new("file")).unwrap();
    write(&folder, "keep", b"kept");
    drop((folder, parent));
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
    stage
        .compose(
            Some(View {
                directory: &receiver.directory,
                tree: &receiver.reviewed,
            }),
            Incoming {
                directory: &fixture.incoming,
                inventory: fixture.inventory(),
                delete: &DeleteScope::All,
            },
            receiver.reviewed.stamp.volume(),
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
