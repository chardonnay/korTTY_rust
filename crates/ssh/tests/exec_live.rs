//! Explicit live-host acceptance for the terminal and authenticated exec channels.

use std::fs;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kortty_ssh::{
    Authentication, Connect, Endpoint, ExecOpener, HostKey, SshReader, SshSession, TrustMode,
    TrustStore, WindowSize,
};

const INITIAL: WindowSize = WindowSize {
    columns: 132,
    rows: 43,
    pixel_width: 0,
    pixel_height: 0,
};

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_ADDRESS, USER, PRIVATE_KEY and HOST_KEY fixtures"]
fn live_exec_preserves_the_authenticated_terminal_and_separate_outputs() {
    let address: SocketAddr = required("ADDRESS").parse().unwrap();
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let directory = Scratch::new();
    let trust = TrustStore::new(directory.0.join("pins"), TrustMode::Strict);
    trust
        .remember(
            &endpoint,
            &HostKey::from_openssh(&required("HOST_KEY")).unwrap(),
        )
        .unwrap();
    let mut shell = SshSession::connect(
        Connect {
            address,
            endpoint,
            username: required("USER"),
            authentication: Authentication::private_key(fs::read(required("PRIVATE_KEY")).unwrap()),
            term: "xterm".into(),
            size: INITIAL,
        },
        trust,
    )
    .unwrap();
    let opener = shell.exec_opener().unwrap();
    let mut input = shell.take_writer().unwrap();
    let mut output = shell.take_reader().unwrap();
    assert_shell_size(&mut input, &mut output, INITIAL);
    let resized = WindowSize {
        columns: 113,
        rows: 41,
        ..INITIAL
    };
    shell.resize(resized).unwrap();
    assert_shell_size(&mut input, &mut output, resized);

    let mut exec = opener
        .open_cancellable("printf 'exec-out'; printf 'exec-err' >&2; exit 23", || {
            false
        })
        .unwrap();
    exec.take_writer().unwrap().close_input().unwrap();
    assert_eq!(read_all(exec.take_reader().unwrap()), b"exec-out");
    assert_eq!(read_all(exec.take_stderr().unwrap()), b"exec-err");
    let deadline = Instant::now() + Duration::from_secs(5);
    while exec.exit_code().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(exec.exit_code(), Some(23));
    drop(exec);
    assert_shell_usable(&mut input, &mut output, "after-exec");

    let mut pending = reopen(&opener, "printf ready; sleep 1; printf finished");
    let mut pending_output = pending.take_reader().unwrap();
    let mut ready = [0; 5];
    read_exact_before(&mut pending_output, &mut ready);
    assert_eq!(&ready, b"ready");
    pending.close();
    drop(pending);
    assert_shell_usable(&mut input, &mut output, "after-abort");

    // The aborted fixture is finite even on servers that keep a non-PTY child
    // alive after channel close. Leave no remote test process behind.
    std::thread::sleep(Duration::from_millis(1200));
    let mut next = reopen(&opener, "printf reopened");
    next.take_writer().unwrap().close_input().unwrap();
    assert_eq!(read_all(next.take_reader().unwrap()), b"reopened");
    drop(next);
    shell.close().unwrap();
    assert!(opener.is_closed());
}

fn required(suffix: &str) -> String {
    std::env::var(format!("KORTTY_SSH_EXEC_{suffix}"))
        .expect("provide the explicit live SSH fixture")
}

fn reopen(opener: &ExecOpener, command: &str) -> kortty_ssh::ExecLease {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match opener.open_cancellable(command, || false) {
            Ok(lease) => return lease,
            Err(kortty_ssh::ConnectError::Io(error))
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("could not reopen the authenticated exec channel: {error}"),
        }
    }
}

fn read_all(mut reader: SshReader) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "exec output timed out");
        let count = reader.read_timeout(&mut buffer, left).unwrap();
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
        assert!(output.len() <= 4096);
    }
}

fn read_exact_before(reader: &mut SshReader, mut target: &mut [u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !target.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "exec marker timed out");
        let count = reader.read_timeout(target, left).unwrap();
        assert_ne!(count, 0, "exec channel closed before its marker");
        target = &mut target[count..];
    }
}

fn assert_shell_usable(input: &mut impl Write, output: &mut SshReader, suffix: &str) {
    writeln!(
        input,
        "HISTFILE=/dev/null; printf '%s%s\\n' 'kortty-' '{suffix}'"
    )
    .unwrap();
    input.flush().unwrap();
    expect_shell_marker(output, &format!("kortty-{suffix}"));
}

fn assert_shell_size(input: &mut impl Write, output: &mut SshReader, size: WindowSize) {
    writeln!(
        input,
        "HISTFILE=/dev/null; printf '%s%s%s\\n' 'kortty-grid:' \"$(stty size)\" ':end'"
    )
    .unwrap();
    input.flush().unwrap();
    expect_shell_marker(
        output,
        &format!("kortty-grid:{} {}:end", size.rows, size.columns),
    );
    println!(
        "Server reported PTY size {} columns by {} rows",
        size.columns, size.rows
    );
}

fn expect_shell_marker(output: &mut SshReader, wanted: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed = Vec::new();
    let mut buffer = [0; 2048];
    while !observed
        .windows(wanted.len())
        .any(|bytes| bytes == wanted.as_bytes())
    {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "terminal marker {wanted:?} timed out");
        let count = output
            .read_timeout(&mut buffer, left)
            .unwrap_or_else(|error| {
                panic!("could not read terminal marker {wanted:?}: {error}");
            });
        assert_ne!(count, 0, "terminal closed with its exec channel");
        observed.extend_from_slice(&buffer[..count]);
        assert!(observed.len() <= 64 * 1024);
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("kortty-exec-live-{:x}", u128::from_le_bytes(nonce)));
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder.create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
