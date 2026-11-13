use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kortty_server_archive::draft_replace::{ARGUMENT, COMMAND};
use kortty_ssh::{Authentication, Connect, Endpoint, SshSession, WindowSize};

use crate::scheduler_window::script::shell_quote;
use crate::session_security::IsolationLevel;
use crate::ssh_connection::Connected;
use crate::ssh_worker::{OpenSshFixture, dial_test_executable, test_executable};
use crate::workspace::SshTarget;

use super::super::worker::{Connection, Failure, Operation, Reply, Worker, draft_file, sync};
use super::{Location, RemoteEditorPolicy};

const ORIGINAL: &[u8] = b"name=caf\xe9\r\nmode=original\r\n";
const DRAFT: &str = "name=café\r\nmode=draft\r\n";

#[test]
#[ignore = "requires native OpenSSH, SFTP_SERVER, KORTTY_SSH_TEST_EXE and KORTTY_TEST_DRAFT_HELPER"]
fn openssh_ai_right_server_draft_replaces_exact_bytes_over_direct_and_process_routes() {
    for isolation in [IsolationLevel::None, IsolationLevel::Process] {
        exercise(isolation);
    }
}

fn exercise(isolation: IsolationLevel) {
    let fixture = OpenSshFixture::new();
    let root = fixture.directory();
    let directory = root.join("owner-files");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let remote = directory.join("reviewed.conf");
    let backup = directory.join("reviewed.conf.bak");
    let left = directory.join("left.conf");
    fs::write(&left, b"mode=left\n").unwrap();
    write_original(&remote, ORIGINAL);
    let initial = fs::metadata(&remote).unwrap();
    let invocations = root.join("helper-invocations");
    let force = forced_command(root, &invocations);
    let daemon = fixture.serve_with_extra("host", &force);
    let trust = fixture.pinned_trust();
    let mut request = fixture.request(root.join("client"));
    request.isolation = isolation;
    let target =
        SshTarget::from_imported_route(request.target.endpoint.clone(), None, isolation).unwrap();
    let ssh = if isolation == IsolationLevel::Process {
        Connected::Worker(Box::new(
            dial_test_executable(request, true, &test_executable(), &trust).unwrap(),
        ))
    } else {
        let login = request.target.endpoint;
        Connected::Direct(Box::new(
            SshSession::connect_subsystem(
                Connect {
                    address: SocketAddr::from(([127, 0, 0, 1], login.port)),
                    endpoint: Endpoint::new(&login.host, login.port).unwrap(),
                    username: login.username,
                    authentication: Authentication::private_key(
                        fs::read(root.join("client")).unwrap(),
                    ),
                    term: "xterm".into(),
                    size: WindowSize {
                        columns: 80,
                        rows: 24,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                },
                trust,
                &[],
                "sftp",
            )
            .unwrap(),
        ))
    };
    let worker = Worker::start(Connection::Standalone(ssh), RemoteEditorPolicy::Allow).unwrap();
    assert!(worker.verified_host_key().is_some());
    let pair = [
        Location::Local(left),
        Location::Remote(remote.as_os_str().as_encoded_bytes().to_vec()),
    ];
    let compared = submit(&worker, Operation::CompareFiles(pair.clone())).unwrap();
    let Reply::Compared(compared) = compared.as_ref() else {
        panic!("expected the exact pair");
    };
    let origin = compared.remote_origins[1]
        .as_ref()
        .expect("eligible right file");
    assert_eq!(
        origin.original().digest,
        draft_file::digest(ORIGINAL).unwrap()
    );
    assert_eq!(origin.original().mode, 0o640);

    let permitted = Arc::new(AtomicBool::new(true));
    let draft: Arc<str> = DRAFT.into();
    let review = || {
        let permitted = Arc::clone(&permitted);
        let request = draft_file::Request::replacing(
            origin,
            true,
            Arc::clone(&draft),
            &target,
            &worker,
            Arc::new(move || {
                permitted
                    .load(Ordering::Acquire)
                    .then_some(())
                    .ok_or_else(|| "Fixture approval revoked".into())
            }),
        )
        .unwrap();
        let result = submit(
            &worker,
            Operation::ReviewDraftFile(Box::new(request), sync::Cancel::default()),
        )
        .unwrap();
        let Reply::DraftFileReviewed(plan) = result.as_ref() else {
            panic!("expected a helper review");
        };
        assert!(plan.matches_replacement(origin, true, &draft));
        assert!(!plan.matches_replacement(origin, false, &draft));
        assert_eq!(fs::read(&remote).unwrap(), ORIGINAL);
        assert!(!backup.exists());
        Arc::clone(plan)
    };
    let publish = |plan| {
        submit(
            &worker,
            Operation::PublishDraftFile(plan, sync::Cancel::default()),
        )
    };

    let revoked = review();
    permitted.store(false, Ordering::Release);
    assert!(
        publish(revoked)
            .unwrap_err()
            .message
            .contains("Fixture approval revoked")
    );
    permitted.store(true, Ordering::Release);
    let denied = review();
    worker.set_transfer_allowed(false);
    assert!(publish(denied).is_err());
    worker.set_transfer_allowed(true);
    assert_eq!(fs::read_to_string(&invocations).unwrap().lines().count(), 2);
    assert_eq!(fs::read(&remote).unwrap(), ORIGINAL);
    assert!(!backup.exists());

    let stale = review();
    let substitute = directory.join("substitute");
    write_original(&substitute, ORIGINAL);
    fs::rename(&substitute, &remote).unwrap();
    assert_ne!(fs::metadata(&remote).unwrap().ino(), initial.ino());
    let error = publish(stale).unwrap_err();
    assert!(error.message.contains("changed"), "{error:?}");
    assert_eq!(fs::read(&remote).unwrap(), ORIGINAL);
    assert!(!backup.exists());

    let plan = review();
    let published = publish(Arc::clone(&plan)).unwrap();
    let Reply::DraftFilePublished(_, Some(saved)) = published.as_ref() else {
        panic!("expected a helper receipt");
    };
    assert_eq!(saved.path, origin.path());
    assert_eq!(saved.digest, draft_file::digest(DRAFT.as_bytes()).unwrap());
    assert!(saved.note.contains("Backup"));
    assert_eq!(fs::read(&remote).unwrap(), DRAFT.as_bytes());
    assert_eq!(fs::read(&backup).unwrap(), ORIGINAL);
    for path in [&remote, &backup] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o640);
        assert_eq!(
            (metadata.uid(), metadata.gid()),
            (initial.uid(), initial.gid())
        );
    }
    let reopened = submit(&worker, Operation::CompareFiles(pair)).unwrap();
    let Reply::Compared(reopened) = reopened.as_ref() else {
        panic!("expected the reloaded pair");
    };
    assert_eq!(
        reopened.remote_origins[1]
            .as_ref()
            .unwrap()
            .original()
            .digest,
        saved.digest
    );
    assert!(publish(plan).unwrap_err().message.contains("already used"));
    assert_eq!(fs::read_to_string(&invocations).unwrap().lines().count(), 6);
    assert_eq!(fs::read_dir(directory).unwrap().count(), 3);

    assert!(worker.stop().recv_blocking().is_err());
    let log = daemon.join().unwrap();
    let log = String::from_utf8_lossy(&log.stderr);
    assert_eq!(log.matches("Accepted publickey for").count(), 1, "{log}");
}

fn submit(worker: &Worker, operation: Operation) -> Result<Arc<Reply>, Failure> {
    worker.submit(operation).unwrap().recv_blocking().unwrap()
}

fn write_original(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
}

fn required_path(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}")));
    assert!(path.is_absolute() && path.is_file(), "invalid {name}");
    path
}

fn forced_command(root: &Path, invocations: &Path) -> String {
    let quote = |value: &str| {
        let mut quoted = String::new();
        shell_quote(&mut quoted, value);
        quoted
    };
    let helper = required_path("KORTTY_TEST_DRAFT_HELPER");
    let sftp = required_path("SFTP_SERVER");
    let script = root.join("dispatch.sh");
    fs::write(&script, format!(
        "set -eu\ncase \"${{SSH_ORIGINAL_COMMAND:-}}\" in\n  internal-sftp) exec {} ;;\n  {}) printf '%s\\n' replacement >> {}; exec {} {} ;;\n  *) exit 126 ;;\nesac\n",
        quote(sftp.to_str().unwrap()), quote(COMMAND), quote(invocations.to_str().unwrap()),
        quote(helper.to_str().unwrap()), quote(ARGUMENT),
    )).unwrap();
    format!("ForceCommand /bin/sh {}\n", quote(script.to_str().unwrap()))
}
