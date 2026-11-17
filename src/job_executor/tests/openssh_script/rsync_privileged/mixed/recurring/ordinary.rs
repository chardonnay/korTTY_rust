use kortty_server_archive::rsync_tree::RecurringReceiver;

use super::*;

#[test]
#[ignore = "requires an ordinary Linux account, native OpenSSH, rsync and the current korTTY helper"]
fn openssh_ordinary_recurring_rsync_creates_and_replaces_server_files() {
    let server = ordinary_server();
    let (fixture, _) = ordinary_fixture(&[&server]);
    let source = fixture.profile.0.join("source-file");
    write(&source, CHANGED[0]);
    let remote = server.profile.0.join("receiver-file");
    let destination = remote_path(&remote);
    let review = || {
        preview_receiver(
            &fixture,
            &server,
            rsync::RecurringReceiverTarget {
                destination: &destination,
                directory: false,
                delete: &DeleteScope::Retain,
            },
        )
    };
    let action = |receiver: RecurringReceiver| {
        let (bytes, digest) = rsync::upload_digest(&source).unwrap();
        let mut approval = approval(&fixture);
        approval["upload"] =
            json!({"source":source,"destination":destination,"bytes":bytes,"digest":digest});
        let settings = json!({"include_source_name":false,"delete":false});
        approval["ordinary_receiver"] = json!({
            "destination":destination,"directory":false,"settings":settings,"receiver":receiver,
        });
        serde_json::from_value(json!({
            "connection":fixture.reference,"remote":{
                "kind":"rsync_upload","program":fixture.program,"local":source,
                "remote":destination,"existing":settings,"allow_vacant_server_file_upload":true,
                "approval":approval,
            },
        }))
        .unwrap()
    };
    let receiver = review();
    assert!(matches!(receiver, RecurringReceiver::Vacant { .. }));
    let job = "ordinary-vacant-server-file";
    let recurring = Recurring::new(&fixture, job, action(receiver), false, 0);
    let fire = |round| Peer::Ordinary(&server).run(|| recurring.fire(round, false));

    write(&remote, b"arrived after review");
    let incumbent = identity(&remote);
    assert_blocked(&fire(0));
    assert_eq!(identity(&remote), incumbent);
    assert_eq!(fs::read(&remote).unwrap(), b"arrived after review");
    fs::remove_file(&remote).unwrap();

    let first = fire(1);
    assert_success(&first);
    assert!(!first.detail.unwrap().contains("original is retained at "));
    assert_eq!(fs::read(&remote).unwrap(), CHANGED[0]);
    assert_confirmed(&fixture, job, &destination, review());
    let original = identity(&remote);

    fs::write(&source, CHANGED[1]).unwrap();
    let second = fire(2);
    assert_success(&second);
    assert_eq!(
        fs::read(recovered(&second, &remote, original)).unwrap(),
        CHANGED[0]
    );
    assert_eq!(fs::read(&remote).unwrap(), CHANGED[1]);
    let existing = review();
    assert_confirmed(&fixture, job, &destination, existing);

    fs::write(&source, b"new independent approval").unwrap();
    let job = "ordinary-existing-server-file";
    let renewed = Recurring::new(&fixture, job, action(existing), true, 0);
    let previous = identity(&remote);
    let run = Peer::Ordinary(&server).run(|| renewed.fire(0, false));
    assert_success(&run);
    assert_eq!(
        fs::read(recovered(&run, &remote, previous)).unwrap(),
        CHANGED[1]
    );
    assert_eq!(fs::read(&remote).unwrap(), b"new independent approval");
    assert_confirmed(&fixture, job, &destination, review());
}

#[test]
#[ignore = "requires an ordinary Linux account, native OpenSSH, rsync and the current korTTY helper"]
fn openssh_ordinary_recurring_rsync_creates_and_replaces_server_trees() {
    let server = ordinary_server();
    let (fixture, _) = ordinary_fixture(&[&server]);
    let report = fixture.profile.0.join("report");
    write(&report, REPORT);
    for (name, named, mixed, chain) in [
        ("contents", false, false, false),
        ("named", true, false, true),
        ("mixed", false, true, true),
    ] {
        let job = format!("ordinary-vacant-server-tree-{name}");
        let source = fixture.profile.0.join(format!("source-{name}"));
        directory(&source);
        write(&source.join("removed"), CHANGED[0]);
        let parent = server.profile.0.join(&job);
        directory(&parent);
        write(&parent.join("sibling"), b"outside the reviewed destination");
        let basename = source.file_name().unwrap().to_str().unwrap();
        let remote = parent.join(if named { basename } else { "receiver" });
        let destination = remote_path(&remote);
        let base = if named {
            remote_path(&parent)
        } else {
            destination.clone()
        };
        let settings = json!({"include_source_name":named,"delete":true});
        let delete = if mixed {
            DeleteScope::Named(vec![basename.into()])
        } else {
            DeleteScope::All
        };
        let review = || {
            preview_receiver(
                &fixture,
                &server,
                rsync::RecurringReceiverTarget {
                    destination: &destination,
                    directory: true,
                    delete: &delete,
                },
            )
        };
        let mut remote_action = json!({
            "kind":"rsync_upload","directory":true,"program":fixture.program,"local":source,
            "remote":base,"existing":settings,"allow_vacant_server_tree_upload":true,
        });
        let mut approval = approval(&fixture);
        if mixed {
            let sources = [&source, &report]
                .map(|path| path.to_str().unwrap().to_owned())
                .to_vec();
            let mut target = target(
                &fixture,
                true,
                &remote,
                sources.clone(),
                ReceiverMode::ScopedMirror,
            );
            target.elevation = Elevation::Ordinary;
            target.review_vacant = true;
            let daemon = server.spawn();
            let checked = preview(&fixture, &target);
            let trace = daemon.join().unwrap();
            let checked = checked.unwrap_or_else(|error| {
                panic!(
                    "Could not preview the ordinary mixed rsync receiver ({error})\n{}",
                    String::from_utf8_lossy(&trace.stderr)
                )
            });
            assert!(target.matches(&checked));
            assert_eq!(checked.plan.delete, delete);
            assert!(matches!(
                checked.ordinary_receiver,
                Some(RecurringReceiver::Vacant { .. })
            ));
            assert!(checked.privileged.is_none());
            remote_action["mixed"] = json!(sources);
            approval["mixed"] = json!(checked);
        } else {
            let checked = review();
            assert!(matches!(checked, RecurringReceiver::Vacant { .. }));
            approval["directory_upload"] = json!({
                "source":source,"destination":base,"inventory":rsync::upload_tree_inventory(&source).unwrap(),
            });
            approval["ordinary_receiver"] = json!({
                "destination":destination,"directory":true,"settings":settings,"receiver":checked,
            });
        }
        remote_action["approval"] = approval;
        let action = serde_json::from_value(json!({
            "connection":fixture.reference,"remote":remote_action,
        }))
        .unwrap();
        let recurring = Recurring::new(&fixture, &job, action, chain, 1);
        let fire = |round| Peer::Ordinary(&server).run(|| recurring.fire(round, false));
        let first = fire(0);
        assert_success(&first);
        assert!(!first.detail.unwrap().contains("original is retained at "));
        let placed = if mixed {
            remote.join(basename)
        } else {
            remote.clone()
        };
        assert_eq!(fs::read(placed.join("removed")).unwrap(), CHANGED[0]);
        assert_confirmed(&fixture, &job, &destination, review());
        let original = identity(&remote);

        fs::remove_file(source.join("removed")).unwrap();
        write(&source.join("current"), CHANGED[1]);
        let second = fire(1);
        assert_success(&second);
        let previous = recovered(&second, &remote, original);
        let previous = if mixed {
            previous.join(basename)
        } else {
            previous.to_owned()
        };
        assert_eq!(fs::read(previous.join("removed")).unwrap(), CHANGED[0]);
        assert_eq!(fs::read(placed.join("current")).unwrap(), CHANGED[1]);
        assert!(!placed.join("removed").exists());
        assert_eq!(
            fs::read(parent.join("sibling")).unwrap(),
            b"outside the reviewed destination"
        );
        if mixed {
            assert_eq!(fs::read(remote.join("report")).unwrap(), REPORT);
        }
        assert_confirmed(&fixture, &job, &destination, review());
    }
}

fn ordinary_server() -> Sshd {
    assert!(
        !rustix::process::geteuid().is_root(),
        "Run the ordinary rsync fixture as a non-root user"
    );
    let server = Sshd::new();
    let marker = server.profile.0.join("ordinary-recurring-helper-enabled");
    write(&marker, b"enable the pinned ordinary helper");
    configure_rsync_fixture(&server, Some(&marker));
    server
}

fn remote_path(path: &Path) -> RemotePath {
    RemotePath::try_from(path.to_str().unwrap().to_owned()).unwrap()
}

fn approval(fixture: &Fixture) -> serde_json::Value {
    json!({
        "program":fixture.program,"digest":rsync::program_digest(&fixture.program).unwrap(),
        "reference":fixture.reference,"pins":sync::fingerprints(&sync::pins(&fixture.reference,&fixture.trust).unwrap()),
    })
}

fn preview_receiver(
    fixture: &Fixture,
    server: &Sshd,
    target: rsync::RecurringReceiverTarget<'_>,
) -> RecurringReceiver {
    let credentials = Credentials::resolve_directory_browser(
        &fixture.reference,
        fixture.connections.get(fixture.reference.id).unwrap(),
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    let daemon = server.spawn();
    let result = rsync::preview_recurring_receiver(
        credentials,
        &fixture.reference,
        target,
        &fixture.trust,
        Arc::new(AtomicBool::new(false)),
        || Ok(()),
    );
    let trace = daemon.join().unwrap();
    result.unwrap_or_else(|error| {
        panic!(
            "Could not review the ordinary rsync receiver ({error})\n{}",
            String::from_utf8_lossy(&trace.stderr)
        )
    })
}

fn assert_confirmed(
    fixture: &Fixture,
    job: &str,
    destination: &RemotePath,
    reviewed: RecurringReceiver,
) {
    let RecurringReceiver::Existing { snapshot } = reviewed else {
        panic!("expected the published receiver");
    };
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.profile.0.join("job-rsync-standing-reviews.json")).unwrap(),
    )
    .unwrap();
    let receiver = &ledger[job]["grant"]["routes"][0]["receiver"];
    assert_eq!(receiver["destination"], json!(destination));
    assert_eq!(receiver["snapshot"], json!(snapshot));
}
