//! Runs an approved command in an independent native shell process.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use super::decision::valid_command;
use super::runner::{Cancel, Outcome, Status};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{WORKER_ARGUMENT, worker};

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum Shell {
    #[cfg(unix)]
    Posix(PathBuf),
    #[cfg(windows)]
    PowerShell(PathBuf),
    #[cfg(windows)]
    CommandPrompt(PathBuf),
}

impl Shell {
    pub(super) fn executable(&self) -> &Path {
        match self {
            #[cfg(unix)]
            Self::Posix(path) => path,
            #[cfg(windows)]
            Self::PowerShell(path) | Self::CommandPrompt(path) => path,
        }
    }
}

pub(super) fn run(
    shell: &Shell,
    cwd: &Path,
    text: &str,
    cancel: &Cancel,
) -> Result<Outcome, String> {
    if cancel.requested() {
        return Ok(Outcome {
            status: Status::Cancelled,
            stdout: Default::default(),
            stderr: Default::default(),
        });
    }
    if !cwd.is_absolute()
        || !cwd.is_dir()
        || !shell.executable().is_absolute()
        || !valid_command(text)
    {
        return Err("The approved local shell, directory or command is unavailable.".into());
    }
    #[cfg(unix)]
    let result = unix::run(shell, cwd, text, cancel);
    #[cfg(windows)]
    let result = windows::run(shell, cwd, text, cancel);
    result.map_err(|_| "Could not start or supervise the approved local command.".into())
}

fn exit_status(status: ExitStatus) -> Status {
    status.code().map_or(Status::MissingExitCode, |code| {
        Status::Exited(code.cast_unsigned())
    })
}
