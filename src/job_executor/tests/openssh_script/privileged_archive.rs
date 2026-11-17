use std::fs;
use std::io::{self, Write as _};
use std::net::TcpStream;
use std::process::Output;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kortty_server_archive::manifest::{Manifest, Member, MemberKind};
use kortty_server_archive::protocol::{
    self, CreateFormat, CreateReview, CreateSpec, CreatedPayload, PrivilegedReply, UpdateDecision,
    UpdateReply, UpdateRequest,
};
use kortty_ssh::{
    Authentication, ConnectOptions, ExecConnection, HostKey, SensitiveLine, SshTarget,
};
use sha2::{Digest, Sha256};

use crate::sftp_window::{
    PrivilegedArchiveFailure, exchange_privileged_archive_update,
    exchange_privileged_server_archive,
};

use super::Sshd;

const COMMAND: &str = "exec /usr/bin/sudo -k -n -u root -- /usr/libexec/kortty/kortty-server-archive --privileged-create-v1";
const UPDATE_COMMAND: &str = "exec /usr/bin/sudo -k -n -u root -- /usr/libexec/kortty/kortty-server-archive --privileged-update-v1";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const RECOVERY: &str =
    "/fixture/exports/.kortty-0123456789abcdef0123456789abcdef.kortty-part/original";
const HELPER: &str = include_str!("privileged_archive_fixture.py");
const PASSWORD: &str = "test-only archive password\n東京";
const SUDO_PASSWORD: &str = "test-only elevated password 東京";
const COPY: &[u8] = b"new root-owned archive\0bytes\n";

fn archive_fixture(mode: &str, command: &str) -> Sshd {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let script = root.join("archive-fixture.py");
    fs::write(&script, HELPER).unwrap();
    fs::write(root.join("mode"), mode).unwrap();
    fs::write(root.join("command"), command).unwrap();
    if mode.starts_with("sudo-") {
        fs::write(
            root.join("expected-sudo-password"),
            format!("{SUDO_PASSWORD}\n"),
        )
        .unwrap();
    }
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(&fixture.config)
        .unwrap();
    let mut quoted = String::new();
    crate::scheduler_window::script::shell_quote(&mut quoted, script.to_str().unwrap());
    writeln!(config, "ForceCommand /usr/bin/python3 -I -S {quoted}").unwrap();
    fixture
}

fn connect_fixture(
    fixture: &Sshd,
    mode: &str,
    stopped: impl Fn() -> bool,
) -> (ExecConnection, HostKey, JoinHandle<Output>) {
    let daemon = fixture.spawn();
    let address = fixture.listener.local_addr().unwrap();
    let (endpoint, pin) = fixture.pin();
    let target = SshTarget {
        endpoint,
        username: fixture.user.clone(),
        authentication: Authentication::private_key(fs::read(&fixture.user_key).unwrap()),
    };
    match ExecConnection::connect_preconnected(
        TcpStream::connect(address).unwrap(),
        (target, Vec::new()),
        fixture.trust.clone(),
        ConnectOptions::default(),
        stopped,
        || Ok(()),
    ) {
        Ok(connection) => (connection, pin, daemon),
        Err(error) => {
            let output = daemon.join().unwrap();
            panic!(
                "The archive fixture could not authenticate ({mode}): {error:?}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn review(spec: &CreateSpec) -> CreateReview {
    CreateReview {
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
    }
}

#[test]
#[ignore = "requires native OpenSSH sshd, ssh-keygen and Python 3"]
fn openssh_privileged_archive_exchange_commits_once_and_reports_uncertainty() {
    for (mode, expected) in [
        ("applied", Ok(())),
        ("revoked", Err(PrivilegedArchiveFailure::BeforeCommit)),
        ("lost", Err(PrivilegedArchiveFailure::Uncertain)),
        ("password", Ok(())),
        (
            "password-revoked",
            Err(PrivilegedArchiveFailure::BeforeCommit),
        ),
        ("copy", Ok(())),
        ("password-copy", Ok(())),
        ("copy-short", Err(PrivilegedArchiveFailure::CopyFailed)),
        ("copy-invalid", Err(PrivilegedArchiveFailure::CopyFailed)),
        ("copy-trailing", Err(PrivilegedArchiveFailure::CopyFailed)),
        ("copy-withdrawn", Err(PrivilegedArchiveFailure::CopyFailed)),
        ("copy-midstream", Err(PrivilegedArchiveFailure::CopyFailed)),
        ("sudo-password", Ok(())),
        ("sudo-zip", Ok(())),
        ("sudo-no-prompt", Ok(())),
        (
            "sudo-repeated-prompt",
            Err(PrivilegedArchiveFailure::BeforeCommit),
        ),
        (
            "sudo-bad-greeting",
            Err(PrivilegedArchiveFailure::BeforeCommit),
        ),
        (
            "sudo-revoked-before-auth",
            Err(PrivilegedArchiveFailure::BeforeCommit),
        ),
        (
            "sudo-revoked-before-review",
            Err(PrivilegedArchiveFailure::BeforeCommit),
        ),
    ] {
        let uses_vault = mode.starts_with("sudo-");
        let protected = mode.starts_with("password") || mode == "sudo-zip";
        let revoked = mode.ends_with("revoked");
        let copies_archive = mode.contains("copy");
        let withdrawn = mode.ends_with("withdrawn");
        let withdrawn_during_copy = mode == "copy-midstream";
        let withdrawn_before_auth = mode == "sudo-revoked-before-auth";
        let withdrawn_before_review = mode == "sudo-revoked-before-review";
        let reaches_review = !(uses_vault && expected.is_err());
        let sends_sudo_password = uses_vault && mode != "sudo-no-prompt" && !withdrawn_before_auth;
        let fixture = archive_fixture(mode, COMMAND);
        let root = &fixture.profile.0;

        let spec = CreateSpec {
            sources: vec!["/fixture/pinned/data".into()],
            destination: if protected {
                "/fixture/exports/job.zip"
            } else {
                "/fixture/exports/job.tar"
            }
            .into(),
            format: if protected {
                CreateFormat::ZipPassword
            } else {
                CreateFormat::Tar
            },
            level: 6,
            excludes: Vec::new(),
            output: None,
            report_payload: copies_archive,
            member_layout: protocol::ArchiveMemberLayout::SourceBasename,
        };
        protocol::write_privileged_reply(
            fs::File::create(root.join("review")).unwrap(),
            &PrivilegedReply::Review {
                review: review(&spec),
            },
        )
        .unwrap();
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
        if protected {
            let mut body = protocol::creation_password_body(PASSWORD).unwrap();
            body.extend_from_slice(protocol::PASSWORD_COMMIT);
            fs::write(root.join("expected-password"), &body[..]).unwrap();
        }
        let mut applied = Vec::new();
        let reply = if copies_archive {
            let digest = if mode == "copy-invalid" {
                [0; 32]
            } else {
                Sha256::digest(COPY).into()
            };
            PrivilegedReply::AppliedCopy {
                destination: spec.destination.clone(),
                payload: CreatedPayload {
                    bytes: COPY.len() as u64,
                    sha256: digest,
                },
            }
        } else {
            PrivilegedReply::Applied {
                destination: spec.destination.clone(),
            }
        };
        protocol::write_privileged_reply(&mut applied, &reply).unwrap();
        if withdrawn_during_copy {
            fs::write(root.join("copy-frame-bytes"), applied.len().to_string()).unwrap();
        }
        if copies_archive {
            if mode == "copy-short" {
                applied.extend_from_slice(&COPY[..COPY.len() - 1]);
            } else {
                applied.extend_from_slice(COPY);
            }
            if mode == "copy-trailing" {
                applied.push(1);
            }
        }
        fs::write(root.join("applied"), applied).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let stopped = move || Instant::now() >= deadline;
        let (connection, pin, daemon) = connect_fixture(&fixture, mode, stopped);
        let opener = connection.exec_opener().unwrap();
        assert_eq!(opener.verified_host_key(), Some(&pin));
        let denied = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&denied);
        let review_seen = root.join("review-seen");
        let committed = root.join("commit-and-eof-seen");
        let copy_revoked = root.join("copy-revoked");
        let sudo_revoked = root.join("sudo-revoked");
        let destination = root.join("private-archive-stage");
        let copy = copies_archive.then(|| fs::File::create(&destination).unwrap());
        let result = exchange_privileged_server_archive(
            &opener,
            &spec,
            uses_vault.then(|| SensitiveLine::new(SUDO_PASSWORD).unwrap()),
            protected.then_some(PASSWORD),
            copy.as_ref(),
            stopped,
            move || {
                if (revoked && review_seen.exists())
                    || (withdrawn && committed.exists())
                    || (withdrawn_during_copy && copy_revoked.exists())
                    || ((withdrawn_before_auth || withdrawn_before_review) && sudo_revoked.exists())
                {
                    observed.store(true, Ordering::Release);
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            },
        );
        drop(opener);
        drop(connection);
        drop(copy);
        let output = daemon.join().unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        assert_eq!(result, expected, "{mode}: {log}");
        assert_eq!(
            root.join("review-seen").is_file(),
            reaches_review,
            "{mode}: {log}"
        );
        assert_eq!(
            root.join("sudo-password-seen").is_file(),
            sends_sudo_password,
            "{mode}: {log}"
        );
        assert_eq!(
            denied.load(Ordering::Acquire),
            revoked
                || withdrawn
                || withdrawn_during_copy
                || withdrawn_before_auth
                || withdrawn_before_review
        );
        if copies_archive && expected.is_ok() {
            assert_eq!(fs::read(&destination).unwrap(), COPY, "{mode}: {log}");
        }
        if withdrawn || withdrawn_during_copy {
            assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
        }
        assert_eq!(
            root.join("commit-and-eof-seen").is_file(),
            reaches_review && !revoked,
            "{mode}: {log}"
        );
        assert!(!root.join("unexpected-data").exists(), "{mode}: {log}");
        assert!(!log.contains(PASSWORD));
        assert!(!log.contains(SUDO_PASSWORD));
    }
}

#[test]
#[ignore = "requires native OpenSSH sshd, ssh-keygen and Python 3"]
fn openssh_privileged_archive_update_keeps_publication_when_copy_fails() {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CopyResult {
        None,
        Verified,
        Short,
        Denied,
        Revoked,
    }

    for (mode, copy_result) in [
        ("update-no-download", CopyResult::None),
        ("sudo-update-zip-copy", CopyResult::Verified),
        ("update-copy-short", CopyResult::Short),
        ("sudo-update-copy-denied", CopyResult::Denied),
        ("update-copy-revoked", CopyResult::Revoked),
    ] {
        let fixture = archive_fixture(mode, UPDATE_COMMAND);
        let root = &fixture.profile.0;
        let protected = mode == "sudo-update-zip-copy";
        let uses_vault = mode.starts_with("sudo-");
        let spec = CreateSpec {
            sources: vec!["/fixture/pinned/data".into()],
            destination: if protected {
                "/fixture/exports/job.zip"
            } else {
                "/fixture/exports/job.tar"
            }
            .into(),
            format: if protected {
                CreateFormat::ZipPassword
            } else {
                CreateFormat::Tar
            },
            level: 6,
            excludes: Vec::new(),
            output: None,
            report_payload: copy_result != CopyResult::None,
            member_layout: protocol::ArchiveMemberLayout::SourceBasename,
        };
        let previous = b"prior root-owned archive";
        let payload = CreatedPayload {
            bytes: COPY.len() as u64,
            sha256: Sha256::digest(COPY).into(),
        };
        let create = |name: &str| fs::File::create(root.join(name)).unwrap();
        for (name, request) in [
            (
                "expected-review",
                UpdateRequest::Review { spec: spec.clone() },
            ),
            (
                "expected-prepare",
                UpdateRequest::Prepare {
                    spec: spec.clone(),
                    token: TOKEN.into(),
                },
            ),
        ] {
            protocol::write_update_request(create(name), &request).unwrap();
        }
        for (name, decision) in [
            ("expected-quarantine", UpdateDecision::Quarantine),
            ("expected-publish", UpdateDecision::Publish),
            ("expected-fetch-copy", UpdateDecision::FetchCopy),
        ] {
            protocol::write_update_decision(create(name), &decision).unwrap();
        }
        for (name, reply) in [
            (
                "review",
                UpdateReply::Reviewed {
                    review: review(&spec),
                    existing: Some(CreatedPayload {
                        bytes: previous.len() as u64,
                        sha256: Sha256::digest(previous).into(),
                    }),
                    receiver: [1; 32],
                },
            ),
            ("password-required", UpdateReply::PasswordRequired),
            ("prepared", UpdateReply::Prepared { payload }),
            (
                "quarantined",
                UpdateReply::Quarantined {
                    recovery: RECOVERY.into(),
                },
            ),
            (
                "published",
                UpdateReply::Published {
                    destination: spec.destination.clone(),
                    payload,
                    recovery: Some(RECOVERY.into()),
                    receiver: [2; 32],
                },
            ),
        ] {
            protocol::write_update_reply(create(name), &reply).unwrap();
        }
        if protected {
            let body = protocol::creation_password_body(PASSWORD).unwrap();
            fs::write(root.join("expected-password"), &body[..]).unwrap();
        }
        match copy_result {
            CopyResult::Verified | CopyResult::Short => {
                let mut copy = create("copy");
                protocol::write_update_reply(&mut copy, &UpdateReply::CopyReady { payload })
                    .unwrap();
                let count = COPY.len() - usize::from(copy_result == CopyResult::Short);
                copy.write_all(&COPY[..count]).unwrap();
            }
            CopyResult::Denied => {
                protocol::write_update_reply(create("copy"), &UpdateReply::CopyFailed).unwrap();
            }
            CopyResult::None | CopyResult::Revoked => {}
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        let stopped = move || Instant::now() >= deadline;
        let (connection, pin, daemon) = connect_fixture(&fixture, mode, stopped);
        let opener = connection.exec_opener().unwrap();
        assert_eq!(opener.verified_host_key(), Some(&pin));
        let published = root.join("publish-seen");
        let denied = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&denied);
        let copy = spec.report_payload.then(|| create("private-archive-stage"));
        let result = exchange_privileged_archive_update(
            &opener,
            &spec,
            uses_vault.then(|| SensitiveLine::new(SUDO_PASSWORD).unwrap()),
            protected.then_some(PASSWORD),
            copy.as_ref(),
            stopped,
            move || {
                if copy_result == CopyResult::Revoked && published.is_file() {
                    observed.store(true, Ordering::Release);
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            },
        );
        drop(opener);
        drop(connection);
        drop(copy);
        let output = daemon.join().unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        let publication = result.unwrap_or_else(|error| panic!("{mode}: {error:?}\n{log}"));
        assert_eq!(publication.payload, payload, "{mode}: {log}");
        assert_eq!(
            publication.recovery.as_deref(),
            Some(RECOVERY),
            "{mode}: {log}"
        );
        assert_eq!(
            publication.copy_verified,
            copy_result == CopyResult::Verified
        );
        for phase in [
            "update-channel",
            "review-seen",
            "prepare-seen",
            "quarantine-seen",
            "publish-seen",
        ] {
            assert!(root.join(phase).is_file(), "{mode}, {phase}: {log}");
        }
        assert_eq!(root.join("sudo-password-seen").is_file(), uses_vault);
        assert_eq!(root.join("archive-password-seen").is_file(), protected);
        assert_eq!(
            root.join("fetch-copy-seen").is_file(),
            matches!(
                copy_result,
                CopyResult::Verified | CopyResult::Short | CopyResult::Denied
            ),
            "{mode}: {log}"
        );
        assert_eq!(
            root.join("copy-not-requested").is_file(),
            copy_result == CopyResult::Revoked
        );
        assert_eq!(
            denied.load(Ordering::Acquire),
            copy_result == CopyResult::Revoked
        );
        if spec.report_payload {
            let bytes = fs::read(root.join("private-archive-stage")).unwrap();
            if copy_result == CopyResult::Verified {
                assert_eq!(bytes, COPY, "{mode}: {log}");
            } else {
                assert!(bytes.is_empty(), "{mode}: {log}");
            }
        }
        assert!(!log.contains(PASSWORD));
        assert!(!log.contains(SUDO_PASSWORD));
    }
}
