use std::io::{BufRead as _, BufReader};
use std::process::Child;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

const TEST: &str = "parent_exit::tests::tracks_native_parent_exit";
const ROLE: &str = "KORTTY_TEST_PARENT_EXIT_ROLE";
const TIMEOUT: Duration = Duration::from_secs(8);
const READY: &str = "parent-exit-watcher-ready";
const DONE: &str = "parent-exit-watcher-done";

#[test]
fn tracks_native_parent_exit() {
    match std::env::var(ROLE).as_deref() {
        Ok("watcher") => {
            let watcher = ParentExit::watch().unwrap();
            // Avoid leaving an orphan running if the regression check fails.
            thread::spawn(|| {
                thread::sleep(TIMEOUT * 3);
                std::process::exit(2);
            });
            println!("{READY}");
            watcher.wait().unwrap();
            println!("{DONE}");
            std::process::exit(0);
        }
        Ok("parent") => {
            let mut command = test_command("watcher");
            command.stdout(Stdio::inherit());
            let guard = ParentExit::attach(&mut command).unwrap();
            #[cfg(target_os = "macos")]
            // Deliberately keep a writer open in the worker. On macOS the
            // process event must still arrive even when pipe EOF cannot.
            command.stdin(Stdio::from(guard._writer.try_clone().unwrap()));
            let mut child = command.spawn().unwrap();
            let _ = child.wait().unwrap();
            drop(guard);
            std::process::exit(1);
        }
        Ok("invalid") => {
            assert!(ParentExit::watch().is_err());
            std::process::exit(0);
        }
        _ => {}
    }

    let mut command = test_command("watcher");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let guard = ParentExit::attach(&mut command).unwrap();
    let mut child = command.spawn().unwrap();
    let markers = output_lines(&mut child);
    wait_marker(&mut child, &markers, READY);
    assert!(child.try_wait().unwrap().is_none());
    // Keep the command-input pipe open until the watcher has exited.
    drop(guard);
    wait_marker(&mut child, &markers, DONE);
    assert!(child.wait().unwrap().success());

    let mut parent = test_command("parent")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let markers = output_lines(&mut parent);
    wait_marker(&mut parent, &markers, READY);
    parent.kill().unwrap();
    assert!(!parent.wait().unwrap().success());
    wait_marker(&mut parent, &markers, DONE);

    assert!(
        test_command("invalid")
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

fn test_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(ROLE, role);
    command
}

fn output_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    let output = child.stdout.take().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

fn wait_marker(child: &mut Child, markers: &mpsc::Receiver<String>, marker: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = markers.recv_timeout(remaining).unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child failed to report {marker}: {error}");
        });
        if line.contains(marker) {
            return;
        }
    }
}
