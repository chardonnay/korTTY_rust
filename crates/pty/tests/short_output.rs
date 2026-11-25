#![cfg(target_os = "macos")]

use std::io::{self, Read};
use std::thread;
use std::time::{Duration, Instant};

use kortty_pty::{LaunchOptions, Pty};

fn delayed_command(output: &str) -> Pty {
    let options = LaunchOptions {
        command: Some(vec!["/usr/bin/printf".into(), "%s".into(), output.into()]),
        ..Default::default()
    };
    let pty = Pty::spawn_with_options(80, 24, "xterm-256color", &options).unwrap();
    // Model a descheduled session reader after exec of a very short command.
    // Do not require the kernel's final slave close to finish before draining
    // the output that close may be waiting for.
    thread::sleep(Duration::from_secs(1));
    pty
}

#[test]
fn delayed_reader_preserves_short_command_output_and_eof_after_resize() {
    let expected = "short command output ".repeat(4);
    let mut pty = delayed_command(&expected);
    pty.resize_with_pixels(97, 31, 970, 620).unwrap();
    let mut reader = pty.take_reader().unwrap();
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    let mut output = Vec::new();
    let mut buffer = [0; 7];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    wait_for_success(&mut pty);
    assert_eq!(String::from_utf8(output).unwrap(), expected);
}

#[test]
fn delayed_nonblocking_reader_drains_output_before_releasing_startup_slave() {
    let expected = "nonblocking short command";
    let mut pty = delayed_command(expected);
    let mut reader = pty.take_reader().unwrap();
    assert_eq!(reader.read_nonblocking(&mut []).unwrap(), 0);
    let mut output = Vec::new();
    let mut buffer = [0; 3];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "short command did not reach EOF");
        match reader.read_nonblocking(&mut buffer) {
            Ok(0) => break,
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("could not drain output: {error}"),
        }
    }
    wait_for_success(&mut pty);
    assert_eq!(String::from_utf8(output).unwrap(), expected);
}

#[test]
fn silent_short_command_still_reaches_eof() {
    let mut pty = delayed_command("");
    let mut output = Vec::new();
    pty.take_reader().unwrap().read_to_end(&mut output).unwrap();
    wait_for_success(&mut pty);
    assert!(output.is_empty());
}

fn wait_for_success(pty: &mut Pty) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = pty.try_wait().unwrap() {
            assert!(status.success());
            pty.shutdown().unwrap();
            return;
        }
        assert!(Instant::now() < deadline, "short command did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}
