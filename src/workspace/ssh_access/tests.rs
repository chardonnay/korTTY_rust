use serde_json::json;

use super::*;
use crate::ssh_dialog::LoginMessage;
#[cfg(unix)]
use crate::workspace::initial_grid;

#[cfg(unix)]
mod native_registered_key;

const PROFILE: TerminalProfile = TerminalProfile::Vt220;
const FONT: u8 = 17;
const TEST_PASSWORD: &str = "workspace-test-password";

#[test]
fn mcp_masking_uses_ssh_secrets_when_journaling_and_logging_are_off() {
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(
        serde_json::from_value(json!({"general": {"features_enabled": false}})).unwrap(),
    );
    workspace.clipboard.policy.session_journal_allowed = false;
    assert!(workspace.ssh_needs_terminal_secrets(false, false, false));
    assert!(!workspace.ssh_needs_terminal_secrets(true, false, false));
    assert!(!workspace.ssh_needs_terminal_secrets(false, true, false));
    workspace.clipboard.policy.mcp_server_allowed = false;
    assert!(!workspace.ssh_needs_terminal_secrets(false, false, false));
}

#[test]
fn terminal_ai_masking_is_independent_of_journal_mcp_and_private_persistence() {
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(crate::ai_manager::Configuration::default());
    workspace.clipboard.policy.session_journal_allowed = false;
    workspace.clipboard.policy.mcp_server_allowed = false;
    for private in [false, true] {
        assert!(workspace.ssh_needs_terminal_secrets(private, false, false));
        assert!(!workspace.ssh_needs_terminal_secrets(private, true, false));
    }
    let target = SshTarget::from_imported_route(
        LoginTarget {
            host: "fixture.invalid".into(),
            port: 22,
            username: "fixture".into(),
        },
        None,
        IsolationLevel::None,
    )
    .unwrap();
    let logging: crate::terminal_logging::Config =
        serde_json::from_value(json!({"enabled": true})).unwrap();
    let public = workspace.ssh_terminal_capture(false, logging.clone(), "Fixture".into(), &target);
    assert!(public.enabled());
    let private = workspace.ssh_terminal_capture(true, logging, "Fixture".into(), &target);
    assert!(!private.enabled());
    private.secrets().add("fixture-private-password");
    let redactor = crate::terminal_logging::Redactor::for_journal(
        private.secrets().seal().unwrap(),
        Default::default(),
    );
    assert_eq!(
        redactor
            .journal_text(
                "fixture-private-password",
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .unwrap()
            .as_str(),
        "***",
    );

    workspace.clipboard.policy.ai_chat_allowed = false;
    workspace.clipboard.policy.ai_agent_feature_allowed = false;
    workspace.clipboard.policy.ai_swarm_feature_allowed = false;
    workspace.clipboard.policy.ai_planning_feature_allowed = false;
    assert!(!workspace.ssh_needs_terminal_secrets(false, false, false));
    assert!(!workspace.ssh_needs_terminal_secrets(true, false, false));
    workspace.clipboard.policy.ai_planning_feature_allowed = true;
    assert!(workspace.ssh_needs_terminal_secrets(false, false, false));
    assert!(workspace.ssh_needs_terminal_secrets(true, false, false));
    workspace.clipboard.policy.ai_allowed = false;
    assert!(!workspace.ssh_needs_terminal_secrets(false, false, false));
    assert!(!workspace.ssh_needs_terminal_secrets(true, false, false));
    workspace.clipboard.policy.ai_allowed = true;
    workspace.set_test_ai_configuration(
        serde_json::from_value(json!({"general": {"features_enabled": false}})).unwrap(),
    );
    assert!(!workspace.ssh_needs_terminal_secrets(false, false, false));
    assert!(!workspace.ssh_needs_terminal_secrets(true, false, false));
}

#[test]
fn managed_servers_check_the_submitted_route_and_the_in_flight_attempt() {
    let mut workspace = Workspace::default();
    workspace.clipboard.policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'allow'\nhosts = ['gateway.example:22']",
    ).unwrap();
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.update_ssh(Message::Show(owner, None));
    let password = || LoginMessage::Password(ssh_dialog::Password::new(TEST_PASSWORD.into()));
    for message in [
        ssh_dialog::Message::Target(LoginMessage::Host("blocked.example".into())),
        ssh_dialog::Message::Target(LoginMessage::Username("fixture".into())),
        ssh_dialog::Message::Target(password()),
        ssh_dialog::Message::UseJump(true),
        ssh_dialog::Message::Jump(LoginMessage::Host("gateway.example".into())),
        ssh_dialog::Message::Jump(LoginMessage::Username("gateway".into())),
        ssh_dialog::Message::Jump(password()),
        ssh_dialog::Message::Connect,
    ] {
        let _ = workspace.update_ssh(Message::Form(owner, message));
    }
    let state = &workspace.windows[&owner];
    assert!(
        state
            .notice
            .as_deref()
            .unwrap()
            .contains("blocked.example:22")
    );
    assert!(
        matches!(&state.pending_ssh.as_ref().unwrap().stage, Stage::Form(form) if form.can_connect())
    );

    let _ = workspace.update_ssh(Message::Form(
        owner,
        ssh_dialog::Message::Jump(LoginMessage::Port("2222".into())),
    ));
    let _ = workspace.connect_ssh(owner);
    assert!(
        workspace.windows[&owner]
            .notice
            .as_deref()
            .unwrap()
            .contains("gateway.example:2222")
    );

    let _ = workspace.update_ssh(Message::Form(owner, ssh_dialog::Message::UseJump(false)));
    let _ = workspace.update_ssh(Message::Form(
        owner,
        ssh_dialog::Message::Target(LoginMessage::Host("gateway.example".into())),
    ));
    let dial = workspace.connect_ssh(owner);
    let Some(PendingSsh {
        stage: Stage::Dialing { id, .. },
        ..
    }) = workspace.pending_ssh(owner)
    else {
        panic!("the allowed direct route should be ready to dial");
    };
    let id = *id;
    drop(dial);
    workspace.clipboard.policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['gateway.example']",
    ).unwrap();
    let _ = workspace.ssh_dialed(owner, id, DialResponse(Arc::new(Mutex::new(None))));
    assert!(workspace.windows[&owner].pending_ssh.is_none());
    assert!(
        workspace.windows[&owner]
            .notice
            .as_deref()
            .unwrap()
            .contains("gateway.example:22")
    );
}

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
fn teamwork_ssh_requires_fresh_authentication_and_keeps_the_source_route_through_retry() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let login = LoginTarget {
        host: "shared.example".into(),
        port: 2222,
        username: "operator".into(),
    };
    let jump = LoginTarget {
        host: "gateway.example".into(),
        port: 22,
        username: "gateway".into(),
    };
    let fixture = TeamworkFixture::new();
    let catalog = fixture.catalog(&login.host);
    let selected = fixture.selection(&catalog);
    workspace.set_teamwork_catalog_for_test(catalog);
    let _ = workspace.show_teamwork_ssh(owner, selected, false);
    let key = workspace.guarded_ssh_form_key(owner);
    assert!(matches!(key, Some(FormKey::Teamwork(_))));
    let password = || LoginMessage::Password(ssh_dialog::Password::new(TEST_PASSWORD.into()));
    let _ = workspace.update_ssh(Message::Form(
        owner,
        ssh_dialog::Message::Target(password()),
    ));
    let pending = workspace.pending_ssh_mut(owner).unwrap();
    let Stage::Form(form) = &pending.stage else {
        panic!("expected a Teamwork login form")
    };
    assert!(!form.can_connect());
    assert!(form.vault_password().is_none());
    assert_eq!(form.profile(), PROFILE);
    assert_eq!(pending.saved, None);

    for message in [
        ssh_dialog::Message::Target(LoginMessage::Host("unrelated.example".into())),
        ssh_dialog::Message::Target(LoginMessage::Username("unrelated".into())),
        ssh_dialog::Message::Jump(LoginMessage::Port("2223".into())),
        ssh_dialog::Message::UseJump(false),
        ssh_dialog::Message::Isolation(IsolationLevel::None),
        ssh_dialog::Message::Target(password()),
        ssh_dialog::Message::Jump(password()),
        ssh_dialog::Message::Connect,
    ] {
        let _ = workspace.update_ssh(form_message(owner, key, message));
    }
    let pending = workspace.pending_ssh_mut(owner).unwrap();
    let Stage::Dialing {
        target,
        command_timestamps,
        connection_group,
        ..
    } = &pending.stage
    else {
        panic!("expected an authenticated Teamwork dial request")
    };
    assert!(target.matches_endpoints(&login, Some(&jump)));
    assert_eq!(target.saved_id(), None);
    assert_eq!(target.isolation, IsolationLevel::Process);
    assert_eq!(*command_timestamps, Some(true));
    assert_eq!(*connection_group, None);
    assert_eq!(
        pending.retry.as_ref().unwrap().startup,
        ConnectOptions::new(9, 2).unwrap()
    );
    let target = target.clone();
    let _ = workspace.reopen_ssh_form(owner, target, PROFILE, "Test retry.".into());
    let retry_key = workspace.guarded_ssh_form_key(owner);
    assert_ne!(retry_key, key);
    let _ = workspace.update_ssh(form_message(
        owner,
        key,
        ssh_dialog::Message::Target(password()),
    ));
    let _ = workspace.update_ssh(form_message(
        owner,
        retry_key,
        ssh_dialog::Message::Target(LoginMessage::Host("unrelated.example".into())),
    ));
    let Stage::Form(form) = &workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected a Teamwork retry form")
    };
    assert!(!form.can_connect());
    assert_eq!(form.target_endpoint(), Some(login));
}

#[test]
fn teamwork_ssh_rejects_changed_or_removed_selections_before_open_and_submit() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let fixture = TeamworkFixture::new();
    let original = fixture.catalog("shared.example");
    let selected = fixture.selection(&original);
    let changed = fixture.catalog("replacement.example");
    assert_eq!(
        changed.entries().next().unwrap().identity(),
        &selected.identity
    );

    for (current, sftp) in [(changed, false), (teamwork::Catalog::default(), true)] {
        workspace.set_teamwork_catalog_for_test(current.clone());
        let _ = workspace.show_teamwork_ssh(owner, selected.clone(), sftp);
        assert!(workspace.pending_ssh_mut(owner).is_none());
        assert_eq!(
            workspace.windows[&owner].notice.as_deref(),
            Some(TEAMWORK_RESELECT)
        );

        workspace.set_teamwork_catalog_for_test(original.clone());
        let _ = workspace.show_teamwork_ssh(owner, selected.clone(), sftp);
        let key = workspace.guarded_ssh_form_key(owner);
        assert!(matches!(key, Some(FormKey::Teamwork(_))));
        let password = || LoginMessage::Password(ssh_dialog::Password::new(TEST_PASSWORD.into()));
        for message in [
            ssh_dialog::Message::Target(password()),
            ssh_dialog::Message::Jump(password()),
        ] {
            let _ = workspace.update_ssh(form_message(owner, key, message));
        }
        let pending = workspace.pending_ssh_mut(owner).unwrap();
        assert!(matches!(&pending.stage, Stage::Form(form) if form.can_connect()));

        workspace.set_teamwork_catalog_for_test(current);
        let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Connect));
        assert!(workspace.pending_ssh_mut(owner).is_none());
        assert_eq!(
            workspace.windows[&owner].notice.as_deref(),
            Some(TEAMWORK_RESELECT)
        );
    }
}

#[test]
fn teamwork_hide_on_disk_cancels_a_stale_ssh_launch_and_blocks_sftp_until_restored() {
    let fixture = TeamworkFixture::new();
    let catalog = fixture.catalog("shared.example");
    let selected = fixture.selection(&catalog);
    let hidden = teamwork::HiddenEntry::from(catalog.entries().next().unwrap());
    let mut store = Store::open(selected.store_path.clone()).unwrap();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.set_teamwork_catalog_for_test(catalog);
    let _ = workspace.show_teamwork_ssh(owner, selected.clone(), false);
    let key = workspace.guarded_ssh_form_key(owner);
    assert!(matches!(key, Some(FormKey::Teamwork(_))));

    store.hide_teamwork(std::slice::from_ref(&hidden)).unwrap();
    let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Connect));
    assert!(workspace.pending_ssh_mut(owner).is_none());
    assert_eq!(
        workspace.windows[&owner].notice.as_deref(),
        Some(TEAMWORK_RESELECT)
    );
    let _ = workspace.show_teamwork_ssh(owner, selected.clone(), true);
    assert!(workspace.pending_ssh_mut(owner).is_none());

    store.restore_teamwork(hidden.identity()).unwrap();
    let _ = workspace.show_teamwork_ssh(owner, selected, true);
    assert!(
        workspace
            .pending_ssh_mut(owner)
            .is_some_and(|pending| pending.sftp)
    );
}

#[test]
fn teamwork_default_vault_password_is_bound_to_the_effective_target_and_scope() {
    use kortty_vault::{CredentialInput, Secret, Vault};

    let fixture = TeamworkFixture::new();
    let vault_path = fixture.0.join("vault.bin");
    let mut vault = Vault::create(&vault_path, b"teamwork-auth-test-master").unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Teamwork fixture".into(),
                username: "vault-user".into(),
                server_pattern: "shared.example".into(),
                ..CredentialInput::default()
            },
            Secret::new(TEST_PASSWORD.into()),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.vault = Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
        vault_path, vault,
    ));
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::Password {
        credential: credential.clone(),
    });

    let eligible = fixture.catalog_with_auth("shared.example", "", "");
    let selected = fixture.selection(&eligible);
    assert!(selected.default_auth_eligible);
    workspace.set_teamwork_catalog_for_test(eligible);
    let _ = workspace.show_teamwork_ssh(owner, selected, false);
    let key = workspace.guarded_ssh_form_key(owner);
    let _ = workspace.update_ssh(form_message(
        owner,
        key,
        ssh_dialog::Message::Jump(LoginMessage::Password(ssh_dialog::Password::new(
            "separate-jump-password".into(),
        ))),
    ));
    let Stage::Form(form) = &mut workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected the shared login form");
    };
    assert_eq!(form.target_endpoint().unwrap().username, "vault-user");
    let chosen = form.explicit_vault();
    let mut request = form.take_request().unwrap();
    assert!(
        matches!(&request.target.auth, ssh_dialog::Authentication::VaultPassword(id) if id == &credential)
    );
    assert_eq!(request.jump.as_ref().unwrap().endpoint.username, "gateway");
    assert!(
        matches!(&request.jump.as_ref().unwrap().auth, ssh_dialog::Authentication::Password(password) if password.as_str() == "separate-jump-password")
    );
    assert!(saved_credentials_match(&request, None, chosen.as_ref()));
    request.target.endpoint.host = "different.example".into();
    assert!(!saved_credentials_match(&request, None, chosen.as_ref()));

    for (host, reference) in [
        ("different.example", String::new()),
        (
            "shared.example",
            format!("<credentialId>{credential}</credentialId>"),
        ),
    ] {
        let catalog = fixture.catalog_with_auth(host, "operator", &reference);
        let selection = fixture.selection(&catalog);
        workspace.set_teamwork_catalog_for_test(catalog);
        let _ = workspace.show_teamwork_ssh(owner, selection, false);
        let Stage::Form(form) = &workspace.pending_ssh_mut(owner).unwrap().stage else {
            panic!("expected a manual shared login form");
        };
        assert_eq!(form.target_endpoint().unwrap().username, "operator");
        assert!(form.vault_password().is_none());
    }
}

#[test]
fn teamwork_vault_unlock_resumes_the_same_target_after_the_prompt_closes() {
    use crate::vault_window::{Completion, Message as VaultMessage, VaultWindow};
    use kortty_vault::{CredentialInput, Secret, Vault};

    const MASTER: &[u8] = b"teamwork-unlock-test-master";
    let fixture = TeamworkFixture::new();
    let path = fixture.0.join("vault.bin");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Teamwork unlock fixture".into(),
                username: "vault-operator".into(),
                server_pattern: "shared.example".into(),
                ..CredentialInput::default()
            },
            Secret::new(TEST_PASSWORD.into()),
        )
        .unwrap();
    drop(vault);
    let catalog = fixture.catalog_with_auth("shared.example", "", "");
    let selection = fixture.selection(&catalog);
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.vault = Some(VaultWindow::open(path.clone()).unwrap());
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::Password {
        credential: credential.clone(),
    });
    workspace.set_teamwork_catalog_for_test(catalog);

    let _ = workspace.show_teamwork_ssh(owner, selection, true);
    let unlock = workspace.vault_unlock_for_owner(owner).unwrap();
    assert!(workspace.pending_ssh_mut(owner).is_none());
    let _ = workspace.update_vault(
        unlock,
        VaultMessage::Completed(Completion::unlock_fixture(&path, MASTER, true)),
    );
    assert!(workspace.vault_id.is_none());
    assert!(workspace.pending_ssh_mut(owner).is_none());
    let _ = workspace.update(WorkspaceMessage::Window(unlock, window::Event::Closed));

    let pending = workspace.pending_ssh_mut(owner).unwrap();
    assert!(pending.sftp);
    let Stage::Form(form) = &pending.stage else {
        panic!("expected the deferred SFTP login form");
    };
    let (endpoint, selected_credential) = form.explicit_vault().unwrap();
    assert_eq!(endpoint.username, "vault-operator");
    assert_eq!(endpoint.host, "shared.example");
    assert_eq!(selected_credential, credential);
}

#[test]
fn cancelling_teamwork_vault_unlock_preserves_the_source_and_rejects_a_changed_preference() {
    use crate::vault_window::VaultWindow;
    use kortty_vault::Vault;

    let fixture = TeamworkFixture::new();
    let path = fixture.0.join("vault.bin");
    drop(Vault::create(&path, b"teamwork-cancel-test-master").unwrap());
    let catalog = fixture.catalog_with_auth("shared.example", "operator", "");
    let selection = fixture.selection(&catalog);
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.vault = Some(VaultWindow::open(path).unwrap());
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::Password {
        credential: "fixture-credential".into(),
    });
    workspace.set_teamwork_catalog_for_test(catalog);

    let _ = workspace.show_teamwork_ssh(owner, selection.clone(), false);
    let unlock = workspace.vault_unlock_for_owner(owner).unwrap();
    let _ = workspace.close_profile_tool(unlock, false);
    let _ = workspace.update(WorkspaceMessage::Window(unlock, window::Event::Closed));
    let Stage::Form(form) = &workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected the manual Teamwork login form");
    };
    assert_eq!(form.target_endpoint().unwrap().username, "operator");
    assert!(form.explicit_vault().is_none());

    let _ = workspace.cancel_ssh_login(owner);
    let _ = workspace.show_teamwork_ssh(owner, selection, false);
    let unlock = workspace.vault_unlock_for_owner(owner).unwrap();
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::Prompt);
    let _ = workspace.close_profile_tool(unlock, false);
    let _ = workspace.update(WorkspaceMessage::Window(unlock, window::Event::Closed));
    assert!(workspace.pending_ssh_mut(owner).is_none());
    assert!(
        workspace.windows[&owner]
            .notice
            .as_deref()
            .unwrap()
            .contains("authentication changed")
    );
}

#[test]
fn teamwork_temporary_key_ignores_osc_permission_and_rejects_stale_paste_requests() {
    let fixture = TeamworkFixture::new();
    let catalog = fixture.catalog_with_auth("shared.example", "operator", "");
    let selection = fixture.selection(&catalog);
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.set_teamwork_catalog_for_test(catalog);
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::TemporaryKey {
        username: " key-user ".into(),
    });
    assert!(!workspace.clipboard.read_allowed());
    let _ = workspace.show_teamwork_ssh(owner, selection.clone(), true);
    let initial = workspace.guarded_ssh_form_key(owner);
    let stale_form = system_temporary_key_paste(
        request_temporary_key_paste(&mut workspace, owner, initial),
        TEAMWORK_TEST_KEY,
    );
    let _ = workspace.show_teamwork_ssh(owner, selection, true);
    let Some(FormKey::Teamwork(revision)) = workspace.guarded_ssh_form_key(owner) else {
        panic!("expected a revision-bound shared form");
    };
    assert_ne!(initial, Some(FormKey::Teamwork(revision)));
    let _ = workspace.update_ssh(stale_form);
    let paste = |workspace: &mut Workspace, contents| {
        system_temporary_key_paste(
            request_temporary_key_paste(workspace, owner, Some(FormKey::Teamwork(revision))),
            contents,
        )
    };
    let jump_password = || {
        ssh_dialog::Message::Jump(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        )))
    };
    let _ = workspace.update_ssh(Message::TeamworkForm(owner, revision, jump_password()));
    assert!(!teamwork_form_can_connect(&mut workspace, owner));

    let stale_lifetime = paste(&mut workspace, TEAMWORK_TEST_KEY);
    for minutes in ["15", "60"] {
        let _ = workspace.update_ssh(Message::TeamworkForm(
            owner,
            revision,
            ssh_dialog::Message::Target(LoginMessage::TemporaryLifetime(minutes.into())),
        ));
    }
    let _ = workspace.update_ssh(stale_lifetime);
    assert!(!teamwork_form_can_connect(&mut workspace, owner));

    let stale_method = paste(&mut workspace, TEAMWORK_TEST_KEY);
    for method in [
        ssh_dialog::Method::Password,
        ssh_dialog::Method::TemporaryKey,
    ] {
        let _ = workspace.update_ssh(Message::TeamworkForm(
            owner,
            revision,
            ssh_dialog::Message::Target(LoginMessage::Authentication(method)),
        ));
    }
    let _ = workspace.update_ssh(stale_method);
    assert!(!teamwork_form_can_connect(&mut workspace, owner));

    let superseded = paste(&mut workspace, "obsolete clipboard contents");
    let latest = paste(&mut workspace, TEAMWORK_TEST_KEY);
    assert!(!format!("{latest:?}").contains("fixture-only"));
    let Message::TemporaryKeyPasted(_, ticket, _) = latest.clone() else {
        panic!("expected a temporary key paste result");
    };
    let _ = workspace.update_ssh(latest);
    let _ = workspace.update_ssh(superseded);
    let _ = workspace.update_ssh(Message::TemporaryKeyPasted(
        owner,
        ticket,
        ssh_dialog::Password::new("duplicate clipboard delivery".into()),
    ));
    let _ = workspace.update_ssh(Message::TeamworkForm(
        owner,
        revision,
        ssh_dialog::Message::Connect,
    ));
    let Stage::Dialing { target, .. } = &workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected a dial attempt with a live temporary key");
    };
    let target = target.clone();
    assert_eq!(target.username(), "key-user");
    assert_eq!(target.jump.as_ref().unwrap().username, "gateway");
    assert_eq!(target.saved_id(), None);

    let _ = workspace.reopen_ssh_form(owner, target.clone(), PROFILE, "Review the key.".into());
    let retry = workspace.guarded_ssh_form_key(owner);
    assert_ne!(retry, Some(FormKey::Teamwork(revision)));
    let _ = workspace.update_ssh(form_message(owner, retry, jump_password()));
    let Stage::Form(form) = &mut workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected the retry form")
    };
    let request = form.take_request().unwrap();
    assert!(!format!("{request:?}").contains("fixture-only"));
    let ssh_dialog::Authentication::TemporaryKey(key) = request.target.auth else {
        panic!("the checked retry should retain its temporary key");
    };
    assert_eq!(
        &**key.copy_for_attempt().unwrap(),
        format!("{TEAMWORK_TEST_KEY}\n").as_bytes()
    );

    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::Prompt);
    let _ = workspace.reopen_ssh_form(owner, target, PROFILE, "Review the key again.".into());
    assert!(workspace.pending_ssh_mut(owner).is_none());
    assert!(
        workspace.windows[&owner]
            .notice
            .as_deref()
            .unwrap()
            .contains("authentication changed")
    );
}

#[test]
fn teamwork_temporary_key_uses_managed_clipboard_mode_and_rechecks_teamwork_policy() {
    use iced::futures::executor::block_on;
    use iced_runtime::{Action, task};

    let fixture = TeamworkFixture::new();
    let catalog = fixture.catalog_with_auth("shared.example", "operator", "");
    let selection = fixture.selection(&catalog);
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    workspace.set_teamwork_catalog_for_test(catalog);
    workspace.set_teamwork_auth_for_test(teamwork::DefaultAuth::TemporaryKey {
        username: String::new(),
    });
    workspace.clipboard.policy.read_allowed = false;
    workspace.clipboard.policy.paste_events_allowed = false;
    let _ = workspace.show_teamwork_ssh(owner, selection, false);
    let revision = workspace.guarded_ssh_form_key(owner);
    let _ = workspace.update_ssh(form_message(
        owner,
        revision,
        ssh_dialog::Message::Jump(LoginMessage::Password(ssh_dialog::Password::new(
            TEST_PASSWORD.into(),
        ))),
    ));
    let old_mode = system_temporary_key_paste(
        request_temporary_key_paste(&mut workspace, owner, revision),
        TEAMWORK_TEST_KEY,
    );
    workspace.clipboard.policy.internal = true;
    let _ = workspace.update_ssh(old_mode);
    assert!(!teamwork_form_can_connect(&mut workspace, owner));
    workspace.clipboard.buffer.set(TEAMWORK_TEST_KEY.into());
    let internal_paste = |workspace: &mut Workspace| {
        let task = request_temporary_key_paste(workspace, owner, revision);
        let mut actions = task::into_stream(task).unwrap();
        let Some(Action::Output(WorkspaceMessage::Ssh(message))) = block_on(actions.next()) else {
            panic!("the internal clipboard must produce a result without a system read");
        };
        message
    };
    let allowed = internal_paste(&mut workspace);
    let _ = workspace.update_ssh(allowed);
    assert!(teamwork_form_can_connect(&mut workspace, owner));

    let denied = internal_paste(&mut workspace);
    workspace.clipboard.policy.teamwork_allowed = false;
    let _ = workspace.update_ssh(denied);
    assert!(workspace.pending_ssh_mut(owner).is_none());
    assert_eq!(
        workspace.windows[&owner].notice.as_deref(),
        Some("Managed policy blocks Teamwork connections.")
    );
    assert_eq!(
        request_temporary_key_paste(&mut workspace, owner, revision).units(),
        0
    );
}

const TEAMWORK_TEST_KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nfixture-only\n-----END OPENSSH PRIVATE KEY-----";

fn request_temporary_key_paste(
    workspace: &mut Workspace,
    owner: window::Id,
    key: Option<FormKey>,
) -> Task<WorkspaceMessage> {
    workspace.update_ssh(form_message(
        owner,
        key,
        ssh_dialog::Message::PasteTemporaryKey,
    ))
}

fn system_temporary_key_paste(task: Task<WorkspaceMessage>, contents: &str) -> Message {
    use iced::futures::executor::block_on;
    use iced_runtime::{Action, clipboard, task};

    let mut actions = task::into_stream(task).unwrap();
    let Some(Action::Clipboard(clipboard::Action::Read { channel, .. })) = block_on(actions.next())
    else {
        panic!("the explicit paste should request the system clipboard");
    };
    assert!(channel.send(Some(contents.into())).is_ok());
    let Some(Action::Output(WorkspaceMessage::Ssh(message))) = block_on(actions.next()) else {
        panic!("expected a revision-bound paste result");
    };
    message
}

fn teamwork_form_can_connect(workspace: &mut Workspace, owner: window::Id) -> bool {
    let Stage::Form(form) = &workspace.pending_ssh_mut(owner).unwrap().stage else {
        panic!("expected the key form");
    };
    assert!(form.temporary_key_selected());
    form.can_connect()
}

struct TeamworkFixture(PathBuf);

impl TeamworkFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-teamwork-ssh-{}",
            crate::private_files::nonce().unwrap()
        ));
        crate::private_files::Directory::open(&root, true).unwrap();
        Self(root)
    }

    fn selection(&self, catalog: &teamwork::Catalog) -> TeamworkSelection {
        TeamworkSelection::new(
            catalog.entries().next().unwrap(),
            self.0.join("connections.json"),
        )
    }

    fn catalog(&self, host: &str) -> teamwork::Catalog {
        self.catalog_with_auth(
            host,
            "operator",
            "<credentialId>foreign-password-id</credentialId>",
        )
    }

    fn catalog_with_auth(&self, host: &str, username: &str, reference: &str) -> teamwork::Catalog {
        let path = self.0.join("shared.xml");
        std::fs::write(
            &path,
            format!(
                r#"<connections schemaVersion="2"><connection id="shared-fixture">
                <name>Shared fixture</name><host>{host}</host><port>2222</port><username>{username}</username>
                {reference}
                <jumpServer><enabled>true</enabled><host>gateway.example</host><username>gateway</username>
                    <authMethod>PUBLIC_KEY</authMethod><sshKeyId>23</sshKeyId>
                </jumpServer>
                <terminalEmulationType>{}</terminalEmulationType><isolationMode>process</isolationMode>
                <connectionTimeoutSeconds>9</connectionTimeoutSeconds><retryCount>2</retryCount>
                <settings><useGlobalSettings>false</useGlobalSettings>
                    <commandTimestampsEnabled>true</commandTimestampsEnabled>
                </settings>
                </connection></connections>"#,
                PROFILE.term_name()
            ),
        )
        .unwrap();
        let settings = teamwork::Settings {
            sources: vec![teamwork::Source {
                location: path.to_str().unwrap().into(),
                enabled: true,
                kind: teamwork::SourceKind::File,
                check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
            }],
            ..teamwork::Settings::default()
        };
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap())
    }
}

impl Drop for TeamworkFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
        .store_disconnected_ssh(target, PROFILE, FONT, Default::default())
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
fn restored_project_ssh_sign_in_uses_project_size_after_layout_measurement() {
    use crate::profile::PaneLayout;
    use crate::workspace::reconnect::ReconnectTarget;

    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_columns = Some(99);
    workspace.window_settings.terminal_rows = Some(31);
    let initial_size = InitialTerminalSize {
        columns: Some(132),
        rows: None,
    };
    let pane = PaneLayout::Ssh {
        font: FONT,
        focused: true,
        term: PROFILE.term_name().into(),
        target: SshTarget {
            login: LoginTarget {
                host: "project-terminal-size.invalid".into(),
                port: 2222,
                username: "operator".into(),
            },
            jump: None,
            saved: None,
            isolation: IsolationLevel::None,
        },
        initial_size,
    };
    let (owner, _) = workspace.open_window(false);
    let mut active = None;
    let _ = workspace
        .restore_pane(&pane, &mut Vec::new(), &mut active, None)
        .unwrap();
    let source = active.unwrap();
    let grid = workspace.sessions[&source].grid;
    assert_eq!((grid.columns, grid.rows), (132, 31));
    workspace.add_tab(owner, source);
    let tab = workspace.windows[&owner].tab().unwrap().id;
    let measured = GridSize {
        columns: 105,
        rows: 39,
        pixel_width: 840,
        pixel_height: 624,
        ..GridSize::default()
    };
    let _ = workspace.update(WorkspaceMessage::PaneResized(source, measured));
    assert_eq!(workspace.capture_layout().unwrap().windows[0].tabs[0], pane);

    for review in [false, true] {
        if review {
            let target = ReconnectTarget { tab, source };
            let _ = workspace.review_project_ssh_reconnect(owner, target);
        } else {
            let _ = workspace.update_ssh(Message::Show(owner, Some(source)));
        }
        let key = workspace.guarded_ssh_form_key(owner);
        let _ = workspace.update_ssh(form_message(
            owner,
            key,
            ssh_dialog::Message::Target(LoginMessage::Password(ssh_dialog::Password::new(
                TEST_PASSWORD.into(),
            ))),
        ));
        // Prepare the actual dial task without polling it or opening a network connection.
        let dial = workspace.connect_ssh(owner);
        let Stage::Dialing { grid, .. } = &workspace.pending_ssh(owner).unwrap().stage else {
            panic!("expected a prepared project SSH dial");
        };
        assert_eq!(
            (*grid, workspace.live_ssh_grid(owner)),
            (
                GridSize {
                    columns: 132,
                    rows: 31,
                    ..GridSize::default()
                },
                Some((measured, false)),
            )
        );
        drop(dial);
        let _ = workspace.update_ssh(Message::Cancel(owner));
        assert!(workspace.sessions[&source].project_initial_ssh_pending);
    }
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
        window_geometry: None,
        terminal: Default::default(),
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
        window_geometry: None,
        terminal: Default::default(),
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
fn saved_incognito_sftp_retains_privacy_before_authentication() {
    let connection: Connection = serde_json::from_value(serde_json::json!({
        "id": 3,
        "name": "Private SFTP fixture",
        "tag": "",
        "login": {"endpoint": {"host": "private.example", "port": 22, "username": "operator"}, "auth": "prompt"},
        "jump": null,
        "emulation": PROFILE.term_name(),
        "tunnels": [],
        "incognito": true
    })).unwrap();
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let _ = workspace.show_saved_ssh(window, connection.clone(), true);
    let pending = workspace.pending_ssh_mut(window).unwrap();
    assert!(pending.sftp && pending.incognito);
    let _ = workspace.update_ssh(Message::Cancel(window));
    workspace.clipboard.policy.incognito_allowed = false;
    let _ = workspace.show_saved_ssh(window, connection, true);
    assert!(workspace.pending_ssh_mut(window).is_none());
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
        window_geometry: None,
        terminal: Default::default(),
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
