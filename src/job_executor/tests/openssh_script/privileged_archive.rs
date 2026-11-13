use std::cell::Cell;
use std::fs;
use std::io::{self, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use kortty_server_archive::manifest::{Manifest, Member, MemberKind};
use kortty_server_archive::protocol::{
    self, CreateFormat, CreateReview, CreateSpec, PrivilegedReply,
};
use kortty_ssh::{Authentication, ConnectOptions, ExecConnection, SshTarget};

use crate::sftp_window::{PrivilegedArchiveFailure, exchange_privileged_server_archive};

use super::Sshd;

const COMMAND: &str = "exec /usr/bin/sudo -k -n -u root -- /usr/libexec/kortty/kortty-server-archive --privileged-create-v1";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HELPER: &str = include_str!("privileged_archive_fixture.py");

#[test]
#[ignore = "requires native OpenSSH sshd, ssh-keygen and Python 3"]
fn openssh_privileged_archive_exchange_commits_once_and_reports_uncertainty() {
    for (mode, expected) in [
        ("applied", Ok(())),
        ("revoked", Err(PrivilegedArchiveFailure::BeforeCommit)),
        ("lost", Err(PrivilegedArchiveFailure::Uncertain)),
    ] {
        let fixture = Sshd::new();
        let root = &fixture.profile.0;
        let script = root.join("archive-fixture.py");
        fs::write(&script, HELPER).unwrap();
        fs::write(root.join("mode"), mode).unwrap();
        fs::write(root.join("command"), COMMAND).unwrap();

        let spec = CreateSpec {
            sources: vec!["/fixture/pinned/data".into()],
            destination: "/fixture/exports/job.tar".into(),
            format: CreateFormat::Tar,
            level: 6,
            excludes: Vec::new(),
        };
        let mut review = Vec::new();
        protocol::write_privileged_reply(
            &mut review,
            &PrivilegedReply::Review {
                review: CreateReview {
                    spec: spec.clone(),
                    token: TOKEN.into(),
                    manifest: Manifest {
                        members: vec![Member {
                            path: "data".into(),
                            kind: MemberKind::File,
                            size: 1,
                        }],
                        expanded_bytes: 1,
                    },
                    excluded: 0,
                },
            },
        )
        .unwrap();
        fs::write(root.join("review"), review).unwrap();
        let mut expected_review = Vec::new();
        protocol::write_privileged_request(
            &mut expected_review,
            &protocol::PrivilegedRequest::Review { spec: spec.clone() },
        )
        .unwrap();
        fs::write(root.join("expected-review"), expected_review).unwrap();
        let mut expected_commit = Vec::new();
        protocol::write_privileged_request(
            &mut expected_commit,
            &protocol::PrivilegedRequest::Commit {
                token: TOKEN.into(),
            },
        )
        .unwrap();
        fs::write(root.join("expected-commit"), expected_commit).unwrap();
        let mut applied = Vec::new();
        protocol::write_privileged_reply(
            &mut applied,
            &PrivilegedReply::Applied {
                destination: spec.destination.clone(),
            },
        )
        .unwrap();
        fs::write(root.join("applied"), applied).unwrap();
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(&fixture.config)
            .unwrap();
        let script = script.to_str().unwrap();
        let mut command = String::new();
        crate::scheduler_window::script::shell_quote(&mut command, script);
        writeln!(config, "ForceCommand /usr/bin/python3 -I -S {command}").unwrap();

        let daemon = fixture.spawn();
        let address = fixture.listener.local_addr().unwrap();
        let (endpoint, pin) = fixture.pin();
        let target = SshTarget {
            endpoint,
            username: fixture.user.clone(),
            authentication: Authentication::private_key(fs::read(&fixture.user_key).unwrap()),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let stopped = move || Instant::now() >= deadline;
        let connection = ExecConnection::connect_preconnected(
            TcpStream::connect(address).unwrap(),
            (target, Vec::new()),
            fixture.trust.clone(),
            ConnectOptions::default(),
            stopped,
            || Ok(()),
        )
        .unwrap();
        let opener = connection.exec_opener().unwrap();
        assert_eq!(opener.verified_host_key(), Some(&pin));
        let denied = Cell::new(false);
        let result = exchange_privileged_server_archive(&opener, &spec, stopped, || {
            if mode == "revoked" && root.join("review-seen").exists() {
                denied.set(true);
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
        drop(opener);
        drop(connection);
        let output = daemon.join().unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        assert_eq!(result, expected, "{mode}: {log}");
        assert!(root.join("review-seen").is_file(), "{mode}: {log}");
        assert_eq!(denied.get(), mode == "revoked");
        assert_eq!(
            root.join("commit-and-eof-seen").is_file(),
            mode != "revoked",
            "{mode}: {log}"
        );
        assert!(!root.join("unexpected-data").exists(), "{mode}: {log}");
    }
}
