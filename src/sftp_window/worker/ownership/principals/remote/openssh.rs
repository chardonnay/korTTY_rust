use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::process::Command;
use std::sync::Arc;

use kortty_ssh::{Endpoint, SshTarget as Peer, StartupChannel};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session_security::IsolationLevel;
use crate::sftp_window::worker::{Connection, Operation, Reply, Worker, ownership, sync};
use crate::sftp_window::{Entry, EntryName};
use crate::ssh_connection::{self, Connected};
use crate::ssh_dialog::Request;
use crate::ssh_worker::{OpenSshFixture, dial_test_executable, test_executable};
use crate::workspace::SshTarget;

use super::*;

const WHEEL: u32 = 10;

#[test]
#[ignore = "requires Fedora OpenSSH/getent, wheel membership, and KORTTY_SSH_TEST_EXE"]
fn openssh_named_ownership_resolves_and_applies_on_direct_and_isolated_routes() {
    assert!(
        identity("-G")
            .split_ascii_whitespace()
            .any(|group| group == "10")
    );
    let uid: u32 = identity("-u").parse().unwrap();
    for isolation in [IsolationLevel::None, IsolationLevel::Process] {
        let fixture = OpenSshFixture::new();
        let folder = fixture.directory().canonicalize().unwrap();
        let key = folder.join("client");
        let payload = folder.join("payload");
        fs::write(&payload, b"reviewed name-to-ID fixture").unwrap();
        let original = fs::metadata(&payload).unwrap();
        assert_ne!(
            original.gid(),
            WHEEL,
            "fixture must begin in a group other than wheel"
        );
        let mut request = fixture.request(key.clone());
        request.isolation = isolation;
        let name = request.target.endpoint.username.clone();
        let route =
            SshTarget::from_imported_route(request.target.endpoint.clone(), None, isolation)
                .unwrap();
        let server = fixture.serve_with_extra("host", "");
        let connection = connect(&fixture, request, &key);
        let opener = connection.exec_opener().expect("verified SSH exec opener");
        assert_eq!(
            matches!(&opener, ExecOpener::Worker(_)),
            isolation == IsolationLevel::Process
        );
        if let ExecOpener::Worker(worker) = &opener {
            assert!(
                worker.open_exec("true", || false).is_err(),
                "standalone SFTP must refuse general exec"
            );
        }
        assert_eq!(
            resolve(&opener, Kind::User, &name, false, &|| Ok(()))
                .unwrap_or_else(|error| panic!("{isolation:?} user lookup: {error:?}")),
            uid
        );
        assert_eq!(
            resolve(&opener, Kind::Group, "wheel", false, &|| Ok(()))
                .unwrap_or_else(|error| panic!("{isolation:?} group lookup: {error:?}")),
            WHEEL
        );
        let unknown = format!(
            "kortty-no-account-{}",
            crate::private_files::nonce().unwrap()
        );
        let missing = resolve(&opener, Kind::User, &unknown, false, &|| Ok(())).unwrap_err();
        assert!(missing.message.contains("was not found"));
        drop(opener);

        let worker =
            Worker::start(Connection::Standalone(connection), RemoteEditorPolicy::Deny).unwrap();
        let directory = folder.as_os_str().as_bytes().to_vec();
        let selected = initial_selection(&worker, &folder, &directory);
        let principals = ownership::principals::Selection::parse(&name, "wheel").unwrap();
        let review = ownership::recursive::Request::new(
            directory,
            vec![selected],
            principals,
            false,
            route.clone(),
            &worker,
        )
        .unwrap();
        let reply = receive(
            &worker,
            Operation::ReviewRemoteOwnership(Box::new(review), sync::Cancel::default()),
        );
        let Reply::OwnershipReview(review) = reply.as_ref() else {
            panic!("expected named ownership review")
        };
        assert_eq!(review.uids(), (Some(uid), Some(WHEEL)));
        assert_eq!(
            review.principal_labels(),
            (format!("{name} → {uid}"), "wheel → 10".to_owned())
        );
        assert_eq!(fs::metadata(&payload).unwrap().gid(), original.gid());
        assert!(
            matches!(receive(&worker, review.apply(&worker, &route).unwrap()).as_ref(),
            Reply::Changed { note, directory: Some(_), .. } if note.contains("1 changed, 0 already matched"))
        );
        assert!(review.apply(&worker, &route).is_none());
        let applied = fs::metadata(&payload).unwrap();
        assert_eq!((applied.uid(), applied.gid()), (uid, WHEEL));
        finish(worker, server);
    }
}

#[test]
#[ignore = "requires Fedora OpenSSH/getent and wheel membership"]
fn openssh_sftp_only_keeps_numeric_ownership_when_name_lookup_is_refused() {
    let fixture = OpenSshFixture::new();
    let folder = fixture.directory().canonicalize().unwrap();
    let key = folder.join("client");
    let payload = folder.join("payload");
    fs::write(&payload, b"SFTP-only ownership fixture").unwrap();
    assert_ne!(fs::metadata(&payload).unwrap().gid(), WHEEL);
    let mut request = fixture.request(key.clone());
    request.isolation = IsolationLevel::None;
    let route =
        SshTarget::from_imported_route(request.target.endpoint.clone(), None, request.isolation)
            .unwrap();
    let server = fixture.serve_with_extra("host", "ForceCommand internal-sftp\n");
    let connection = connect(&fixture, request, &key);
    let opener = connection.exec_opener().unwrap();
    let denied = resolve(&opener, Kind::Group, "wheel", false, &|| Ok(())).unwrap_err();
    assert!(denied.message.contains("Enter a numeric UID or GID"));
    drop(opener);

    let worker =
        Worker::start(Connection::Standalone(connection), RemoteEditorPolicy::Deny).unwrap();
    let directory = folder.as_os_str().as_bytes().to_vec();
    let selected = initial_selection(&worker, &folder, &directory);
    assert!(matches!(receive(&worker, Operation::ChangeOwnership {
        directory,
        selected: vec![selected],
        uid: None,
        gid: Some(WHEEL),
        route,
        permitted: sync::Cancel::default(),
    }).as_ref(), Reply::Changed { note, directory: Some(_), .. } if note.contains("1 changed")));
    assert_eq!(fs::metadata(&payload).unwrap().gid(), WHEEL);
    finish(worker, server);
}

fn finish(worker: Worker, server: thread::JoinHandle<std::process::Output>) {
    assert!(worker.stop().recv_blocking().is_err());
    let output = server.join().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("Accepted publickey")
            .count(),
        1
    );
}

fn connect(fixture: &OpenSshFixture, request: Request, key: &std::path::Path) -> Connected {
    let trust = fixture.pinned_trust();
    if request.isolation == IsolationLevel::Process {
        return Connected::Worker(Box::new(
            dial_test_executable(request, true, &test_executable(), &trust).unwrap(),
        ));
    }
    let login = request.target.endpoint;
    let target = Peer {
        endpoint: Endpoint::new(&login.host, login.port).unwrap(),
        username: login.username,
        authentication: ssh_connection::private_key(key).unwrap(),
    };
    Connected::Direct(Box::new(
        ssh_connection::dial(
            (target, None),
            trust,
            StartupChannel::Subsystem("sftp"),
            request.options,
            None,
            || false,
        )
        .unwrap(),
    ))
}

fn initial_selection(worker: &Worker, folder: &std::path::Path, remote: &[u8]) -> Entry {
    let reply = receive(
        worker,
        Operation::Initial {
            local: Some(folder.to_owned()),
            remote: Some(remote.to_vec()),
        },
    );
    let Reply::Initial { remote, .. } = reply.as_ref() else {
        panic!("expected live SFTP listing")
    };
    remote
        .entries
        .iter()
        .find(|entry| matches!(&entry.name, EntryName::Remote(name) if name == b"payload"))
        .unwrap()
        .clone()
}

fn receive(worker: &Worker, operation: Operation) -> Arc<Reply> {
    worker
        .submit(operation)
        .unwrap()
        .recv_blocking()
        .unwrap()
        .unwrap()
}

fn identity(option: &str) -> String {
    let output = Command::new("/usr/bin/id").arg(option).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}
