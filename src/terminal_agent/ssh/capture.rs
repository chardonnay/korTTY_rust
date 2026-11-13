//! Shared stream supervision for attached and standalone SSH exec transports.

use std::io;
use std::thread;

use kortty_ssh::{SshAbort, SshReader, SshWriter};

use super::super::runner::{Cancel, Outcome, capture};
use super::{status, wait, worker_error};

pub(super) fn supervise(
    abort: SshAbort,
    stdout: SshReader,
    stderr: SshReader,
    mut stdin: SshWriter,
    exit_code: impl FnOnce() -> Option<u32>,
    cancel: &Cancel,
) -> io::Result<Outcome> {
    thread::scope(|scope| {
        let stdout = thread::Builder::new()
            .name("kortty-agent-ssh-stdout".into())
            .spawn_scoped(scope, move || capture(stdout))?;
        let stderr = match thread::Builder::new()
            .name("kortty-agent-ssh-stderr".into())
            .spawn_scoped(scope, move || capture(stderr))
        {
            Ok(stderr) => stderr,
            Err(error) => {
                abort.abort();
                return Err(error);
            }
        };
        let stdin = match thread::Builder::new()
            .name("kortty-agent-ssh-eof".into())
            .spawn_scoped(scope, move || stdin.close_input().is_ok())
        {
            Ok(stdin) => stdin,
            Err(error) => {
                abort.abort();
                return Err(error);
            }
        };
        let stopped = wait(cancel, || {
            stdout.is_finished() && stderr.is_finished() && stdin.is_finished()
        });
        if stopped.is_some() {
            abort.abort();
        }
        let (stdout, stdout_ok) = stdout.join().map_err(|_| worker_error())?;
        let (stderr, stderr_ok) = stderr.join().map_err(|_| worker_error())?;
        let input_ok = stdin.join().map_err(|_| worker_error())?;
        let status =
            stopped.unwrap_or_else(|| status(exit_code(), stdout_ok && stderr_ok, input_ok));
        Ok(Outcome {
            status,
            stdout,
            stderr,
        })
    })
}
