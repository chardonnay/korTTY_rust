#[cfg(unix)]
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use iced::futures::StreamExt;
use iced_runtime::{Action, task};

use crate::connection_store::LocalProfile;
use crate::session::{Session, Status as SessionStatus};
use crate::session_journal::CaptureKind;
use crate::workspace::{InputSource, PaneInput};

use super::tests::{Scratch, deliver_journal_attachment, wait};
use super::*;

const CONSOLE_READY: &str = "console-ready";
const PROMPTS: [(&str, char, bool, &str, u8); 5] = [
    ("ATTACH-READY", '>', true, "BEFORE AFTER", 0),
    ("PROMPT-A-ONLY", '>', false, "UNCONFIRMED", 0),
    ("CAPTURE-READY", '>', true, "record JOURNAL_PRIVATE", 7),
    ("PASTE-READY", '>', true, "PASTED tail", 0),
    ("Password", ':', true, "DUMMY_PASSWORD", 0),
];

#[test]
fn saved_shell_journal_captures_only_opted_in_commands_at_ready_prompts() {
    for enabled in [false, true] {
        let scratch = Scratch::new();
        let mut workspace = Workspace::default();
        workspace.window_settings.session_journal.directory =
            scratch.0.join("journals").display().to_string();
        workspace.clipboard.policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n\
             [[rule.session-journal.replace]]\npattern = 'JOURNAL_PRIVATE'\nreplacement = '***'",
        )
        .unwrap();
        let (owner, _) = workspace.open_window(false);
        let mut profile = LocalProfile {
            id: 0,
            name: "Native journal input fixture".into(),
            shell: Default::default(),
            command: Some(fixture_command(&scratch.0)),
            working_directory: Some(scratch.0.clone()),
            isolation: Default::default(),
            shell_integration: false,
            emulation: "xterm-256color".into(),
            terminal_effect: None,
            terminal: Default::default(),
        };
        profile.terminal.journal.auto_start = true;
        profile.terminal.journal.capture_input = enabled;
        let connect = workspace.open_saved_local(owner, profile);
        let active = workspace.windows[&owner].tab().unwrap();
        let tab = active.id;
        let id = active.focused_session().unwrap();
        let terminal = &workspace.sessions[&id].session;
        wait(|| terminal.journal_available() && terminal.frame().status == SessionStatus::Running);
        let console_ready = scratch.0.join(CONSOLE_READY);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !console_ready.is_file() {
            let frame = terminal.frame();
            let status = &frame.status;
            assert!(
                *status == SessionStatus::Running && Instant::now() < deadline,
                "PTY fixture failed to configure the console with input capture {enabled}: {status:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        terminal.send(b"\r".to_vec()).unwrap();
        wait_for_text(terminal, "ATTACH-READY>");
        send_key(&mut workspace, owner, id, b"BEFORE ", false);
        let start = Task::batch([connect, workspace.update(Desktop::SessionUpdate(id))]);
        let mut events = task::into_stream(start).unwrap();
        let started = loop {
            if let Action::Output(Desktop::JournalCapture(started @ Message::Started(..))) =
                block_on(events.next()).expect("automatic local journal startup")
            {
                break started;
            }
        };
        let attach = workspace.update_journal_capture(started);
        deliver_journal_attachment(&mut workspace, attach);
        assert_eq!(
            workspace.sessions[&id].session.journal_input_enabled(),
            enabled
        );
        let capture = workspace.journal_captures[&tab]
            .recording()
            .unwrap()
            .clone();

        send_line(&mut workspace, owner, id, b"AFTER");
        wait_for_text(&workspace.sessions[&id].session, "PROMPT-A-ONLY>");
        send_line(&mut workspace, owner, id, b"UNCONFIRMED");
        wait_for_text(&workspace.sessions[&id].session, "CAPTURE-READY>");
        send_line(&mut workspace, owner, id, b"record JOURNAL_PRIVATE");
        wait_for_text(&workspace.sessions[&id].session, "PASTE-READY>");
        workspace.send_to(owner, &[id], InputSource::Paste, |_, _| {
            Some(PaneInput::Local(b"PASTED ".to_vec()))
        });
        send_line(&mut workspace, owner, id, b"tail");
        wait_for_text(&workspace.sessions[&id].session, "Password:");
        send_line(&mut workspace, owner, id, b"DUMMY_PASSWORD");
        wait(|| workspace.sessions[&id].session.frame().status != SessionStatus::Running);
        let _ = workspace.update(Desktop::SessionUpdate(id));
        wait(|| capture.done().is_closed());

        let page = ViewerSource::capture(&capture)
            .capture_page(None, None, None)
            .unwrap();
        assert!(page.status.notice().is_none(), "{page:?}");
        let input: Vec<_> = page
            .records
            .iter()
            .filter(|record| record.kind == CaptureKind::Input)
            .collect();
        let command_end = page
            .records
            .iter()
            .find(|record| {
                record.kind == CaptureKind::Note
                    && record.text == "shell integration: command finished (exit 7)"
            })
            .expect("recorded command completion");
        if enabled {
            assert_eq!(input.len(), 1, "{page:?}");
            assert_eq!(input[0].text, "record ***");
            assert!(input[0].sequence < command_end.sequence, "{page:?}");
        } else {
            assert!(input.is_empty(), "{page:?}");
        }
        assert!(page.records.iter().any(|record| {
            record.kind == CaptureKind::Output && record.text == "FIXTURE-STAGE-3"
        }));
        assert!(page.records.iter().all(|record| {
            !record.text.contains("JOURNAL_PRIVATE") && !record.text.contains("DUMMY_PASSWORD")
        }));
        workspace.stop_session(id);
        for done in std::mem::take(&mut workspace.pending_shutdown) {
            let _ = done.recv_blocking();
        }
    }
}

#[cfg(unix)]
fn fixture_command(_: &Path) -> Vec<String> {
    vec![
        std::env::current_exe()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap(),
        "--exact".into(),
        "workspace::journal_capture_access::input_tests::journal_input_native_child".into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]
}

#[cfg(unix)]
#[test]
#[ignore = "PTY subprocess for the session journal input test"]
fn journal_input_native_child() {
    assert!(
        std::env::current_dir()
            .unwrap()
            .file_name()
            .is_some_and(|name| { name.to_string_lossy().starts_with("kortty-journal-root-") }),
        "run this fixture in its private test directory"
    );
    assert!(
        std::process::Command::new("stty")
            .args(["icanon", "-echo"])
            .status()
            .unwrap()
            .success()
    );
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    // The parent has started both PTY workers when it sends the first line.
    std::fs::write(CONSOLE_READY, b"ready").unwrap();
    let mut startup = String::new();
    input.read_line(&mut startup).unwrap();
    assert!(matches!(startup.as_str(), "\n" | "\r\n"));

    for (index, (prompt, end, ready, expected, status)) in PROMPTS.into_iter().enumerate() {
        write!(output, "\x1b]133;A\x07{prompt}").unwrap();
        if ready {
            output.write_all(b"\x1b]133;B\x07").unwrap();
        }
        // The parent waits for a glyph rendered after the optional mark. A
        // Windows console redraw can omit trailing spaces at the cursor.
        write!(output, "{end} ").unwrap();
        output.flush().unwrap();
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end_matches(['\r', '\n']), expected);
        write!(
            output,
            "\x1b]133;C\x07\r\nFIXTURE-STAGE-{}\r\n\x1b]133;D;{status}\x07",
            index + 1
        )
        .unwrap();
        output.flush().unwrap();
    }
}

#[cfg(windows)]
fn fixture_command(directory: &Path) -> Vec<String> {
    use base64::Engine as _;

    let powershell = kortty_pty::shell_program(kortty_pty::LocalShell::PowerShell).unwrap();
    std::fs::write(
        directory.join("prompts.json"),
        serde_json::to_vec(&PROMPTS).unwrap(),
    )
    .unwrap();
    let script: Vec<u8> = WINDOWS_FIXTURE
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    vec![
        powershell.into_os_string().into_string().unwrap(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        base64::engine::general_purpose::STANDARD.encode(script),
    ]
}

#[cfg(windows)]
const WINDOWS_FIXTURE: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class JournalConsole {
    [DllImport("kernel32.dll")] private static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetConsoleMode(IntPtr handle, uint mode);

    public static void EnableVtOutput() {
        const uint ProcessedOutput = 1;
        const uint VirtualTerminalProcessing = 4;
        IntPtr output = GetStdHandle(-11);
        uint mode;
        if (!GetConsoleMode(output, out mode) ||
            !SetConsoleMode(output, mode | ProcessedOutput | VirtualTerminalProcessing)) {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
    }
}
'@

function Read-FixtureLine {
    $line = New-Object System.Text.StringBuilder
    while ($true) {
        $key = [Console]::ReadKey($true)
        if ($key.Key -eq [ConsoleKey]::Enter) { return $line.ToString() }
        if ($key.KeyChar -ne [char]0) { [void]$line.Append($key.KeyChar) }
    }
}

[JournalConsole]::EnableVtOutput()
$directory = (Get-Location).ProviderPath
$prompts = ConvertFrom-Json -InputObject (Get-Content -Raw -LiteralPath (Join-Path $directory 'prompts.json'))
[IO.File]::WriteAllText((Join-Path $directory 'console-ready'), 'ready')
if ((Read-FixtureLine) -cne '') { throw 'Unexpected startup input' }
$esc = [char]27
$bell = [char]7
$stage = 0
foreach ($entry in $prompts) {
    $prompt, $end, $ready, $expected, $status = $entry
    [Console]::Write("${esc}]133;A${bell}$prompt")
    if ($ready) { [Console]::Write("${esc}]133;B$bell") }
    [Console]::Write("$end ")
    if ((Read-FixtureLine) -cne $expected) { throw "Unexpected input at $prompt" }
    $stage++
    [Console]::Write("${esc}]133;C$bell`r`nFIXTURE-STAGE-$stage`r`n${esc}]133;D;$status$bell")
}
"#;

fn send_line(workspace: &mut Workspace, owner: window::Id, id: SessionId, bytes: &[u8]) {
    send_key(workspace, owner, id, bytes, false);
    send_key(workspace, owner, id, b"\r", true);
}

fn send_key(
    workspace: &mut Workspace,
    owner: window::Id,
    id: SessionId,
    bytes: &[u8],
    enter: bool,
) {
    workspace.send_to(
        owner,
        &[id],
        InputSource::Keyboard {
            enter,
            predict: false,
            shifted_enter: false,
            physical_pane: Some(id),
            dim_agent_hud: false,
        },
        |_, _| Some(PaneInput::Local(bytes.to_vec())),
    );
}

#[track_caller]
fn wait_for_text(session: &Session, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let frame = session.frame();
        let text = frame
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        if text.contains(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline && frame.status == SessionStatus::Running,
            "expected {expected:?} in the PTY fixture, found {:?} with screen {text:?}",
            frame.status,
        );
        thread::sleep(Duration::from_millis(5));
    }
}
