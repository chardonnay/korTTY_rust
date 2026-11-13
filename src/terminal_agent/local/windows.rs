use std::io::{self, Read as _, Write as _};
use std::os::windows::{io::OwnedHandle, process::CommandExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

use super::super::decision::{MAX_COMMAND_BYTES, valid_command};
use super::super::runner::{COMMAND_LIMIT, Cancel, Outcome, POLL, Status, capture};
use super::{Shell, exit_status};

pub(in crate::terminal_agent) const WORKER_ARGUMENT: &str = "--terminal-agent-command-worker";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(test)]
mod tests;

pub(super) fn run(shell: &Shell, cwd: &Path, text: &str, cancel: &Cancel) -> io::Result<Outcome> {
    run_executable(shell, cwd, text, cancel, &std::env::current_exe()?)
}

fn run_executable(
    shell: &Shell,
    cwd: &Path,
    text: &str,
    cancel: &Cancel,
    executable: &Path,
) -> io::Result<Outcome> {
    let kind = match shell {
        Shell::PowerShell(_) => "powershell",
        Shell::CommandPrompt(_) => "cmd",
    };
    let child = Command::new(executable)
        .args([WORKER_ARGUMENT, kind])
        .arg(shell.executable())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let mut process = Process { child, job: None };
    // The worker waits for its input frame before launching any model text.
    // Assign the job first so cancellation owns every descendant from launch.
    process.job = Some(kortty_pty::contain_child(&process.child)?);
    let mut input = process.child.stdin.take().ok_or_else(missing_pipe)?;
    if cancel.requested() {
        return Ok(Outcome {
            status: Status::Cancelled,
            stdout: Default::default(),
            stderr: Default::default(),
        });
    }
    input.write_all(&(text.len() as u32).to_be_bytes())?;
    input.write_all(text.as_bytes())?;
    drop(input);
    let stdout = process.child.stdout.take().ok_or_else(missing_pipe)?;
    let stderr = process.child.stderr.take().ok_or_else(missing_pipe)?;
    let deadline = Instant::now() + COMMAND_LIMIT;
    std::thread::scope(|scope| {
        let stdout = std::thread::Builder::new()
            .name("kortty-agent-stdout".into())
            .spawn_scoped(scope, move || capture(stdout))?;
        let stderr = match std::thread::Builder::new()
            .name("kortty-agent-stderr".into())
            .spawn_scoped(scope, move || capture(stderr))
        {
            Ok(stderr) => stderr,
            Err(error) => {
                process.stop();
                return Err(error);
            }
        };
        let status = loop {
            if cancel.requested() {
                break Ok(Status::Cancelled);
            }
            if Instant::now() >= deadline {
                break Ok(Status::TimedOut);
            }
            match process.child.try_wait() {
                Ok(Some(status)) => break Ok(exit_status(status)),
                Ok(None) => std::thread::sleep(POLL),
                Err(error) => break Err(error),
            }
        };
        process.stop();
        let (mut stdout, stdout_ok) = stdout
            .join()
            .map_err(|_| io::Error::other("Command output worker stopped"))?;
        let (mut stderr, stderr_ok) = stderr
            .join()
            .map_err(|_| io::Error::other("Command error worker stopped"))?;
        let mut status = status?;
        if matches!(status, Status::Exited(_)) && !(stdout_ok && stderr_ok) {
            status = Status::TransportFailed;
        }
        if matches!(shell, Shell::CommandPrompt(_)) {
            stdout.use_oem_fallback();
            stderr.use_oem_fallback();
        }
        Ok(Outcome {
            status,
            stdout,
            stderr,
        })
    })
}

pub(in crate::terminal_agent) fn worker(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> io::Result<i32> {
    let kind = args
        .next()
        .and_then(|kind| kind.into_string().ok())
        .ok_or_else(invalid)?;
    let executable = args.next().map(PathBuf::from).ok_or_else(invalid)?;
    if args.next().is_some() || !executable.is_absolute() {
        return Err(invalid());
    }
    let input = io::stdin();
    let mut input = input.lock();
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_COMMAND_BYTES {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(vec![0; len]);
    input.read_exact(&mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    if !valid_command(text) {
        return Err(invalid());
    }
    let mut command = Command::new(executable);
    match kind.as_str() {
        "powershell" => {
            // Keep the reviewed script as a separate, exact UTF-8 payload so
            // encoding setup does not change its param block or initial syntax.
            let script = Zeroizing::new(STANDARD.encode(text.as_bytes()));
            let wrapper = Zeroizing::new(format!(
                concat!(
                    "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); ",
                    "$OutputEncoding = [Console]::OutputEncoding; ",
                    ". ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))))"
                ),
                *script
            ));
            let encoded = Zeroizing::new(
                wrapper
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-InputFormat",
                    "Text",
                    "-OutputFormat",
                    "Text",
                    "-EncodedCommand",
                ])
                .arg(STANDARD.encode(&*encoded));
        }
        "cmd" => {
            command
                .args(["/d", "/s", "/c"])
                .raw_arg(format!("\"{text}\""));
        }
        _ => return Err(invalid()),
    }
    let status = command
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid terminal agent worker request",
    )
}

fn missing_pipe() -> io::Error {
    io::Error::other("Local command pipe was unavailable")
}

struct Process {
    child: Child,
    job: Option<OwnedHandle>,
}

impl Process {
    fn stop(&mut self) {
        drop(self.job.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}
