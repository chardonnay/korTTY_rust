use super::*;
use crate::sftp_window::{self, worker};
use crate::ssh_dialog::{LoginMessage, Method};
use crate::workspace::quick_connect::{Message as Quick, Protocol};

const TEST: &str = concat!(
    "workspace::ssh_access::tests::native_isolation_reconnect::",
    "quick_temporary_key::openssh_quick_temporary_key_authenticates_and_recovers_after_expiry"
);

#[test]
#[ignore = "requires OpenSSH and waits for a real one-minute key expiry"]
fn openssh_quick_temporary_key_authenticates_and_recovers_after_expiry() {
    let Some(profile) = fixture_profile(TEST) else {
        return;
    };
    crate::private_files::Directory::open(&profile, true).unwrap();
    let peer = OpenSshFixture::new();
    let directory = peer.directory().canonicalize().unwrap();
    let authorized = directory.join("client");
    let endpoint = peer.request(authorized.clone()).target.endpoint;
    let pin = fs::read_to_string(directory.join("host.pub")).unwrap();
    let pin = HostKey::from_openssh(pin.trim()).unwrap();
    let host = Endpoint::new(&endpoint.host, endpoint.port).unwrap();
    trust_store().unwrap().remember(&host, &pin).unwrap();
    let public = fs::read_to_string(authorized.with_extension("pub")).unwrap();
    let fingerprint = HostKey::from_openssh(public.trim()).unwrap().fingerprint();

    let mut workspace = Workspace::default();
    workspace.window_settings.temporary_ssh_key_enabled = true;
    workspace.window_settings.open_tool_windows_as_tabs = false;
    workspace.clipboard.policy.internal = true;
    let (terminal, _) = workspace.open_window(false);
    let (files, _) = workspace.open_window(false);
    let shell = directory.join("quick-key-shell");
    fs::write(
        &shell,
        "printf 'quick-key-terminal-ready\n'\nIFS= read -r line\n",
    )
    .unwrap();
    let shell = format!(
        "PermitUserRC no\nSetEnv \"HOME={0}\" \"ZDOTDIR={0}\" ENV=/dev/null BASH_ENV=/dev/null\nForceCommand /bin/sh '{1}'\n",
        directory.display(),
        shell.display(),
    );

    let denied = peer.serve_with_extra("host", &shell);
    let attempt = submit_quick_key(
        &mut workspace,
        terminal,
        Protocol::Ssh,
        &endpoint,
        &directory.join("rotated"),
    );
    let (id, response) = dial_result(attempt, terminal);
    assert!(response.0.lock().unwrap().as_ref().unwrap().is_err());
    let _ = workspace.update_ssh(Message::Dialed(terminal, id, response));
    assert!(
        !String::from_utf8_lossy(&denied.join().unwrap().stderr).contains("Accepted publickey")
    );
    assert!(matches!(&workspace.pending_ssh(terminal).unwrap().stage,
        Stage::Form(form) if form.temporary_key_selected() && form.can_connect()));
    let retry_key = workspace
        .pending_ssh(terminal)
        .unwrap()
        .retry
        .as_ref()
        .unwrap()
        .temporary_key
        .as_ref()
        .unwrap()
        .clone();

    let remote = directory.join("files");
    fs::create_dir(&remote).unwrap();
    let contents = b"private Quick Connect SFTP fixture\n";
    fs::write(remote.join("quick-key-fixture.txt"), contents).unwrap();
    let server = peer.serve_with_extra(
        "host",
        &format!(
            "PermitUserRC no\nForceCommand internal-sftp -d {}\n",
            remote.display()
        ),
    );
    let attempt = submit_quick_key(
        &mut workspace,
        files,
        Protocol::Sftp,
        &endpoint,
        &authorized,
    );
    let (id, response) = dial_result(attempt, files);
    let initialized = workspace.update_ssh(Message::Dialed(files, id, response));
    let listing = next_output(initialized, |message| match message {
        WorkspaceMessage::Sftp(id, sftp_window::Message::Done(ticket, outcome)) => {
            Some((id, ticket, outcome))
        }
        _ => None,
    });
    let (id, ticket, outcome) = listing;
    let directory_listing = outcome.unwrap();
    let worker::Reply::Initial {
        remote: listing, ..
    } = directory_listing.as_ref()
    else {
        panic!("expected a native SFTP initialization result");
    };
    assert_eq!(
        listing.path,
        sftp_window::Location::Remote(remote.as_os_str().as_encoded_bytes().to_vec())
    );
    assert!(listing.entries.iter().any(|entry| {
        entry.display == "quick-key-fixture.txt" && entry.size == Some(contents.len() as u64)
    }));
    let _ = workspace.update_sftp(
        id,
        sftp_window::Message::Done(ticket, Ok(directory_listing)),
    );
    assert!(workspace.sftp[&id].target().saved_id().is_none());
    let _ = workspace.finish_sftp_close(id);
    wait_for_shutdown(&mut workspace);
    assert_fingerprint(server.join().unwrap(), &fingerprint);

    let deadline = Instant::now() + Duration::from_secs(65);
    while retry_key.is_current() {
        assert!(
            Instant::now() < deadline,
            "the one-minute retry key did not expire"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(retry_key.copy_for_attempt().is_err());
    assert!(matches!(&workspace.pending_ssh(terminal).unwrap().stage,
        Stage::Form(form) if form.temporary_key_selected() && !form.can_connect()));

    workspace
        .clipboard
        .buffer
        .set(fs::read_to_string(&authorized).unwrap());
    let paste = workspace.update_ssh(Message::Form(
        terminal,
        ssh_dialog::Message::PasteTemporaryKey,
    ));
    let result = next_output(paste, |message| match message {
        WorkspaceMessage::Ssh(result @ Message::TemporaryKeyPasted(..)) => Some(result),
        _ => None,
    });
    workspace.clipboard.buffer = Default::default();
    let _ = workspace.update_ssh(result);
    let server = peer.serve_with_extra("host", &shell);
    let attempt = workspace.update_ssh(Message::Form(terminal, ssh_dialog::Message::Connect));
    finish_wire_attempt(&mut workspace, terminal, attempt, IsolationLevel::None);
    let tab = workspace.windows[&terminal].tab().unwrap();
    let id = tab.focused_session().unwrap();
    let pane = &workspace.sessions[&id];
    assert!(pane.ssh.as_ref().unwrap().saved_id().is_none());
    wait_for_output(&pane.session, "quick-key-terminal-ready");
    workspace.stop_session(id);
    wait_for_shutdown(&mut workspace);
    assert_fingerprint(server.join().unwrap(), &fingerprint);

    let store = Store::open(crate::profile::connections_path().unwrap()).unwrap();
    assert!(store.connections().is_empty());
    assert!(!crate::profile::vault_path().unwrap().exists());
}

fn submit_quick_key(
    workspace: &mut Workspace,
    owner: window::Id,
    protocol: Protocol,
    endpoint: &LoginTarget,
    key: &Path,
) -> Task<WorkspaceMessage> {
    let _ = workspace.show_quick_connect(owner);
    let id = workspace.quick_connect_for_owner(owner).unwrap();
    let _ = workspace.update_quick_connect(id, Quick::Protocol(protocol));
    for field in [
        LoginMessage::Host(endpoint.host.clone()),
        LoginMessage::Port(endpoint.port.to_string()),
        LoginMessage::Username(endpoint.username.clone()),
        LoginMessage::Authentication(Method::TemporaryKey),
        LoginMessage::TemporaryLifetime("1".into()),
    ] {
        let _ = workspace.update_quick_connect(id, Quick::Ssh(ssh_dialog::Message::Target(field)));
    }
    let _ = workspace.update_quick_connect(id, Quick::SaveConnection(true));
    workspace
        .clipboard
        .buffer
        .set(fs::read_to_string(key).unwrap());
    let paste =
        workspace.update_quick_connect(id, Quick::Ssh(ssh_dialog::Message::PasteTemporaryKey));
    let result = next_output(paste, |message| match message {
        WorkspaceMessage::QuickConnect(window, result @ Quick::TemporaryKeyPasted(..)) => {
            assert_eq!(window, id);
            Some(result)
        }
        _ => None,
    });
    workspace.clipboard.buffer = Default::default();
    let _ = workspace.update_quick_connect(id, result);
    workspace.update_quick_connect(id, Quick::Connect)
}

fn next_output<T>(
    task: Task<WorkspaceMessage>,
    select: impl Fn(WorkspaceMessage) -> Option<T>,
) -> T {
    let mut actions = task::into_stream(task).expect("expected a native task");
    while let Some(action) = block_on(actions.next()) {
        if let Action::Output(message) = action
            && let Some(result) = select(message)
        {
            return result;
        }
    }
    panic!("the native task ended without the expected reply");
}

fn dial_result(task: Task<WorkspaceMessage>, owner: window::Id) -> (u64, DialResponse) {
    next_output(task, |message| match message {
        WorkspaceMessage::Ssh(Message::Dialed(window, id, result)) => {
            assert_eq!(window, owner);
            Some((id, result))
        }
        _ => None,
    })
}

fn wait_for_shutdown(workspace: &mut Workspace) {
    for shutdown in std::mem::take(&mut workspace.pending_shutdown) {
        let _ = shutdown.recv_blocking();
    }
}

fn assert_fingerprint(result: std::process::Output, fingerprint: &str) {
    let log = String::from_utf8_lossy(&result.stderr);
    let accepted = log
        .lines()
        .filter(|line| line.contains("Accepted publickey"))
        .collect::<Vec<_>>();
    assert!(matches!(&accepted[..], [line] if line.contains(fingerprint)));
}
