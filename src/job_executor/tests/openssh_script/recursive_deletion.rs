use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _, symlink};
use std::os::unix::net::UnixListener;

use kortty_ssh::{ConnectOptions, StartupChannel};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session_security::IsolationLevel;
use crate::sftp_window::{Change, EntryName, Message, SftpWindow, Side, worker};
use crate::ssh_connection::Connected;
use crate::workspace::SshTarget;

use super::{Command, Duration, Instant, Sshd, TrustStore, fs, thread};

#[test]
#[ignore = "requires a local OpenSSH daemon and a usable account"]
fn pinned_openssh_recursive_deletion_uses_the_authenticated_sftp_manager_worker() {
    exercise_recursive_deletion(None);
}

#[test]
#[ignore = "requires a local OpenSSH daemon and a usable account"]
fn pinned_openssh_recursive_deletion_uses_the_authenticated_sftp_manager_through_a_jump() {
    let jump = Sshd::new();
    exercise_recursive_deletion(Some(&jump));
}

fn exercise_recursive_deletion(jump: Option<&Sshd>) {
    let fixture = Sshd::new();
    let parent = fixture.profile.0.join("remote");
    let selected = parent.join("selected");
    let child = selected.join("child");
    let input = child.join("file.txt");
    let late = child.join("late.txt");
    let sibling = parent.join("retained.txt");
    let retained_folder = fixture.profile.0.join("retained");
    let retained_child = retained_folder.join("outside.txt");
    let folder_link = child.join("directory-link");
    let broken_link = child.join("broken-link");
    let selected_link = parent.join("selected-link");
    let selected_fifo = parent.join("selected-fifo");
    let socket_path = child.join("socket");
    fs::create_dir_all(&child).unwrap();
    fs::create_dir(&retained_folder).unwrap();
    fs::write(&input, b"selected").unwrap();
    fs::write(&sibling, b"unselected").unwrap();
    fs::write(&retained_child, b"outside the reviewed tree").unwrap();
    symlink("../../../retained", &folder_link).unwrap();
    symlink("missing-a", &broken_link).unwrap();
    symlink("retained.txt", &selected_link).unwrap();
    let make_fifo = || {
        assert!(
            Command::new("mkfifo")
                .args(["-m", "600"])
                .arg(&selected_fifo)
                .status()
                .unwrap()
                .success()
        );
    };
    make_fifo();
    let socket = UnixListener::bind(&socket_path).unwrap();
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).unwrap();

    let daemon = fixture.spawn();
    let jump_daemon = jump.map(Sshd::spawn);
    let (endpoint, fingerprint) = fixture.pin();
    let login = fixture.saved_login(None).endpoint;
    let target = SshTarget::from_imported_route(
        login,
        jump.map(|jump| jump.saved_login(None).endpoint),
        IsolationLevel::None,
    )
    .unwrap();
    let trust = if let Some(jump) = jump {
        TrustStore::from_pins(vec![(endpoint.clone(), fingerprint.clone()), jump.pin()]).unwrap()
    } else {
        fixture.trust.clone()
    };
    let login = |server: &Sshd, endpoint| kortty_ssh::SshTarget {
        endpoint,
        username: server.user.clone(),
        authentication: crate::ssh_connection::private_key(&server.user_key).unwrap(),
    };
    let ssh = match crate::ssh_connection::dial(
        (
            login(&fixture, endpoint),
            jump.map(|jump| login(jump, jump.pin().0)),
        ),
        trust,
        StartupChannel::Subsystem("sftp"),
        ConnectOptions::QUICK,
        None,
        || false,
    ) {
        Ok(ssh) => ssh,
        Err(error) => {
            let jump_log = jump_daemon.map(daemon_log).unwrap_or_default();
            let target_log = daemon_log(daemon);
            panic!("SFTP connection failed: {error:?}\n{jump_log}\n{target_log}");
        }
    };
    assert_eq!(ssh.verified_host_key(), Some(&fingerprint));
    let (mut window, _) = SftpWindow::open_project(
        target,
        Connected::Direct(Box::new(ssh)),
        RemoteEditorPolicy::Deny,
        Some(fixture.profile.0.clone()),
        Some(parent.to_str().unwrap().to_owned()),
    )
    .unwrap();
    assert!(matches!(
        finish(&mut window).unwrap().as_ref(),
        worker::Reply::Initial { .. }
    ));

    let review = |window: &mut SftpWindow| {
        let _ = window.update(Message::Select(
            Side::Remote,
            EntryName::Remote(b"selected".to_vec()),
        ));
        let _ = window.update(Message::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"selected-link".to_vec()),
            true,
        ));
        let _ = window.update(Message::ToggleSelection(
            Side::Remote,
            EntryName::Remote(b"selected-fifo".to_vec()),
            true,
        ));
        let _ = window.update(Message::Change(Side::Remote, Change::Delete));
        let _ = window.update(Message::ReviewDeletion);
        assert!(matches!(
            finish(window).unwrap().as_ref(),
            worker::Reply::RecursiveDeletionReviewed(_)
        ));
    };
    review(&mut window);
    fs::write(&late, b"appeared after confirmation preview").unwrap();
    let _ = window.update(Message::ConfirmChange);
    let stale = finish(&mut window).unwrap_err();
    assert!(stale.message.contains("selected tree changed"), "{stale:?}");
    assert_eq!(fs::read(&input).unwrap(), b"selected");
    assert!(late.is_file());
    review(&mut window);
    fs::remove_file(&broken_link).unwrap();
    symlink("missing-b", &broken_link).unwrap();
    let _ = window.update(Message::ConfirmChange);
    let stale = finish(&mut window).unwrap_err();
    assert!(stale.message.contains("selected tree changed"), "{stale:?}");
    assert_eq!(
        fs::read_link(&broken_link).unwrap().to_str(),
        Some("missing-b")
    );
    review(&mut window);
    fs::remove_file(&selected_fifo).unwrap();
    fs::write(&selected_fifo, b"replacement").unwrap();
    let _ = window.update(Message::ConfirmChange);
    let stale = finish(&mut window).unwrap_err();
    assert!(stale.message.contains("selected tree changed"), "{stale:?}");
    assert_eq!(fs::read(&selected_fifo).unwrap(), b"replacement");
    assert!(
        fs::symlink_metadata(&socket_path)
            .unwrap()
            .file_type()
            .is_socket()
    );
    fs::remove_file(&selected_fifo).unwrap();
    make_fifo();
    review(&mut window);
    let _ = window.update(Message::ConfirmChange);
    let result = finish(&mut window).unwrap();
    assert!(
        matches!(result.as_ref(), worker::Reply::Changed { directory: Some(listing), note }
        if note == "Deleted 9 of 9 reviewed entries"
            && listing.entries.len() == 1
            && listing.entries[0].display == "retained.txt"),
        "{result:?}"
    );
    assert!(!selected.exists());
    assert!(!selected_fifo.exists());
    drop(socket);
    assert_eq!(fs::read(&sibling).unwrap(), b"unselected");
    assert_eq!(
        fs::read(&retained_child).unwrap(),
        b"outside the reviewed tree"
    );
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);

    for stopped in window.stop() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stopped.is_closed() {
            assert!(
                Instant::now() < deadline,
                "the SFTP worker is still running"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    let log = daemon_log(daemon);
    assert!(
        log.contains("Accepted publickey") && log.contains("subsystem"),
        "{log}"
    );
    if let Some(jump_daemon) = jump_daemon {
        let log = daemon_log(jump_daemon);
        assert!(
            log.contains("Accepted publickey") && log.contains("direct-tcpip"),
            "{log}"
        );
    }
}

fn daemon_log(daemon: thread::JoinHandle<std::process::Output>) -> String {
    daemon.join().map_or_else(
        |_| "No SSH daemon output was collected".to_owned(),
        |output| String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn finish(window: &mut SftpWindow) -> Result<std::sync::Arc<worker::Reply>, worker::Failure> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(Message::Done(ticket, result)) = window.poll_worker_reply_for_test() {
            let _ = window.update(Message::Done(ticket, result.clone()));
            return result;
        }
        assert!(Instant::now() < deadline, "the SFTP worker did not reply");
        thread::sleep(Duration::from_millis(10));
    }
}
