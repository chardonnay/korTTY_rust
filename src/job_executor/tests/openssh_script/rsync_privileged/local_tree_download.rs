use super::local_file_download::{Peer, ordinary_fixture, require_file_fixture};
use super::mixed::recurring::Recurring;
use super::*;
use crate::scheduler_window::rsync::standing::Budget;

const FIRST: &[u8] = b"first direct rsync directory";
const NEXT: &[u8] = b"changed direct rsync directory";

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_direct_rsync_download_creates_and_replaces_reviewed_local_trees() {
    let server = Sshd::new();
    let (fixture, _) = ordinary_fixture(&[&server]);
    let peer = Peer::Ordinary(&server);
    exercise(&fixture, &fixture.profile.0, peer, false, false);
    one_use(&fixture, peer, false);
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync helper fixture"]
fn openssh_passwordless_recurring_direct_rsync_download_creates_and_replaces_reviewed_local_trees()
{
    require_file_fixture();
    let fixture = Fixture::new();
    exercise(
        &fixture,
        Path::new("/test-rsync/read"),
        Peer::Privileged,
        true,
        false,
    );
    assert!(
        fs::read_dir("/test-rsync/handoff/probe")
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_direct_rsync_download_creates_reviewed_local_hierarchies() {
    let server = Sshd::new();
    let (fixture, _) = ordinary_fixture(&[&server]);
    let peer = Peer::Ordinary(&server);
    exercise(&fixture, &fixture.profile.0, peer, false, true);
    one_use(&fixture, peer, true);
    tight_entry_budget(&fixture, &fixture.profile.0, peer, false);
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync helper fixture"]
fn openssh_passwordless_recurring_direct_rsync_download_creates_reviewed_local_hierarchies() {
    require_file_fixture();
    let fixture = Fixture::new();
    let root = Path::new("/test-rsync/read");
    exercise(&fixture, root, Peer::Privileged, true, true);
    tight_entry_budget(&fixture, root, Peer::Privileged, true);
    assert!(
        fs::read_dir("/test-rsync/handoff/probe")
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_rsync_group_shares_reviewed_local_parents() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    let peers = [Peer::Ordinary(&first), Peer::Ordinary(&second)];
    exercise_shared_group(&fixture, &fixture.profile.0, &additional[0], peers);
    let root = fixture.profile.0.join("manual-shared");
    let source = fixture.profile.0.join("manual-group-source");
    seed_source(&source);
    let destinations = ["first", "second"].map(|name| root.join(name));
    let action = reviewed_shared_download(
        &fixture,
        peers[0],
        &source,
        &additional[0],
        &destinations,
        true,
    );
    let fire = || peers[0].run(|| peers[1].run(|| fixture.execute(action.clone(), true)));
    let created = fire();
    assert_eq!(created.outcome, RunOutcome::Success, "{created:?}");
    let effective = destinations.map(|destination| destination.join("manual-group-source"));
    let originals = effective.each_ref().map(|path| {
        assert_eq!(fs::read(path.join("nested/content")).unwrap(), FIRST);
        rsync::review_local_existing(path, &AtomicBool::new(false)).unwrap()
    });

    let replay = fire();
    assert_eq!(replay.outcome, RunOutcome::Blocked, "{replay:?}");
    for (path, first) in effective.iter().zip(originals) {
        assert_eq!(
            rsync::review_local_existing(path, &AtomicBool::new(false)).unwrap(),
            first
        );
        assert!(recovery_paths(path).is_empty());
    }
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync helper fixture"]
fn openssh_passwordless_rsync_group_shares_reviewed_local_parents() {
    require_file_fixture();
    let mut fixture = Fixture::new();
    let additional = fixture.second_pinned_reference();
    exercise_shared_group(
        &fixture,
        Path::new("/test-rsync/read"),
        &additional,
        [Peer::Privileged; 2],
    );
    assert!(
        fs::read_dir("/test-rsync/handoff/probe")
            .unwrap()
            .next()
            .is_none()
    );
}

fn seed_source(source: &Path) {
    fs::create_dir(source).unwrap();
    fs::set_permissions(source, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(source.join("nested")).unwrap();
    fs::create_dir(source.join("empty")).unwrap();
    fs::write(source.join("nested/content"), FIRST).unwrap();
    fs::write(source.join("obsolete"), b"removed by a later source change").unwrap();
}

fn reviewed_download(
    fixture: &Fixture,
    peer: Peer<'_>,
    source: &Path,
    configured: &Path,
    effective: &Path,
    named: bool,
) -> SshCommand {
    let remote = source.to_str().unwrap();
    let mut action = if matches!(peer, Peer::Privileged) {
        serde_json::to_value(fixture.action(&Case {
            local: configured,
            path: remote,
            upload: false,
            directory: true,
            replace: false,
        }))
        .unwrap()
    } else {
        let pins = sync::fingerprints(&sync::pins(&fixture.reference, &fixture.trust).unwrap());
        let digest = rsync::program_digest(&fixture.program).unwrap();
        json!({
            "connection": fixture.reference,
            "remote": {
                "kind": "rsync_download", "directory": true, "program": fixture.program,
                "local": configured, "remote": remote,
                "approval": {
                    "program": fixture.program, "digest": digest,
                    "reference": fixture.reference, "pins": pins,
                    "directory": {"source": remote, "destination": configured},
                }
            }
        })
    };
    let settings = json!({"delete": true, "include_source_name": named});
    let snapshot = rsync::review_local_vacant_tree(effective, &AtomicBool::new(false)).unwrap();
    action["remote"]["existing_download"] = settings.clone();
    action["remote"]["allow_vacant_directory_download"] = json!(true);
    action["remote"]["approval"]["vacant_directory_download"] =
        json!({"destination": effective, "snapshot": snapshot, "settings": settings});
    serde_json::from_value::<SshCommand>(action)
        .unwrap()
        .validated()
        .unwrap()
}

fn reviewed_shared_download(
    fixture: &Fixture,
    peer: Peer<'_>,
    source: &Path,
    additional: &ConnectionRef,
    destinations: &[PathBuf; 2],
    named: bool,
) -> SshCommand {
    let effective = |destination: &Path| {
        if named {
            destination.join(source.file_name().unwrap())
        } else {
            destination.to_owned()
        }
    };
    let [first, second] = destinations;
    let mut action = serde_json::to_value(reviewed_download(
        fixture,
        peer,
        source,
        first,
        &effective(first),
        named,
    ))
    .unwrap();
    let mut approval = action["remote"]["approval"].clone();
    let second_effective = effective(second);
    let snapshot =
        rsync::review_local_vacant_tree(&second_effective, &AtomicBool::new(false)).unwrap();
    approval["reference"] = json!(additional);
    approval["pins"] = json!(sync::fingerprints(
        &sync::pins(additional, &fixture.trust).unwrap()
    ));
    approval["directory"]["destination"] = json!(second);
    approval["vacant_directory_download"]["destination"] = json!(second_effective);
    approval["vacant_directory_download"]["snapshot"] = json!(snapshot);
    action["additional_connections"] = json!([additional]);
    action["remote"]["routes"] = json!([{
        "profile": additional.id,
        "destination": second,
        "approval": approval,
    }]);
    serde_json::from_value::<SshCommand>(action)
        .unwrap()
        .validated()
        .unwrap()
}

fn exercise_shared_group(
    fixture: &Fixture,
    source_root: &Path,
    additional: &ConnectionRef,
    peers: [Peer<'_>; 2],
) {
    let source = source_root.join("same-source-for-both-routes");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let data = source.join("report");
    fs::write(&data, FIRST).unwrap();
    let chain = matches!(peers[0], Peer::Privileged);
    let group = |name| {
        let shared = fixture.profile.0.join(name);
        let destinations = ["first", "second"].map(|name| shared.join(name));
        let command =
            reviewed_shared_download(fixture, peers[0], &source, additional, &destinations, false);
        let job = Recurring::with_budget(
            fixture,
            name,
            command,
            chain,
            Budget {
                bytes: 1024,
                entries: 3,
                deletes: 1,
            },
        );
        assert!(!shared.exists(), "review created a missing parent");
        (job, shared, destinations)
    };
    let fire = |job: &Recurring<'_>, round| peers[0].run(|| peers[1].run(|| job.fire(round, true)));

    let (job, shared, targets) = group("only-count-shared-parent-once");
    fs::create_dir(&shared).unwrap();
    let occupant = shared.join("external");
    fs::write(&occupant, b"external occupancy").unwrap();
    let blocked = fire(&job, 0);
    assert_eq!(blocked.outcome, RunOutcome::Blocked, "{blocked:?}");
    assert_eq!(fs::read(&occupant).unwrap(), b"external occupancy");
    assert!(targets.iter().all(|path| !path.exists()));
    fs::remove_file(occupant).unwrap();
    fs::remove_dir(shared).unwrap();

    // Both routes fit exactly: two source files plus one new common parent.
    let published = fire(&job, 1);
    assert_eq!(published.outcome, RunOutcome::Success, "{published:?}");
    for target in &targets {
        assert_eq!(fs::read(target.join("report")).unwrap(), FIRST);
        assert!(recovery_paths(target).is_empty());
    }

    let (retry, shared, [first, second]) = group("resume-sibling-with-parent-receipt");
    let extra = source.join("consume-first-route-budget");
    fs::write(&extra, b"extra").unwrap();
    let partial = fire(&retry, 0);
    assert_ne!(partial.outcome, RunOutcome::Success, "{partial:?}");
    assert!(
        partial
            .detail
            .as_deref()
            .unwrap()
            .contains("1 of 2 targets succeeded")
    );
    assert_eq!(fs::read(first.join("report")).unwrap(), FIRST);
    assert!(!second.exists());

    // Leave the original parent available for a later exact recovery, but
    // refuse a new lookalike at the same path while the second route is pending.
    let held = shared.with_file_name("temporarily-displaced-shared-parent");
    fs::rename(&shared, &held).unwrap();
    fs::create_dir(&shared).unwrap();
    let marker = shared.join("external-replacement");
    fs::write(&marker, b"unauthorized parent").unwrap();
    let changed = fire(&retry, 1);
    assert_eq!(changed.outcome, RunOutcome::Blocked, "{changed:?}");
    assert!(!first.exists() && !second.exists());
    assert_eq!(fs::read(&marker).unwrap(), b"unauthorized parent");
    fs::remove_file(marker).unwrap();
    fs::remove_dir(&shared).unwrap();
    fs::rename(&held, &shared).unwrap();
    fs::remove_file(extra).unwrap();
    fs::write(data, NEXT).unwrap();

    // Recurring::fire reopens the Scheduler and its on-disk grant for each run.
    let resumed = fire(&retry, 2);
    assert_eq!(resumed.outcome, RunOutcome::Success, "{resumed:?}");
    for target in [&first, &second] {
        assert_eq!(fs::read(target.join("report")).unwrap(), NEXT);
        assert!(!target.join("consume-first-route-budget").exists());
    }
    let recovery = recovery_paths(&first);
    let [original] = recovery.as_slice() else {
        panic!("expected exactly one retained receiver");
    };
    assert_eq!(fs::read(original.join("report")).unwrap(), FIRST);
    assert!(original.join("consume-first-route-budget").is_file());
}

fn exercise(
    fixture: &Fixture,
    source_root: &Path,
    peer: Peer<'_>,
    chain: bool,
    missing_parents: bool,
) {
    for (named, name) in [(false, "direct-contents"), (true, "direct-named")] {
        let name = if missing_parents {
            format!("{name}-hierarchy")
        } else {
            name.to_owned()
        };
        let source = source_root.join(format!("{name}-source"));
        seed_source(&source);
        let local = fixture.profile.0.join(&name);
        fs::create_dir(&local).unwrap();
        let prefix = local.join("new parent");
        let configured = if missing_parents {
            prefix.join("inner").join("target")
        } else {
            local.join("target")
        };
        let effective = if named {
            if !missing_parents {
                fs::create_dir(&configured).unwrap();
            }
            configured.join(source.file_name().unwrap())
        } else {
            configured.clone()
        };
        let action = reviewed_download(fixture, peer, &source, &configured, &effective, named);
        let job = Recurring::new(fixture, &name, action, chain, 1);

        let first_missing = if missing_parents { &prefix } else { &effective };
        assert!(!first_missing.exists(), "review created the vacant path");
        fs::create_dir(first_missing).unwrap();
        fs::write(first_missing.join("occupant"), b"independent arrival").unwrap();
        let blocked = peer.run(|| job.fire(0, true));
        assert_eq!(blocked.outcome, RunOutcome::Blocked, "{blocked:?}");
        assert_eq!(
            fs::read(first_missing.join("occupant")).unwrap(),
            b"independent arrival"
        );
        assert!(recovery_paths(first_missing).is_empty());
        if missing_parents {
            assert!(!effective.exists());
        }
        fs::rename(first_missing, local.join("preserved-late-arrival")).unwrap();

        let first = peer.run(|| job.fire(1, true));
        assert_eq!(first.outcome, RunOutcome::Success, "{first:?}");
        assert_eq!(fs::read(effective.join("nested/content")).unwrap(), FIRST);
        assert!(effective.join("empty").is_dir());
        assert!(recovery_paths(&effective).is_empty());
        let sibling = effective.parent().unwrap().join("sibling");
        fs::write(&sibling, b"outside the receiver").unwrap();
        let original = rsync::review_local_existing(&effective, &AtomicBool::new(false)).unwrap();

        fs::write(source.join("nested/content"), NEXT).unwrap();
        fs::remove_file(source.join("obsolete")).unwrap();
        let next = peer.run(|| job.fire(2, true));
        assert_eq!(next.outcome, RunOutcome::Success, "{next:?}");
        assert_eq!(fs::read(effective.join("nested/content")).unwrap(), NEXT);
        assert!(!effective.join("obsolete").exists());
        assert!(effective.join("empty").is_dir());
        let current = rsync::review_local_existing(&effective, &AtomicBool::new(false)).unwrap();
        assert!(!current.aliases(&original));
        let recoveries = recovery_paths(&effective);
        let [recovery] = recoveries.as_slice() else {
            panic!("expected exactly one retained original")
        };
        assert!(
            next.detail
                .as_deref()
                .unwrap()
                .contains(recovery.to_str().unwrap())
        );
        assert!(
            rsync::review_local_existing(recovery, &AtomicBool::new(false))
                .unwrap()
                .aliases(&original)
        );
        assert_eq!(fs::read(recovery.join("nested/content")).unwrap(), FIRST);
        assert!(recovery.join("obsolete").is_file());
        assert_eq!(
            fs::read(local.join("preserved-late-arrival/occupant")).unwrap(),
            b"independent arrival"
        );
        assert_eq!(fs::read(sibling).unwrap(), b"outside the receiver");
    }
}

fn one_use(fixture: &Fixture, peer: Peer<'_>, missing_parent: bool) {
    let source = fixture.profile.0.join("manual-direct-source");
    seed_source(&source);
    let parent = fixture.profile.0.join("manual-direct-workspace");
    fs::create_dir(&parent).unwrap();
    let destination = if missing_parent {
        parent.join("new workspace").join("tree")
    } else {
        parent.join("tree")
    };
    let action = reviewed_download(fixture, peer, &source, &destination, &destination, false);
    let created = peer.run(|| fixture.execute(action.clone(), true));
    assert_eq!(created.outcome, RunOutcome::Success, "{created:?}");
    let first = rsync::review_local_existing(&destination, &AtomicBool::new(false)).unwrap();
    let replay = peer.run(|| fixture.execute(action, true));
    assert_eq!(replay.outcome, RunOutcome::Blocked, "{replay:?}");
    assert_eq!(
        rsync::review_local_existing(&destination, &AtomicBool::new(false)).unwrap(),
        first
    );
    assert_eq!(fs::read(destination.join("nested/content")).unwrap(), FIRST);
    assert!(recovery_paths(&destination).is_empty());
}

fn tight_entry_budget(fixture: &Fixture, source_root: &Path, peer: Peer<'_>, chain: bool) {
    let source = source_root.join("direct-empty-source");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let prefix = fixture.profile.0.join("missing-parent");
    let destination = prefix.join("empty-destination");
    let action = reviewed_download(fixture, peer, &source, &destination, &destination, false);
    let job = Recurring::with_budget(
        fixture,
        "direct-one-parent-budget",
        action,
        chain,
        Budget {
            bytes: 16,
            entries: 1,
            deletes: 0,
        },
    );
    let extra = source.join("extra-entry");
    fs::write(&extra, b"x").unwrap();
    let refused = peer.run(|| job.fire(0, true));
    assert_ne!(refused.outcome, RunOutcome::Success, "{refused:?}");
    assert!(!prefix.exists());
    assert!(recovery_paths(&prefix).is_empty());

    fs::remove_file(extra).unwrap();
    let created = peer.run(|| job.fire(1, true));
    assert_eq!(created.outcome, RunOutcome::Success, "{created:?}");
    assert!(fs::read_dir(&destination).unwrap().next().is_none());
    assert!(recovery_paths(&destination).is_empty());

    fs::write(source.join("new-entry"), b"x").unwrap();
    let replaced = peer.run(|| job.fire(2, true));
    assert_eq!(replaced.outcome, RunOutcome::Success, "{replaced:?}");
    assert_eq!(fs::read(destination.join("new-entry")).unwrap(), b"x");
}

fn recovery_paths(destination: &Path) -> Vec<PathBuf> {
    fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            kortty_server_archive::rsync_tree::valid_stage(path.file_name()?.to_str()?)
                .then(|| path.join("original"))
        })
        .collect()
}
