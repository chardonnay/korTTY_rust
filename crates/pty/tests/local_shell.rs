use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kortty_pty::{LaunchOptions, LocalShell, Pty, Sandbox};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_MARKER: &[u8] = b"kortty_pty_wait";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kortty-pty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn runs_resizes_and_reaps_a_local_shell() {
    check_shell(
        Pty::spawn(80, 24).expect("start a real shell"),
        "xterm-256color",
    );
}

#[test]
fn sets_selected_term_in_the_shell() {
    check_shell(
        Pty::spawn_with_term(80, 24, "vt100").expect("start a real VT100 shell"),
        "vt100",
    );
}

#[cfg(windows)]
#[test]
fn windows_presets_run_distinct_system_shells() {
    for (shell, command, marker) in [
        (
            LocalShell::PowerShell,
            b"if ($PSVersionTable.PSEdition -eq 'Desktop') { [Console]::WriteLine(('kortty-native-' + 'powershell')); exit 0 }; exit 17\r".as_slice(),
            "kortty-native-powershell",
        ),
        (
            LocalShell::CommandPrompt,
            b"echo kortty-native-^cmd\rexit /b 0\r".as_slice(),
            "kortty-native-cmd",
        ),
    ] {
        let mut pty = Pty::spawn_with_options(
            80,
            24,
            "xterm-256color",
            &LaunchOptions {
                shell,
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        let (status, output) = run_shell(&mut pty, command);
        assert!(status.success() && output.contains(marker), "{shell:?}: {status} {output}");
    }
}

#[cfg(not(windows))]
#[test]
fn an_explicit_windows_shell_is_unavailable_on_other_hosts() {
    let result = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            shell: LocalShell::CommandPrompt,
            ..LaunchOptions::default()
        },
    );
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn working_directory_starts_a_separate_real_process() {
    let directory = TestDirectory::new();
    let mut pty = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(directory.0.clone()),
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    assert_ne!(pty.shell_pid(), std::process::id());
    assert_eq!(pty.sandbox_backend(), None);
    #[cfg(unix)]
    {
        let pid = pty.shell_pid() as libc::pid_t;
        // SAFETY: the live child PID is read without modifying its state.
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        // SAFETY: the live child PID is read without modifying its state.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    }
    #[cfg(unix)]
    let command = b"printf '%s' directory-test > cwd-proof.txt; exit\n";
    #[cfg(windows)]
    let command = b"[IO.File]::WriteAllText((Join-Path (Get-Location) 'cwd-proof.txt'), 'directory-test'); exit\r";
    let (status, _) = run_shell(&mut pty, command);
    assert!(status.success());
    assert_eq!(
        fs::read(directory.0.join("cwd-proof.txt")).unwrap(),
        b"directory-test"
    );
}

#[cfg(unix)]
#[test]
fn terminal_file_directory_tracks_cd_and_rejects_a_replaced_shell() {
    let root = TestDirectory::new();
    let root_path = root.0.canonicalize().unwrap();
    let next = root_path.join("next");
    fs::create_dir(&next).unwrap();
    let mut pty = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(root_path.clone()),
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    // zsh waits for pending terminal output before switching input modes.
    let mut reader = pty.take_reader().unwrap();
    let drain = thread::spawn(move || io::copy(&mut reader, &mut io::sink()));
    let mut writer = pty.take_writer().unwrap();
    let until_directory = |pty: &mut Pty, expected: &std::path::Path| {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            let actual = pty.shell_directory();
            if actual.as_ref().is_ok_and(|path| path == expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected:?}, got {actual:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    };
    until_directory(&mut pty, &root_path);
    writer.write_all(b"cd next\n").unwrap();
    until_directory(&mut pty, &next);
    writer.write_all(b"exec /bin/sleep 8\n").unwrap();
    let deadline = Instant::now() + TEST_TIMEOUT;
    while pty.shell_directory().is_ok() {
        assert!(
            Instant::now() < deadline,
            "replacement process was accepted"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        pty.try_wait().unwrap().is_none(),
        "the replacement must still be running"
    );
    pty.shutdown().unwrap();
    drain.join().unwrap().unwrap();
}

#[test]
fn sandbox_requires_an_explicit_workspace() {
    let directory = TestDirectory::new();
    let error = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            sandbox: Some(Sandbox {
                profile_root: directory.0.clone(),
            }),
            ..LaunchOptions::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn unavailable_required_sandbox_is_refused_without_an_unrestricted_shell() {
    let directory = TestDirectory::new();
    let error = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(directory.0.clone()),
            sandbox: Some(Sandbox {
                profile_root: directory.0.join("profile"),
            }),
            ..LaunchOptions::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
#[ignore = "requires an unprivileged Linux user and Landlock ABI 6 or later"]
fn linux_sandbox_restricts_real_profile_keys_ipc_and_writes() {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::ptr;

    const PROBE_FD: &str = "KORTTY_TEST_SANDBOX_PROBE_FD";
    const PROBE_SHM: &str = "KORTTY_TEST_SANDBOX_PROBE_SHM";

    struct SharedMemory(libc::c_int);

    impl Drop for SharedMemory {
        fn drop(&mut self) {
            // SAFETY: this is our segment ID. IPC_RMID ignores the buffer.
            unsafe { libc::shmctl(self.0, libc::IPC_RMID, ptr::null_mut()) };
        }
    }

    fn probe_ip_and_threads() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut sender = TcpStream::connect_timeout(&address, TEST_TIMEOUT).unwrap();
        let (mut receiver, _) = listener.accept().unwrap();
        thread::spawn(move || sender.write_all(b"ok").unwrap())
            .join()
            .unwrap();
        let mut received = [0_u8; 2];
        receiver.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"ok");

        if let Err(error) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) {
            assert!(
                matches!(
                    error.raw_os_error(),
                    Some(libc::EAFNOSUPPORT | libc::EPROTONOSUPPORT | libc::EADDRNOTAVAIL)
                ),
                "IPv6 socket unexpectedly failed: {error}"
            );
        }
    }

    if let Some(descriptor) = std::env::var_os(PROBE_FD) {
        // This branch runs the same test executable inside the real sandbox.
        let descriptor = descriptor.to_str().unwrap().parse::<libc::c_int>().unwrap();
        // SAFETY: F_GETFD only queries the numeric descriptor.
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        assert_eq!(
            UnixStream::connect("host.sock").unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );

        // SAFETY: this valid, unprivileged netlink socket call returns an owned fd on success.
        let socket = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE) };
        if socket >= 0 {
            // SAFETY: the successful socket call returned a new descriptor.
            drop(unsafe { OwnedFd::from_raw_fd(socket) });
            panic!("sandbox allowed a netlink socket");
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

        let segment = std::env::var(PROBE_SHM).unwrap().parse().unwrap();
        // SAFETY: segment is an existing test ID. The kernel selects the mapping address.
        let mapping = unsafe { libc::shmat(segment, ptr::null(), libc::SHM_RDONLY) };
        if mapping as isize != -1 {
            // SAFETY: the unexpected success returned the attached address.
            unsafe { libc::shmdt(mapping) };
            panic!("sandbox attached host System V shared memory");
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

        // SAFETY: the queue name is a C string, and O_RDONLY needs no variadic arguments.
        let queue = unsafe { libc::mq_open(c"/kortty-sandbox-probe".as_ptr(), libc::O_RDONLY) };
        if queue >= 0 {
            // SAFETY: the unexpected success returned an owned message queue descriptor.
            unsafe { libc::mq_close(queue) };
            panic!("sandbox opened a POSIX message queue");
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        probe_ip_and_threads();
        return;
    }

    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let profile = directory.0.join("profile");
    let profile_alias = directory.0.join("profile-alias");
    let ssh = directory.0.join("keys");
    for path in [&workspace, &profile, &ssh] {
        fs::create_dir(path).unwrap();
    }
    symlink(&profile, &profile_alias).unwrap();
    let secret = profile.join("secret");
    let key = ssh.join("ssh-key");
    fs::write(&secret, b"private profile").unwrap();
    fs::write(&key, b"private key").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&secret, workspace.join("profile-link")).unwrap();
    let _host_socket = UnixListener::bind(workspace.join("host.sock")).unwrap();
    fs::copy(
        std::env::current_exe().unwrap(),
        workspace.join("runtime-probe"),
    )
    .unwrap();
    let secret_handle = fs::File::open(&secret).unwrap();
    // SAFETY: F_DUPFD duplicates our live file descriptor without setting CLOEXEC.
    let inherited = unsafe { libc::fcntl(secret_handle.as_raw_fd(), libc::F_DUPFD, 64) };
    assert!(inherited >= 64);
    // SAFETY: F_DUPFD returned a new owned descriptor.
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    // SAFETY: IPC_PRIVATE creates a new segment for this test and cannot access another one.
    let segment = unsafe { libc::shmget(libc::IPC_PRIVATE, 1, libc::IPC_CREAT | 0o600) };
    assert!(segment >= 0, "{}", io::Error::last_os_error());
    let shared_memory = SharedMemory(segment);

    let options = LaunchOptions {
        working_directory: Some(workspace.clone()),
        sandbox: Some(Sandbox {
            profile_root: profile_alias,
        }),
        ..LaunchOptions::default()
    };
    let mut pty = Pty::spawn_with_options(80, 24, "xterm-256color", &options).unwrap();
    assert_eq!(pty.sandbox_backend(), Some("landlock"));
    let command = format!(
        concat!(
            "test \"$KORTTY_SANDBOX\" = landlock || exit 10; ",
            "test \"$HOME\" = \"$TMPDIR\" && test \"$HISTFILE\" = \"$TMPDIR/history\" || exit 11; ",
            "if cat '{}' >/dev/null 2>&1; then exit 12; fi; ",
            "if cat '{}' >/dev/null 2>&1; then exit 13; fi; ",
            "if cat profile-link >/dev/null 2>&1; then exit 14; fi; ",
            "if (printf changed > '{}') 2>/dev/null; then exit 15; fi; ",
            "if chmod 644 '{}' 2>/dev/null; then exit 16; fi; ",
            "if kill -0 {} 2>/dev/null; then exit 17; fi; ",
            "{}={} {}={} ./runtime-probe --ignored --exact linux_sandbox_restricts_real_profile_keys_ipc_and_writes || exit 21; ",
            "printf '%s' workspace-ok > proof || exit 18; ",
            "printf '%s' scratch-ok > \"$TMPDIR/proof\" || exit 19; ",
            "printf '%s' \"$TMPDIR\" > scratch-path || exit 20; exit 0\n",
        ),
        secret.display(),
        key.display(),
        secret.display(),
        secret.display(),
        std::process::id(),
        PROBE_FD,
        inherited.as_raw_fd(),
        PROBE_SHM,
        shared_memory.0,
    );
    let (status, output) = run_shell(&mut pty, command.as_bytes());
    assert!(
        status.success(),
        "sandbox probe failed: {status:?}, {output:?}"
    );
    assert_eq!(fs::read(workspace.join("proof")).unwrap(), b"workspace-ok");
    assert_eq!(fs::read(&secret).unwrap(), b"private profile");
    assert_eq!(
        fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let scratch = PathBuf::from(fs::read_to_string(workspace.join("scratch-path")).unwrap());
    assert!(scratch.is_absolute());
    assert!(!scratch.exists());
    assert!(!scratch.parent().unwrap().exists());

    let error = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(directory.0.clone()),
            ..options
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

fn run_shell(pty: &mut Pty, command: &[u8]) -> (std::process::ExitStatus, String) {
    let mut reader = pty.take_reader().unwrap();
    let mut writer = pty.take_writer().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let drain = thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output).map(|_| output);
        let _ = send.send(result);
    });
    writer.write_all(command).unwrap();
    let output = receive.recv_timeout(TEST_TIMEOUT).unwrap().unwrap();
    let status = wait_for_exit(pty);
    pty.shutdown().unwrap();
    drain.join().unwrap();
    (status, String::from_utf8_lossy(&output).into_owned())
}

#[cfg(windows)]
#[test]
fn uses_conpty_when_the_launcher_redirects_standard_handles() {
    use std::process::{Command, Stdio};

    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "runs_resizes_and_reaps_a_local_shell"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the shell test with redirected standard handles");
    assert!(
        output.status.success(),
        "redirected launch failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn check_shell(mut pty: Pty, expected_term: &str) {
    let mut reader = pty.take_reader().expect("output handle");
    let mut writer = pty.take_writer().expect("input handle");
    assert!(pty.take_reader().is_none());
    assert!(pty.take_writer().is_none());

    let (output_sender, output_receiver) = mpsc::sync_channel(1);
    let drain = thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output).map(|_| output);
        let _ = output_sender.send(result);
    });

    pty.resize(98, 31).expect("resize the real terminal");
    #[cfg(unix)]
    let command = b"stty size; printf 'kortty_term=%s\\n' \"$TERM\"; printf '%s%s\\n' 'kortty_' 'pty_marker'; exit 7\n";
    #[cfg(windows)]
    let command = b"$s=$Host.UI.RawUI.WindowSize; Write-Output \"$($s.Height) $($s.Width)\"; Write-Output ('kortty_term='+$env:TERM); Write-Output ('kortty_'+'pty_marker'); exit 7\r";
    writer
        .write_all(command)
        .expect("send a command to the shell");

    let output = output_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("terminal output reached EOF")
        .expect("read shell output");
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("31 98"), "wrong terminal size: {output:?}");
    assert!(
        output.contains(&format!("kortty_term={expected_term}")),
        "wrong child TERM: {output:?}"
    );
    assert!(
        output.contains("kortty_pty_marker"),
        "command did not run: {output:?}"
    );
    assert_eq!(wait_for_exit(&mut pty).code(), Some(7));
    pty.shutdown().expect("clean up after natural shell exit");
    drain.join().expect("reader thread exited");
}

#[test]
fn shutdown_interrupts_terminal_io() {
    let mut pty = Pty::spawn(80, 24).expect("start a real shell");
    let mut reader = pty.take_reader().expect("output handle");
    let mut writer = pty.take_writer().expect("input handle");
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let (read_sender, read_receiver) = mpsc::sync_channel(1);
    let drain = thread::spawn(move || {
        let mut ready_sender = Some(ready_sender);
        let mut recent = Vec::new();
        let mut buffer = [0; 4096];
        let result = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(size) => {
                    if ready_sender.is_some() {
                        recent.extend_from_slice(&buffer[..size]);
                        if recent
                            .windows(WAIT_MARKER.len())
                            .any(|bytes| bytes == WAIT_MARKER)
                        {
                            if let Some(sender) = ready_sender.take() {
                                let _ = sender.send(());
                            }
                        } else if recent.len() > WAIT_MARKER.len() {
                            recent.drain(..recent.len() - WAIT_MARKER.len());
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = read_sender.send(result);
    });

    #[cfg(unix)]
    let command = b"stty raw -echo; printf '%s%s\\n' 'kortty_' 'pty_wait'; exec sleep 60\n";
    #[cfg(windows)]
    let command = b"Write-Output ('kortty_'+'pty_wait'); Start-Sleep -Seconds 60\r";
    writer
        .write_all(command)
        .expect("start a shell command that will block");
    ready_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("shell command started");

    let (write_sender, write_receiver) = mpsc::sync_channel(1);
    let write = thread::spawn(move || {
        let result = writer.write_all(&vec![b'x'; 16 * 1024 * 1024]);
        let _ = write_sender.send(result);
    });
    let early_write = write_receiver.recv_timeout(Duration::from_millis(75));
    #[cfg(unix)]
    assert!(matches!(early_write, Err(mpsc::RecvTimeoutError::Timeout)));
    #[cfg(windows)]
    if let Ok(result) = &early_write {
        assert!(result.is_ok(), "writer failed before shutdown: {result:?}");
    }

    pty.shutdown()
        .expect("shutdown cancels blocked I/O and reaps the child");
    pty.shutdown().expect("shutdown is repeatable");
    assert!(pty.try_wait().expect("query exit status").is_some());
    assert_eq!(
        pty.resize(80, 24).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    read_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("reader woke up")
        .expect("reader reached EOF");
    if matches!(early_write, Err(mpsc::RecvTimeoutError::Timeout)) {
        let result = write_receiver
            .recv_timeout(TEST_TIMEOUT)
            .expect("writer woke up");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
    drain.join().expect("reader thread exited");
    write.join().expect("writer thread exited");
}

#[test]
fn rejects_invalid_dimensions_before_starting_a_shell() {
    for (columns, rows) in [(0, 24), (80, 0), (u16::MAX, 24), (80, u16::MAX)] {
        assert_eq!(
            Pty::spawn(columns, rows).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn rejects_unsupported_or_unsafe_term_names() {
    for term in ["", "dumb", "VT100", "-vt100", "vt100\n", "vt100\0", "vţ100"] {
        let result = Pty::spawn_with_term(80, 24, term);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }
}

fn wait_for_exit(pty: &mut Pty) -> std::process::ExitStatus {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(status) = pty.try_wait().expect("query exit status") {
            return status;
        }
        assert!(Instant::now() < deadline, "shell did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}
