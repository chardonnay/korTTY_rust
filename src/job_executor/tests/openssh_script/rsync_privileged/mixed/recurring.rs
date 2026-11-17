use std::num::NonZeroU32;

use kortty_jobs::{DependencyMode, JobSchedule, TimeZone};

use crate::scheduler_window::rsync::standing;

use super::super::local_file_download::{Peer, ordinary_fixture, require_file_fixture};
use super::*;

mod ordinary;

pub(in crate::job_executor::tests::openssh_script::rsync_privileged) const FIRING_BYTES: u64 = 2048;
const CHANGED: [&[u8]; 2] = [b"protected first firing\n", b"protected second firing\n"];

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync helper fixture"]
fn openssh_passwordless_recurring_rsync_creates_vacant_server_trees() {
    require_file_fixture();
    let fixture = Fixture::new();
    let digest = rsync::program_digest(&fixture.program).unwrap();
    let pins = sync::fingerprints(&sync::pins(&fixture.reference, &fixture.trust).unwrap());
    for (name, named, mixed, chain) in [
        ("contents", false, false, false),
        ("named", true, false, true),
        ("mixed", false, true, true),
    ] {
        let job = format!("protected-vacant-tree-{name}");
        let source = fixture.profile.0.join(format!("source-{name}"));
        directory(&source);
        write(&source.join("removed"), CHANGED[0]);
        let report = fixture.profile.0.join("report");
        fs::write(&report, REPORT).unwrap();
        let parent = Path::new(ROOT).join("replace").join(&job);
        directory(&parent);
        let destination = parent.join(if named {
            source.file_name().unwrap()
        } else {
            std::ffi::OsStr::new("receiver")
        });
        let base = if named { &parent } else { &destination };
        let scope = if mixed {
            DeleteScope::Named(vec![source.file_name().unwrap().to_str().unwrap().into()])
        } else {
            DeleteScope::All
        };
        let spec = Spec {
            path: destination.to_str().unwrap().into(),
            operation: Operation::Upload {
                directory: true,
                replace: true,
                delete: scope.clone(),
            },
        };
        let mut approval = json!({"program":fixture.program,"digest":digest,
            "reference":fixture.reference,"pins":pins});
        let mut transfer = json!({"kind":"rsync_upload","program":fixture.program,
            "local":source,"remote":base,"directory":true,"elevation":"passwordless",
            "existing":{"include_source_name":named,"delete":true},
            "allow_vacant_server_tree_upload":true});
        if mixed {
            let sources = [&source, &report]
                .map(|path| path.to_str().unwrap().to_owned())
                .to_vec();
            let mut target = target(
                &fixture,
                true,
                &destination,
                sources.clone(),
                ReceiverMode::ScopedMirror,
            );
            target.review_vacant = true;
            let review = preview(&fixture, &target).unwrap();
            assert_eq!(review.plan.delete, scope);
            assert!(
                matches!(&review.privileged, Some(MixedPrivilegedReview::Upload(proof))
                if proof.first_tree && proof.spec == spec && proof.inventory.is_none())
            );
            approval["mixed"] = json!(review);
            transfer["mixed"] = json!(sources);
        } else {
            let credentials = Credentials::resolve_directory_browser(
                &fixture.reference,
                fixture.connections.get(fixture.reference.id).unwrap(),
                &ServerAccess::default(),
                |_, _, _| Err("unexpected credential"),
            )
            .unwrap();
            let review = rsync::preview_privileged_tree_creation(
                credentials,
                &fixture.reference,
                &spec,
                &fixture.trust,
                Arc::new(AtomicBool::new(false)),
                || Ok(()),
            )
            .unwrap();
            assert!(review.first_tree && review.spec == spec && review.inventory.is_none());
            approval["privileged"] = json!(review);
            approval["directory_upload"] = json!({"source":source,"destination":base,
                "inventory":rsync::upload_tree_inventory(&source).unwrap()});
        }
        transfer["approval"] = approval;
        let action: SshCommand = serde_json::from_value(json!({
            "connection": fixture.reference, "remote": transfer,
        }))
        .unwrap();
        let recurring = Recurring::new(&fixture, &job, action, chain, 1);

        directory(&destination);
        write(&destination.join("outsider"), b"arrived after review");
        let outsider = identity(&destination);
        assert_blocked(&recurring.fire(0, true));
        assert_eq!(identity(&destination), outsider);
        assert_eq!(
            fs::read(destination.join("outsider")).unwrap(),
            b"arrived after review"
        );
        fs::remove_file(destination.join("outsider")).unwrap();
        fs::remove_dir(&destination).unwrap();
        assert_handoff_cleared();

        let first = recurring.fire(1, true);
        assert_success(&first);
        assert!(!first.detail.unwrap().contains("original is retained at "));
        let placed = if mixed {
            destination.join(source.file_name().unwrap())
        } else {
            destination.clone()
        };
        assert_eq!(fs::read(placed.join("removed")).unwrap(), CHANGED[0]);
        let original = identity(&destination);
        let ledger: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.profile.0.join("job-rsync-standing-reviews.json")).unwrap(),
        )
        .unwrap();
        let receiver = &ledger[&job]["grant"]["routes"][0]["receiver"];
        let proof = fixture.preview(&spec).unwrap();
        assert_eq!(
            receiver["snapshot"]["fingerprint"],
            json!(proof.fingerprint)
        );
        assert_eq!(receiver["snapshot"]["inventory"], json!(proof.inventory));

        fs::rename(source.join("removed"), source.join("current")).unwrap();
        fs::write(source.join("current"), CHANGED[1]).unwrap();
        let result = recurring.fire(2, true);
        assert_success(&result);
        let recovery = recovered(&result, &destination, original);
        let original_scope = if mixed {
            recovery.join(source.file_name().unwrap())
        } else {
            recovery.to_owned()
        };
        assert_eq!(
            fs::read(original_scope.join("removed")).unwrap(),
            CHANGED[0]
        );
        assert_eq!(fs::read(placed.join("current")).unwrap(), CHANGED[1]);
        assert!(!placed.join("removed").exists());
        if mixed {
            assert_eq!(fs::read(destination.join("report")).unwrap(), REPORT);
        }
        assert_handoff_cleared();
    }
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_rsync_group_creates_reviewed_local_trees() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    recurring_vacant_group(
        &fixture,
        &fixture.profile.0.join("recurring-grouped-tree-inputs"),
        &additional[0],
        [Peer::Ordinary(&first), Peer::Ordinary(&second)],
        false,
    );
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_manual_mixed_rsync_group_shares_reviewed_local_parents() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    let additional = &additional[0];
    let root = &fixture.profile.0;
    let source = root.join("reports");
    directory(&source);
    write(&source.join("downloaded"), b"named directory contents");
    let report = root.join("report.txt");
    write(&report, b"separate file source");
    let sources = [&source, &report].map(|path| path.to_str().unwrap().to_owned());
    let shared = root.join("missing-shared-parent");
    let destinations = ["first", "second"].map(|name| shared.join(name));
    let digest = rsync::program_digest(&fixture.program).unwrap();
    let review = |server: &Sshd, reference: &ConnectionRef, destination: &Path| {
        let target = MixedReviewTarget {
            reference: reference.clone(),
            sources: sources.to_vec(),
            destination: MixedDestination::Local(destination.to_owned()),
            delete: true,
            elevation: Elevation::Ordinary,
            review_vacant: true,
        };
        let daemon = server.spawn();
        let reviewed = preview(&fixture, &target);
        let trace = daemon.join().unwrap().stderr;
        assert!(String::from_utf8_lossy(&trace).contains("Accepted publickey"));
        let reviewed = reviewed.unwrap();
        assert!(
            matches!(&reviewed.receiver, MixedReceiver::Local { vacant: Some(snapshot), snapshot: None, .. } if snapshot.missing_parent_count() == 1)
        );
        json!({"program":fixture.program,"digest":digest,"reference":reference,
            "pins":sync::fingerprints(&sync::pins(reference, &fixture.trust).unwrap()),"mixed":reviewed})
    };
    let action: SshCommand = serde_json::from_value(json!({
        "connection":fixture.reference,"additional_connections":[additional],
        "remote":{
            "kind":"rsync_download","directory":true,"program":fixture.program,
            "local":destinations[0],"remote":source,"mixed":sources,
            "existing_download":{"include_source_name":false,"delete":true},
            "allow_vacant_directory_download":true,
            "approval":review(&first, &fixture.reference, &destinations[0]),
            "routes":[{"profile":additional.id,"destination":destinations[1],
                "approval":review(&second, additional, &destinations[1])}]
        }
    }))
    .unwrap();
    let action = action.validated().unwrap();
    let fire = || {
        Peer::Ordinary(&first)
            .run(|| Peer::Ordinary(&second).run(|| fixture.execute(action.clone(), false)))
    };
    assert!(!shared.exists());
    assert_success(&fire());
    let cancelled = AtomicBool::new(false);
    let snapshots = destinations.each_ref().map(|path| {
        assert_eq!(
            fs::read(path.join("reports/downloaded")).unwrap(),
            b"named directory contents"
        );
        assert_eq!(
            fs::read(path.join("report.txt")).unwrap(),
            b"separate file source"
        );
        rsync::review_local_existing(path, &cancelled).unwrap()
    });
    assert_blocked(&fire());
    for (path, original) in destinations.iter().zip(snapshots) {
        assert_eq!(
            rsync::review_local_existing(path, &cancelled).unwrap(),
            original
        );
    }
    assert_eq!(fs::read_dir(&shared).unwrap().count(), 2);
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_group_creates_reviewed_local_trees() {
    require_file_fixture();
    let mut fixture = Fixture::new();
    let additional = fixture.second_pinned_reference();
    recurring_vacant_group(
        &fixture,
        Path::new("/test-rsync/read/recurring-grouped-tree-inputs"),
        &additional,
        [Peer::Privileged; 2],
        false,
    );
    assert_handoff_cleared();
}

#[test]
#[ignore = "requires native OpenSSH, rsync 3.4.1 or later, and the current korTTY bridge"]
fn openssh_recurring_rsync_group_creates_disjoint_local_hierarchies() {
    let first = Sshd::new();
    let second = Sshd::new();
    let (fixture, additional) = ordinary_fixture(&[&first, &second]);
    recurring_vacant_group(
        &fixture,
        &fixture.profile.0.join("recurring-hierarchy-inputs"),
        &additional[0],
        [Peer::Ordinary(&first), Peer::Ordinary(&second)],
        true,
    );
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_group_creates_disjoint_local_hierarchies() {
    require_file_fixture();
    let mut fixture = Fixture::new();
    let additional = fixture.second_pinned_reference();
    recurring_vacant_group(
        &fixture,
        Path::new("/test-rsync/read/recurring-hierarchy-inputs"),
        &additional,
        [Peer::Privileged; 2],
        true,
    );
    assert_handoff_cleared();
}

fn recurring_vacant_group(
    fixture: &Fixture,
    inputs: &Path,
    additional: &ConnectionRef,
    peers: [Peer<'_>; 2],
    missing_parents: bool,
) {
    directory(inputs);
    let report = inputs.join("report");
    write(&report, b"second mixed input");
    let chain = matches!(peers[0], Peer::Privileged);
    for (name, contents, mixed) in [
        ("named", false, false),
        ("contents", true, false),
        ("mixed", false, true),
    ] {
        let source = inputs.join(name);
        directory(&source);
        write(&source.join("removed"), b"reviewed original tree");
        let mut sources = vec![format!(
            "{}{}",
            source.display(),
            if contents { "/" } else { "" }
        )];
        if mixed {
            sources.push(report.to_str().unwrap().to_owned());
        }
        let first_prefix = fixture.profile.0.join(format!("first-{name}"));
        let second_prefix = fixture.profile.0.join(format!("second-{name}"));
        let destination = |prefix: &Path| {
            if missing_parents {
                prefix.join("inner").join("receiver")
            } else {
                prefix.to_owned()
            }
        };
        let first = destination(&first_prefix);
        let second = destination(&second_prefix);
        let settings = json!({"include_source_name":false,"delete":true});
        let approval = |reference: &ConnectionRef, path: &Path, peer: Peer<'_>| {
            let target = MixedReviewTarget {
                reference: reference.clone(),
                sources: sources.clone(),
                destination: MixedDestination::Local(path.to_owned()),
                delete: true,
                elevation: if chain {
                    Elevation::Passwordless
                } else {
                    Elevation::Ordinary
                },
                review_vacant: true,
            };
            let daemon = match peer {
                Peer::Ordinary(server) => Some(server.spawn()),
                Peer::Privileged => None,
            };
            let review = preview(fixture, &target);
            if let Some(daemon) = daemon {
                let trace = daemon.join().unwrap();
                assert!(String::from_utf8_lossy(&trace.stderr).contains("Accepted publickey"));
            }
            let review = review.unwrap();
            assert!(matches!(
                &review.receiver,
                MixedReceiver::Local {
                    snapshot: None,
                    vacant: Some(_),
                    ..
                }
            ));
            assert_eq!(
                review.plan.delete,
                if contents {
                    DeleteScope::All
                } else {
                    DeleteScope::Named(vec![name.into()])
                }
            );
            let digest = rsync::program_digest(&fixture.program).unwrap();
            let pins = sync::fingerprints(&sync::pins(reference, &fixture.trust).unwrap());
            json!({"program":fixture.program,"digest":digest,"reference":reference,"pins":pins,"mixed":review})
        };
        let action = serde_json::from_value(json!({
            "connection":fixture.reference,"additional_connections":[additional],
            "remote":{
                "kind":"rsync_download","directory":true,"program":fixture.program,
                "local":first,"remote":source,"mixed":sources,"existing_download":settings,
                "elevation":if chain { Elevation::Passwordless } else { Elevation::Ordinary },
                "approval":approval(&fixture.reference, &first, peers[0]),
                "routes":[{"profile":additional.id,"destination":second,
                    "approval":approval(additional, &second, peers[1])}]
            }
        }))
        .unwrap();
        let recurring =
            Recurring::new(fixture, &format!("recurring-{name}-tree"), action, chain, 1);
        let fire = |round| peers[0].run(|| peers[1].run(|| recurring.fire(round, true)));
        let relative = if contents {
            Path::new("")
        } else {
            Path::new(name)
        };
        assert!(!first_prefix.exists());
        assert!(!second_prefix.exists());
        directory(&second_prefix);
        write(&second_prefix.join("occupant"), b"arrived after review");
        let occupant = identity(&second_prefix);
        let partial = fire(0);
        assert_blocked(&partial);
        assert!(
            partial
                .detail
                .as_deref()
                .unwrap()
                .contains("1 of 2 targets succeeded")
        );
        assert_eq!(
            fs::read(first.join(relative).join("removed")).unwrap(),
            b"reviewed original tree"
        );
        assert_eq!(fs::metadata(&first).unwrap().mode() & 0o777, 0o700);
        assert_eq!(identity(&second_prefix), occupant);
        assert_eq!(
            fs::read(second_prefix.join("occupant")).unwrap(),
            b"arrived after review"
        );
        if missing_parents {
            assert!(!second.exists());
        }

        let original = identity(&first);
        let sibling = first.parent().unwrap().join(format!("untouched-{name}"));
        write(&sibling, b"outside the approved receiver");
        fs::remove_file(second_prefix.join("occupant")).unwrap();
        fs::remove_dir(second_prefix).unwrap();
        fs::rename(source.join("removed"), source.join("current")).unwrap();
        fs::write(source.join("current"), CHANGED[0]).unwrap();
        let published = fire(1);
        assert_success(&published);
        let recovery = recovered(&published, &first, original);
        assert_eq!(
            fs::read(recovery.join(relative).join("removed")).unwrap(),
            b"reviewed original tree"
        );
        for destination in [&first, &second] {
            verify_grouped_tree(destination, relative, CHANGED[0], mixed);
        }

        fs::write(source.join("current"), CHANGED[1]).unwrap();
        let replaced = fire(2);
        assert_success(&replaced);
        assert_eq!(
            replaced
                .detail
                .as_deref()
                .unwrap()
                .matches("original is retained at ")
                .count(),
            2
        );
        for destination in [&first, &second] {
            verify_grouped_tree(destination, relative, CHANGED[1], mixed);
        }
        assert_eq!(fs::read(sibling).unwrap(), b"outside the approved receiver");
    }
}

fn verify_grouped_tree(destination: &Path, relative: &Path, expected: &[u8], mixed: bool) {
    let tree = destination.join(relative);
    assert_eq!(fs::read(tree.join("current")).unwrap(), expected);
    assert!(!tree.join("removed").exists());
    if mixed {
        assert_eq!(
            fs::read(destination.join("report")).unwrap(),
            b"second mixed input"
        );
    }
}

#[test]
#[ignore = "requires the isolated native OpenSSH and recurring passwordless rsync fixture"]
fn openssh_passwordless_recurring_rsync_advances_direct_receivers() {
    require_isolated_root();
    let fixture = Fixture::new();
    let server = Path::new(ROOT);
    let source = fixture.profile.0.join("recurring-direct-source-file");
    let destination = server.join("replace/recurring-direct-file");
    write(&source, b"reviewed local source\n");
    write(&destination, b"reviewed protected receiver\n");
    let action = fixture.action(&Case {
        local: &source,
        path: destination.to_str().unwrap(),
        upload: true,
        directory: false,
        replace: true,
    });
    let recurring = Recurring::new(&fixture, "protected-recurring-file", action, false, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(&source, bytes).unwrap();
        let before = identity(&destination);
        let previous = fs::read(&destination).unwrap();
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        assert_ne!(identity(&destination), before);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(fs::read(recovery).unwrap(), previous);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
    }
    assert_blocked(&recurring.fire(2, false));
    assert_eq!(fs::read(&destination).unwrap(), CHANGED[1]);
    assert_handoff_cleared();

    direct_tree_upload(&fixture, server);

    let source = server.join("read/recurring-direct-tree");
    let destination = fixture.profile.0.join("recurring-direct-download");
    directory(&source);
    directory(&destination);
    write(&source.join("content"), b"reviewed protected source\n");
    write(&destination.join("removed"), b"first receiver\n");
    let action = fixture.action(&Case {
        local: &destination,
        path: source.to_str().unwrap(),
        upload: false,
        directory: true,
        replace: true,
    });
    let recurring = Recurring::new(&fixture, "protected-recurring-tree", action, true, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(source.join("content"), bytes).unwrap();
        let before = identity(&destination);
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(recovery.join("removed").exists(), iteration == 0);
        assert_eq!(fs::read(destination.join("content")).unwrap(), bytes);
        assert!(!destination.join("removed").exists());
    }

    let displaced = fixture.profile.0.join("recurring-displaced-download");
    fs::rename(&destination, &displaced).unwrap();
    directory(&destination);
    write(&destination.join("content"), CHANGED[1]);
    let occupant = identity(&destination);
    assert_blocked(&recurring.fire(2, true));
    assert_eq!(identity(&destination), occupant);
    assert_eq!(fs::read(destination.join("content")).unwrap(), CHANGED[1]);
    assert_eq!(fs::read(displaced.join("content")).unwrap(), CHANGED[1]);
    assert_handoff_cleared();
}

fn direct_tree_upload(fixture: &Fixture, server: &Path) {
    let source = fixture.profile.0.join("recurring-direct-source-tree");
    let destination = server.join("replace/recurring-direct-upload");
    directory(&source);
    directory(&destination);
    write(&source.join("content"), b"reviewed source tree\n");
    write(&destination.join("removed"), b"previous receiver\n");
    let action = fixture.action(&Case {
        local: &source,
        path: destination.to_str().unwrap(),
        upload: true,
        directory: true,
        replace: true,
    });
    let recurring = Recurring::new(fixture, "protected-recurring-tree-upload", action, false, 1);
    for (iteration, bytes) in CHANGED.into_iter().enumerate() {
        fs::write(source.join("content"), bytes).unwrap();
        let before = identity(&destination);
        let completion = recurring.fire(iteration, true);
        assert_success(&completion);
        let recovery = recovered(&completion, &destination, before);
        assert_eq!(recovery.join("removed").exists(), iteration == 0);
        assert_eq!(fs::read(destination.join("content")).unwrap(), bytes);
        assert!(!destination.join("removed").exists());
    }
    assert_handoff_cleared();
}

#[test]
#[ignore = "requires the isolated native OpenSSH and recurring passwordless mixed-rsync fixture"]
fn openssh_passwordless_recurring_mixed_rsync_limits_losing_inputs_and_advances_receivers() {
    require_isolated_root();
    let fixture = Fixture::new();
    let server = Path::new(ROOT);
    for upload in [true, false] {
        let (name, inputs, destination) = if upload {
            (
                "protected-recurring-mixed-upload",
                fixture.profile.0.join("recurring-mixed-upload-inputs"),
                server.join("replace/recurring-mixed-upload"),
            )
        } else {
            (
                "protected-recurring-mixed-download",
                server.join("read/recurring-mixed-download-inputs"),
                fixture.profile.0.join("recurring-mixed-download"),
            )
        };
        seed_inputs(&inputs);
        seed_receiver(&destination);
        let mode = ReceiverMode::ScopedMirror;
        let target = target(&fixture, upload, &destination, sources(&inputs, mode), mode);
        let review = preview(&fixture, &target).unwrap();
        let handoff = handoff(&review).to_owned();
        let recurring = Recurring::new(&fixture, name, action(&fixture, &target, review), false, 1);
        for (iteration, bytes) in CHANGED.into_iter().enumerate() {
            fs::write(inputs.join("first/report"), bytes).unwrap();
            if iteration == 1 {
                fs::rename(
                    inputs.join("named/scope/new"),
                    inputs.join("named/scope/next"),
                )
                .unwrap();
            }
            let before = identity(&destination);
            let previous = fs::read(destination.join("report")).unwrap();
            let completion = recurring.fire(iteration, true);
            assert_success(&completion);
            let recovery = recovered(&completion, &destination, before);
            assert_eq!(fs::read(recovery.join("report")).unwrap(), previous);
            assert_eq!(fs::read(destination.join("report")).unwrap(), bytes);
            assert!(!destination.join("scope/obsolete").exists());
            assert!(destination.join("scope-more/keep").is_file());
            assert!(destination.join("outside").is_file());
            assert_eq!(destination.join("scope/next").exists(), iteration == 1);
            assert_empty_handoff(&handoff);
        }

        let before = identity(&destination);
        fs::write(
            inputs.join("losing/report"),
            vec![b'x'; FIRING_BYTES as usize + 1],
        )
        .unwrap();
        assert_blocked(&recurring.fire(2, true));
        assert_eq!(identity(&destination), before);
        assert_eq!(fs::read(destination.join("report")).unwrap(), CHANGED[1]);
        assert_empty_handoff(&handoff);
    }
}

pub(in crate::job_executor::tests::openssh_script::rsync_privileged) struct Recurring<'a> {
    fixture: &'a Fixture,
    path: PathBuf,
    id: JobId,
    issued: SystemTime,
    predecessor: Option<JobId>,
}

impl<'a> Recurring<'a> {
    pub(in crate::job_executor::tests::openssh_script::rsync_privileged) fn new(
        fixture: &'a Fixture,
        name: &str,
        action: SshCommand,
        chain: bool,
        deletes: u32,
    ) -> Self {
        Self::with_budget(
            fixture,
            name,
            action,
            chain,
            standing::Budget {
                bytes: FIRING_BYTES,
                entries: 32,
                deletes,
            },
        )
    }

    pub(in crate::job_executor::tests::openssh_script::rsync_privileged) fn with_budget(
        fixture: &'a Fixture,
        name: &str,
        action: SshCommand,
        chain: bool,
        budget: standing::Budget,
    ) -> Self {
        let id = JobId::new(name).unwrap();
        let predecessor = chain.then(|| JobId::new("protected-recurring-predecessor").unwrap());
        let issued = SystemTime::now() - Duration::from_secs(15 * 60);
        let mut action = serde_json::to_value(action).unwrap();
        action["remote"]["standing"] = json!({
            "settings": {
                "days": 2, "firings": 4,
                "budget": budget,
            }
        });
        let job = |action| {
            let action: SshCommand = serde_json::from_value(action).unwrap();
            let job = Job::new(id.clone(), name, action.validated().unwrap()).unwrap();
            if let Some(predecessor) = &predecessor {
                job.with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None)
            } else {
                job.with_schedule(JobSchedule::default().with_interval_minutes(NonZeroU32::new(1)))
            }
        };
        let grant = standing::Grant::new(&job(action.clone()), &TimeZone::UTC, issued).unwrap();
        action["remote"]["standing"]["approval"] = json!(grant.approval);
        let path = fixture.profile.0.join(format!("{name}.json"));
        let mut jobs = vec![job(action)];
        if let Some(predecessor) = &predecessor {
            let command = serde_json::from_value(json!({
                "connection": fixture.reference, "command": "true",
            }))
            .unwrap();
            jobs.push(Job::new(predecessor.clone(), "Successful predecessor", command).unwrap());
        }
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
        standing::Store::new(fixture.profile.0.clone())
            .swap(&id, grant, |_| true)
            .unwrap();
        Self {
            fixture,
            path,
            id,
            issued,
            predecessor,
        }
    }

    pub(in crate::job_executor::tests::openssh_script::rsync_privileged) fn fire(
        &self,
        iteration: usize,
        sudo_allowed: bool,
    ) -> Completion {
        let mut runtime = SchedulerWindow::open(self.path.clone()).unwrap();
        runtime.set_connections(Some(&self.fixture.connections));
        runtime.set_execution_allowed(true);
        let scheduled_at = self.issued + Duration::from_secs(iteration as u64 * 120);
        runtime
            .initialize_schedules_at(TimeZone::UTC, scheduled_at)
            .unwrap();
        let (mut requests, trigger, finished_at) = if let Some(predecessor) = &self.predecessor {
            let mut view = runtime.new_view();
            runtime.update_view(&mut view, Message::Select(predecessor.clone()));
            let request = runtime
                .update_view(&mut view, Message::Run(false))
                .pop()
                .unwrap();
            let completion = Completion::from_result(request.id, Ok(0));
            (
                runtime.update_runtime(Message::Finished(completion)),
                RunTrigger::Dependency,
                SystemTime::now(),
            )
        } else {
            let now = scheduled_at + Duration::from_secs(90);
            (
                runtime.update_runtime_at(Message::Tick, now),
                RunTrigger::Scheduled,
                now + Duration::from_secs(10),
            )
        };
        let request = requests.pop().unwrap();
        assert_eq!(&request.job, &self.id);
        assert_eq!(request.trigger, trigger);
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.sudo_allowed = sudo_allowed;
        policy.rsync = runtime.try_rsync_run_context(&request).unwrap();
        let trust = || Ok(self.fixture.trust.clone());
        let connections = &self.fixture.connections;
        let result = if request.action.additional_connections.is_empty() {
            let saved = Credentials::resolve(
                &request.action,
                connections.get(self.fixture.reference.id).unwrap(),
                &policy.server_access,
                |_, _, _| Err("unexpected credential"),
            )
            .map(Some);
            run(&request, None, None, &policy, saved, Ok(None), trust)
        } else {
            let saved = Credentials::resolve_batch(
                &request.action,
                connections,
                &policy.server_access,
                |_, _, _| Err("unexpected credential"),
            );
            batch::run(&request, None, &policy, saved, Ok(None), trust)
        };
        runtime.update_runtime_at(Message::Finished(result.clone()), finished_at);
        result
    }
}

fn assert_success(completion: &Completion) {
    assert_eq!(completion.outcome, RunOutcome::Success, "{completion:?}");
}

fn assert_blocked(completion: &Completion) {
    assert_eq!(completion.outcome, RunOutcome::Blocked, "{completion:?}");
}

fn assert_handoff_cleared() {
    assert_empty_handoff("/test-rsync/handoff/probe");
}
