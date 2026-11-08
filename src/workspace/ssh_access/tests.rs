use serde_json::json;

use super::*;
use crate::ssh_dialog::LoginMessage;

#[cfg(unix)]
mod native_registered_key;

const PROFILE: TerminalProfile = TerminalProfile::Vt220;
const FONT: u8 = 17;
const TEST_PASSWORD: &str = "workspace-test-password";

#[test]
fn managed_process_floor_cannot_be_lowered_in_the_ssh_form() {
    let mut workspace = Workspace::default();
    workspace.clipboard.policy.minimum_isolation = IsolationLevel::Process;
    let (window, _) = workspace.open_window(false);
    let _ = workspace.update_ssh(Message::Show(window, None));
    for message in [
        ssh_dialog::Message::Target(LoginMessage::Host("server.example".into())),
        ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        ))),
        ssh_dialog::Message::Isolation(IsolationLevel::None),
    ] {
        let _ = workspace.update_ssh(Message::Form(window, message));
    }
    let pending = workspace.pending_ssh_mut(window).unwrap();
    let Stage::Form(form) = &mut pending.stage else {
        panic!("expected the SSH login form");
    };
    assert_eq!(
        form.take_request().unwrap().isolation,
        IsolationLevel::Process
    );
}

#[test]
fn process_isolation_survives_a_disconnected_ssh_layout() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, window, "server.example");
    workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .ssh
        .as_mut()
        .unwrap()
        .isolation = IsolationLevel::Process;
    let layout = workspace.capture_layout().unwrap();
    let encoded = serde_json::to_value(&layout).unwrap();
    assert_eq!(
        encoded["windows"][0]["tabs"][0]["Ssh"]["target"]["isolation"],
        "process"
    );
    let mut restored = Workspace::default();
    let _ = restored.restore_layout(layout);
    assert_eq!(
        restored
            .sessions
            .values()
            .next()
            .unwrap()
            .isolation_name(&Status::Disconnected),
        "none"
    );
    assert_eq!(
        restored
            .sessions
            .values()
            .next()
            .unwrap()
            .ssh
            .as_ref()
            .unwrap()
            .isolation,
        IsolationLevel::Process
    );
}

fn offline_tab(workspace: &mut Workspace, window: window::Id, host: &str) -> SessionId {
    let target = SshTarget {
        login: LoginTarget {
            host: host.into(),
            port: 2222,
            username: "operator".into(),
        },
        jump: None,
        saved: None,
        isolation: IsolationLevel::None,
    };
    let session = workspace
        .store_disconnected_ssh(target, PROFILE, FONT)
        .unwrap();
    workspace.add_tab(window, session);
    session
}

fn enter_password(workspace: &mut Workspace, window: window::Id, session: SessionId) {
    let _ = workspace.update(wrap(Message::Show(window, Some(session))));
    let _ = workspace.update(wrap(Message::Form(
        window,
        ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        ))),
    )));
}

#[test]
fn ssh_layout_contains_target_metadata_and_restores_disconnected() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, window, "server.example");
    enter_password(&mut workspace, window, session);
    assert!(workspace.windows[&window].pending_ssh.is_some());

    let layout = workspace.capture_layout().unwrap();
    let expected_pane = json!({
        "Ssh": {
            "font": FONT,
            "focused": true,
            "term": PROFILE.term_name(),
            "target": {"host": "server.example", "port": 2222, "username": "operator"},
        },
    });
    let encoded = serde_json::to_string(&layout).unwrap();
    assert!(!encoded.contains(TEST_PASSWORD));

    let test_key_path = if cfg!(windows) {
        "C:\\test\\private-key-sentinel"
    } else {
        "/test/private-key-sentinel"
    };
    let _ = workspace.update(wrap(Message::Form(
        window,
        ssh_dialog::Message::Target(LoginMessage::Authentication(ssh_dialog::Method::PrivateKey)),
    )));
    let _ = workspace.update(wrap(Message::Form(
        window,
        ssh_dialog::Message::Target(LoginMessage::KeyFile(test_key_path.into())),
    )));
    assert_eq!(workspace.capture_layout().as_ref(), Some(&layout));
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["windows"][0]["tabs"][0], expected_pane);

    let mut restored = Workspace::default();
    let _ = restored.restore_layout(serde_json::from_str(&encoded).unwrap());
    let (&restored_window, state) = restored.windows.iter().next().unwrap();
    let active = state.tab().unwrap().focused_session().unwrap();
    let pane = &restored.sessions[&active];
    assert_eq!(pane.session.frame().status, Status::Disconnected);
    assert_eq!(pane.session.frame().screen.profile, PROFILE);
    assert_eq!(
        pane.ssh.as_ref().unwrap().label(),
        "operator@server.example:2222"
    );
    assert_eq!(pane.font, FONT);
    assert!(state.pending_ssh.is_none());
    assert_eq!(restored.capture_layout().as_ref(), Some(&layout));

    let _ = restored.update(wrap(Message::Show(restored_window, Some(active))));
    let task = restored.update(wrap(Message::Form(
        restored_window,
        ssh_dialog::Message::Connect,
    )));
    assert_eq!(task.units(), 0);
    assert!(matches!(
        &restored.windows[&restored_window]
            .pending_ssh
            .as_ref()
            .unwrap()
            .stage,
        Stage::Form(_)
    ));
}

#[test]
fn layout_roundtrips_jump_host_metadata_without_credentials() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, window, "internal.example.");
    workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .ssh
        .as_mut()
        .unwrap()
        .jump = Some(LoginTarget {
        host: "jump.example".into(),
        port: 22,
        username: "bastion-user".into(),
    });
    enter_password(&mut workspace, window, session);
    let _ = workspace.update(wrap(Message::Form(
        window,
        ssh_dialog::Message::Jump(LoginMessage::Password(ssh_dialog::Password::new(
            "jump-test-password".into(),
        ))),
    )));
    let layout = workspace.capture_layout().unwrap();
    let encoded = serde_json::to_string(&layout).unwrap();
    assert!(!encoded.contains("test-password"));
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        value["windows"][0]["tabs"][0]["Ssh"]["target"],
        json!({
            "host": "internal.example.", "port": 2222, "username": "operator",
            "jump": {"host": "jump.example", "port": 22, "username": "bastion-user"},
        }),
    );
    let mut restored = Workspace::default();
    let _ = restored.restore_layout(serde_json::from_str(&encoded).unwrap());
    assert_eq!(restored.capture_layout().as_ref(), Some(&layout));
}

#[test]
fn profile_changes_invalidate_an_open_vault_login() {
    use crate::connection_store::{Auth, SavedLogin};
    use crate::ssh_dialog::Authentication as Input;

    let endpoint = LoginTarget {
        host: "server.example".into(),
        port: 22,
        username: "operator".into(),
    };
    let mut profile = Connection {
        id: 1,
        name: "Example".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: endpoint.clone(),
            auth: Auth::VaultPassword("original".into()),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: PROFILE.term_name().into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: IsolationLevel::None,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
    };
    let request = ssh_dialog::Request {
        target: LoginRequest {
            endpoint,
            auth: Input::VaultPassword("original".into()),
            keyboard_interactive: false,
        },
        jump: None,
        profile: PROFILE,
        isolation: IsolationLevel::None,
        options: Default::default(),
    };
    assert!(saved_credentials_match(&request, Some(&profile), None));
    profile.login.auth = Auth::VaultPassword("replacement".into());
    assert!(!saved_credentials_match(&request, Some(&profile), None));
    assert!(!saved_credentials_match(&request, None, None));
    let selection = (request.target.endpoint.clone(), "original".into());
    assert!(saved_credentials_match(&request, None, Some(&selection)));
    let wrong_route = (
        LoginTarget {
            host: "other.example".into(),
            ..selection.0
        },
        selection.1,
    );
    assert!(!saved_credentials_match(&request, None, Some(&wrong_route)));
}

#[test]
fn registered_keys_from_saved_logins_cannot_be_reused_after_reassignment() {
    use crate::connection_store::{Auth, SavedLogin};
    use crate::ssh_dialog::Authentication as Input;

    let login = |host: &str, id| SavedLogin {
        endpoint: LoginTarget {
            host: host.into(),
            port: 22,
            username: "operator".into(),
        },
        auth: Auth::StoredKey(id),
        keyboard_interactive: false,
    };
    let mut connection = Connection {
        id: 1,
        name: "Registered route".into(),
        tag: String::new(),
        login: login("target.example", 7),
        jump: Some(login("jump.example", 8)),
        emulation: PROFILE.term_name().into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: IsolationLevel::None,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
    };
    let mut form = SshDialog::from_saved(PROFILE, &connection);
    let mut request = form.take_request().unwrap();
    assert!(saved_credentials_match(&request, Some(&connection), None));
    assert!(!saved_credentials_match(&request, None, None));

    connection.jump.as_mut().unwrap().auth = Auth::StoredKey(9);
    assert!(!saved_credentials_match(&request, Some(&connection), None));

    request.jump.as_mut().unwrap().auth = Input::StoredKey {
        id: 8,
        explicit: true,
    };
    assert!(saved_credentials_match(&request, Some(&connection), None));
    assert!(!saved_credentials_match(&request, None, None));
}

#[test]
fn incognito_ssh_form_honors_managed_privacy_policy() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let _ = workspace.update_ssh(Message::ShowIncognito(window));
    assert!(
        workspace.windows[&window]
            .pending_ssh
            .as_ref()
            .unwrap()
            .incognito
    );
    let _ = workspace.update_ssh(Message::Cancel(window));
    workspace.clipboard.policy.incognito_allowed = false;
    let _ = workspace.update_ssh(Message::ShowIncognito(window));
    assert!(workspace.windows[&window].pending_ssh.is_none());
}

#[test]
fn stale_ssh_dial_and_trust_messages_leave_replacement_untouched() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let original = offline_tab(&mut workspace, window, "original.example");
    let old_id = start_unpolled_dial(&mut workspace, window, original);
    let original_tab = workspace.windows[&window].tab().unwrap().id;
    let _ = workspace.update(WorkspaceMessage::CloseTab(window, original_tab));
    assert!(!workspace.sessions.contains_key(&original));

    let replacement = offline_tab(&mut workspace, window, "replacement.example");
    let current_id = start_unpolled_dial(&mut workspace, window, replacement);
    assert_ne!(old_id, current_id);

    // Test-double worker result. The actual dial tasks are never polled.
    let stale = DialResponse(Arc::new(Mutex::new(Some(Err(ConnectError::Io(
        io::Error::other("stale test dial result"),
    ))))));
    let _ = workspace.update(wrap(Message::Dialed(window, old_id, stale.clone())));
    assert!(stale.0.lock().unwrap().is_some());
    assert!(matches!(
        &workspace.windows[&window].pending_ssh.as_ref().unwrap().stage,
        Stage::Dialing { id, target, .. } if *id == current_id && target.login.host == "replacement.example"
    ));

    // RFC 8032 test-vector public key, encoded as an OpenSSH test fixture.
    let offered = HostKey::from_openssh(concat!(
        "ssh-ed25519 ",
        "AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea"
    ))
    .unwrap();
    let unknown = ConnectError::UnknownHostKey {
        endpoint: Endpoint::new("replacement.example", 2222).unwrap(),
        offered,
    };
    let _ = workspace.update(wrap(Message::Dialed(
        window,
        current_id,
        DialResponse(Arc::new(Mutex::new(Some(Err(unknown))))),
    )));
    assert_eq!(
        workspace
            .update(wrap(Message::Remember(window, old_id)))
            .units(),
        0
    );
    assert!(matches!(
        &workspace.windows[&window].pending_ssh.as_ref().unwrap().stage,
        Stage::Unknown { id, .. } if *id == current_id
    ));

    // The returned trust-write task is deliberately left unpolled.
    let remember = workspace.update(wrap(Message::Remember(window, current_id)));
    assert_eq!(remember.units(), 1);
    let _ = workspace.update(wrap(Message::Remembered(window, old_id, Ok(()))));

    let state = &workspace.windows[&window];
    assert!(state.notice.is_none());
    assert_eq!(state.tab().unwrap().focused_session(), Some(replacement));
    assert_eq!(
        workspace.sessions[&replacement].session.frame().status,
        Status::Disconnected
    );
    let pending = state.pending_ssh.as_ref().unwrap();
    assert_eq!(pending.replace, Some(replacement));
    assert!(matches!(
        &pending.stage,
        Stage::Remembering { id, target, .. } if *id == current_id && target.login.host == "replacement.example"
    ));
}

fn start_unpolled_dial(workspace: &mut Workspace, window: window::Id, session: SessionId) -> u64 {
    enter_password(workspace, window, session);
    let dial = workspace.update(wrap(Message::Form(window, ssh_dialog::Message::Connect)));
    assert_eq!(dial.units(), 1);
    match &workspace.windows[&window]
        .pending_ssh
        .as_ref()
        .unwrap()
        .stage
    {
        Stage::Dialing { id, .. } => *id,
        _ => panic!("expected an SSH dial request"),
    }
}

#[test]
fn keyboard_interactive_desktop_routes_answers_and_rejects_stale_rounds() {
    use std::time::Duration;

    use kortty_ssh::{KeyboardChallenge, KeyboardPrompt};

    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, window, "operator.example");
    let dial = start_unpolled_dial(&mut workspace, window, session);
    let Stage::Dialing { _cancellation, .. } = &workspace.pending_ssh_mut(window).unwrap().stage
    else {
        panic!("expected active dial");
    };
    let (events, messages) = async_channel::bounded(4);
    let forwarder = challenge::Forwarder {
        events,
        window,
        dial,
        cancellation: _cancellation.0.clone(),
        rounds: Arc::default(),
        secrets: None,
    };
    let worker = std::thread::spawn(move || {
        // Test-double server challenges. Only the verified route can create
        // these events in production.
        let request = KeyboardChallenge {
            endpoint: Endpoint::new("operator.example", 2222).unwrap(),
            username: "operator".into(),
            name: "Test challenge".into(),
            instruction: String::new(),
            prompts: vec![KeyboardPrompt {
                text: "Code".into(),
                echo: false,
            }],
            response_timeout: Duration::from_secs(3),
        };
        let answer = forwarder.request(&request).unwrap();
        assert_eq!(answer[0].as_str(), "first answer");
        assert_eq!(
            forwarder.request(&request).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    });
    let _ = workspace.update_ssh(messages.recv_blocking().unwrap());
    let message = |round, value: &str| {
        Message::PromptAnswer(
            window,
            dial,
            round,
            0,
            ssh_dialog::Password::new(value.into()),
        )
    };
    let _ = workspace.update_ssh(message(0, &"x".repeat(4097)));
    let _ = workspace.update_ssh(Message::PromptSubmit(window, dial, 0));
    assert!(
        workspace
            .ssh_challenge_slot(window, dial)
            .unwrap()
            .is_some()
    );
    let _ = workspace.update_ssh(message(0, "first answer"));
    let _ = workspace.update_ssh(Message::PromptSubmit(window, dial, 0));
    let _ = workspace.update_ssh(messages.recv_blocking().unwrap());
    let _ = workspace.update_ssh(message(0, "stale answer"));
    let _ = workspace.update_ssh(Message::PromptSubmit(window, dial, 0));
    assert!(
        workspace
            .ssh_challenge_slot(window, dial)
            .unwrap()
            .is_some()
    );
    let _ = workspace.update_ssh(Message::Cancel(window));
    assert!(workspace.pending_ssh_mut(window).is_none());
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "requires OpenSSH ssh-keygen and a test Python with Paramiko 4.0.0"]
fn keyboard_interactive_desktop_direct_round_trip() {
    use std::io::Read as _;
    use std::path::Path;
    use std::process::Command;

    use crate::ssh_test_fixture::TempDirectory;
    use crate::ssh_test_peer as keyboard_peer;

    let directory = TempDirectory::new("ssh-desktop-auth");
    let key = directory.0.join("host");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(generated.status.success(), "{generated:?}");
    let key_text = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/ssh/tests/support/keyboard_peer.py");
    let mut peer = keyboard_peer::start(&script, "direct", &key, "");
    let pin = HostKey::from_openssh(key_text.trim()).unwrap();
    let trust = TrustStore::from_pins(vec![(peer.endpoint(), pin)]).unwrap();

    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, window, "127.0.0.1");
    let id = start_unpolled_dial(&mut workspace, window, session);
    let Stage::Dialing { _cancellation, .. } = &workspace.pending_ssh_mut(window).unwrap().stage
    else {
        panic!("expected active dial");
    };
    let (events, messages) = async_channel::bounded(4);
    let prompt = challenge::Forwarder {
        events,
        window,
        dial: id,
        cancellation: _cancellation.0.clone(),
        rounds: Arc::default(),
        secrets: None,
    };
    let request = ssh_dialog::Request {
        target: LoginRequest {
            endpoint: LoginTarget {
                host: "127.0.0.1".into(),
                port: peer.address.port(),
                username: keyboard_peer::USER.into(),
            },
            auth: ssh_dialog::Authentication::KeyboardInteractive,
            keyboard_interactive: false,
        },
        jump: None,
        profile: PROFILE,
        isolation: IsolationLevel::None,
        options: Default::default(),
    };
    let worker = thread::spawn(move || {
        let size = ssh_size(initial_grid(request.profile));
        let mut connection = dial_direct(request, false, &prompt, trust, size).unwrap();
        let mut text = String::new();
        connection
            .take_reader()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "keyboard-interactive-ok\n");
        connection.close().unwrap();
    });
    for (round, answers) in [&[][..], &["ä123", ""], &["yes"]].into_iter().enumerate() {
        let _ = workspace.update_ssh(messages.recv_blocking().unwrap());
        let round = round as u64;
        for (index, value) in answers.iter().enumerate() {
            let _ = workspace.update_ssh(Message::PromptAnswer(
                window,
                id,
                round,
                index,
                ssh_dialog::Password::new((*value).into()),
            ));
        }
        let _ = workspace.update_ssh(Message::PromptSubmit(window, id, round));
    }
    worker.join().unwrap();
    assert_eq!(peer.responses(), 3);
}

#[test]
fn confirmed_quit_cancels_pending_shell_and_sftp_authentication() {
    let mut workspace = Workspace::default();
    let (shell, _) = workspace.open_window(false);
    let (sftp, _) = workspace.open_window(false);
    let mut cancellations = Vec::new();
    for window in [shell, sftp] {
        let session = offline_tab(&mut workspace, window, "quit.example");
        let _ = start_unpolled_dial(&mut workspace, window, session);
        let Stage::Dialing { _cancellation, .. } =
            &workspace.pending_ssh_mut(window).unwrap().stage
        else {
            panic!("expected a pending dial");
        };
        cancellations.push(_cancellation.0.clone());
    }
    // The SFTP window uses its own pending-login collection.
    let mut pending = workspace
        .windows
        .get_mut(&sftp)
        .unwrap()
        .pending_ssh
        .take()
        .unwrap();
    pending.sftp = true;
    workspace.sftp_logins.insert(sftp, pending);
    assert!(cancellations.iter().all(|dial| !dial.is_cancelled()));

    let _ = workspace.quit_confirmed(false);
    assert!(workspace.windows[&shell].pending_ssh.is_none());
    assert!(workspace.sftp_logins.is_empty());
    assert!(cancellations.iter().all(DialCancellation::is_cancelled));
}

#[test]
fn host_key_review_retains_an_ad_hoc_key_path_without_retaining_its_passphrase() {
    let endpoint = LoginTarget {
        host: "test.example".into(),
        port: 22,
        username: "guest".into(),
    };
    let key = std::env::temp_dir().join("kortty-review-key");
    let request = ssh_dialog::Request {
        target: LoginRequest {
            endpoint: endpoint.clone(),
            auth: ssh_dialog::Authentication::KeyWithPassphrase(
                key.clone(),
                ssh_dialog::Password::new("one-use passphrase".into()),
            ),
            keyboard_interactive: false,
        },
        jump: None,
        profile: PROFILE,
        isolation: IsolationLevel::None,
        options: Default::default(),
    };
    let retry = RetryLogin::new(&request, None, Default::default());
    let mut form = SshDialog::with_target(PROFILE, &endpoint, None);
    retry.restore(&mut form, &endpoint);
    let login = form.take_request().unwrap();
    assert!(matches!(
        login.target.auth,
        ssh_dialog::Authentication::PrivateKey(restored) if restored == key
    ));
}

#[test]
fn changed_host_key_replacement_requires_the_current_verified_review_and_policy() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let _ = workspace.update_ssh(Message::Show(window, None));
    let login = LoginTarget {
        host: "changed.example".into(),
        port: 2222,
        username: "guest".into(),
    };
    let endpoint = Endpoint::new(&login.host, login.port).unwrap();
    let expected = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap();
    let offered = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo",
    )
    .unwrap();
    workspace.pending_ssh_mut(window).unwrap().stage = Stage::Changed {
        id: 3,
        target: SshTarget {
            login,
            jump: None,
            saved: None,
            isolation: IsolationLevel::None,
        },
        profile: PROFILE,
        review: ChangedKey {
            endpoint,
            expected,
            offered,
            reviewing: false,
            verified: false,
        },
    };
    let _ = workspace.update_ssh(Message::VerifyChanged(window, 3, true));
    let _ = workspace.update_ssh(Message::ReplaceChanged(window, 3));
    assert!(!workspace.changed_key(window, 3).unwrap().verified);
    let _ = workspace.update_ssh(Message::ReviewChanged(window, 3, true));
    let _ = workspace.update_ssh(Message::VerifyChanged(window, 2, true));
    assert!(!workspace.changed_key(window, 3).unwrap().verified);
    let _ = workspace.update_ssh(Message::VerifyChanged(window, 3, true));
    workspace.clipboard.policy.host_pins_locked = true;
    let _ = workspace.update_ssh(Message::ReplaceChanged(window, 3));
    assert!(workspace.changed_key(window, 3).is_some());
    workspace.clipboard.policy.host_pins_locked = false;
    let _ = workspace.update_ssh(Message::ReplaceChanged(window, 3));
    assert!(matches!(
        workspace.pending_ssh_mut(window).unwrap().stage,
        Stage::Remembering {
            replacing: true,
            ..
        }
    ));
}

#[test]
fn quick_split_ssh_keeps_the_source_through_host_key_review_and_drops_stale_approval() {
    use super::super::quick_connect;
    use iced::widget::pane_grid::Axis;

    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let source = offline_tab(&mut workspace, window, "original.example");
    let _ = workspace.show_split_connection(window, source, Axis::Vertical);
    let dialog = workspace.quick_connect_for_owner(window).unwrap();
    for message in [
        quick_connect::Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Host(
            "split-target.example".into(),
        ))),
        quick_connect::Message::Ssh(ssh_dialog::Message::Target(LoginMessage::Password(
            ssh_dialog::Password::new(TEST_PASSWORD.into()),
        ))),
        quick_connect::Message::Connect,
    ] {
        let _ = workspace.update_quick_connect(dialog, message);
    }
    let pending = workspace.pending_ssh_mut(window).unwrap();
    assert!(pending.replace.is_none());
    assert!(pending.depends_on(source));
    let Stage::Dialing {
        id, _cancellation, ..
    } = &pending.stage
    else {
        panic!("expected the normal SSH worker path");
    };
    let id = *id;
    let cancellation = _cancellation.0.clone();

    // Test-double worker result. The dial task is deliberately unpolled.
    // The offered public key is the RFC 8032 test vector in OpenSSH form.
    let offered = HostKey::from_openssh(concat!(
        "ssh-ed25519 ",
        "AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea",
    ))
    .unwrap();
    let unknown = ConnectError::UnknownHostKey {
        endpoint: Endpoint::new("split-target.example", 22).unwrap(),
        offered,
    };
    let _ = workspace.update_ssh(Message::Dialed(
        window,
        id,
        DialResponse(Arc::new(Mutex::new(Some(Err(unknown))))),
    ));
    assert!(cancellation.is_cancelled());
    let pending = workspace.pending_ssh_mut(window).unwrap();
    assert!(pending.depends_on(source));
    assert!(matches!(&pending.stage, Stage::Unknown { id: active, .. } if *active == id));

    // Switch tabs through the model while the old approval message is queued.
    let other = offline_tab(&mut workspace, window, "other.example");
    let rejected = workspace.update_ssh(Message::Remember(window, id));
    assert_eq!(rejected.units(), 0);
    assert!(workspace.pending_ssh_mut(window).is_none());
    assert_eq!(
        workspace.windows[&window].tab().unwrap().focused_session(),
        Some(other)
    );
    assert_eq!(workspace.windows[&window].tabs.len(), 2);
    assert_eq!(workspace.sessions.len(), 2);
}

#[cfg(unix)]
impl Workspace {
    pub(in crate::workspace) fn receive_ssh_fixture_connection(
        &mut self,
        window: window::Id,
        connection: Connected,
    ) -> Task<WorkspaceMessage> {
        self.receive_ssh_fixture_result(window, Ok(connection))
    }

    pub(in crate::workspace) fn receive_ssh_fixture_result(
        &mut self,
        window: window::Id,
        result: Result<Connected, ConnectError>,
    ) -> Task<WorkspaceMessage> {
        let Stage::Dialing { id, .. } = self.pending_ssh_mut(window).unwrap().stage else {
            panic!("the desktop must request a dial before accepting the fixture result");
        };
        self.update(wrap(Message::Dialed(
            window,
            id,
            DialResponse(Arc::new(Mutex::new(Some(result)))),
        )))
    }

    pub(in crate::workspace) fn launch_saved_reopen_fixture(
        &mut self,
        window: window::Id,
        store: &crate::connection_store::Store,
        id: u64,
    ) -> Task<WorkspaceMessage> {
        let token = self.reopen_ssh_token(window).unwrap();
        self.discard_reopen_ssh(window, token);
        let saved = reopen::SavedSshProfile::load(store.path().to_path_buf(), id)
            .unwrap()
            .unwrap();
        self.launch_saved_reopen_ssh(window, token, saved, None, true)
    }
}

fn reopen_unsaved_ssh_fixture(workspace: &mut Workspace, owner: window::Id) -> (ReopenToken, u64) {
    let source = offline_tab(workspace, owner, "closed-origin.example");
    let tab = workspace.windows[&owner].tab().unwrap().id;
    let _ = workspace.update(WorkspaceMessage::CloseTab(owner, tab));
    assert!(!workspace.sessions.contains_key(&source));
    assert!(workspace.has_recently_closed());
    let entry = workspace.recently_closed_menu_rows()[0].0;

    let _ = workspace.reopen_closed_tab(Some(owner), Some(entry));
    let token = workspace
        .reopen_ssh_token(owner)
        .expect("active reopen login");
    (token, entry)
}

#[test]
fn reconnect_locks_the_ad_hoc_ssh_route_and_rejects_stale_credentials_after_transfer() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let session = offline_tab(&mut workspace, owner, "original-route.example");
    let tab = workspace.windows[&owner].tab().unwrap().id;
    let _ = workspace.reconnect_tab(owner, tab);
    let key = workspace.guarded_ssh_form_key(owner);
    assert!(matches!(key, Some(FormKey::Reconnect(_))));
    let password = || {
        ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        )))
    };
    let _ = workspace.update_ssh(Message::Form(owner, password()));
    let _ = workspace.update_ssh(form_message(
        owner,
        key,
        ssh_dialog::Message::Target(LoginMessage::Host("different-route.example".into())),
    ));
    let Stage::Form(form) = &workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected a reconnect form");
    };
    assert!(!form.can_connect());
    assert_eq!(
        form.target_endpoint().unwrap().host,
        "original-route.example"
    );

    // Force a model move while the reconnect form still owns this window.
    let _ = workspace.transfer(owner, Some(destination), tab);
    let _ = workspace.update_ssh(form_message(owner, key, password()));
    let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Connect));
    assert!(workspace.pending_ssh_mut(owner).is_none());
    assert_eq!(
        workspace.windows[&destination]
            .tab()
            .unwrap()
            .focused_session(),
        Some(session)
    );
}

#[test]
fn reopen_closed_ssh_rechecks_host_key_and_ignores_stale_form_messages() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (first, entry) = reopen_unsaved_ssh_fixture(&mut workspace, owner);
    let key = workspace.guarded_ssh_form_key(owner);
    let submit = || form_message(owner, key, ssh_dialog::Message::Connect);

    // A queued edit from an ordinary SSH form must not become this tab's credential.
    let _ = workspace.update_ssh(Message::Form(
        owner,
        ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        ))),
    ));
    let _ = workspace.update_ssh(submit());
    assert!(matches!(
        &workspace.pending_ssh_mut(owner).unwrap().stage,
        Stage::Form(form) if !form.can_connect()
    ));
    for input in [
        LoginMessage::Host("ignored-route.example".into()),
        LoginMessage::Password(ssh_dialog::Password::new(TEST_PASSWORD.into())),
    ] {
        let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Target(input)));
    }
    let task = workspace.update_ssh(submit());
    assert_eq!(task.units(), 1);
    let Stage::Dialing {
        id,
        target,
        profile,
        ..
    } = &workspace.pending_ssh_mut(owner).unwrap().stage
    else {
        panic!("expected the standard SSH dial stage");
    };
    assert_eq!(target.login.host, "closed-origin.example");
    assert_eq!(*profile, PROFILE);
    let dial = *id;

    // Test-double worker result only. The dial task remains unpolled.
    let expected = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap();
    let offered = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo",
    )
    .unwrap();
    let _ = workspace.update_ssh(Message::Dialed(
        owner,
        dial,
        DialResponse(Arc::new(Mutex::new(Some(Err(
            ConnectError::ChangedHostKey {
                endpoint: Endpoint::new("closed-origin.example", 2222).unwrap(),
                expected,
                offered,
            },
        ))))),
    ));
    assert!(matches!(
        &workspace.pending_ssh_mut(owner).unwrap().stage,
        Stage::Changed { id, .. } if *id == dial
    ));
    assert!(workspace.windows[&owner].tabs.is_empty());
    assert!(workspace.has_recently_closed_entries());

    let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Cancel));
    assert!(workspace.reopen_ssh_token(owner).is_none());
    let _ = workspace.update_recently_closed(super::super::recently_closed::Message::Advance(
        owner, entry,
    ));
    assert!(workspace.has_recently_closed());

    let _ = workspace.reopen_closed_tab(Some(owner), Some(entry));
    let second = workspace.reopen_ssh_token(owner).unwrap();
    assert_ne!(first, second);
    let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Cancel));
    assert_eq!(workspace.reopen_ssh_token(owner), Some(second));
    let _ = workspace.clear_recently_closed();
    assert!(workspace.reopen_ssh_token(owner).is_none());
    assert!(!workspace.has_recently_closed_entries());
}

#[test]
fn reopen_closed_ssh_refreshes_edited_saved_route_without_reusing_typed_password() {
    use crate::connection_store::{Auth, SavedLogin, Store};
    use crate::snippets::tests::Profile;

    let directory = Profile::new();
    let mut store = Store::open(directory.0.join("connections.json")).unwrap();
    let mut profile = Connection {
        id: 0,
        name: "Saved reopen profile".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "saved-before-edit.example".into(),
                port: 22,
                username: "old-user".into(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
        },
        jump: None,
        emulation: PROFILE.term_name().into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: IsolationLevel::None,
        startup: ConnectOptions::QUICK,
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
    };
    profile.id = store.save(profile.clone()).unwrap();

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (token, entry) = reopen_unsaved_ssh_fixture(&mut workspace, owner);
    workspace.discard_reopen_ssh(owner, token);
    let saved = reopen::SavedSshProfile::load(store.path().to_path_buf(), profile.id)
        .unwrap()
        .unwrap();
    let _ = workspace.launch_saved_reopen_ssh(owner, token, saved, None, true);
    let old_form = workspace.guarded_ssh_form_key(owner);
    let password = || {
        ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        )))
    };
    let _ = workspace.update_ssh(form_message(owner, old_form, password()));
    assert!(workspace.refresh_reopen_ssh_config(owner).is_none());

    profile.login.endpoint.host = "saved-after-edit.example".into();
    profile.login.endpoint.port = 2201;
    profile.login.endpoint.username = "new-user".into();
    profile.emulation = TerminalProfile::Xterm.term_name().into();
    profile.isolation = IsolationLevel::Process;
    assert_eq!(store.save(profile.clone()).unwrap(), profile.id);
    let _ = workspace.update_ssh(form_message(owner, old_form, ssh_dialog::Message::Connect));
    assert!(workspace.guarded_ssh_form_key(owner) != old_form);
    let _ = workspace.update_ssh(form_message(owner, old_form, password()));
    let pending = workspace.pending_ssh_mut(owner).unwrap();
    assert_eq!(pending.reopen_token(), Some(token));
    assert_eq!(pending.saved, Some(profile.id));
    let Stage::Form(form) = &mut pending.stage else {
        panic!("an edited saved profile must require fresh confirmation");
    };
    assert!(!form.can_connect());
    assert_eq!(form.target_endpoint(), Some(profile.login.endpoint));
    assert_eq!(form.profile(), TerminalProfile::Xterm);
    form.update(password());
    assert_eq!(
        form.take_request().unwrap().isolation,
        IsolationLevel::Process
    );

    store.delete(profile.id).unwrap();
    let _ = workspace.refresh_reopen_ssh_config(owner).unwrap();
    assert!(workspace.reopen_ssh_token(owner).is_none());
    let _ = workspace.update_recently_closed(super::super::recently_closed::Message::Advance(
        owner, entry,
    ));
    assert!(!workspace.has_recently_closed_entries());
    assert!(workspace.windows[&owner].tabs.is_empty());
}
