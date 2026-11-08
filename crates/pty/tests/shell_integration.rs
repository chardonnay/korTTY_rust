#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kortty_pty::{LaunchOptions, Pty, ShellIntegrationSnippet as Snippet};

const CASE: &str = "KORTTY_SHELL_STARTUP_TEST_CASE";

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn opted_in_local_shell_reads_its_startup_then_emits_marks_on_the_pty() {
    let mut count = 0;
    for snippet in Snippet::ALL {
        let Some(shell) = installed(snippet.name()) else {
            eprintln!("{} is unavailable on this test host", snippet.name());
            continue;
        };
        count += 1;
        run_case(snippet, &shell);
    }
    assert!(
        count != 0,
        "no reference shell is installed on this Unix test host"
    );
}

fn installed(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .chain([PathBuf::from("/bin"), PathBuf::from("/usr/bin")])
        .map(|directory| directory.join(name))
        .find(|program| program.is_absolute() && program.is_file())
}

fn run_case(snippet: Snippet, shell: &Path) {
    let case = snippet.name();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture =
        Fixture(env::temp_dir().join(format!("kortty-shell-test-{}-{stamp}", std::process::id())));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&fixture.0)
        .unwrap();
    let directory = fixture.0.join("cwd proof'");
    fs::create_dir(&directory).unwrap();
    let home = &fixture.0;
    let marks_supported = if snippet == Snippet::Bash {
        Command::new(shell)
            .env_remove("BASH_ENV")
            .args([
                "-c",
                "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))",
            ])
            .status()
            .unwrap()
            .success()
    } else {
        true
    };
    match snippet {
        Snippet::Bash => fs::write(
            home.join(".bashrc"),
            "printf '%s-%s\\n' startup verified\nPS1='bash test> '\n",
        )
        .unwrap(),
        Snippet::Zsh => {
            fs::create_dir(home.join("moved")).unwrap();
            fs::write(home.join(".zshenv"), "export ZDOTDIR=$HOME/moved\n").unwrap();
            fs::write(home.join("moved/.zshrc"), "[[ $ZDOTDIR = $HOME/moved ]] && printf '%s-%s\\n' startup verified\nPS1='zsh test> '\n").unwrap();
        }
        Snippet::Fish => {
            fs::create_dir_all(home.join(".config/fish")).unwrap();
            fs::write(home.join(".config/fish/config.fish"), "printf '%s-%s\\n' startup verified\nfunction fish_prompt; echo 'fish test> '; end\n").unwrap();
        }
    }
    for integration in [false, true] {
        let output = run_fixture_pty(home, &directory, shell, snippet, integration);
        assert!(
            output.contains("startup-verified"),
            "{case} startup: {output:?}"
        );
        assert_eq!(
            output.contains("\x1b]133;A\x07"),
            integration && marks_supported,
            "{case} prompt: {output:?}"
        );
        if integration && marks_supported {
            assert!(
                output.contains("\x1b]133;C\x07"),
                "{case} output start: {output:?}"
            );
            assert!(
                output.contains("\x1b]133;D;1\x07"),
                "{case} exit status: {output:?}"
            );
            assert!(
                output.contains("\x1b]7;file://") && output.contains("/cwd%20proof%27\x07"),
                "{case} cwd: {output:?}"
            );
        }
    }
}

fn run_fixture_pty(
    home: &Path,
    directory: &Path,
    shell: &Path,
    snippet: Snippet,
    integration: bool,
) -> String {
    let exe = env::current_exe().unwrap();
    // The actual PTY spawn inherits fixture variables from an isolated test process.
    let output = Command::new(exe)
        .args(["--exact", "fixture_pty", "--ignored", "--nocapture"])
        .env(CASE, snippet.name())
        .env("KORTTY_SHELL_TEST_CWD", directory)
        .env(
            "KORTTY_SHELL_TEST_ENABLE",
            if integration { "1" } else { "0" },
        )
        .env("HOME", home)
        .env("SHELL", shell)
        .env_remove("ZDOTDIR")
        .env_remove("BASH_ENV")
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "fixture helper launched with an isolated home and shell environment"]
fn fixture_pty() {
    let Some(directory) = env::var_os("KORTTY_SHELL_TEST_CWD") else {
        return;
    };
    let expected = Snippet::from_program(&env::var(CASE).unwrap());
    assert!(expected.is_some());
    assert_eq!(Snippet::local_default(), expected);
    let mut pty = Pty::spawn_with_options(
        100,
        24,
        "xterm-256color",
        &LaunchOptions {
            working_directory: Some(directory.into()),
            shell_integration: env::var("KORTTY_SHELL_TEST_ENABLE").as_deref() == Ok("1"),
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    let mut reader = pty.take_reader().unwrap();
    let mut writer = pty.take_writer().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        let _ = send.send(output);
    });
    writer.write_all(b"false\nexit\n").unwrap();
    let output = receive.recv_timeout(Duration::from_secs(8)).unwrap();
    worker.join().unwrap();
    pty.shutdown().unwrap();
    print!("{}", String::from_utf8_lossy(&output));
}
