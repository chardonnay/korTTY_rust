use super::*;

#[test]
#[ignore = "requires native OpenSSH, rsync, and a local account"]
fn openssh_scheduled_rsync_routes_publish_at_distinct_reviewed_destinations() {
    use crate::scheduler_window::sync;

    let first = Sshd::new();
    let second = Sshd::new();
    let root = &first.profile.0;
    let (_, mut request, connections) = scheduled_batch(root, &[&first, &second], "true", vec![]);
    let primary = request.action.connection.as_ref().unwrap();
    let additional = &request.action.additional_connections[0];
    let program = PathBuf::from(
        std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").expect("set KORTTY_RSYNC_TEST_PROGRAM"),
    )
    .canonicalize()
    .unwrap();
    let source = root.join("rsync-source");
    let first_destination = root.join("first-download");
    let second_destination = root.join("second-download");
    let content = b"rsync through independently pinned saved SSH connections";
    fs::write(&source, content).unwrap();
    let digest = crate::job_executor::rsync::program_digest(&program).unwrap();
    let approval = |reference: &ConnectionRef, destination: &Path, trust: &TrustStore| {
        let pins = sync::fingerprints(&sync::pins(reference, trust).unwrap());
        json!({
            "program": program, "digest": digest, "reference": reference, "pins": pins,
            "download": { "source": source, "destination": destination },
        })
    };
    let action: SshCommand = serde_json::from_value(json!({
        "connection": primary, "additional_connections": [additional],
        "remote": {
            "kind": "rsync_download", "program": program, "local": first_destination, "remote": source,
            "approval": approval(primary, &first_destination, &first.trust),
            "routes": [{
                "profile": additional.id, "destination": second_destination,
                "approval": approval(additional, &second_destination, &second.trust),
            }],
        },
    }))
    .unwrap();
    request.action = Arc::new(action.validated().unwrap());
    let servers = [&first, &second];
    let all_pins = servers.map(Sshd::pin).into();
    let combined_trust = TrustStore::from_pins(all_pins).unwrap();
    let execute = |trust| {
        let policy = ExecutionPolicy::unrestricted(true);
        batch::run(
            &request,
            None,
            &policy,
            Credentials::resolve_batch(
                &request.action,
                &connections,
                &policy.server_access,
                |_, _, _| Err("unexpected credential"),
            ),
            Ok(None),
            || Ok(trust),
        )
    };
    let server = first.spawn();
    let blocked = execute(first.trust.clone());
    server.join().unwrap();
    assert_eq!(blocked.outcome, RunOutcome::Blocked);
    assert!(blocked.detail.unwrap().contains("1 of 2 targets succeeded"));
    assert_eq!(fs::read(&first_destination).unwrap(), content);
    assert!(!second_destination.exists());
    fs::remove_file(&first_destination).unwrap();

    let running = servers.map(Sshd::spawn);
    let completed = execute(combined_trust);
    for server in running {
        server.join().unwrap();
    }
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    assert_eq!(fs::read(first_destination).unwrap(), content);
    assert_eq!(fs::read(second_destination).unwrap(), content);
}
