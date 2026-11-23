use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, executor::block_on};
use iced_runtime::{Action, task};

use crate::connection_store::{Auth, SavedLogin};
use crate::ssh_test_fixture::TempDirectory;
use crate::ssh_worker::OpenSshFixture;
use crate::workspace::reconnect::ReconnectTarget;

use super::*;

const HOME_MARKER: &str = "KORTTY_ISOLATION_RECONNECT_TEST_HOME";
const TEST: &str = "workspace::ssh_access::tests::native_isolation_reconnect::openssh_reconnect_keeps_saved_and_teamwork_isolation";

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_reconnect_keeps_saved_and_teamwork_isolation() {
    let Some(home) = std::env::var_os(HOME_MARKER).map(PathBuf::from) else {
        let scratch = TempDirectory::new("isolation-reconnect-home");
        let home = scratch.0.canonicalize().unwrap();
        let run = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", TEST, "--nocapture"])
            .env(HOME_MARKER, &home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(
            run.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
        return;
    };
    let root = crate::profile::root().unwrap();
    assert!(
        home.file_name().is_some_and(|name| name
            .to_string_lossy()
            .starts_with("kortty-isolation-reconnect-home-"))
            && root.starts_with(&home)
            && !root.exists(),
        "reconnect fixture requires a fresh private profile"
    );
    let peer = OpenSshFixture::new();
    let client = peer.directory().join("client");
    let endpoint = peer.request(client.clone()).target.endpoint;
    let host_key = fs::read_to_string(peer.directory().join("host.pub")).unwrap();
    let host_key = HostKey::from_openssh(host_key.trim()).unwrap();
    let host = Endpoint::new(&endpoint.host, endpoint.port).unwrap();
    crate::private_files::Directory::open(&root, true).unwrap();
    assert_eq!(
        trust_store().unwrap().remember(&host, &host_key).unwrap(),
        HostKeyStatus::Known
    );

    let path = crate::profile::connections_path().unwrap();
    let mut store = Store::open(path.clone()).unwrap();
    let folder = store.create_group(None, "Fixtures").unwrap();
    let id = store
        .save_in_group(
            Connection {
                id: 0,
                name: "Saved isolation fixture".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: endpoint.clone(),
                    auth: Auth::KeyFile {
                        path: client.clone(),
                        passphrase: None,
                    },
                    keyboard_interactive: false,
                },
                jump: None,
                emulation: PROFILE.term_name().into(),
                tunnels: Vec::new(),
                incognito: false,
                isolation: IsolationLevel::None,
                inherit_isolation: true,
                startup: Default::default(),
                logging: Default::default(),
                terminal_effect: None,
                command_timestamps: None,
                tab_color: None,
                paste_warning: None,
                window_geometry: None,
                terminal: Default::default(),
            },
            Some(&folder),
        )
        .unwrap();
    let saved = store.get(id).unwrap().clone();
    let catalog = catalog(peer.directory(), &endpoint);
    let selection = TeamworkSelection::new(catalog.entries().next().unwrap(), path);
    let mut workspace = Workspace::default();
    workspace.set_teamwork_catalog_for_test(catalog);

    let fixture_shell = peer.directory().join("fixture-shell");
    fs::write(
        &fixture_shell,
        "printf 'isolation-wire-ready\\n'\nIFS= read -r line\n",
    )
    .unwrap();
    let shell = fixture_shell.to_str().unwrap().replace('\'', "'\\''");
    let force_shell = format!("PermitUserRC no\nForceCommand /bin/sh '{shell}'\n");
    for shared in [false, true] {
        let (owner, _) = workspace.open_window(false);
        let initial = if shared {
            IsolationLevel::Process
        } else {
            IsolationLevel::None
        };
        if shared {
            let _ = workspace.show_teamwork_ssh(owner, selection.clone(), false);
            enter_teamwork_key(&mut workspace, owner, &client);
        } else {
            let _ = workspace.show_saved_ssh(owner, saved.clone(), false);
        }
        let first = peer.serve_with_extra("host", &force_shell);
        let launch = workspace.connect_ssh(owner);
        finish_wire_attempt(&mut workspace, owner, launch, initial);
        let tab = workspace.windows[&owner].tab().unwrap();
        let source = tab.focused_session().unwrap();
        let target = ReconnectTarget {
            tab: tab.id,
            source,
        };
        wait_for_output(&workspace.sessions[&source].session);
        assert_eq!(workspace.sessions[&source].teamwork_ssh.is_some(), shared);

        if shared {
            store
                .set_group_isolation(&folder, Some(IsolationLevel::Process), None)
                .unwrap();
        } else {
            store
                .set_group_isolation(&folder, None, Some(IsolationLevel::Process))
                .unwrap();
        }
        let next = peer.serve_with_extra("host", &force_shell);
        let launch = workspace.start_ssh_reconnect(owner, target);
        let launch = if shared {
            enter_teamwork_key(&mut workspace, owner, &client);
            workspace.connect_ssh(owner)
        } else {
            launch
        };
        finish_wire_attempt(&mut workspace, owner, launch, IsolationLevel::Process);
        if let Some(candidate) = workspace.pending_reconnects.keys().next().copied() {
            workspace.wait_for_test_reconnect(candidate);
        }
        let tab = workspace.windows[&owner].tab().unwrap();
        assert_eq!(tab.id, target.tab);
        let replacement = tab.focused_session().unwrap();
        assert_ne!(replacement, source);
        let pane = &workspace.sessions[&replacement];
        assert_eq!(
            pane.ssh.as_ref().unwrap().isolation,
            IsolationLevel::Process
        );
        assert_eq!(pane.teamwork_ssh.is_some(), shared);
        wait_for_output(&pane.session);
        workspace.stop_session(replacement);
        for shutdown in std::mem::take(&mut workspace.pending_shutdown) {
            let _ = shutdown.recv_blocking();
        }
        for server in [first, next] {
            let log = server.join().unwrap();
            assert!(String::from_utf8_lossy(&log.stderr).contains("Accepted publickey"));
        }
    }
}

fn catalog(directory: &Path, endpoint: &LoginTarget) -> teamwork::Catalog {
    let path = directory.join("shared.xml");
    fs::write(&path, format!(
        r#"<connections schemaVersion="2"><connection id="shared-isolation-fixture">
          <name>Shared isolation fixture</name><host>{}</host><port>{}</port><username>{}</username>
          <group>Fixtures</group><terminalEmulationType>{}</terminalEmulationType><isolationMode>none</isolationMode>
        </connection></connections>"#,
        endpoint.host, endpoint.port, endpoint.username, PROFILE.term_name()
    )).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: path.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..Default::default()
    };
    teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap())
}

fn enter_teamwork_key(workspace: &mut Workspace, owner: window::Id, client: &Path) {
    let key = workspace.guarded_ssh_form_key(owner);
    assert!(matches!(
        key,
        Some(FormKey::Teamwork(_) | FormKey::Reconnect(_))
    ));
    for edit in [
        LoginMessage::Authentication(ssh_dialog::Method::PrivateKey),
        LoginMessage::KeyFile(client.to_str().unwrap().into()),
    ] {
        let _ = workspace.update_ssh(form_message(owner, key, ssh_dialog::Message::Target(edit)));
    }
}

fn finish_wire_attempt(
    workspace: &mut Workspace,
    owner: window::Id,
    dial: Task<WorkspaceMessage>,
    level: IsolationLevel,
) {
    let Stage::Dialing { target, .. } =
        &workspace.pending_ssh(owner).expect("expected login").stage
    else {
        panic!(
            "expected a wire dial at {level}: {:?}",
            workspace.windows[&owner].notice
        );
    };
    assert_eq!(target.isolation, level);
    let mut actions = task::into_stream(dial).unwrap();
    let Some(Action::Output(WorkspaceMessage::Ssh(Message::Dialed(window, id, response)))) =
        block_on(actions.next())
    else {
        panic!("expected a native dial response");
    };
    assert_eq!(window, owner);
    {
        let result = response.0.lock().unwrap();
        match result.as_ref().unwrap() {
            Ok(connection) => assert_eq!(
                matches!(connection, Connected::Worker(_)),
                level >= IsolationLevel::Process
            ),
            Err(error) => panic!("native SSH dial failed: {error}"),
        }
    }
    let _ = workspace.update_ssh(Message::Dialed(window, id, response));
}

fn wait_for_output(session: &Session) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let frame = session.frame();
        if frame
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains("isolation-wire-ready")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no marker from live SSH session: {:?}",
            frame.status
        );
        thread::sleep(Duration::from_millis(10));
    }
}
