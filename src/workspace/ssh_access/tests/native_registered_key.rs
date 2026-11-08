use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use kortty_ssh::sftp::Sftp;
use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

use super::*;
use crate::connection_store::{Auth, SavedLogin, Store};
use crate::ssh_key_catalog::{Catalog, Edit, KeyDraft, KeyInput};
use crate::ssh_test_fixture::TempDirectory;
use crate::ssh_worker::{OpenSshFixture, dial_test_executable, test_executable};
use crate::vault_window::VaultWindow;

const HOME_MARKER: &str = "KORTTY_REGISTERED_SSH_TEST_HOME";
const TEST_NAME: &str = "workspace::ssh_access::tests::native_registered_key::openssh_registered_copy_reaches_kitty_desktop_session";
const PASSPHRASE: &str = "native-registered-ssh-key-fixture-only";
const DEADLINE: Duration = Duration::from_secs(8);

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_registered_copy_reaches_kitty_desktop_session() {
    let Some(home) = std::env::var_os(HOME_MARKER).map(PathBuf::from) else {
        // Re-exec only this test with a private profile. Other tests in the
        // current process may be reading their own environment concurrently.
        let scratch = TempDirectory::new("ssh-registered-home");
        let home = scratch.0.canonicalize().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
            .env(HOME_MARKER, &home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    };
    let root = crate::profile::root().unwrap();
    assert!(
        home.is_dir()
            && home.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("kortty-ssh-registered-home-")
            })
            && root.starts_with(home)
            && !root.exists(),
        "registered SSH test requires a fresh private profile"
    );

    let peer = OpenSshFixture::new();
    let peer_root = peer.directory().canonicalize().unwrap();
    let source = peer_root.join("client");
    let encrypted = Command::new("ssh-keygen")
        .args(["-q", "-p", "-P", "", "-N", PASSPHRASE, "-f"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(encrypted.status.success(), "{:?}", encrypted.stderr);
    let endpoint = peer.request(source.clone()).target.endpoint;

    let vault_path = crate::profile::vault_path().unwrap();
    fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    let mut vault = Vault::create(&vault_path, b"registered-ssh-native-test-master").unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Native fixture passphrase".into(),
                username: endpoint.username.clone(),
                server_pattern: endpoint.host.clone(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new(PASSPHRASE.into()),
        )
        .unwrap();

    let mut catalog = Catalog::open_profile().unwrap();
    let id = catalog
        .apply(vec![Edit::Add(KeyDraft {
            name: "Native encrypted copy".into(),
            description: String::new(),
            input: KeyInput::Copy(source.clone()),
            passphrase: Some(credential),
        })])
        .unwrap()
        .added[0];
    // Keep the authorized public key unchanged. The new source key can never
    // authenticate to this server, so success requires the managed copy.
    fs::copy(peer_root.join("rotated"), &source).unwrap();

    let profile = TerminalProfile::Xterm;
    let connections = crate::profile::connections_path().unwrap();
    let saved_id = Store::open(connections.clone())
        .unwrap()
        .save(Connection {
            id: 0,
            name: "Registered SSH fixture".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint,
                auth: Auth::StoredKey(id),
                keyboard_interactive: false,
            },
            jump: None,
            emulation: profile.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: IsolationLevel::None,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: Some(true),
            tab_color: None,
            paste_warning: None,
        })
        .unwrap();
    let store = Store::open(connections).unwrap();
    let saved = store.get(saved_id).unwrap();
    let request = || {
        SshDialog::from_saved(profile, saved)
            .take_request()
            .unwrap()
    };

    let mut desktop = Workspace::default();
    assert!(matches!(
        desktop.resolve_ssh_logins(&mut request(), Some(saved)),
        Err("Unlock the vault before connecting.")
    ));
    let mut vault_window = VaultWindow::open(vault_path).unwrap();
    vault_window.restore_after_ssh_keys(vault);
    desktop.vault = Some(vault_window);

    let fixture_shell = peer_root.join("fixture-shell");
    fs::write(
        &fixture_shell,
        br#"#!/bin/sh
printf '\033_Ga=T,i=17,f=24,s=1,v=1,q=2,C=1;/wAA\033\\'
IFS= read -r input && [ "$input" = 'kitty-input-check' ]
"#,
    )
    .unwrap();
    let shell = fixture_shell.to_str().unwrap().replace('\'', "'\\''");
    let shell_home = peer_root
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let server_config = format!(
        "SetEnv \"HOME={shell_home}\" \"ZDOTDIR={shell_home}\" ENV=/dev/null BASH_ENV=/dev/null\nForceCommand /bin/sh '{shell}'\n"
    );
    let trust = peer.pinned_trust();
    let authorized = fs::read_to_string(peer_root.join("client.pub")).unwrap();
    let fingerprint = HostKey::from_openssh(authorized.trim())
        .unwrap()
        .fingerprint();
    let executable = test_executable();
    let grid = initial_grid(profile);
    let (events, messages) = async_channel::bounded(1);
    let prompt = challenge::Forwarder {
        events,
        window: window::Id::unique(),
        dial: 1,
        cancellation: Default::default(),
        rounds: Arc::default(),
        secrets: None,
    };
    let remote_file = peer_root.join("registered-sftp.bin");
    let remote_path = remote_file.to_str().unwrap().as_bytes();
    let contents = b"\0registered-copy SFTP payload\xff";
    fs::write(&remote_file, contents).unwrap();
    for isolation in [IsolationLevel::None, IsolationLevel::Process] {
        let connect = |desktop: &Workspace, sftp| {
            let mut request = request();
            request.isolation = isolation;
            let snapshot = desktop
                .resolve_ssh_logins(&mut request, Some(saved))
                .unwrap();
            assert!(snapshot.is_current());
            if isolation.is_none() {
                dial_direct(request, sftp, &prompt, trust.clone(), ssh_size(grid)).unwrap()
            } else {
                let worker = dial_test_executable(request, sftp, &executable, &trust).unwrap();
                Connected::Worker(Box::new(worker))
            }
        };
        let server = peer.serve_with_extra("host", &server_config);
        let (owner, _) = desktop.open_window(false);
        let _ = desktop.show_saved_ssh(owner, saved.clone(), false);
        let _ = desktop.update_ssh(Message::Form(
            owner,
            ssh_dialog::Message::Isolation(isolation),
        ));
        // The GUI's dial task is deliberately unpolled. Its completion is
        // supplied by the real isolated OpenSSH fixture below.
        let _ = desktop.connect_ssh(owner);
        let Stage::Dialing {
            id,
            command_timestamps: Some(true),
            ..
        } = desktop.pending_ssh_mut(owner).unwrap().stage
        else {
            panic!("the saved timestamp choice must be captured by the SSH attempt");
        };
        let connected = connect(&desktop, false);
        let _ = desktop.update_ssh(Message::Dialed(
            owner,
            id,
            DialResponse(Arc::new(Mutex::new(Some(Ok(connected))))),
        ));
        let tab = desktop.windows[&owner].tab().unwrap();
        assert!(tab.command_timestamps);
        let session = tab.focused_session().unwrap();
        let terminal = &desktop.sessions[&session].session;
        let frame = wait_for_frame(terminal, |frame| !frame.screen.images.is_empty());
        let [image] = frame.screen.images.as_slice() else {
            panic!("the SSH fixture sent one image");
        };
        assert_eq!(image.placement.image.id, 17);
        assert_eq!(image.placement.image.rgba.as_ref(), &[255, 0, 0, 255]);
        terminal.send(b"kitty-input-check\r".to_vec()).unwrap();
        let frame = wait_for_frame(terminal, |frame| matches!(frame.status, Status::Exited(_)));
        assert_eq!(frame.status, Status::Exited(Some(0)));
        let log = String::from_utf8_lossy(&server.join().unwrap().stderr).into_owned();
        assert!(log.contains("Accepted publickey") && log.contains(&fingerprint));
        assert!(!log.contains(PASSPHRASE));
        desktop.stop_session(session);
        for worker in std::mem::take(&mut desktop.pending_shutdown) {
            let _ = worker.recv_blocking();
        }

        let server = peer.serve_with_extra("host", "ForceCommand internal-sftp\n");
        let mut connected = connect(&desktop, true);
        let mut sftp = Sftp::new(
            connected.take_reader().unwrap(),
            connected.take_writer().unwrap(),
        )
        .unwrap();
        let mut downloaded = Vec::new();
        sftp.download(remote_path, &mut downloaded).unwrap();
        assert_eq!(downloaded, contents);
        connected.close().unwrap();
        let log = String::from_utf8_lossy(&server.join().unwrap().stderr).into_owned();
        assert!(log.contains("Accepted publickey") && log.contains(&fingerprint));
        assert!(messages.is_empty());
    }
}

fn wait_for_frame(
    terminal: &Session,
    ready: impl Fn(&crate::session::Frame) -> bool,
) -> Arc<crate::session::Frame> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let frame = terminal.frame();
        if ready(&frame) {
            return frame;
        }
        assert!(
            Instant::now() < deadline && !matches!(frame.status, Status::Error(_)),
            "SSH terminal did not reach the expected state: {:?}",
            frame.status
        );
        thread::sleep(Duration::from_millis(10));
    }
}
