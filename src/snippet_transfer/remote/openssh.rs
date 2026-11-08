//! Opt-in integration tests against an isolated OpenSSH server.

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::process::Output;
use std::time::Instant;

use kortty_ssh::{Authentication, Connect, Endpoint, SshSession, WindowSize};

use super::*;
use crate::ssh_worker::{OpenSshFixture, dial_test_executable, test_executable};

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_terminal_snippet_transfer() {
    let fixture = OpenSshFixture::new();
    let key = fixture.directory().join("client");
    let target = fixture.request(key.clone()).target.endpoint;
    let server = fixture.serve_with_extra("host", "");
    let mut shell = SshSession::connect(
        Connect {
            address: (Ipv4Addr::LOCALHOST, target.port).into(),
            endpoint: Endpoint::new(&target.host, target.port).unwrap(),
            username: target.username,
            authentication: Authentication::private_key(fs::read(key).unwrap()),
            term: "xterm".into(),
            size: WindowSize {
                columns: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        fixture.pinned_trust(),
    )
    .unwrap();
    let mut input = shell.take_writer().unwrap();
    let mut output = shell.take_reader().unwrap();
    exercise(
        &fixture,
        SftpOpener::Direct(shell.sftp_opener().unwrap()),
        &mut input,
        |bytes, timeout| output.read_timeout(bytes, timeout),
    );
    assert_eq!(shell.exit_code(), None);
    shell.close().unwrap();
    verify_single_login(server.join().unwrap());
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_snippet_transfer() {
    let fixture = OpenSshFixture::new();
    let server = fixture.serve_with_extra("host", "");
    let mut shell = dial_test_executable(
        fixture.request(fixture.directory().join("client")),
        false,
        &test_executable(),
        &fixture.pinned_trust(),
    )
    .unwrap();
    let mut input = shell.take_writer().unwrap();
    let mut output = shell.take_reader().unwrap();
    exercise(
        &fixture,
        SftpOpener::Worker(shell.opener().unwrap()),
        &mut input,
        |bytes, timeout| output.read_timeout(bytes, timeout),
    );
    assert_eq!(shell.exit_code(), None);
    shell.close().unwrap();
    verify_single_login(server.join().unwrap());
}

fn exercise(
    fixture: &OpenSshFixture,
    opener: SftpOpener,
    input: &mut dyn Write,
    mut read_shell: impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
) {
    let cwd = fixture.directory().canonicalize().unwrap().join("working");
    fs::create_dir(&cwd).unwrap();
    let quoted = cwd.to_str().unwrap().replace('\'', "'\\''");
    writeln!(
        input,
        "HISTFILE=/dev/null; cd '{quoted}' && printf '\\n%s:%s\\n' 'snippet-cwd' \"$(pwd -P)\""
    )
    .unwrap();
    wait_for_shell(
        &mut read_shell,
        format!("snippet-cwd:{}", cwd.display()).as_bytes(),
    );
    let confirmed_cwd = || Some(cwd.as_os_str().as_bytes().to_vec());
    let cancelled = AtomicBool::new(false);
    let existing = cwd.join("existing.sh");
    fs::write(&existing, "keep me\n").unwrap();
    let layout = Layout {
        directories: vec!["empty".into(), "nested".into()],
        entries: vec![
            entry("existing.sh", "#!/bin/sh\necho updated\n", 0o755),
            entry("nested/new.txt", "new snippet\n", 0o644),
        ],
    };
    let prepare_layout =
        |layout: &Layout| prepare(opener.clone(), confirmed_cwd(), layout, &cancelled).unwrap();
    let plan = prepare_layout(&layout);
    assert_eq!(plan.target(), display_path(cwd.as_os_str().as_bytes()));
    assert_eq!(plan.conflicts(), &["existing.sh"]);
    assert!(plan.can_overwrite());
    let skip = plan
        .run(&layout, Choice::Skip, &cancelled, &mut |_| {})
        .unwrap();
    assert_eq!((skip.written, skip.skipped, skip.cancelled), (1, 1, false));
    assert_eq!(fs::read_to_string(&existing).unwrap(), "keep me\n");
    assert_eq!(
        fs::read_to_string(cwd.join("nested/new.txt")).unwrap(),
        "new snippet\n"
    );
    assert_eq!(mode_of(&cwd.join("nested/new.txt")), 0o644);
    let directory_mode = mode_of(&cwd.join("empty"));
    assert_eq!(directory_mode & 0o700, 0o700);
    assert_eq!(directory_mode & !0o755, 0);
    assert_eq!(fs::read_dir(cwd.join("empty")).unwrap().count(), 0);

    let overwrite = prepare_layout(&layout)
        .run(&layout, Choice::Overwrite, &cancelled, &mut |_| {})
        .unwrap();
    assert_eq!((overwrite.written, overwrite.skipped), (2, 0));
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "#!/bin/sh\necho updated\n"
    );
    assert_eq!(mode_of(&existing), 0o755);

    let late = Layout {
        directories: Vec::new(),
        entries: vec![entry("late.txt", "proposed\n", 0o644)],
    };
    let plan = prepare_layout(&late);
    assert!(plan.conflicts().is_empty());
    fs::write(cwd.join("late.txt"), "concurrent file\n").unwrap();
    assert!(
        matches!(plan.run(&late, Choice::Copy, &cancelled, &mut |_| {}),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(
        fs::read_to_string(cwd.join("late.txt")).unwrap(),
        "concurrent file\n"
    );

    let link = Layout {
        directories: Vec::new(),
        entries: vec![entry("link.txt", "forbidden\n", 0o644)],
    };
    symlink(&existing, cwd.join("link.txt")).unwrap();
    let plan = prepare_layout(&link);
    assert_eq!(plan.conflicts(), &["link.txt"]);
    assert!(!plan.can_overwrite());
    let skipped = plan
        .run(&link, Choice::Skip, &cancelled, &mut |_| {})
        .unwrap();
    assert_eq!((skipped.written, skipped.skipped), (0, 1));
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "#!/bin/sh\necho updated\n"
    );

    for directory in [&cwd, &cwd.join("nested")] {
        assert!(fs::read_dir(directory).unwrap().all(|item| {
            !item
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".kortty-")
        }));
    }
    writeln!(input, "printf '\\n%s:%s\\n' 'snippet-copy' 'shell-alive'").unwrap();
    wait_for_shell(&mut read_shell, b"snippet-copy:shell-alive");
}

fn entry(path: &str, content: &str, mode: u32) -> Entry {
    Entry {
        path: path.into(),
        content: content.into(),
        mode,
    }
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn wait_for_shell(
    read: &mut impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
    expected: &[u8],
) {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut output = Vec::new();
    let mut bytes = [0; 4096];
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        assert!(
            !timeout.is_zero() && output.len() < 128 * 1024,
            "{:?}",
            String::from_utf8_lossy(&output)
        );
        let count = read(&mut bytes, timeout).unwrap();
        assert_ne!(count, 0);
        output.extend_from_slice(&bytes[..count]);
        if output
            .windows(expected.len())
            .any(|window| window == expected)
        {
            return;
        }
    }
}

fn verify_single_login(server: Output) {
    let log = String::from_utf8_lossy(&server.stderr);
    assert_eq!(log.matches("Accepted publickey for").count(), 1, "{log}");
}
