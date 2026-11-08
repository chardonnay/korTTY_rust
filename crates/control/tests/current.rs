use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kortty_control::{Method, Request, Server};
use kortty_pty::Pty;
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: usize = 128 * 1024;
const CLI: &str = env!("CARGO_BIN_EXE_kortty-cli");

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        #[cfg(unix)]
        let temporary = PathBuf::from("/tmp");
        #[cfg(windows)]
        let temporary = std::env::temp_dir();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = temporary.join(format!("kt-current-{:016x}", u64::from_ne_bytes(nonce)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        }
        #[cfg(windows)]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn next_request(server: &Server, output: Option<&Mutex<Vec<u8>>>) -> Request {
    let requests = server.requests();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match requests.try_recv() {
            Ok(request) => return request,
            Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                let terminal =
                    output.map_or_else(Vec::new, |output| output.lock().unwrap().clone());
                panic!(
                    "CLI request did not arrive: {error}; terminal: {}",
                    String::from_utf8_lossy(&terminal)
                );
            }
        }
    }
}

#[cfg(unix)]
fn shell_command(cli: &str, profile: &Path) -> String {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    format!(
        "{} --config-dir {} pane get --current\n",
        quote(cli),
        quote(profile.to_str().unwrap())
    )
}

#[cfg(windows)]
fn shell_command(cli: &str, profile: &Path) -> String {
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if powershell.is_file() {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
        format!(
            "& {} --config-dir {} pane get --current\r",
            quote(cli),
            quote(profile.to_str().unwrap())
        )
    } else {
        format!(
            "\"{cli}\" --config-dir \"{}\" pane get --current\r",
            profile.display()
        )
    }
}

#[test]
fn cli_current_uses_real_pty_ancestry_and_fails_without_a_match() {
    // A native runner may transfer a cross-built executable to another path.
    let cli = std::env::var("KORTTY_CONTROL_TEST_CLI").unwrap_or_else(|_| CLI.into());
    assert!(Path::new(&cli).is_file(), "missing test CLI at {cli}");
    let profile = Profile::new();
    let server = Server::start(&profile.0).unwrap();
    let mut pty = Pty::spawn(120, 24).unwrap();
    let shell = pty.shell_pid();
    let mut reader = pty.take_reader().unwrap();
    let collected = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&collected);
    let drain = thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            let mut output = output.lock().unwrap();
            let retained = count.min(MAX_OUTPUT.saturating_sub(output.len()));
            output.extend_from_slice(&bytes[..retained]);
        }
    });
    let mut writer = pty.take_writer().unwrap();
    writer
        .write_all(shell_command(&cli, &profile.0).as_bytes())
        .unwrap();

    // Test double for desktop dispatch. The CLI, OS ancestry, PTY and
    // authenticated request/reply transport are real.
    let resolve = next_request(&server, Some(&collected));
    assert_eq!(resolve.method, Method::Resolve);
    let pids = resolve.params.resolve_pids().unwrap();
    assert_ne!(pids.first().copied(), Some(shell));
    assert!(pids.contains(&shell), "{pids:?} lacks PTY shell {shell}");
    resolve.reply.send(Ok(json!({
        "pane": {"pane_id": "p11111111", "shell_pid": shell}, "matched_pid": shell
    })));

    let get = next_request(&server, Some(&collected));
    assert_eq!(get.method, Method::Pane);
    assert_eq!(get.params.string("pane").unwrap(), Some("p11111111"));
    get.reply.send(Ok(json!({"native_current_proof": true})));
    let deadline = Instant::now() + TIMEOUT;
    let marker = b"native_current_proof";
    loop {
        let output = collected.lock().unwrap();
        if output.windows(marker.len()).any(|part| part == marker) {
            break;
        }
        assert!(
            output.len() < MAX_OUTPUT && Instant::now() < deadline,
            "CLI response did not reach PTY: {}",
            String::from_utf8_lossy(&output)
        );
        drop(output);
        thread::sleep(Duration::from_millis(5));
    }
    thread::scope(|scope| {
        let external = scope.spawn(|| {
            Command::new(&cli)
                .arg("--config-dir")
                .arg(&profile.0)
                .args(["pane", "get", "--current"])
                .output()
                .unwrap()
        });
        let resolve = next_request(&server, None);
        assert_eq!(resolve.method, Method::Resolve);
        assert!(!resolve.params.resolve_pids().unwrap().contains(&shell));
        resolve
            .reply
            .send(Ok(json!({"pane": null, "matched_pid": null})));
        let outside = external.join().unwrap();
        assert_eq!(outside.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&outside.stderr).contains("--current needs"));
        assert!(server.requests().is_empty());
    });
    pty.shutdown().unwrap();
    drain.join().unwrap();
    server.stop();
}
