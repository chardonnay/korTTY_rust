#![cfg(windows)]

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kortty_pty::{LaunchOptions, LocalShell, Pty, PtyWriter};

const CASE: &str = "KORTTY_NATIVE_PROMPT_FIXTURE";
const A: &[u8] = b"\x1b]133;A\x07";
const B: &[u8] = b"\x1b]133;B\x07";
const BASH_PROGRAM: &str = "KORTTY_TEST_WINDOWS_BASH";

#[test]
fn native_windows_shells_emit_fresh_prompt_boundaries_only_when_enabled() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = env::temp_dir().join(format!(
        "kortty-native-prompt-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        for shell in ["cmd", "powershell"] {
            for enabled in [false, true] {
                let child = Command::new(env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "fixture_windows_prompt",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env(CASE, shell)
                    .env(
                        "KORTTY_NATIVE_PROMPT_ENABLED",
                        if enabled { "1" } else { "0" },
                    )
                    .env("KORTTY_NATIVE_PROMPT_ROOT", &root)
                    .env("APPDATA", &root)
                    .env("LOCALAPPDATA", &root)
                    .env("USERPROFILE", &root)
                    .env("TEMP", &root)
                    .env("TMP", &root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                eprintln!("owned native shell fixture PID {}", child.id());
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{shell} enabled={enabled}\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    });
    // The child process owns every profile/cache file in this newly created root.
    assert!(root.starts_with(env::temp_dir()));
    fs::remove_dir_all(&root).unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "requires an explicitly selected native Git Bash executable"]
fn native_windows_custom_bash_preserves_startup_and_emits_shell_marks() {
    let program = PathBuf::from(
        env::var_os(BASH_PROGRAM)
            .expect("set KORTTY_TEST_WINDOWS_BASH to an approved full Git Bash path"),
    );
    assert!(program.is_absolute() && program.is_file());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = env::temp_dir().join(format!("kortty-git-bash-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("cwd proof'")).unwrap();
    const RC: &str = "printf '%s-%s\\n' startup verified\nPS1='bash fixture> '\n";
    const PROFILE: &str = "printf '%s-%s\\n' login verified\n. ~/.bashrc\n";
    fs::write(root.join(".bashrc"), RC).unwrap();
    fs::write(root.join(".bash_profile"), PROFILE).unwrap();
    let checked = std::panic::catch_unwind(|| {
        for (flags, enabled) in [("-i", false), ("-i", true), ("-il", true)] {
            let child = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "fixture_windows_prompt",
                    "--ignored",
                    "--nocapture",
                ])
                .env(CASE, "bash")
                .env(BASH_PROGRAM, &program)
                .env("KORTTY_TEST_WINDOWS_BASH_FLAGS", flags)
                .env(
                    "KORTTY_NATIVE_PROMPT_ENABLED",
                    if enabled { "1" } else { "0" },
                )
                .env("KORTTY_NATIVE_PROMPT_ROOT", &root)
                .env("HOME", &root)
                .env("USERPROFILE", &root)
                .env("APPDATA", &root)
                .env("LOCALAPPDATA", &root)
                .env("TEMP", &root)
                .env("TMP", &root)
                .env_remove("BASH_ENV")
                .env_remove("ENV")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            eprintln!("owned Git Bash fixture PID {}", child.id());
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "Git Bash {flags} enabled={enabled}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read_to_string(root.join(".bashrc")).unwrap(), RC);
            assert_eq!(
                fs::read_to_string(root.join(".bash_profile")).unwrap(),
                PROFILE
            );
            assert!(
                !fs::read_dir(&root).unwrap().any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("kortty-shell-")),
                "private startup wrapper survived child shutdown"
            );
        }
    });
    assert!(root.starts_with(env::temp_dir()));
    fs::remove_dir_all(&root).unwrap();
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "private native shell child fixture"]
fn fixture_windows_prompt() {
    let Some(case) = env::var_os(CASE) else {
        return;
    };
    let powershell = case == "powershell";
    let bash = case == "bash";
    let enabled = env::var("KORTTY_NATIVE_PROMPT_ENABLED").as_deref() == Ok("1");
    let root = PathBuf::from(env::var_os("KORTTY_NATIVE_PROMPT_ROOT").unwrap());
    let preset = if powershell {
        LocalShell::PowerShell
    } else {
        LocalShell::CommandPrompt
    };
    let mut command = if bash {
        vec![
            env::var(BASH_PROGRAM).unwrap(),
            env::var("KORTTY_TEST_WINDOWS_BASH_FLAGS").unwrap(),
        ]
    } else {
        vec![
            kortty_pty::shell_program(preset)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ]
    };
    if !bash {
        command.extend(
            if powershell {
                ["-NoProfile", "-NoLogo"]
            } else {
                ["/d", "/q"]
            }
            .map(str::to_owned),
        );
    }
    let mut pty = Pty::spawn_with_options(
        100,
        24,
        "xterm-256color",
        &LaunchOptions {
            command: Some(command),
            working_directory: Some(if bash { root.join("cwd proof'") } else { root }),
            shell_integration: enabled,
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    eprintln!("owned native shell PID {}", pty.shell_pid());
    let mut reader = pty.take_reader().unwrap();
    let mut writer = pty.take_writer().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut bytes = [0; 4096];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(length) if send.send(bytes[..length].to_vec()).is_err() => break,
                Ok(_) => {}
            }
        }
    });
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut output = Vec::new();
        read_until(&receive, &mut writer, &mut output, |bytes| {
            if enabled {
                occurrences(bytes, B) >= 1
            } else {
                bytes.contains(&b'>')
            }
        });
        if bash {
            assert!(
                occurrences(&output, b"startup-verified") > 0,
                "{}",
                String::from_utf8_lossy(&output)
            );
            assert_eq!(
                occurrences(&output, b"login-verified") > 0,
                env::var("KORTTY_TEST_WINDOWS_BASH_FLAGS").as_deref() == Ok("-il"),
                "{}",
                String::from_utf8_lossy(&output)
            );
        }
        let before = occurrences(&output, B);
        writer
            .write_all(if bash {
                b"printf '%s-%s\\n' kortty first\r"
            } else if powershell {
                b"[Console]::WriteLine(('kortty-' + 'first')); $null = 1\r"
            } else {
                b"echo kortty-^first\r"
            })
            .unwrap();
        read_until(&receive, &mut writer, &mut output, |bytes| {
            bytes.windows(12).any(|window| window == b"kortty-first")
                && (!enabled || occurrences(bytes, B) > before)
        });
        let before = occurrences(&output, B);
        writer
            .write_all(if bash {
                b"false\r"
            } else {
                b"cmd.exe /d /c exit 7\r"
            })
            .unwrap();
        if enabled {
            read_until(&receive, &mut writer, &mut output, |bytes| {
                occurrences(bytes, B) > before
            });
            assert!(
                occurrences(&output, A) >= 3,
                "{}",
                String::from_utf8_lossy(&output)
            );
            if bash {
                for marker in [
                    b"\x1b]133;C\x07".as_slice(),
                    b"\x1b]133;D;1\x07",
                    b"\x1b]7;file://",
                    b"/cwd%20proof%27\x07",
                ] {
                    assert!(
                        occurrences(&output, marker) > 0,
                        "missing {:?} in {}",
                        String::from_utf8_lossy(marker),
                        String::from_utf8_lossy(&output)
                    );
                }
            } else if powershell {
                assert!(
                    occurrences(&output, b"\x1b]7;file:///") > 0,
                    "{}",
                    String::from_utf8_lossy(&output)
                );
            } else {
                assert!(!output.windows(7).any(|w| w == b"\x1b]133;C"));
                assert!(!output.windows(7).any(|w| w == b"\x1b]133;D"));
            }
        } else {
            assert_eq!(occurrences(&output, A), 0);
            assert_eq!(occurrences(&output, B), 0);
        }
        writer.write_all(b"exit 0\r").unwrap();
        eprintln!(
            "native prompt {case:?} enabled={enabled}, A={}, B={}",
            occurrences(&output, A),
            occurrences(&output, B)
        );
    }));
    pty.shutdown().unwrap();
    drop(writer);
    reader.join().unwrap();
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}

fn read_until(
    receive: &Receiver<Vec<u8>>,
    writer: &mut PtyWriter,
    output: &mut Vec<u8>,
    condition: impl Fn(&[u8]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition(output) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let bytes = receive.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!(
                "native shell timeout {error}: {}",
                String::from_utf8_lossy(output)
            )
        });
        if bytes.windows(4).any(|part| part == b"\x1b[6n") {
            writer.write_all(b"\x1b[1;1R").unwrap();
        }
        output.extend(bytes);
        assert!(
            output.len() <= 1024 * 1024,
            "native shell output exceeded test bound"
        );
    }
}

fn occurrences(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .filter(|part| *part == needle)
        .count()
}
