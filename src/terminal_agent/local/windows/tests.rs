use std::time::Duration;

use super::*;
use crate::snippets::tests::Profile;

mod swarm;

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing to the actual kortty executable"]
fn native_agent_windows_worker_runs_exact_shell_and_owns_background_children() {
    let executable = std::env::var_os("KORTTY_SSH_TEST_EXE")
        .map(PathBuf::from)
        .expect("set KORTTY_SSH_TEST_EXE to the built desktop executable")
        .canonicalize()
        .unwrap();
    let profile = Profile::new();
    let shell =
        Shell::PowerShell(kortty_pty::shell_program(kortty_pty::LocalShell::PowerShell).unwrap());
    let background = "Start-Sleep -Milliseconds 1100; [IO.File]::WriteAllText('escaped', 'leaked')";
    let encoded = STANDARD.encode(
        background
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let command = format!(
        concat!(
            "$ProgressPreference = 'SilentlyContinue'; ",
            "$worker = Start-Process -PassThru -WindowStyle Hidden -FilePath (Join-Path $PSHOME 'powershell.exe') ",
            "-WorkingDirectory (Get-Location).Path -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{}'; ",
            "[Console]::Out.WriteLine($worker.Id); [Console]::Out.Write('Grüße 東京'); [Console]::Error.Write('Fehler 東京'); exit 19"
        ),
        encoded
    );
    let result = run_executable(
        &shell,
        &profile.0,
        &command,
        &Cancel::default(),
        &executable,
    )
    .unwrap();
    assert_eq!(result.status, Status::Exited(19));
    let stdout = result.stdout.text();
    let (pid, text) = stdout.split_once('\n').unwrap();
    let owned = pid.trim().parse::<u32>().unwrap();
    assert!(owned > 1);
    assert_eq!(text, "Grüße 東京");
    assert_eq!(&*result.stderr.text(), "Fehler 東京");
    std::thread::sleep(Duration::from_millis(1350));
    assert!(
        !profile.0.join("escaped").exists(),
        "owned process {owned} escaped the JobObject"
    );

    let shell = Shell::CommandPrompt(
        kortty_pty::shell_program(kortty_pty::LocalShell::CommandPrompt).unwrap(),
    );
    let letter = (0x80..=0xff)
        .find_map(|byte| {
            let decoded = kortty_pty::decode_oem_output(&[byte]).ok()?;
            let mut chars = decoded.chars();
            let first = chars.next()?;
            (chars.next().is_none() && !first.is_ascii() && first.is_alphabetic()).then_some(first)
        })
        .unwrap_or('ü');
    let command = format!("echo agent-{letter}& echo err-{letter} 1>&2& exit /b 13");
    let result = run_executable(
        &shell,
        &profile.0,
        &command,
        &Cancel::default(),
        &executable,
    )
    .unwrap();
    assert_eq!(result.status, Status::Exited(13));
    assert_eq!(result.stdout.text().trim(), format!("agent-{letter}"));
    assert_eq!(result.stderr.text().trim(), format!("err-{letter}"));
}
