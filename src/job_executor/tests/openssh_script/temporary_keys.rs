use kortty_vault::TemporarySshKeyOptions;

use crate::job_executor::TemporaryKeys;
use crate::scheduler_window::sync;
use crate::vault_window::{VaultWindow, test_vault_password};

use super::*;

#[test]
#[ignore = "requires disposable OpenSSH listeners, with optional native rsync"]
fn openssh_scheduler_temporary_keys_authenticate_the_exact_target_and_jump() {
    let target = Sshd::new();
    let jump = Sshd::new();
    let root = &target.profile.0;
    let vault_path = root.join("vault.bin");
    let mut vault = Vault::create(&vault_path, &test_vault_password()[..]).unwrap();
    let mut saved_login = |server: &Sshd| {
        let mut login = server.saved_login(None);
        let endpoint = &login.endpoint;
        let private_key = Secret::new(fs::read_to_string(&server.user_key).unwrap());
        fs::remove_file(&server.user_key).unwrap();
        let options = TemporarySshKeyOptions {
            minutes: 10,
            automatic: true,
        };
        let id = vault
            .save_temporary_ssh_key(
                "Disposable scheduler key".into(),
                (&endpoint.host, endpoint.port, &endpoint.username),
                options,
                private_key,
            )
            .unwrap();
        login.auth = Auth::TemporaryKey {
            credential: Some(id),
            minutes: options.minutes,
            automatic: options.automatic,
        };
        login
    };
    let mut store = Store::open(crate::profile::connections_path_at(root)).unwrap();
    let id = store
        .save(Connection {
            id: 0,
            name: "Temporary keys through a jump".into(),
            tag: String::new(),
            login: saved_login(&target),
            jump: Some(saved_login(&jump)),
            emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            inherit_isolation: false,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        })
        .unwrap();
    let profile = store.get(id).unwrap();
    let reference = ConnectionRef::from(profile);
    let settings = crate::profile::window_settings_path_at(root);
    fs::write(&settings, br#"{"temporary_ssh_key_enabled":true}"#).unwrap();
    let mut vault = VaultWindow::with_unlocked_fixture(vault_path, vault);
    let policy = ExecutionPolicy::unrestricted(true);
    let resolve = |reference: &ConnectionRef, current: &Connection, vault, enabled| {
        Credentials::resolve_scheduled(
            reference,
            current,
            false,
            &policy.server_access,
            |_, _, _| panic!("temporary keys require the dedicated vault lookup"),
            TemporaryKeys {
                enabled,
                vault,
                connections: store.path(),
            },
        )
    };
    assert!(matches!(
        resolve(&reference, profile, None, true),
        Err(crate::vault_window::VAULT_LOCKED)
    ));
    assert!(
        matches!(resolve(&reference, profile, Some(&vault), false), Err(error) if error.contains("disabled"))
    );
    let mut legacy = reference.clone();
    legacy.temporary_keys = None;
    assert!(
        matches!(resolve(&legacy, profile, Some(&vault), true), Err(error) if error.contains("route changed"))
    );
    let mut changed = profile.clone();
    let bastion = changed.jump.as_mut().unwrap();
    bastion.endpoint.username.push_str("-changed");
    assert!(
        matches!(resolve(&reference, &changed, Some(&vault), true), Err(error) if error.contains("route changed"))
    );
    assert!(matches!(
        resolve(&ConnectionRef::from(&changed), &changed, Some(&vault), true),
        Err(error) if error.contains("belongs to another SSH target")
    ));
    let mut swapped = profile.clone();
    std::mem::swap(
        &mut swapped.login.auth,
        &mut swapped.jump.as_mut().unwrap().auth,
    );
    assert!(matches!(
        resolve(&ConnectionRef::from(&swapped), &swapped, Some(&vault), true),
        Err(error) if error.contains("belongs to another SSH target")
    ));

    let pins = TrustStore::from_pins(vec![target.pin(), jump.pin()]).unwrap();
    let (_, mut request) = scheduled_request();
    request.action = Arc::new(
        serde_json::from_value(json!({
            "connection": reference, "command": "exit 23",
        }))
        .unwrap(),
    );
    let execute = |request: &RunRequest<SshCommand>,
                   credentials: Result<Credentials, &'static str>,
                   trust: TrustStore| {
        run(
            request,
            None,
            None,
            &policy,
            credentials.map(Some),
            Ok(None),
            || Ok(trust),
        )
    };
    let traces = [jump.spawn(), target.spawn()];
    let completed = execute(
        &request,
        resolve(&reference, profile, Some(&vault), true),
        pins.clone(),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Failed {
            exit_code: Some(23)
        },
        "{:?}",
        completed.detail
    );
    assert_logins(traces, [&jump, &target]);

    if let Some(program) = std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM") {
        let program = PathBuf::from(program).canonicalize().unwrap();
        assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
        let content = b"scheduled rsync through separately authorized temporary SSH keys\n";
        let source = root.join("fixture-source");
        let destination = root.join("fixture-download");
        fs::write(&source, content).unwrap();
        let digest = crate::job_executor::rsync::program_digest(&program).unwrap();
        let fingerprints = sync::fingerprints(&sync::pins(&reference, &pins).unwrap());
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": "rsync_download", "program": program, "local": destination, "remote": source,
                "approval": {
                    "program": program, "digest": digest, "reference": reference, "pins": fingerprints,
                    "download": { "source": source, "destination": destination },
                },
            },
        }))
        .unwrap();
        request.action = Arc::new(action.validated().unwrap());
        let traces = [jump.spawn(), target.spawn()];
        let completed = execute(
            &request,
            resolve(&reference, profile, Some(&vault), true),
            pins.clone(),
        );
        assert_eq!(
            completed.outcome,
            RunOutcome::Success,
            "{:?}",
            completed.detail
        );
        assert_logins(traces, [&jump, &target]);
        assert_eq!(fs::read(destination).unwrap(), content);
    }

    let queued = resolve(&reference, profile, Some(&vault), true).unwrap();
    let mut replacement = profile.clone();
    let Auth::TemporaryKey { credential, .. } = &mut replacement.jump.as_mut().unwrap().auth else {
        unreachable!();
    };
    *credential = Some("0123456789abcdef0123456789abcdef".into());
    let mut editor = Store::open(store.path().to_owned()).unwrap();
    editor.save(replacement).unwrap();
    let changed = execute(&request, Ok(queued), pins.clone());
    assert_eq!(changed.outcome, RunOutcome::Blocked);
    assert!(changed.detail.unwrap().contains("SSH route changed"));
    editor.save(profile.clone()).unwrap();

    let queued = resolve(&reference, profile, Some(&vault), true).unwrap();
    let (target_route, jumps) = queued
        .into_route()
        .map_err(|failure| failure.detail)
        .unwrap();
    let daemon = jump.spawn();
    let stream = TcpStream::connect(("127.0.0.1", jump.port)).unwrap();
    fs::write(&settings, br#"{"temporary_ssh_key_enabled":false}"#).unwrap();
    let handshake = kortty_ssh::SshSession::connect_exec_preconnected_cancellable(
        stream,
        target_route,
        jumps,
        pins.clone(),
        "true",
        None,
        || false,
    );
    assert!(matches!(handshake, Err(kortty_ssh::ConnectError::Io(error))
        if error.to_string().contains("settings or managed policy")));
    let log = daemon.join().unwrap();
    assert!(!String::from_utf8_lossy(&log.stderr).contains("Accepted publickey"));
    fs::write(&settings, br#"{"temporary_ssh_key_enabled":true}"#).unwrap();

    let mut direct = profile.clone();
    direct.jump = None;
    editor.save(direct.clone()).unwrap();
    let direct_reference = ConnectionRef::from(&direct);
    let (_, mut direct_request) = scheduled_request();
    direct_request.action = Arc::new(
        serde_json::from_value(json!({
            "connection": direct_reference, "command": "true",
        }))
        .unwrap(),
    );
    let trace = target.spawn();
    let completed = execute(
        &direct_request,
        resolve(&direct_reference, &direct, Some(&vault), true),
        pins.clone(),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    assert_logins([trace], [&target]);
    editor.save(profile.clone()).unwrap();

    let queued = resolve(&reference, profile, Some(&vault), true).unwrap();
    let _ = vault.update(crate::vault_window::Message::Lock);
    let blocked = execute(&request, Ok(queued), pins.clone());
    assert_eq!(blocked.outcome, RunOutcome::Blocked);
    assert!(
        blocked
            .detail
            .unwrap()
            .contains("vault changed or was locked")
    );
    assert_eq!(
        jump.listener.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

fn assert_logins<const N: usize>(traces: [JoinHandle<Output>; N], servers: [&Sshd; N]) {
    for (trace, server) in traces.into_iter().zip(servers) {
        let trace = String::from_utf8_lossy(&trace.join().unwrap().stderr).into_owned();
        let public_key = fs::read_to_string(server.user_key.with_extension("pub")).unwrap();
        let fingerprint = HostKey::from_openssh(public_key.trim())
            .unwrap()
            .fingerprint();
        assert!(
            trace
                .lines()
                .any(|line| line.contains("Accepted publickey") && line.contains(&fingerprint)),
            "{trace}"
        );
    }
}
