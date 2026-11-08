use std::fs;
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kortty_ssh::{Authentication, Connect, Endpoint, HostKey, SshSession, TrustStore};
use kortty_terminal::TerminalProfile;

use crate::session::{Session, Status, ssh_size};
use crate::session_security::IsolationLevel;
use crate::sftp_window::Message as SftpMessage;
use crate::ssh_connection::Connected;
use crate::ssh_dialog::LoginTarget;
use crate::ssh_test_fixture::TempDirectory;
use crate::workspace::{Message as WorkspaceMessage, SshTarget, Workspace, chrome, initial_grid};

use super::*;

const TIMEOUT: Duration = Duration::from_secs(8);

fn wait_for<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = attempt() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "OpenSSH sidebar fixture timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn complete(browser: &mut Browser) -> Task<Completion> {
    let busy = browser
        .busy
        .as_ref()
        .expect("expected an actual worker request");
    let result = wait_for(|| busy.reply.try_recv().ok());
    browser.finish(Completion {
        request: busy.request,
        result,
    })
}

fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

// Test-only OpenSSH server on a private loopback listener with an explicit host pin.
fn connect(directory: &Path) -> (Session, SshTarget, JoinHandle<Output>) {
    for name in ["host", "client"] {
        let result = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(directory.join(name))
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }
    let user = Command::new("id").arg("-un").output().unwrap();
    assert!(user.status.success(), "{user:?}");
    let user = String::from_utf8(user.stdout).unwrap().trim().to_owned();
    let config = directory.join("sshd.conf");
    fs::write(
        &config,
        format!(
            "HostKey \"{}\"\nAuthorizedKeysFile \"{}\"\nAllowUsers {user}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
             UsePAM no\nStrictModes yes\nPermitTTY yes\nSubsystem sftp internal-sftp\n",
            directory.join("host").display(),
            directory.join("client.pub").display(),
        ),
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let target = SshTarget::from_imported_route(
        LoginTarget {
            host: address.ip().to_string(),
            port: address.port(),
            username: user.clone(),
        },
        None,
        IsolationLevel::None,
    )
    .unwrap();
    let host = fs::read_to_string(directory.join("host.pub")).unwrap();
    let pin = HostKey::from_openssh(host.trim()).unwrap();
    let trust = TrustStore::from_pins(vec![(endpoint.clone(), pin)]).unwrap();
    let server = thread::spawn(move || {
        let input: OwnedFd = listener.accept().unwrap().0.into();
        let output = input.try_clone().unwrap();
        Command::new("/usr/sbin/sshd")
            .args(["-ie", "-f"])
            .arg(config)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    });
    let profile = TerminalProfile::Xterm;
    let grid = initial_grid(profile);
    let connection = SshSession::connect(
        Connect {
            address,
            endpoint,
            username: user,
            authentication: Authentication::private_key(
                fs::read(directory.join("client")).unwrap(),
            ),
            term: profile.term_name().into(),
            size: ssh_size(grid),
        },
        trust,
    )
    .unwrap();
    let session = Session::ssh(
        Some(Connected::Direct(Box::new(connection))),
        grid,
        profile,
        true,
        None,
    )
    .unwrap();
    wait_for(|| (session.frame().status == Status::Running).then_some(()));
    (session, target, server)
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen, sshd with internal-sftp, and a usable local account"]
fn openssh_remote_dock_coalesces_listing_transfers_files_and_only_closes_its_channel() {
    let fixture = TempDirectory::new("remote-dock");
    let root = fixture.0.canonicalize().unwrap();
    let first = root.join("first");
    let latest = root.join("latest");
    let download = root.join("download");
    for folder in [&first, &latest, &download] {
        fs::create_dir(folder).unwrap();
    }
    let raw = b"raw-\n".to_vec();
    let raw_name = std::ffi::OsString::from_vec(raw.clone());
    fs::write(latest.join(&raw_name), b"remote payload").unwrap();
    fs::write(latest.join("upload.txt"), b"existing").unwrap();
    let upload = root.join("upload.txt");
    fs::write(&upload, b"local payload").unwrap();
    let link = root.join("link");
    symlink(&upload, &link).unwrap();
    let (session, target, server) = connect(&root);

    let (mut browser, _) = Browser::open(
        SessionId(7),
        session.sftp_opener().unwrap(),
        Some(path_bytes(&first)),
        RemoteEditorPolicy::Allow,
    )
    .unwrap();
    assert_eq!(browser.session(), SessionId(7));
    let _ = browser.browse(path_bytes(&latest));
    let _ = complete(&mut browser);
    assert_eq!(browser.path(), None, "the stale listing was presented");
    assert_eq!(browser.listing_revision(), 0);
    let _ = complete(&mut browser);
    assert_eq!(browser.listing_revision(), 1);
    assert_eq!(browser.path(), Some(path_bytes(&latest).as_slice()));
    let source = latest.join("terminal-source.txt");
    fs::write(&source, "original file\n").unwrap();
    let worker = browser.terminal_editor_worker().unwrap();
    let editor = worker.editor().unwrap();
    let reply = worker
        .submit(Operation::OpenTerminalEditor(path_bytes(
            &latest.join("../latest/terminal-source.txt"),
        )))
        .unwrap();
    let reply = wait_for(|| reply.try_recv().ok()).unwrap();
    let Reply::EditorOpened(document) = reply.as_ref() else {
        panic!("unexpected SFTP response");
    };
    assert_eq!(document.path(), path_bytes(&source));
    assert_eq!(document.text().as_ref(), "original file\n");
    let saved = iced::futures::executor::block_on(
        editor.save(document.clone(), Arc::from("saved from terminal\n")),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "saved from terminal\n"
    );
    assert!(browser.terminal_editor_worker().is_some());
    assert!(browser.entries().iter().any(|entry| {
        entry.name == EntryName::Remote(raw.clone())
            && entry.display == "raw-\\n"
            && entry.size == Some(14)
            && entry.modified.is_some()
    }));

    let _ = browser.browse(path_bytes(&root.join("missing")));
    let _ = complete(&mut browser);
    assert!(browser.status().is_some());
    assert!(browser.connected());
    assert_eq!(browser.listing_revision(), 1);
    assert_eq!(browser.path(), Some(path_bytes(&latest).as_slice()));

    let _ = browser.upload(vec![link]);
    let _ = complete(&mut browser);
    assert!(browser.status().unwrap().contains("ordinary files"));
    assert!(!latest.join("link").exists());

    let blocked = root.join("blocked.txt");
    fs::write(&blocked, b"must remain local").unwrap();
    let _ = browser.upload(vec![upload.clone(), blocked]);
    assert_eq!(browser.active_transfer_count(), 2);
    let _ = browser.browse(path_bytes(&latest));
    browser.cancel_pending_transfers();
    let _ = complete(&mut browser);
    assert!(browser.conflict().is_none());
    assert_eq!(browser.active_transfer_count(), 0);
    let _ = complete(&mut browser);
    assert_eq!(browser.listing_revision(), 2);
    assert!(!latest.join("blocked.txt").exists());

    let _ = browser.upload(vec![upload]);
    assert!(browser.transfers_active());
    let _ = complete(&mut browser);
    assert!(browser.conflict().is_some());
    let _ = browser.resolve(ConflictChoice::KeepBoth);
    let _ = complete(&mut browser);
    assert!(!browser.transfers_active());
    assert_eq!(fs::read(latest.join("upload.txt")).unwrap(), b"existing");
    assert_eq!(
        fs::read(latest.join("upload (1).txt")).unwrap(),
        b"local payload"
    );

    let _ = browser.download(vec![EntryName::Remote(raw)], download.clone());
    let _ = complete(&mut browser);
    assert_eq!(
        fs::read(download.join(raw_name)).unwrap(),
        b"remote payload"
    );

    let worker = browser.take_worker().unwrap();
    assert!(!editor.is_closed());
    let _ = browser.resume_with_worker(worker);
    let _ = complete(&mut browser);
    iced::futures::executor::block_on(editor.save(saved, Arc::from("saved after handoff\n")))
        .unwrap();
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "saved after handoff\n"
    );

    let revision = browser.listing_revision();
    let stopped = browser.release().unwrap();
    wait_for(|| stopped.is_closed().then_some(()));
    assert!(!browser.has_worker());
    assert_eq!(browser.path(), Some(path_bytes(&latest).as_slice()));
    assert_eq!(browser.listing_revision(), revision);
    assert!(
        editor.is_closed(),
        "closing the original channel revokes the old editor"
    );
    let _ = browser
        .resume(session.sftp_opener().unwrap(), RemoteEditorPolicy::Deny)
        .unwrap();
    let _ = complete(&mut browser);
    assert!(browser.has_worker() && browser.connected());
    assert_eq!(browser.path(), Some(path_bytes(&latest).as_slice()));
    assert_eq!(browser.listing_revision(), revision + 1);

    let stopped = browser.release().unwrap();
    wait_for(|| stopped.is_closed().then_some(()));
    check_open_sftp_here(session, target, &latest);
    let _ = server.join().unwrap();
}

fn check_open_sftp_here(session: Session, target: SshTarget, directory: &Path) {
    let mut uri = url::Url::from_file_path(directory).unwrap();
    uri.set_host(Some(target.host())).unwrap();
    let quoted_path = directory.to_str().unwrap().replace('\'', "'\\''");
    let quoted_uri = uri.as_str().replace('\'', "'\\''");
    let change = format!("cd '{quoted_path}' && printf '\\033]7;%s\\007' '{quoted_uri}'\r");
    session.send(change.into_bytes()).unwrap();
    wait_for(|| {
        let frame = session.frame();
        frame
            .shell_activity()
            .directory
            .as_ref()
            .is_some_and(|report| report.path.as_bytes() == directory.as_os_str().as_bytes())
            .then_some(())
    });

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let id = workspace.store_session(session, initial_grid(TerminalProfile::Xterm), 14);
    workspace.sessions.get_mut(&id).unwrap().ssh = Some(target);
    workspace.add_tab(owner, id);
    let open =
        || WorkspaceMessage::Chrome(chrome::Message::Invoke(owner, chrome::Action::SftpHere));
    let _ = workspace.update(open());
    assert_eq!(workspace.sftp.len(), 1);
    let (&manager, sftp) = workspace.sftp.iter().next().unwrap();
    assert_eq!(sftp.attached_to(), Some(id));
    let completion = wait_for(|| workspace.sftp[&manager].poll_worker_reply_for_test());
    let SftpMessage::Done(_, Ok(reply)) = &completion else {
        panic!("SFTP Manager failed to list the tracked directory: {completion:?}");
    };
    let Reply::Initial { remote, .. } = reply.as_ref() else {
        panic!("SFTP Manager returned a different worker operation: {reply:?}");
    };
    assert_eq!(remote.path, Location::Remote(path_bytes(directory)));
    assert!(remote.entries.iter().any(|entry| {
        entry.name == EntryName::Remote(b"raw-\n".to_vec()) && entry.size == Some(14)
    }));
    let _ = workspace.update(WorkspaceMessage::Sftp(manager, completion));
    assert!(!workspace.sftp[&manager].transport_busy());
    let _ = workspace.update(open());
    assert_eq!(workspace.sftp.len(), 1);
    assert!(workspace.sftp.contains_key(&manager));
    let _ = workspace.update(WorkspaceMessage::Sftp(manager, SftpMessage::Close));
    assert!(workspace.sftp.is_empty());
    for done in std::mem::take(&mut workspace.pending_shutdown) {
        wait_for(|| done.is_closed().then_some(()));
    }

    let session = &workspace.sessions[&id].session;
    let probe =
        format!("test \"$PWD\" = '{quoted_path}' && printf '%s%s\\n' 'shell-' 'still-live'\r");
    session.send(probe.into_bytes()).unwrap();
    wait_for(|| {
        session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains("shell-still-live")
            .then_some(())
    });
    let stopped = session.stop();
    wait_for(|| stopped.is_closed().then_some(()));
}
