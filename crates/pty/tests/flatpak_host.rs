#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kortty_pty::{LaunchOptions, LocalShell, Pty, Sandbox, flatpak};

const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "requires an installed Flatpak bundle with a private host test environment"]
fn packaged_flatpak_uses_host_pty_and_supervised_commands() {
    assert!(flatpak::running());
    assert!(std::env::var_os("KORTTY_FLATPAK_HOST_VALUE").is_none());
    let base = PathBuf::from(std::env::var_os("KORTTY_FLATPAK_HOST_TEST_ROOT").unwrap());
    assert!(base.is_absolute() && base.join("private-flatpak-test").is_file());
    let workspace = base.join("workspace with 'quotes'");
    let canonical = flatpak::shared_directory(&workspace).unwrap();
    assert_eq!(canonical, workspace);
    assert!(flatpak::shared_directory(Path::new("/usr")).is_err());

    let mut custom = Pty::spawn_with_options(
        100,
        35,
        "xterm-256color",
        &LaunchOptions {
            command: Some(vec![
                "kortty-flatpak-test-tool".into(),
                "literal with spaces; $HOME ' \"".into(),
                "-no-option-parsing".into(),
            ]),
            working_directory: Some(workspace.clone()),
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    assert!(custom.uses_flatpak_host());
    assert!(custom.job_process_probe().is_none());
    assert!(custom.shell_directory().is_err());
    eprintln!(
        "Owned Flatpak host command launcher PID {}",
        custom.shell_pid()
    );
    let output = capture(&mut custom);
    let text = until(&output, b"host-command-complete");
    assert!(text.contains("host-only-env"));
    assert!(text.contains("literal with spaces; $HOME ' \""));
    assert!(text.contains("-no-option-parsing"));
    assert!(text.contains(workspace.to_str().unwrap()));
    assert_eq!(wait(&mut custom), 23);
    custom.shutdown().unwrap();

    let mut shell = Pty::spawn_with_options(
        100,
        35,
        "xterm-256color",
        &LaunchOptions {
            shell: LocalShell::Default,
            working_directory: Some(workspace.clone()),
            shell_integration: true,
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    eprintln!(
        "Owned Flatpak host shell launcher PID {}",
        shell.shell_pid()
    );
    assert!(shell.uses_flatpak_host());
    let output = capture(&mut shell);
    let before = until(&output, b"\x1b]133;B\x07");
    assert!(before.contains("flatpak-login-startup"));
    assert!(before.contains("\x1b]7;file://"));
    let mut input = shell.take_writer().unwrap();
    input
        .write_all(b"test -t 0 && test -w /dev/tty && printf '%s-%s\n' host interactive\n")
        .unwrap();
    assert!(until(&output, b"\x1b]133;B\x07").contains("host-interactive"));
    input
        .write_all(b"sleep 300 & echo $! > host-shell-child.pid; printf '%s-%s\n' job running\n")
        .unwrap();
    assert!(until(&output, b"\x1b]133;B\x07").contains("job-running"));
    shell.shutdown().unwrap();
    drop(input);
    assert!(fs::read_to_string(workspace.join("host-shell-child.pid")).is_ok());

    let (child, mut lease) = flatpak::spawn_approved_command(
        &workspace,
        "test ! -e /.flatpak-info || exit 19; printf '%s\n' \"$KORTTY_FLATPAK_HOST_VALUE\"; exit 47",
    )
    .unwrap();
    eprintln!("Owned Flatpak Agent command launcher PID {}", child.id());
    let output = child.wait_with_output().unwrap();
    assert_eq!(lease.exit_code().unwrap(), Some(47), "{output:?}");
    assert_eq!(output.stdout, b"host-only-env\n", "{output:?}");

    let (mut child, lease) = flatpak::spawn_approved_command(
        &workspace,
        "printf '%s\n' \"$$\" > host-agent.pid; trap '' INT HUP TERM; sleep 300",
    )
    .unwrap();
    eprintln!(
        "Owned cancellable Flatpak Agent launcher PID {}",
        child.id()
    );
    let deadline = Instant::now() + TIMEOUT;
    while !workspace.join("host-agent.pid").is_file() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(workspace.join("host-agent.pid").is_file());
    drop(lease);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("The supervised host command survived cancellation");
        }
        thread::sleep(Duration::from_millis(20));
    }

    verify_required_landlock(&base, workspace);
}

fn verify_required_landlock(base: &Path, workspace: PathBuf) {
    let profile = base.join("private-profile");
    let secret = profile.join("secret");
    fs::write(&secret, b"private profile").unwrap();
    fs::write(base.join("outside"), b"outside workspace").unwrap();
    std::os::unix::fs::symlink(&secret, workspace.join("profile-link")).unwrap();
    let options = LaunchOptions {
        working_directory: Some(workspace.clone()),
        sandbox: Some(Sandbox {
            profile_root: profile,
        }),
        ..LaunchOptions::default()
    };
    let mut confined = Pty::spawn_with_options(80, 24, "xterm-256color", &options).unwrap();
    assert!(!confined.uses_flatpak_host());
    assert_eq!(confined.sandbox_backend(), Some("landlock"));
    eprintln!("Owned Landlock PTY PID {}", confined.shell_pid());
    let output = capture(&mut confined);
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    let script = format!(
        concat!(
            "test \"$KORTTY_SANDBOX\" = landlock && test \"$HOME\" = \"$TMPDIR\" || exit 31; ",
            "cat /etc/passwd /etc/resolv.conf >/dev/null && (: <>/dev/tty) || exit 32; ",
            "if cat ../private-profile/secret >/dev/null 2>&1; then exit 33; fi; ",
            "if cat profile-link >/dev/null 2>&1; then exit 34; fi; ",
            "if cat ../outside >/dev/null 2>&1; then exit 35; fi; ",
            "if (printf corrupt > ../outside) 2>/dev/null; then exit 36; fi; ",
            "if DBUS_SESSION_BUS_ADDRESS='{}' /usr/bin/flatpak-spawn --host --watch-bus ",
            "--directory=/ /bin/true >/dev/null 2>&1; then exit 37; fi; ",
            "printf '%s' workspace-ok > proof && printf '%s' scratch-ok > \"$TMPDIR/proof\" ",
            "&& printf '%s' \"$TMPDIR\" > scratch-path || exit 38; exit 0\n",
        ),
        bus.replace('\'', "'\\''"),
    );
    confined
        .take_writer()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let status = wait(&mut confined);
    confined.shutdown().unwrap();
    assert_eq!(
        status,
        0,
        "{}",
        String::from_utf8_lossy(&output.try_iter().flatten().collect::<Vec<_>>())
    );
    assert_eq!(fs::read(workspace.join("proof")).unwrap(), b"workspace-ok");
    assert_eq!(
        fs::read(base.join("outside")).unwrap(),
        b"outside workspace"
    );
    assert_eq!(fs::read(&secret).unwrap(), b"private profile");
    let scratch = PathBuf::from(fs::read_to_string(workspace.join("scratch-path")).unwrap());
    assert!(scratch.starts_with("/tmp"));
    assert!(!scratch.parent().unwrap().exists());

    let error = Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(base.to_owned()),
            ..options
        },
    )
    .err()
    .expect("the sandbox must reject a workspace containing the profile");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    eprintln!(
        "Landlock enforced workspace and scratch access, profile isolation and blocked host execution"
    );
}

fn capture(pty: &mut Pty) -> mpsc::Receiver<Vec<u8>> {
    let mut reader = pty.take_reader().unwrap();
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0; 4096];
        while let Ok(count @ 1..) = reader.read(&mut buf) {
            if send.send(buf[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    receive
}

fn until(stream: &mpsc::Receiver<Vec<u8>>, marker: &[u8]) -> String {
    let deadline = Instant::now() + TIMEOUT;
    let mut output = Vec::new();
    while !output.windows(marker.len()).any(|bytes| bytes == marker) {
        match stream.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(bytes) => output.extend(bytes),
            Err(error) => panic!("{error}: {}", String::from_utf8_lossy(&output)),
        }
        assert!(output.len() < 128 * 1024);
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn wait(pty: &mut Pty) -> i32 {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = pty.try_wait().unwrap() {
            return status.code().expect("expected a normal PTY exit code");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("The Flatpak PTY process did not exit");
}
