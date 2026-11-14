//! Opt-in interoperability test against an isolated local OpenSSH daemon.
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use kortty_ssh::sftp::{Error as SftpError, FileKind, MAX_EDIT_BYTES, Sftp};
use kortty_ssh::{
    Authentication, Connect, ConnectError, ConnectOptions, DirectChannel, Endpoint, ExecConnection,
    HostKey, HostKeyStatus, JumpRoute, KeepAlive, KnownHostsFile, MAX_FORWARD_STREAMS,
    MAX_SFTP_LEASES, SensitiveLine, SshReader, SshSession, SshTarget, SshWriter, StartupChannel,
    TrustMode, TrustStore, WindowSize,
};

mod support;
use support::TempDirectory;
#[path = "openssh/liveness.rs"]
mod liveness;

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_guard_revocation_stops_guarded_exec_after_authentication() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let temp = TempDirectory::new("ssh-guarded-exec");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let config = write_config(&temp.0, "host", &user);
    // The fixture can run under a sticky temporary root while trusting only its fresh key.
    let settings = fs::read_to_string(&config)
        .unwrap()
        .replace("StrictModes yes", "StrictModes no");
    fs::write(&config, settings).unwrap();
    for pty in [None, Some(("xterm", fixture_size()))] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let target = fixture_target(&temp.0, &user, "client", &listener);
        let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
        pin(&trust, &target.endpoint, &temp.0, "host");
        let server = serve(&listener, &config);
        let checks = AtomicUsize::new(0);
        let deny_at = if pty.is_some() { 4 } else { 3 };
        let result = SshSession::connect_preconnected_guarded(
            TcpStream::connect(address).unwrap(),
            (target, Vec::new()),
            trust,
            StartupChannel::Exec {
                command: "printf unexpected",
                pty,
            },
            || false,
            || {
                if checks.fetch_add(1, Ordering::Relaxed) >= deny_at {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fixture guard revoked",
                    ));
                }
                Ok(())
            },
        );
        let output = server.join().unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        let error = result.err().expect("guard must reject the command");
        assert_eq!(
            error.to_string(),
            "fixture guard revoked",
            "guard calls: {}\n{log}",
            checks.load(Ordering::Relaxed)
        );
        assert!(log.contains("Accepted publickey"), "{log}");
        assert!(!log.contains("request exec"), "{log}");
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_transfer_helper_reserves_two_exec_slots_only_when_requested() {
    let temp = TempDirectory::new("ssh-transfer-exec");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let config = write_config(&temp.0, "host", &user);
    for transfer_helper in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = fixture_target(&temp.0, &user, "client", &listener);
        let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
        pin(&trust, &target.endpoint, &temp.0, "host");
        let server = serve(&listener, &config);
        let mut session = SshSession::connect_preconnected_guarded(
            TcpStream::connect(listener.local_addr().unwrap()).unwrap(),
            (target, Vec::new()),
            trust,
            if transfer_helper {
                StartupChannel::SftpWithTransferHelper
            } else {
                StartupChannel::Subsystem("sftp")
            },
            || false,
            || Ok(()),
        )
        .unwrap();
        let opener = session.exec_opener().unwrap();
        let first = opener.open_cancellable("cat", || false).unwrap();
        let second = opener.open_cancellable("cat", || false);
        if transfer_helper {
            let second = second.unwrap();
            assert!(matches!(
                opener.open_cancellable("cat", || false),
                Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
            drop(second);
        } else {
            assert!(matches!(
                second,
                Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
        }
        drop(first);
        session.close().unwrap();
        let result = server.join().unwrap();
        let log = String::from_utf8_lossy(&result.stderr);
        assert_eq!(log.matches("Accepted publickey").count(), 1, "{log}");
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_configured_idle_keepalive_is_encrypted_and_channel_neutral() {
    let temp = TempDirectory::new("ssh-keepalive");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let config = write_config(&temp.0, "host", &user);
    let debug = fs::read_to_string(&config)
        .unwrap()
        .replace("LogLevel DEBUG2", "LogLevel DEBUG3");
    fs::write(&config, debug).unwrap();
    for enabled in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
        let target = fixture_target(&temp.0, &user, "client", &listener);
        pin(&trust, &target.endpoint, &temp.0, "host");
        let server = serve(&listener, &config);
        let mut shell = SshSession::connect_preconnected_with_options(
            TcpStream::connect(address).unwrap(),
            (target, Vec::new()),
            trust,
            StartupChannel::Terminal {
                term: "xterm",
                size: fixture_size(),
            },
            ConnectOptions::default().with_keepalive(Some(KeepAlive::new(enabled, 5).unwrap())),
            || false,
        )
        .unwrap();
        let mut input = shell.take_writer().unwrap();
        let mut output = shell.take_reader().unwrap();
        assert_shell_usable(&mut input, &mut output, "before-keepalive");
        thread::sleep(Duration::from_secs(6));
        let mut lease = shell.sftp_opener().unwrap().open().unwrap();
        let mut sftp =
            Sftp::new(lease.take_reader().unwrap(), lease.take_writer().unwrap()).unwrap();
        assert_eq!(sftp.stat(".").unwrap().kind(), FileKind::Directory);
        drop(sftp);
        drop(lease);
        assert_shell_usable(&mut input, &mut output, "after-keepalive");
        shell.close().unwrap();
        let result = server.join().unwrap();
        let log = String::from_utf8_lossy(&result.stderr);
        assert!(log.contains("subsystem request for sftp"));
        assert_eq!(
            log.lines()
                .any(|line| line.ends_with("receive packet: type 2")),
            enabled,
            "unexpected transport heartbeat state with keepalive enabled={enabled}"
        );
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_sftp_leases_share_the_terminal_connection_and_close_independently() {
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoint, &temp.0, "host");
    let connect = || {
        SshSession::connect(
            Connect {
                address,
                endpoint: endpoint.clone(),
                username: user.clone(),
                authentication: Authentication::private_key(
                    fs::read(temp.0.join("client")).unwrap(),
                ),
                term: "xterm".into(),
                size: fixture_size(),
            },
            trust.clone(),
        )
        .unwrap()
    };
    let config = write_config(&temp.0, "host", &user);
    let server = serve(&listener, &config);
    let mut shell = connect();
    let mut input = shell.take_writer().unwrap();
    let mut output = shell.take_reader().unwrap();
    let opener = shell.sftp_opener().unwrap();
    assert!(matches!(
        opener.open_cancellable(|| true),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted
    ));
    let mut lease = opener.open().unwrap();
    let extra_leases: Vec<_> = (1..MAX_SFTP_LEASES)
        .map(|_| opener.open().unwrap())
        .collect();
    assert!(matches!(
        opener.open(),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    drop(extra_leases);
    let reader = lease.take_reader().unwrap();
    let writer = lease.take_writer().unwrap();
    let remote_tree = temp.0.canonicalize().unwrap().join("transfer-tree");
    let directory = collect_output(move || {
        let mut sftp = Sftp::new(reader, writer).unwrap();
        assert_eq!(sftp.stat(".").unwrap().kind(), FileKind::Directory);
        let root = remote_tree.as_os_str().as_encoded_bytes();
        let file = remote_tree.join("file");
        let file = file.as_os_str().as_encoded_bytes();
        sftp.mkdir_private(root).unwrap();
        assert!(sftp.mkdir_private(root).is_err());
        sftp.upload_new(file, &b"tree-channel"[..]).unwrap();
        assert_eq!(sftp.read_dir_limited(root, 3, 4096).unwrap().len(), 1);
        let attrs = sftp.lstat(file).unwrap();
        let mut downloaded = Vec::new();
        assert_eq!(
            sftp.download_regular(file, attrs, &mut downloaded).unwrap(),
            12
        );
        assert_eq!(downloaded, b"tree-channel");
        sftp.canonicalize(".").unwrap()
    });
    assert!(directory.starts_with(b"/"));
    assert_shell_usable(&mut input, &mut output, "during");
    let abort = lease.abort_handle();
    lease.close();
    lease.close();
    assert!(abort.is_closed());
    assert_shell_usable(&mut input, &mut output, "after");
    assert_eq!(shell.exit_code(), None);

    let mut lease = opener.open().unwrap();
    let lease_abort = lease.abort_handle();
    let mut reader = lease.take_reader().unwrap();
    shell.close().unwrap();
    assert!(opener.is_closed() && lease_abort.is_closed());
    assert!(opener.open().is_err());
    let error = reader
        .read_timeout(&mut [0], Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    let result = server.join().unwrap();
    let log = String::from_utf8_lossy(&result.stderr);
    assert_eq!(log.matches("Accepted publickey").count(), 1, "{log}");

    for (request, extra) in [("session", "MaxSessions 1\n"), ("subsystem", "")] {
        let config = write_config_with_key(&temp.0, "host", &user, "client", extra);
        if request == "subsystem" {
            let source = fs::read_to_string(&config).unwrap();
            assert!(source.contains("Subsystem sftp internal-sftp\n"));
            fs::write(
                &config,
                source.replace("Subsystem sftp internal-sftp\n", ""),
            )
            .unwrap();
        }
        let server = serve(&listener, &config);
        let mut shell = connect();
        assert!(matches!(
            shell.sftp_opener().unwrap().open(),
            Err(ConnectError::RequestRejected { request: rejected }) if rejected == request
        ));
        assert_shell_usable(
            &mut shell.take_writer().unwrap(),
            &mut shell.take_reader().unwrap(),
            request,
        );
        shell.close().unwrap();
        server.join().unwrap();
    }

    // Test double for a peer that keeps sending unsolicited subsystem bytes.
    let config = write_config(&temp.0, "host", &user);
    let source = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        source.replace(
            "Subsystem sftp internal-sftp\n",
            "Subsystem sftp /usr/bin/yes\n",
        ),
    )
    .unwrap();
    let server = serve(&listener, &config);
    let mut shell = connect();
    let mut lease = shell.sftp_opener().unwrap().open().unwrap();
    let abort = lease.abort_handle();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !abort.is_closed() {
        assert!(
            Instant::now() < deadline,
            "unsolicited subsystem output was not bounded"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let mut reader = lease.take_reader().unwrap();
    let mut data = Vec::new();
    let error = reader.read_to_end(&mut data).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(data.len() <= 512 * 1024);
    assert_shell_usable(
        &mut shell.take_writer().unwrap(),
        &mut shell.take_reader().unwrap(),
        "flood",
    );
    shell.close().unwrap();
    server.join().unwrap();
}

fn assert_shell_usable(input: &mut SshWriter, output: &mut SshReader, tag: &str) {
    writeln!(input, "printf 'sftp-shell-%s-ok\\n' {tag}").unwrap();
    let marker = format!("sftp-shell-{tag}-ok");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        assert!(
            !timeout.is_zero(),
            "shell output: {}",
            String::from_utf8_lossy(&bytes)
        );
        let count = output.read_timeout(&mut chunk, timeout).unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        if bytes
            .windows(marker.len())
            .any(|part| part == marker.as_bytes())
        {
            break;
        }
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_remote_text_checks_versions_modes_links_and_channel_identity() {
    let temp = TempDirectory::new("remote-edit");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let directory = temp.0.join("files");
    fs::create_dir(&directory).unwrap();
    let wire_directory = directory.canonicalize().unwrap();
    let target = directory.join("config.txt");
    let wire_target = wire_directory.join("config.txt");
    let target_path = wire_target.as_os_str().as_encoded_bytes();
    fs::write(&target, "old\r\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoint, &temp.0, "host");
    let config = write_config(&temp.0, "host", &user);
    let connect = || Connect {
        address,
        endpoint: endpoint.clone(),
        username: user.clone(),
        authentication: Authentication::private_key(fs::read(temp.0.join("client")).unwrap()),
        term: "xterm".into(),
        size: fixture_size(),
    };
    let server = serve(&listener, &config);
    let mut shell = SshSession::connect(connect(), trust.clone()).unwrap();
    let opener = shell.sftp_opener().unwrap();
    let mut lease = opener.open().unwrap();
    let mut client = Sftp::new(lease.take_reader().unwrap(), lease.take_writer().unwrap()).unwrap();
    let original = client.open_text(target_path).unwrap();
    assert_eq!(&**original.text(), "old\r\n");
    let observed_private_stage = std::cell::Cell::new(false);
    let saved = client
        .save_text(&original, Arc::from("neu ä\r\n"), || {
            for stage in edit_stages(&directory) {
                assert_eq!(
                    stage.metadata().unwrap().permissions().mode() & 0o777,
                    0o700
                );
                observed_private_stage.set(true);
            }
            false
        })
        .unwrap();
    assert!(observed_private_stage.get());
    assert_eq!(fs::read_to_string(&target).unwrap(), "neu ä\r\n");
    assert_eq!(
        target.metadata().unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(edit_stages(&directory).is_empty());

    let canceled = client.save_text(&saved, Arc::from("cancel"), || {
        edit_stages(&directory)
            .iter()
            .any(|stage| stage.join("document").is_file())
    });
    assert!(matches!(canceled, Err(SftpError::Closed)));
    assert_eq!(fs::read_to_string(&target).unwrap(), "neu ä\r\n");
    assert!(edit_stages(&directory).is_empty());

    let modified = target.metadata().unwrap().modified().unwrap();
    fs::write(&target, "alt é\r\n").unwrap();
    fs::File::open(&target)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(matches!(
        client.save_text(&saved, Arc::from("stale"), || false),
        Err(SftpError::EditConflict(_))
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "alt é\r\n");
    assert!(edit_stages(&directory).is_empty());
    let original = client.open_text(target_path).unwrap();
    let parent_mode = directory.metadata().unwrap().permissions();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o770)).unwrap();
    assert!(matches!(
        client.save_external_text(&original, Arc::from("external"), || false),
        Err(SftpError::Unsupported(_))
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "alt é\r\n");
    assert!(edit_stages(&directory).is_empty());
    fs::set_permissions(&directory, parent_mode).unwrap();

    let external = client
        .save_external_text(&original, Arc::from("external"), || false)
        .unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "external");
    let modified = target.metadata().unwrap().modified().unwrap();
    fs::write(&target, "changed!").unwrap();
    fs::File::open(&target)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(matches!(
        client.save_external_text(&external, Arc::from("stale"), || false),
        Err(SftpError::EditConflict(_))
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "changed!");
    assert!(edit_stages(&directory).is_empty());
    let original = client.open_text(target_path).unwrap();

    let large = wire_directory.join("large.txt");
    fs::File::create(&large)
        .unwrap()
        .set_len(MAX_EDIT_BYTES as u64 + 1)
        .unwrap();
    assert!(matches!(
        client.open_text(large.as_os_str().as_encoded_bytes()),
        Err(SftpError::Limit(_))
    ));
    assert!(matches!(
        client.open_external_file(large.as_os_str().as_encoded_bytes()),
        Err(SftpError::Limit(_))
    ));
    let binary = wire_directory.join("binary.txt");
    for bytes in [b"bad\0data".as_slice(), b"bad\xffdata"] {
        fs::write(&binary, bytes).unwrap();
        assert!(matches!(
            client.open_text(binary.as_os_str().as_encoded_bytes()),
            Err(SftpError::InvalidInput(_))
        ));
    }
    let binary_path = binary.as_os_str().as_encoded_bytes();
    let binary_opened = client.open_external_file(binary_path).unwrap();
    assert_eq!(binary_opened.bytes().as_ref(), b"bad\xffdata");
    let edited: &[u8] = b"new\0\x81data";
    let binary_saved = client
        .save_external_file(&binary_opened, Arc::from(edited), || false)
        .unwrap();
    assert_eq!(fs::read(&binary).unwrap(), edited);
    let modified = binary.metadata().unwrap().modified().unwrap();
    fs::write(&binary, b"new\0\x82data").unwrap();
    fs::File::open(&binary)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(matches!(
        client.save_external_file(&binary_saved, Arc::from(edited), || false),
        Err(SftpError::EditConflict(_))
    ));
    assert_eq!(fs::read(&binary).unwrap(), b"new\0\x82data");
    assert!(edit_stages(&directory).is_empty());

    let separate = directory.join("separate.txt");
    fs::write(&separate, "leave intact").unwrap();
    fs::remove_file(&target).unwrap();
    symlink(&separate, &target).unwrap();
    assert!(matches!(
        client.open_text(target_path),
        Err(SftpError::InvalidInput(_))
    ));
    assert!(matches!(
        client.save_text(&original, Arc::from("follow link"), || false),
        Err(SftpError::EditConflict(_))
    ));
    assert!(target.symlink_metadata().unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&separate).unwrap(), "leave intact");

    fs::remove_file(&target).unwrap();
    fs::write(&target, "parent").unwrap();
    let original = client.open_text(target_path).unwrap();
    let moved = temp.0.join("files-moved");
    fs::rename(&directory, &moved).unwrap();
    symlink(&moved, &directory).unwrap();
    assert!(matches!(
        client.open_text(target_path),
        Err(SftpError::EditConflict(_))
    ));
    assert!(matches!(
        client.save_text(&original, Arc::from("replace parent"), || false),
        Err(SftpError::EditConflict(_))
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "parent");

    drop(client);
    drop(lease);
    let mut next = opener.open().unwrap();
    let mut client = Sftp::new(next.take_reader().unwrap(), next.take_writer().unwrap()).unwrap();
    assert!(matches!(
        client.save_text(&original, Arc::from("new channel"), || false),
        Err(SftpError::EditConflict(
            "The SFTP connection changed. Reopen the file."
        ))
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "parent");
    shell.close().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&server.join().unwrap().stderr)
            .matches("Accepted publickey")
            .count(),
        1
    );

    let server = serve(&listener, &config);
    let mut subsystem = SshSession::connect_subsystem(connect(), trust, &[], "sftp").unwrap();
    let mut client = Sftp::new(
        subsystem.take_reader().unwrap(),
        subsystem.take_writer().unwrap(),
    )
    .unwrap();
    let moved_file = moved.canonicalize().unwrap().join("config.txt");
    let opened = client
        .open_text(moved_file.as_os_str().as_encoded_bytes())
        .unwrap();
    client
        .save_text(&opened, Arc::from("standalone"), || false)
        .unwrap();
    assert_eq!(fs::read_to_string(moved_file).unwrap(), "standalone");
    subsystem.close().unwrap();
    server.join().unwrap();
}

fn edit_stages(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".kortty-edit-")
        })
        .collect()
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_ecdsa_host_pins_are_algorithm_bound_before_userauth() {
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(&temp.0, &["client", "host-ed25519"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let ed25519 = write_config_with_key(
        &temp.0,
        "host-ed25519",
        &user,
        "client",
        "HostKeyAlgorithms ssh-ed25519\n",
    );
    let connect = || Connect {
        address,
        endpoint: endpoint.clone(),
        username: user.clone(),
        authentication: Authentication::private_key(fs::read(temp.0.join("client")).unwrap()),
        term: "xterm".into(),
        size: fixture_size(),
    };
    for bits in ["256", "384", "521"] {
        let host = format!("host-{bits}");
        let rotated = format!("rotated-{bits}");
        for name in [&host, &rotated] {
            let result = Command::new("ssh-keygen")
                .args(["-q", "-t", "ecdsa", "-b", bits, "-N", "", "-f"])
                .arg(temp.0.join(name))
                .output()
                .unwrap();
            assert_success(&result);
        }
        let algorithm = format!("ecdsa-sha2-nistp{bits}");
        let only_curve = format!("HostKeyAlgorithms {algorithm}\n");
        let config = write_config_with_key(&temp.0, &host, &user, "client", &only_curve);
        let trust = TrustStore::new(temp.0.join(format!("pins-{bits}")), TrustMode::Strict);
        let server = serve(&listener, &config);
        let offered = match SshSession::connect(connect(), trust.clone()) {
            Err(ConnectError::UnknownHostKey {
                endpoint: actual,
                offered,
            }) => {
                assert_eq!(actual, endpoint);
                offered
            }
            Err(error) => panic!("expected unknown {algorithm} host, got {error}"),
            Ok(_) => panic!("an unknown {algorithm} host was accepted"),
        };
        assert_no_auth(&server.join().unwrap());
        let fixture = fs::read_to_string(temp.0.join(format!("{host}.pub"))).unwrap();
        assert_eq!(offered, HostKey::from_openssh(fixture.trim()).unwrap());
        assert_eq!(
            trust.remember(&endpoint, &offered).unwrap(),
            HostKeyStatus::Known
        );

        // An existing ECDSA pin remains usable when the server also offers Ed25519.
        let both_keys = format!(
            "HostKey {}\nHostKeyAlgorithms ssh-ed25519,{algorithm}\n",
            temp.0.join("host-ed25519").display(),
        );
        let config = write_config_with_key(&temp.0, &host, &user, "client", &both_keys);
        let server = serve(&listener, &config);
        SshSession::connect(connect(), trust.clone())
            .unwrap()
            .close()
            .unwrap();
        let log = server.join().unwrap().stderr;
        assert!(String::from_utf8_lossy(&log).contains("Accepted publickey"));

        let rotated = write_config_with_key(&temp.0, &rotated, &user, "client", &only_curve);
        for config in [&rotated, &ed25519] {
            let server = serve(&listener, config);
            assert!(matches!(
                SshSession::connect(connect(), trust.clone()),
                Err(ConnectError::ChangedHostKey { endpoint: actual, expected, offered: replacement })
                    if actual == endpoint && expected == offered && replacement != offered
            ));
            assert_no_auth(&server.join().unwrap());
        }
    }
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd, openssl for PEM fixtures, and a local account"]
fn openssh_authenticates_with_all_three_ecdsa_user_curves() {
    const PASSPHRASE: &str = "test-only ecdsa passphrase";
    let temp = TempDirectory::new("openssh-ecdsa-user");
    let user = fixture_user();
    generate_keys(&temp.0, &["host"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoint, &temp.0, "host");
    for (bits, curve) in [
        ("256", "prime256v1"),
        ("384", "secp384r1"),
        ("521", "secp521r1"),
    ] {
        let algorithm = format!("ecdsa-sha2-nistp{bits}");
        for (format, passphrase) in [
            ("openssh", ""),
            ("openssh", PASSPHRASE),
            ("pkcs8", ""),
            ("sec1", ""),
        ] {
            let encrypted = !passphrase.is_empty();
            let name = format!("client-{bits}-{format}-{encrypted}");
            let path = temp.0.join(&name);
            if format == "openssh" {
                let result = Command::new("ssh-keygen")
                    .args(["-q", "-t", "ecdsa", "-b", bits, "-N", passphrase, "-f"])
                    .arg(&path)
                    .output()
                    .unwrap();
                assert_success(&result);
            } else {
                generate_named_ecdsa_pem(&path, curve, format == "pkcs8");
            }
            let key = fs::read(&path).unwrap();
            let header = match format {
                "pkcs8" => b"-----BEGIN PRIVATE KEY-----".as_slice(),
                "sec1" => b"-----BEGIN EC PRIVATE KEY-----".as_slice(),
                _ => b"-----BEGIN OPENSSH PRIVATE KEY-----".as_slice(),
            };
            assert!(key.starts_with(header));
            let server = write_config_with_key(
                &temp.0,
                "host",
                &user,
                &name,
                &format!("PubkeyAcceptedAlgorithms {algorithm}\n"),
            );
            let config = |authentication| Connect {
                address,
                endpoint: endpoint.clone(),
                username: user.clone(),
                authentication,
                term: "xterm".into(),
                size: fixture_size(),
            };
            if encrypted && bits == "384" {
                let daemon = serve(&listener, &server);
                let wrong = Authentication::private_key_with_passphrase(
                    key.clone(),
                    "wrong fixture passphrase".into(),
                );
                assert!(matches!(
                    SshSession::connect(config(wrong), trust.clone()),
                    Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
                ));
                let log = daemon.join().unwrap().stderr;
                assert!(!String::from_utf8_lossy(&log).contains("method publickey"));
            }
            let daemon = serve(&listener, &server);
            let auth = if encrypted {
                Authentication::private_key_with_passphrase(key, passphrase.into())
            } else {
                Authentication::private_key(key)
            };
            let mut session = SshSession::connect_exec(
                config(auth),
                trust.clone(),
                &[],
                "printf 'ecdsa-user-key-ok\\n'",
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            let mut output = session.take_reader().unwrap();
            let received = collect_output(move || {
                let mut bytes = Vec::new();
                output.read_to_end(&mut bytes).unwrap();
                bytes
            });
            assert_eq!(received, b"ecdsa-user-key-ok\n", "{name}");
            session.close().unwrap();
            let log = daemon.join().unwrap().stderr;
            assert!(String::from_utf8_lossy(&log).contains("Accepted publickey"));
        }
    }
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd, OpenSSL for local PEM fixtures, and a local account"]
fn openssh_rsa_sha2_pins_and_private_key_formats() {
    const PASSWORD: &str = "test-only rsa passphrase";
    let temp = TempDirectory::new("openssh-rsa-sha2");
    let user = fixture_user();
    generate_keys(&temp.0, &["host-ed25519"]);
    for name in ["host-rsa", "rotated-rsa", "client-rsa"] {
        generate_rsa_key(&temp.0, name);
    }
    let original = temp.0.join("client-rsa");
    let encrypted_open = temp.0.join("client-encrypted-openssh");
    let pkcs1 = temp.0.join("client-pkcs1");
    for (path, password) in [(&encrypted_open, PASSWORD), (&pkcs1, "")] {
        fs::copy(&original, path).unwrap();
        let mut command = Command::new("ssh-keygen");
        command
            .args(["-q", "-p", "-a", "8", "-P", "", "-N", password, "-f"])
            .arg(path);
        if path == &pkcs1 {
            command.args(["-m", "PEM"]);
        }
        assert_success(&command.output().unwrap());
    }
    let plain_pkcs8 = temp.0.join("client-pkcs8");
    assert_success(
        &Command::new("openssl")
            .args(["pkcs8", "-topk8", "-nocrypt", "-in"])
            .arg(&pkcs1)
            .arg("-out")
            .arg(&plain_pkcs8)
            .output()
            .unwrap(),
    );
    let variants = [
        ("pkcs8-sha1", "hmacWithSHA1", "2048"),
        ("pkcs8-sha256", "hmacWithSHA256", "2048"),
        ("pkcs8-too-expensive", "hmacWithSHA256", "1000001"),
    ];
    for (name, prf, rounds) in variants {
        assert_success(
            &Command::new("openssl")
                .args([
                    "pkcs8",
                    "-topk8",
                    "-v2",
                    "aes-256-cbc",
                    "-v2prf",
                    prf,
                    "-iter",
                    rounds,
                    "-in",
                ])
                .arg(&pkcs1)
                .arg("-out")
                .arg(temp.0.join(name))
                .arg("-passout")
                .arg(format!("pass:{PASSWORD}"))
                .output()
                .unwrap(),
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let config = |authentication| Connect {
        address,
        endpoint: endpoint.clone(),
        username: user.clone(),
        authentication,
        term: "xterm".into(),
        size: fixture_size(),
    };
    let unencrypted = || Authentication::private_key(fs::read(&original).unwrap());
    let host = fs::read_to_string(temp.0.join("host-rsa.pub")).unwrap();
    let key = HostKey::from_openssh(host.trim()).unwrap();
    let trust = TrustStore::new(temp.0.join("rsa-pins"), TrustMode::Strict);
    let rsa_only = "HostKeyAlgorithms rsa-sha2-512\nPubkeyAcceptedAlgorithms rsa-sha2-256\n";
    let sshd = write_config_with_key(&temp.0, "host-rsa", &user, "client-rsa", rsa_only);
    let daemon = serve(&listener, &sshd);
    // A malformed encrypted credential must stay unread until the host is accepted.
    assert!(
        matches!(SshSession::connect(config(Authentication::private_key(b"-----BEGIN ENCRYPTED PRIVATE KEY-----\ninvalid".to_vec())), trust.clone()),
        Err(ConnectError::UnknownHostKey { endpoint: actual, offered }) if actual == endpoint && offered == key)
    );
    assert_no_auth(&daemon.join().unwrap());
    let known_hosts = KnownHostsFile::parse(&format!("{endpoint} {host}")).unwrap();
    let review = trust.preview_import(known_hosts).unwrap();
    assert_eq!(review.report().added.len(), 1);
    trust.import_reviewed(&review).unwrap();
    assert_eq!(trust.check(&endpoint, &key).unwrap(), HostKeyStatus::Known);

    for (host_algorithm, user_algorithm) in [
        ("rsa-sha2-512", "rsa-sha2-256"),
        ("rsa-sha2-256", "rsa-sha2-512"),
    ] {
        let both = format!(
            "HostKey {}\nHostKeyAlgorithms ssh-ed25519,{host_algorithm}\nPubkeyAcceptedAlgorithms {user_algorithm}\n",
            temp.0.join("host-ed25519").display()
        );
        let sshd = write_config_with_key(&temp.0, "host-rsa", &user, "client-rsa", &both);
        for (path, password) in [
            (&original, None),
            (&encrypted_open, Some(PASSWORD)),
            (&pkcs1, None),
            (&plain_pkcs8, None),
            (&temp.0.join("pkcs8-sha1"), Some(PASSWORD)),
            (&temp.0.join("pkcs8-sha256"), Some(PASSWORD)),
        ] {
            let key = fs::read(path).unwrap();
            let auth = match password {
                Some(password) => Authentication::private_key_with_passphrase(key, password.into()),
                None => Authentication::private_key(key),
            };
            let daemon = serve(&listener, &sshd);
            let mut session = SshSession::connect_exec(
                config(auth),
                trust.clone(),
                &[],
                "printf 'rsa-sha2-ok\\n'",
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{} using {host_algorithm}/{user_algorithm}: {error}",
                    path.display()
                )
            });
            let mut output = session.take_reader().unwrap();
            let received = collect_output(move || {
                let mut bytes = Vec::new();
                output.read_to_end(&mut bytes).unwrap();
                bytes
            });
            assert_eq!(received, b"rsa-sha2-ok\n");
            session.close().unwrap();
            assert!(
                String::from_utf8_lossy(&daemon.join().unwrap().stderr)
                    .contains("Accepted publickey")
            );
        }
    }

    let sshd = write_config_with_key(&temp.0, "host-rsa", &user, "client-rsa", rsa_only);
    for (file, passphrase, kind) in [
        ("pkcs8-sha1", None, std::io::ErrorKind::PermissionDenied),
        (
            "pkcs8-sha256",
            Some("wrong fixture passphrase"),
            std::io::ErrorKind::PermissionDenied,
        ),
        (
            "pkcs8-too-expensive",
            Some(PASSWORD),
            std::io::ErrorKind::InvalidInput,
        ),
    ] {
        let key = fs::read(temp.0.join(file)).unwrap();
        let auth = match passphrase {
            Some(password) => Authentication::private_key_with_passphrase(key, password.into()),
            None => Authentication::private_key(key),
        };
        let daemon = serve(&listener, &sshd);
        assert!(
            matches!(SshSession::connect(config(auth), trust.clone()),
            Err(ConnectError::Io(error)) if error.kind() == kind),
            "{file}"
        );
        assert!(
            !String::from_utf8_lossy(&daemon.join().unwrap().stderr).contains("method publickey")
        );
    }

    let rotated = write_config_with_key(&temp.0, "rotated-rsa", &user, "client-rsa", rsa_only);
    let daemon = serve(&listener, &rotated);
    assert!(
        matches!(SshSession::connect(config(unencrypted()), trust.clone()),
        Err(ConnectError::ChangedHostKey { expected, offered, .. }) if expected == key && offered != key)
    );
    assert_no_auth(&daemon.join().unwrap());
    let sha1 = write_config_with_key(
        &temp.0,
        "host-rsa",
        &user,
        "client-rsa",
        "HostKeyAlgorithms ssh-rsa\n",
    );
    let daemon = serve(&listener, &sha1);
    assert!(SshSession::connect(config(unencrypted()), trust).is_err());
    assert_no_auth(&daemon.join().unwrap());
}

fn generate_named_ecdsa_pem(path: &Path, curve: &str, pkcs8: bool) {
    let sec1 = if pkcs8 {
        path.with_extension("sec1")
    } else {
        path.to_owned()
    };
    assert_success(
        &Command::new("openssl")
            .args([
                "ecparam",
                "-name",
                curve,
                "-param_enc",
                "named_curve",
                "-genkey",
                "-noout",
                "-out",
            ])
            .arg(&sec1)
            .output()
            .unwrap(),
    );
    if pkcs8 {
        assert_success(
            &Command::new("openssl")
                .args(["pkcs8", "-topk8", "-nocrypt", "-in"])
                .arg(sec1)
                .arg("-out")
                .arg(path)
                .output()
                .unwrap(),
        );
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    let public = Command::new("ssh-keygen")
        .args(["-y", "-f"])
        .arg(path)
        .output()
        .unwrap();
    assert_success(&public);
    fs::write(path.with_extension("pub"), public.stdout).unwrap();
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_encrypted_ed25519_key_is_decrypted_after_host_verification() {
    const PASSPHRASE: &str = "test-only encrypted key passphrase";
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(&temp.0, &["host"]);
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-a", "16", "-N", PASSPHRASE, "-f"])
        .arg(temp.0.join("client"))
        .output()
        .unwrap();
    assert_success(&output);
    let ssh = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = ssh.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let config = write_config(&temp.0, "host", &user);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    let connect = |passphrase: &str| Connect {
        address,
        endpoint: endpoint.clone(),
        username: user.clone(),
        authentication: Authentication::private_key_with_passphrase(
            fs::read(temp.0.join("client")).unwrap(),
            passphrase.into(),
        ),
        term: "xterm".into(),
        size: fixture_size(),
    };
    let server = serve(&ssh, &config);
    assert!(matches!(
        SshSession::connect(connect("wrong test passphrase"), trust.clone()),
        Err(ConnectError::UnknownHostKey { .. })
    ));
    assert_no_auth(&server.join().unwrap());
    pin(&trust, &endpoint, &temp.0, "host");

    let server = serve(&ssh, &config);
    assert!(matches!(
        SshSession::connect(connect("wrong test passphrase"), trust.clone()),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    let log = server.join().unwrap().stderr;
    assert!(!String::from_utf8_lossy(&log).contains("method publickey"));

    let key = ssh_key::PrivateKey::from_openssh(fs::read(temp.0.join("client")).unwrap()).unwrap();
    let mut bytes = key.to_bytes().unwrap();
    // Test-only mutation of the RFC-style string containing the bcrypt salt and rounds.
    let options = bytes
        .windows(6)
        .position(|bytes| bytes == b"bcrypt")
        .unwrap()
        + 6;
    let length = u32::from_be_bytes(bytes[options..options + 4].try_into().unwrap()) as usize;
    let rounds = options + length;
    assert_eq!(&bytes[rounds..rounds + 4], &16u32.to_be_bytes());
    bytes[rounds..rounds + 4].copy_from_slice(&129u32.to_be_bytes());
    let key = ssh_key::PrivateKey::from_bytes(&bytes)
        .unwrap()
        .to_openssh(ssh_key::LineEnding::LF)
        .unwrap();
    let mut too_expensive = connect(PASSPHRASE);
    too_expensive.authentication =
        Authentication::private_key_with_passphrase(key.as_bytes().to_vec(), PASSPHRASE.into());
    let server = serve(&ssh, &config);
    assert!(matches!(
        SshSession::connect(too_expensive, trust.clone()),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput
    ));
    let log = server.join().unwrap().stderr;
    assert!(!String::from_utf8_lossy(&log).contains("method publickey"));

    let server = serve(&ssh, &config);
    let mut session = SshSession::connect(connect(PASSPHRASE), trust).unwrap();
    let mut writer = session.take_writer().unwrap();
    let mut reader = session.take_reader().unwrap();
    writer
        .write_all(b"HISTFILE=/dev/null; printf '\\nencrypted-key-ok\\n'; exit 6\n")
        .unwrap();
    let bytes = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert!(
        String::from_utf8_lossy(&bytes)
            .lines()
            .any(|line| line.trim() == "encrypted-key-ok")
    );
    assert_eq!(session.exit_code(), Some(6));
    session.close().unwrap();
    let log = server.join().unwrap().stderr;
    assert!(String::from_utf8_lossy(&log).contains("Accepted publickey"));
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_authenticates_with_ed25519_pkcs8_pem() {
    // Public interoperability test vectors from RFC 8410 §10.3.
    const PRIVATE: &str = concat!(
        "-----BEGIN PRIVATE KEY-----\n",
        "MC4CAQAwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC\n",
        "-----END PRIVATE KEY-----\n",
    );
    const PRIVATE_WITH_PUBLIC: &str = concat!(
        "-----BEGIN PRIVATE KEY-----\n",
        "MHICAQEwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC\n",
        "oB8wHQYKKoZIhvcNAQkJFDEPDA1DdXJkbGUgQ2hhaXJzgSEAGb9ECWmEzf6FQbrB\n",
        "Z9w7lshQhqowtrbLDFw4rXAxZuE=\n",
        "-----END PRIVATE KEY-----\n",
    );
    const AUTHORIZED: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh\n";

    let temp = TempDirectory::new("openssh-pkcs8");
    let user = fixture_user();
    generate_keys(&temp.0, &["host"]);
    fs::write(temp.0.join("client.pub"), AUTHORIZED).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let config = write_config(&temp.0, "host", &user);
    let key = fs::read_to_string(temp.0.join("host.pub")).unwrap();
    let key = HostKey::from_openssh(key.trim()).unwrap();
    let trust = TrustStore::from_pins(vec![(endpoint.clone(), key)]).unwrap();
    let connect = |pem: &str| Connect {
        address,
        endpoint: endpoint.clone(),
        username: user.clone(),
        authentication: Authentication::private_key(pem.as_bytes().to_vec()),
        term: "xterm".into(),
        size: fixture_size(),
    };

    // Test-only mutation changes the optional public key without changing its DER framing.
    let mismatched = PRIVATE_WITH_PUBLIC.replacen("Z9w7lshQ", "A9w7lshQ", 1);
    let server = serve(&listener, &config);
    assert!(matches!(
        SshSession::connect(
            connect(&mismatched),
            TrustStore::from_pins(Vec::new()).unwrap()
        ),
        Err(ConnectError::UnknownHostKey { .. })
    ));
    assert_no_auth(&server.join().unwrap());

    let server = serve(&listener, &config);
    assert!(matches!(
        SshSession::connect(connect(&mismatched), trust.clone()),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput
    ));
    let log = server.join().unwrap().stderr;
    assert!(!String::from_utf8_lossy(&log).contains("method publickey"));

    for pem in [PRIVATE, PRIVATE_WITH_PUBLIC] {
        let server = serve(&listener, &config);
        let mut shell = SshSession::connect(connect(pem), trust.clone()).unwrap();
        assert_shell_usable(
            &mut shell.take_writer().unwrap(),
            &mut shell.take_reader().unwrap(),
            "pkcs8",
        );
        shell.close().unwrap();
        let log = server.join().unwrap().stderr;
        assert!(String::from_utf8_lossy(&log).contains("Accepted publickey"));
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_exec_waits_for_pty_before_running_the_command() {
    let temp = TempDirectory::new("openssh-exec-pty");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let allowed = write_config(&temp.0, "host", &user);
    let denied = temp.0.join("deny-pty-config");
    fs::write(
        &denied,
        fs::read_to_string(&allowed)
            .unwrap()
            .replace("PermitTTY yes", "PermitTTY no"),
    )
    .unwrap();
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoint, &temp.0, "host");
    let open = || {
        SshSession::connect_exec_pty_cancellable(
            Connect {
                address,
                endpoint: endpoint.clone(),
                username: user.clone(),
                authentication: Authentication::private_key(
                    fs::read(temp.0.join("client")).unwrap(),
                ),
                term: "xterm-256color".into(),
                size: WindowSize {
                    columns: 117,
                    rows: 35,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            trust.clone(),
            &[],
            "test -t 0 && test -t 1 && test -t 2 || exit 99; stty size; printf '\\033[32mpty-stdout\\033[0m\\n'; printf 'pty-stderr\\n' >&2; exit 7",
            || false,
        )
    };
    let server = serve(&listener, &denied);
    assert!(matches!(
        open(),
        Err(ConnectError::RequestRejected { request: "pty-req" })
    ));
    let log = server.join().unwrap().stderr;
    let log = String::from_utf8_lossy(&log);
    assert!(log.contains("request pty-req"), "{log}");
    assert!(!log.contains("request exec"), "{log}");

    let server = serve(&listener, &allowed);
    let mut session = open().unwrap();
    assert!(session.take_stderr().is_none());
    let mut reader = session.take_reader().unwrap();
    let bytes = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.lines().any(|line| line == "35 117"), "{text}");
    assert!(text.contains("\x1b[32mpty-stdout\x1b[0m"), "{text}");
    assert!(text.lines().any(|line| line == "pty-stderr"), "{text}");
    assert_eq!(session.exit_code(), Some(7));
    session.close().unwrap();
    server.join().unwrap();
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_dedicated_exec_pty_requires_confirmation_and_keeps_ordinary_exec_available() {
    let temp = TempDirectory::new("openssh-dedicated-exec-pty");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = || fixture_target(&temp.0, &user, "client", &listener);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &target().endpoint, &temp.0, "host");
    let allowed = write_config(&temp.0, "host", &user);
    let denied = temp.0.join("deny-pty-config");
    fs::write(
        &denied,
        fs::read_to_string(&allowed)
            .unwrap()
            .replace("PermitTTY yes", "PermitTTY no"),
    )
    .unwrap();
    let connect = || {
        ExecConnection::connect_preconnected(
            TcpStream::connect(listener.local_addr().unwrap()).unwrap(),
            (target(), Vec::new()),
            trust.clone(),
            ConnectOptions::default(),
            || false,
            || Ok(()),
        )
        .unwrap()
    };

    let daemon = serve(&listener, &denied);
    let connection = connect();
    let opener = connection.exec_opener().unwrap();
    assert!(matches!(
        opener.open_pty_cancellable("printf unexpected", || false),
        Err(ConnectError::RequestRejected { request: "pty-req" })
    ));
    drop(connection);
    let log = daemon.join().unwrap().stderr;
    let log = String::from_utf8_lossy(&log);
    assert!(log.contains("request pty-req"), "{log}");
    assert!(!log.contains("request exec"), "{log}");

    let daemon = serve(&listener, &allowed);
    let connection = connect();
    let opener = connection.exec_opener().unwrap();
    let mut pty = opener
        .open_pty_cancellable(
            r#"test -t 0 && test -t 1 && test -t 2 || exit 91
case " $(stty -a) " in *" -echo "*) ;; *) exit 92;; esac
printf 'ready\n' >&2
IFS= read -r credential && [ "$credential" = fixture ] || exit 93
IFS= read -r frame && [ "$frame" = first ] || exit 94
printf 'first-ok\n'
IFS= read -r frame && [ "$frame" = second ] || exit 95
printf 'second-ok\n'
exit 17"#,
            || false,
        )
        .unwrap();
    assert!(pty.take_stderr().is_none());
    let mut output = pty.take_reader().unwrap();
    let mut ready = [0; 6];
    let mut offset = 0;
    while offset < ready.len() {
        let count = output
            .read_timeout(&mut ready[offset..], Duration::from_secs(5))
            .unwrap();
        assert_ne!(count, 0);
        offset += count;
    }
    assert_eq!(&ready, b"ready\n");

    let mut ordinary = opener
        .open_cancellable("test ! -t 0 && printf 'ordinary\n'", || false)
        .unwrap();
    let mut ordinary_reader = ordinary.take_reader().unwrap();
    assert_eq!(
        collect_output(move || {
            let mut bytes = Vec::new();
            ordinary_reader.read_to_end(&mut bytes).unwrap();
            bytes
        }),
        b"ordinary\n"
    );
    assert_eq!(ordinary.exit_code(), Some(0));

    let mut writer = pty
        .take_sensitive_exec_input()
        .unwrap()
        .send_then(SensitiveLine::new("fixture").unwrap(), || true)
        .unwrap();
    writer.write_all(b"first\nsecond\n").unwrap();
    assert_eq!(
        collect_output(move || {
            let mut bytes = Vec::new();
            output.read_to_end(&mut bytes).unwrap();
            bytes
        }),
        b"first-ok\nsecond-ok\n"
    );
    assert_eq!(pty.exit_code(), Some(17));
    drop(connection);
    let log = daemon.join().unwrap().stderr;
    let log = String::from_utf8_lossy(&log);
    assert_eq!(log.matches("Accepted publickey").count(), 1, "{log}");
    assert_eq!(log.matches("request pty-req").count(), 1, "{log}");
    assert_eq!(log.matches("request exec").count(), 2, "{log}");
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_shell_resize_and_changed_key_before_userauth() {
    let temp = TempDirectory::new("openssh");
    for name in ["host", "changed", "client"] {
        let output = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(temp.0.join(name))
            .output()
            .unwrap();
        assert_success(&output);
    }
    let username = Command::new("id").arg("-un").output().unwrap();
    assert_success(&username);
    let username = String::from_utf8(username.stdout).unwrap();
    let username = username.trim();
    let first = write_config(&temp.0, "host", username);
    let changed = write_config(&temp.0, "changed", username);
    let key = fs::read(temp.0.join("client")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let store = temp.0.join("pins");
    let strict = TrustStore::new(store.clone(), TrustMode::Strict);
    let connect = || Connect {
        address,
        endpoint: endpoint.clone(),
        username: username.to_owned(),
        authentication: Authentication::private_key(key.clone()),
        term: "xterm-256color".into(),
        size: WindowSize {
            columns: 83,
            rows: 32,
            pixel_width: 810,
            pixel_height: 610,
        },
    };

    let server = serve(&listener, &first);
    let offered = match SshSession::connect(connect(), strict.clone()) {
        Err(ConnectError::UnknownHostKey {
            endpoint: actual,
            offered,
        }) => {
            assert_eq!(actual, endpoint);
            offered
        }
        Err(error) => panic!("expected unknown host key, got {error}"),
        Ok(_) => panic!("unknown host key was accepted"),
    };
    assert_no_auth(&server.join().unwrap());
    let server_key = fs::read_to_string(temp.0.join("host.pub")).unwrap();
    assert_eq!(offered, HostKey::from_openssh(server_key.trim()).unwrap());
    assert_eq!(
        strict.check(&endpoint, &offered).unwrap(),
        HostKeyStatus::Unknown
    );
    assert_eq!(
        strict.remember(&endpoint, &offered).unwrap(),
        HostKeyStatus::Known
    );

    let server = serve(&listener, &first);
    let mut config = connect();
    config.address.set_port(0);
    let mut session = SshSession::connect_addresses(config, strict.clone(), &[address]).unwrap();
    let mut reader = session.take_reader().unwrap();
    let mut writer = session.take_writer().unwrap();
    session
        .resize(WindowSize {
            columns: 99,
            rows: 41,
            pixel_width: 900,
            pixel_height: 700,
        })
        .unwrap();
    writer
        .write_all(
            b"printf '\\nbegin-grid\\n'; stty size; printf 'bulk-start\\n'; \
              head -c 524288 /dev/zero | tr '\\000' x; printf '\\nbulk-end\\n'; exit 7\n",
        )
        .unwrap();
    writer.flush().unwrap();
    // Give the remote stream an opportunity to fill the bounded read queue
    // before the reader starts. This also exceeds a full SSH receive window.
    thread::sleep(Duration::from_millis(50));
    let (output, events) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .send(reader.read_to_end(&mut bytes).map(|_| bytes))
            .unwrap();
    });
    let bytes = events
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    reader.join().unwrap();
    let output = String::from_utf8_lossy(&bytes);
    assert!(
        output.lines().any(|line| line.trim() == "41 99"),
        "missing remote dimensions"
    );
    let bulk = output.lines().max_by_key(|line| line.len()).unwrap().trim();
    assert_eq!(bulk.len(), 512 * 1024);
    assert!(bulk.bytes().all(|byte| byte == b'x'));
    assert!(output.lines().any(|line| line.trim() == "bulk-end"));
    assert_eq!(session.exit_code(), Some(7));
    session.close().unwrap();
    let served = server.join().unwrap();
    assert!(String::from_utf8_lossy(&served.stderr).contains("Accepted publickey"));

    let server = serve(&listener, &first);
    let mut exec = SshSession::connect_exec(
        connect(),
        strict.clone(),
        &[],
        "if test -t 0 || test -t 1 || test -t 2; then exit 71; fi; \
         cat; printf 'stderr-marker\\n' >&2; exit 23",
    )
    .unwrap();
    let mut stdout = exec.take_reader().unwrap();
    let mut stderr = exec.take_stderr().unwrap();
    let mut stdin = exec.take_writer().unwrap();
    let input = b"binary\0input\r\n";
    stdin.write_all(input).unwrap();
    stdin.close_input().unwrap();
    assert!(stdin.write_all(b"after EOF").is_err());
    let (output, received) = mpsc::channel();
    let errors = output.clone();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        output.send((false, result)).unwrap();
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.read_to_end(&mut bytes).map(|_| bytes);
        errors.send((true, result)).unwrap();
    });
    for _ in 0..2 {
        let (is_stderr, data) = received.recv_timeout(Duration::from_secs(10)).unwrap();
        let data = data.unwrap();
        if is_stderr {
            assert!(data.ends_with(b"stderr-marker\n"));
        } else {
            assert_eq!(data, input);
        }
    }
    out.join().unwrap();
    err.join().unwrap();
    assert_eq!(exec.exit_code(), Some(23));
    exec.close().unwrap();
    server.join().unwrap();

    let server = serve(&listener, &first);
    let mut subsystem =
        SshSession::connect_subsystem(connect(), strict.clone(), &[], "sftp").unwrap();
    assert!(subsystem.take_stderr().is_none());
    let mut writer = subsystem.take_writer().unwrap();
    let mut reader = subsystem.take_reader().unwrap();
    let (reply, received) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut sftp = Sftp::new(&mut reader, &mut writer).unwrap();
        let directory = sftp.canonicalize(".").unwrap();
        let attributes = sftp.stat(".").unwrap();
        reply.send((directory, attributes.kind())).unwrap();
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).unwrap(), 0);
    });
    let (directory, kind) = received.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(directory.starts_with(b"/"));
    assert_eq!(kind, FileKind::Directory);
    subsystem.abort_handle().abort();
    subsystem.close().unwrap();
    worker.join().unwrap();
    server.join().unwrap();

    let server = serve(&listener, &first);
    assert!(matches!(
        SshSession::connect_subsystem(connect(), strict.clone(), &[], "missing"),
        Err(ConnectError::RequestRejected {
            request: "subsystem"
        })
    ));
    let refused = server.join().unwrap();
    let log = String::from_utf8_lossy(&refused.stderr);
    assert!(log.contains("subsystem request for missing"), "{log}");
    assert!(!log.contains("Starting session:"), "{log}");

    let accept_new = TrustStore::new(store, TrustMode::AcceptNew);
    for trust in [strict, accept_new] {
        let server = serve(&listener, &changed);
        match SshSession::connect(connect(), trust) {
            Err(ConnectError::ChangedHostKey {
                endpoint: actual,
                expected,
                offered: different,
            }) => {
                assert_eq!(actual, endpoint);
                assert_eq!(expected, offered);
                assert_ne!(different, offered);
            }
            Err(error) => panic!("expected changed host key, got {error}"),
            Ok(_) => panic!("changed host key was accepted"),
        }
        assert_no_auth(&server.join().unwrap());
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_direct_channels_preserve_half_close_and_reuse_on_one_connection() {
    let forwarding_limit = u8::try_from(MAX_FORWARD_STREAMS).unwrap();
    let fixture_streams = 7 + forwarding_limit;
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let ssh = TcpListener::bind("127.0.0.1:0").unwrap();
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    let service_port = service.local_addr().unwrap().port();
    let config = write_config_with_key(
        &temp.0,
        "host",
        &user,
        "client",
        &format!("PermitOpen 127.0.0.1:{service_port}\n"),
    );
    let service_worker = thread::spawn(move || {
        let mut clients = Vec::new();
        for _ in 0..fixture_streams {
            let (mut socket, _) = service.accept().unwrap();
            clients.push(thread::spawn(move || {
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                socket.write_all(b"remote-half-close").unwrap();
                socket.shutdown(Shutdown::Write).unwrap();
                let mut reply = Vec::new();
                socket.read_to_end(&mut reply).unwrap();
                reply
            }));
        }
        clients
            .into_iter()
            .map(|client| client.join().unwrap())
            .collect::<Vec<_>>()
    });

    let address = ssh.local_addr().unwrap();
    let target = fixture_target(&temp.0, &user, "client", &ssh);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &target.endpoint, &temp.0, "host");
    let server = serve(&ssh, &config);
    let mut session = match SshSession::connect(
        Connect {
            address,
            endpoint: target.endpoint,
            username: target.username,
            authentication: target.authentication,
            term: "xterm-256color".into(),
            size: fixture_size(),
        },
        trust,
    ) {
        Ok(session) => session,
        Err(error) => {
            let output = server.join().unwrap();
            panic!(
                "Could not connect to the OpenSSH fixture ({error:?}): {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    let opener = session.direct_opener();
    let denied = opener.open(Endpoint::new("localhost.", service_port).unwrap(), address);
    assert!(
        matches!(denied, Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused)
    );

    let destination = Endpoint::new("127.0.0.1", service_port).unwrap();
    let held = opener.open(destination.clone(), address).unwrap();
    for index in 0..6 {
        exchange_forward(opener.open(destination.clone(), address).unwrap(), index);
    }
    exchange_forward(held, 6);

    let mut forwards = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while forwards.len() < usize::from(forwarding_limit) {
        match opener.open(destination.clone(), address) {
            Ok(channel) => forwards.push(channel),
            Err(ConnectError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("could not reach the shared forwarding limit: {error}"),
        }
    }
    assert!(matches!(
        opener.open(destination, address),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    let mut sftp_lease = session.sftp_opener().unwrap().open().unwrap();
    let mut sftp = Sftp::new(
        sftp_lease.take_reader().unwrap(),
        sftp_lease.take_writer().unwrap(),
    )
    .unwrap();
    assert!(sftp.canonicalize(".").unwrap().starts_with(b"/"));
    let mut exec = session
        .exec_opener()
        .unwrap()
        .open_cancellable("printf reserved; exit 0", || false)
        .unwrap();
    let _ = exec.take_writer().unwrap().close_input();
    let mut exec_output = exec.take_reader().unwrap();
    assert_eq!(
        collect_output(move || {
            let mut output = Vec::new();
            exec_output.read_to_end(&mut output).unwrap();
            output
        }),
        b"reserved"
    );
    assert_eq!(exec.exit_code(), Some(0));
    drop((exec, sftp, sftp_lease));
    for (index, channel) in (7..fixture_streams).zip(forwards) {
        exchange_forward(channel, index);
    }
    let mut writer = session.take_writer().unwrap();
    let mut reader = session.take_reader().unwrap();
    writer
        .write_all(b"HISTFILE=/dev/null; printf '\\nshell-still-open\\n'; exit 8\n")
        .unwrap();
    let output = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert!(
        String::from_utf8_lossy(&output)
            .lines()
            .any(|line| line.trim() == "shell-still-open")
    );
    assert_eq!(session.exit_code(), Some(8));
    session.close().unwrap();
    assert!(opener.is_closed());
    let mut replies = service_worker.join().unwrap();
    replies.sort();
    assert_eq!(
        replies,
        (0..fixture_streams)
            .map(|index| vec![b'a' + index])
            .collect::<Vec<_>>()
    );
    let server_log = server.join().unwrap().stderr;
    assert!(String::from_utf8_lossy(&server_log).contains("localhost."));
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_jump_chain_verifies_and_authenticates_each_endpoint_separately() {
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(
        &temp.0,
        &[
            "entry",
            "middle",
            "target",
            "rotated",
            "entry-user",
            "middle-user",
            "target-user",
        ],
    );
    let entry = TcpListener::bind("127.0.0.1:0").unwrap();
    let middle = TcpListener::bind("127.0.0.1:0").unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    let entry_config = write_config_with_key(&temp.0, "entry", &user, "entry-user", "");
    let middle_config = write_config_with_key(&temp.0, "middle", &user, "middle-user", "");
    let target_config = write_config_with_key(&temp.0, "target", &user, "target-user", "");
    // Route trust remains strict even if a caller supplies an accept-new store.
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::AcceptNew);
    let route = || JumpRoute {
        address: entry.local_addr().unwrap(),
        additional: Vec::new(),
        jumps: vec![
            fixture_target(&temp.0, &user, "entry-user", &entry),
            fixture_target(&temp.0, &user, "middle-user", &middle),
        ],
        target: fixture_target(&temp.0, &user, "target-user", &destination),
    };
    let endpoints = route();
    pin(&trust, &endpoints.jumps[0].endpoint, &temp.0, "entry");
    pin(&trust, &endpoints.jumps[1].endpoint, &temp.0, "middle");

    let entry_server = serve(&entry, &entry_config);
    let middle_server = serve(&middle, &middle_config);
    let target_server = serve(&destination, &target_config);
    let offered =
        match SshSession::connect_via(route(), trust.clone(), "xterm".into(), fixture_size()) {
            Err(ConnectError::UnknownHostKey { endpoint, offered }) => {
                assert_eq!(endpoint, endpoints.target.endpoint);
                offered
            }
            Err(error) => panic!("unexpected target error {error}"),
            Ok(_) => panic!("the route accepted an unpinned target key"),
        };
    assert_no_auth(&target_server.join().unwrap());
    entry_server.join().unwrap();
    middle_server.join().unwrap();
    assert_eq!(
        trust.check(&endpoints.target.endpoint, &offered).unwrap(),
        HostKeyStatus::Unknown
    );
    pin(&trust, &endpoints.target.endpoint, &temp.0, "target");

    let entry_server = serve(&entry, &entry_config);
    let middle_server = serve(&middle, &middle_config);
    let target_server = serve(&destination, &target_config);
    let mut target =
        SshSession::connect_via(route(), trust.clone(), "xterm".into(), fixture_size()).unwrap();
    let mut reader = target.take_reader().unwrap();
    let mut writer = target.take_writer().unwrap();
    writer
        .write_all(b"HISTFILE=/dev/null; printf '\\njump-shell\\n'; exit 9\n")
        .unwrap();
    let output = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert!(
        String::from_utf8_lossy(&output)
            .lines()
            .any(|line| line.trim() == "jump-shell")
    );
    assert_eq!(target.exit_code(), Some(9));
    target.close().unwrap();
    for daemon in [entry_server, middle_server, target_server] {
        assert!(
            String::from_utf8_lossy(&daemon.join().unwrap().stderr).contains("Accepted publickey")
        );
    }

    let rotated = write_config_with_key(&temp.0, "rotated", &user, "middle-user", "");
    let entry_server = serve(&entry, &entry_config);
    let middle_server = serve(&middle, &rotated);
    assert!(matches!(
        SshSession::connect_via(route(), trust.clone(), "xterm".into(), fixture_size()),
        Err(ConnectError::ChangedHostKey { endpoint, .. }) if endpoint == endpoints.jumps[1].endpoint
    ));
    assert_no_auth(&middle_server.join().unwrap());
    entry_server.join().unwrap();

    let entry_server = serve(&entry, &entry_config);
    let middle_server = serve(&middle, &middle_config);
    let target_server = serve(&destination, &target_config);
    let mut subsystem = SshSession::connect_subsystem_via(route(), trust, "sftp").unwrap();
    let reader = subsystem.take_reader().unwrap();
    let writer = subsystem.take_writer().unwrap();
    let directory = collect_output(move || {
        let mut sftp = Sftp::new(reader, writer).unwrap();
        sftp.canonicalize(".").unwrap()
    });
    assert!(directory.starts_with(b"/"));
    subsystem.close().unwrap();
    for daemon in [entry_server, middle_server, target_server] {
        daemon.join().unwrap();
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_rsa_sha2_jump_chain_pins_and_authenticates_each_endpoint() {
    let temp = TempDirectory::new("openssh-rsa-jump");
    let user = fixture_user();
    let hops = [
        ("entry", "entry-user", "rsa-sha2-512", "rsa-sha2-256"),
        ("middle", "middle-user", "rsa-sha2-256", "rsa-sha2-512"),
        ("target", "target-user", "rsa-sha2-512", "rsa-sha2-256"),
    ];
    let [entry, middle, target] = hops.map(|_| TcpListener::bind("127.0.0.1:0").unwrap());
    let configs = hops.map(|(host, key, host_algorithm, user_algorithm)| {
        generate_rsa_key(&temp.0, host);
        generate_rsa_key(&temp.0, key);
        let algorithms = format!(
            "HostKeyAlgorithms {host_algorithm}\nPubkeyAcceptedAlgorithms {user_algorithm}\n"
        );
        write_config_with_key(&temp.0, host, &user, key, &algorithms)
    });
    let route = || JumpRoute {
        address: entry.local_addr().unwrap(),
        additional: Vec::new(),
        jumps: vec![
            fixture_target(&temp.0, &user, hops[0].1, &entry),
            fixture_target(&temp.0, &user, hops[1].1, &middle),
        ],
        target: fixture_target(&temp.0, &user, hops[2].1, &target),
    };
    let read_public_key = |name| {
        let key = fs::read_to_string(temp.0.join(format!("{name}.pub"))).unwrap();
        HostKey::from_openssh(key.trim()).unwrap()
    };
    let endpoints = route();
    let entry_key = read_public_key(hops[0].0);
    let middle_key = read_public_key(hops[1].0);
    let middle_endpoint = &endpoints.jumps[1].endpoint;
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoints.jumps[0].endpoint, &temp.0, hops[0].0);
    pin(&trust, middle_endpoint, &temp.0, hops[0].0);
    pin(&trust, &endpoints.target.endpoint, &temp.0, hops[2].0);

    let entry_server = serve(&entry, &configs[0]);
    let middle_server = serve(&middle, &configs[1]);
    let rejected = SshSession::connect_via(route(), trust.clone(), "xterm".into(), fixture_size());
    assert!(matches!(
        rejected,
        Err(ConnectError::ChangedHostKey { endpoint, expected, offered })
            if &endpoint == middle_endpoint && expected == entry_key && offered == middle_key
    ));
    assert_no_auth(&middle_server.join().unwrap());
    entry_server.join().unwrap();
    assert!(
        trust
            .replace_pin(middle_endpoint, &entry_key, &middle_key)
            .unwrap()
    );

    let servers = [&entry, &middle, &target]
        .into_iter()
        .zip(&configs)
        .map(|(listener, config)| serve(listener, config))
        .collect::<Vec<_>>();
    let mut session =
        SshSession::connect_via(route(), trust, "xterm".into(), fixture_size()).unwrap();
    let mut reader = session.take_reader().unwrap();
    session
        .take_writer()
        .unwrap()
        .write_all(b"HISTFILE=/dev/null; printf '\\nrsa-jump-ok\\n'; exit 9\n")
        .unwrap();
    let received = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert!(
        String::from_utf8_lossy(&received)
            .lines()
            .any(|line| line.trim() == "rsa-jump-ok")
    );
    assert_eq!(session.exit_code(), Some(9));
    session.close().unwrap();
    for ((host, key, host_algorithm, _), server) in hops.into_iter().zip(servers) {
        let output = server.join().unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        let fingerprint = read_public_key(key).fingerprint();
        assert!(
            log.contains(&format!("kex: host key algorithm: {host_algorithm}")),
            "{host}: {log}"
        );
        assert!(
            log.lines()
                .any(|line| line.contains("Accepted publickey") && line.contains(&fingerprint)),
            "{host}: {log}"
        );
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_exec_only_transport_rechecks_authority_without_starting_a_shell() {
    let temp = TempDirectory::new("openssh-exec-only");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = write_config(&temp.0, "host", &user);
    let target = || fixture_target(&temp.0, &user, "client", &listener);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &target().endpoint, &temp.0, "host");
    let connect = |authorize: &dyn Fn() -> std::io::Result<()>| {
        ExecConnection::connect_preconnected(
            TcpStream::connect(listener.local_addr().unwrap()).unwrap(),
            (target(), Vec::new()),
            trust.clone(),
            ConnectOptions::default(),
            || false,
            authorize,
        )
    };
    let denied = || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let server = serve(&listener, &config);
    assert!(
        matches!(connect(&denied), Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert_no_auth(&server.join().unwrap());

    let server = serve(&listener, &config);
    let auth_checks = std::cell::Cell::new(0);
    let connection = connect(&|| {
        auth_checks.set(auth_checks.get() + 1);
        Ok(())
    })
    .unwrap();
    assert!(auth_checks.get() >= 2);
    let opener = connection.exec_opener().unwrap();
    let unexpected = temp.0.join("unauthorized-command");
    let command = format!("touch '{}'", unexpected.display());
    assert!(matches!(
        opener.open_guarded(&command, || false, denied),
        Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    for (command, expected, code) in [
        ("printf 'first\\n'", b"first\n".as_slice(), 0),
        ("printf 'second\\n'; exit 7", b"second\n".as_slice(), 7),
    ] {
        let mut lease = opener.open_guarded(command, || false, || Ok(())).unwrap();
        let mut reader = lease.take_reader().unwrap();
        let bytes = collect_output(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        assert_eq!(bytes, expected);
        assert_eq!(lease.exit_code(), Some(code));
    }
    drop(connection);
    let output = server.join().unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert_eq!(log.matches("Accepted publickey").count(), 1, "{log}");
    assert_eq!(log.matches("request exec").count(), 2, "{log}");
    assert!(!log.contains("request shell"), "{log}");
    assert!(!unexpected.exists());
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_exec_only_rechecks_authority_before_signing_private_keys() {
    let temp = TempDirectory::new("openssh-guarded-signing");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "ed25519"]);
    generate_rsa_key(&temp.0, "rsa");
    for bits in ["256", "384", "521"] {
        assert_success(
            &Command::new("ssh-keygen")
                .args(["-q", "-t", "ecdsa", "-b", bits, "-N", "", "-f"])
                .arg(temp.0.join(format!("ecdsa{bits}")))
                .output()
                .unwrap(),
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &endpoint, &temp.0, "host");
    for (key, algorithm) in [
        ("ed25519", "ssh-ed25519"),
        ("ecdsa256", "ecdsa-sha2-nistp256"),
        ("ecdsa384", "ecdsa-sha2-nistp384"),
        ("ecdsa521", "ecdsa-sha2-nistp521"),
        ("rsa", "rsa-sha2-256"),
        ("rsa", "rsa-sha2-512"),
    ] {
        let config = write_config_with_key(
            &temp.0,
            "host",
            &user,
            key,
            &format!("PubkeyAcceptedAlgorithms {algorithm}\n"),
        );
        let mut denied_public_key_requests = 0;
        for allow_signature in [false, true] {
            let server = serve(&listener, &config);
            let checks = std::cell::Cell::new(0);
            let result = ExecConnection::connect_preconnected(
                TcpStream::connect(address).unwrap(),
                (fixture_target(&temp.0, &user, key, &listener), Vec::new()),
                trust.clone(),
                ConnectOptions::default(),
                || false,
                || {
                    checks.set(checks.get() + 1);
                    if checks.get() == 3 && !allow_signature {
                        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                    }
                    Ok(())
                },
            );
            assert_eq!(checks.get(), 3, "{algorithm}");
            if allow_signature {
                drop(result.unwrap_or_else(|error| panic!("{algorithm}: {error}")));
            } else {
                assert!(
                    matches!(result, Err(ConnectError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied),
                    "{algorithm}"
                );
            }
            let output = server.join().unwrap();
            let log = String::from_utf8_lossy(&output.stderr);
            assert!(log.contains("Postponed publickey"), "{algorithm}: {log}");
            let requests = log
                .lines()
                .filter(|line| {
                    line.contains("userauth-request for user ") && line.contains("method publickey")
                })
                .count();
            if allow_signature {
                assert!(log.contains("Accepted publickey"), "{algorithm}: {log}");
                assert_eq!(
                    requests,
                    denied_public_key_requests + 1,
                    "{algorithm}: {log}"
                );
            } else {
                assert!(!log.contains("Accepted publickey"), "{algorithm}: {log}");
                denied_public_key_requests = requests;
            }
        }
    }
}

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_remote_forward_confirms_bind_half_closes_and_releases_listener() {
    let temp = TempDirectory::new("openssh");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let ssh = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = write_config_with_key(
        &temp.0,
        "host",
        &user,
        "client",
        "AllowTcpForwarding yes\nGatewayPorts no\n",
    );
    let target = fixture_target(&temp.0, &user, "client", &ssh);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &target.endpoint, &temp.0, "host");
    let server = serve(&ssh, &config);
    let mut session = SshSession::connect(
        Connect {
            address: ssh.local_addr().unwrap(),
            endpoint: target.endpoint,
            username: target.username,
            authentication: target.authentication,
            term: "xterm".into(),
            size: fixture_size(),
        },
        trust,
    )
    .unwrap();
    let opener = session.remote_opener();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(matches!(
        opener.bind(occupied.local_addr().unwrap()),
        Err(ConnectError::RequestRejected {
            request: "tcpip-forward"
        })
    ));
    let requested = "127.0.0.1:0".parse().unwrap();
    let first = opener.clone();
    let second = opener.clone();
    let first = thread::spawn(move || first.bind(requested).unwrap());
    let second = thread::spawn(move || second.bind(requested).unwrap());
    let mut remote = first.join().unwrap();
    let mut other = second.join().unwrap();
    assert_ne!(remote.remote_addr().port(), 0);
    assert_ne!(remote.remote_addr(), other.remote_addr());

    let mut tcp = TcpStream::connect(remote.remote_addr()).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let connection = remote
        .accept_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(connection.origin, tcp.local_addr().unwrap());
    assert!(other.accept_timeout(Duration::ZERO).unwrap().is_none());
    let mut channel = connection.channel;
    let abort = channel.abort_handle();
    let mut from_remote = channel.take_reader().unwrap();
    let mut to_remote = channel.take_writer().unwrap();
    tcp.write_all(b"from the remote socket").unwrap();
    tcp.shutdown(Shutdown::Write).unwrap();
    let received = collect_output(move || {
        let mut bytes = Vec::new();
        from_remote.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert_eq!(received, b"from the remote socket");
    assert!(!abort.is_closed());
    to_remote.write_all(b"response after remote EOF").unwrap();
    to_remote.close_input().unwrap();
    let mut response = Vec::new();
    tcp.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"response after remote EOF");

    let mut live = TcpStream::connect(remote.remote_addr()).unwrap();
    live.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let live_channel = remote
        .accept_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .channel;
    let live_abort = live_channel.abort_handle();
    remote.close().unwrap();
    assert!(live_abort.is_closed());
    assert_eq!(live.read(&mut [0; 1]).unwrap(), 0);
    assert!(remote.accept_timeout(Duration::ZERO).is_err());
    assert_eq!(
        TcpStream::connect(remote.remote_addr()).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused,
    );

    let last = TcpStream::connect(other.remote_addr()).unwrap();
    let last_channel = other
        .accept_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .channel;
    assert_eq!(last.peer_addr().unwrap(), other.remote_addr());
    let last_abort = last_channel.abort_handle();
    session.close().unwrap();
    assert!(last_abort.is_closed());
    assert!(opener.is_closed());
    assert!(other.close().is_err());
    assert!(other.accept_timeout(Duration::ZERO).is_err());
    server.join().unwrap();
    assert_eq!(
        TcpStream::connect(other.remote_addr()).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused,
    );
}

fn exchange_forward(mut channel: DirectChannel, index: u8) {
    let abort = channel.abort_handle();
    let mut reader = channel.take_reader().unwrap();
    let mut writer = channel.take_writer().unwrap();
    let bytes = collect_output(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    });
    assert_eq!(bytes, b"remote-half-close");
    assert!(!abort.is_closed());
    writer.write_all(&[b'a' + index]).unwrap();
    writer.close_input().unwrap();
    // Wait for the remote CLOSE before starting the next channel.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !abort.is_closed() {
        assert!(
            std::time::Instant::now() < deadline,
            "remote direct channel did not close"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn collect_output(work: impl FnOnce() -> Vec<u8> + Send + 'static) -> Vec<u8> {
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || send.send(work()).unwrap());
    let result = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.join().unwrap();
    result
}

fn fixture_user() -> String {
    let user = Command::new("id").arg("-un").output().unwrap();
    assert_success(&user);
    String::from_utf8(user.stdout).unwrap().trim().into()
}

fn fixture_target(dir: &Path, user: &str, key: &str, listener: &TcpListener) -> SshTarget {
    SshTarget {
        endpoint: Endpoint::new("127.0.0.1", listener.local_addr().unwrap().port()).unwrap(),
        username: user.into(),
        authentication: Authentication::private_key(fs::read(dir.join(key)).unwrap()),
    }
}

fn fixture_size() -> WindowSize {
    WindowSize {
        columns: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn generate_keys(dir: &Path, names: &[&str]) {
    for name in names {
        let output = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(dir.join(name))
            .output()
            .unwrap();
        assert_success(&output);
    }
}

fn generate_rsa_key(dir: &Path, name: &str) {
    assert_success(
        &Command::new("ssh-keygen")
            .args(["-q", "-t", "rsa", "-b", "2048", "-N", "", "-f"])
            .arg(dir.join(name))
            .output()
            .unwrap(),
    );
}

fn pin(trust: &TrustStore, endpoint: &Endpoint, dir: &Path, host: &str) {
    let key = fs::read_to_string(dir.join(format!("{host}.pub"))).unwrap();
    let key = HostKey::from_openssh(key.trim()).unwrap();
    assert_eq!(
        trust.remember(endpoint, &key).unwrap(),
        HostKeyStatus::Known
    );
}

fn write_config(dir: &Path, host: &str, username: &str) -> PathBuf {
    write_config_with_key(dir, host, username, "client", "")
}

fn write_config_with_key(
    dir: &Path,
    host: &str,
    username: &str,
    key: &str,
    extra: &str,
) -> PathBuf {
    let path = dir.join(format!("{host}-config"));
    let crypto = match std::env::var("KORTTY_OPENSSH_TEST_CRYPTO").ok().as_deref() {
        Some("hybrid") => {
            "KexAlgorithms mlkem768x25519-sha256\nCiphers chacha20-poly1305@openssh.com\n"
        }
        Some("classic") => {
            "KexAlgorithms curve25519-sha256\nCiphers aes256-ctr\nMACs hmac-sha2-256\n"
        }
        None => "",
        Some(_) => panic!("unexpected SSH test crypto preset"),
    };
    fs::write(
        &path,
        format!(
            "HostKey {}\nAuthorizedKeysFile {}\nAllowUsers {username}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
             UsePAM no\nStrictModes yes\nLogLevel DEBUG2\nPrintMotd no\nPrintLastLog no\nPermitTTY yes\n\
             Subsystem sftp internal-sftp\n{crypto}{extra}AllowTcpForwarding local\n",
            dir.join(host).display(),
            dir.join(format!("{key}.pub")).display()
        ),
    )
    .unwrap();
    path
}

fn serve(listener: &TcpListener, config: &Path) -> thread::JoinHandle<Output> {
    let listener = listener.try_clone().unwrap();
    let config = config.to_owned();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let input: OwnedFd = stream.into();
        let output = input.try_clone().unwrap();
        Command::new("/usr/sbin/sshd")
            .args(["-ie", "-f"])
            .arg(config)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    })
}

fn assert_no_auth(result: &Output) {
    let log = String::from_utf8_lossy(&result.stderr);
    assert!(log.contains("Connection from"), "{log}");
    assert!(!log.contains("userauth-request"), "{log}");
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
