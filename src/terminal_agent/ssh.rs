//! Uses one owned exec lease on the pane's already authenticated SSH connection.

use std::io::{self, Read};
use std::thread;
use std::time::Instant;

use zeroize::Zeroizing;

use crate::{ssh_connection::ExecOpener, ssh_worker};

use super::decision::valid_command;
use super::runner::{COMMAND_LIMIT, Cancel, Captured, Outcome, POLL, Status};

mod capture;
pub(crate) mod password;

pub(super) fn run(
    opener: ExecOpener,
    cwd: &str,
    command: &str,
    cancel: &Cancel,
) -> Result<Outcome, String> {
    run_wire(opener, &remote_command(cwd, command)?, cancel)
}

pub(super) fn run_wire(
    opener: ExecOpener,
    command: &str,
    cancel: &Cancel,
) -> Result<Outcome, String> {
    let gate = cancel.clone();
    let result = match opener {
        ExecOpener::Direct(opener) => opener
            .open_cancellable(command, move || gate.requested())
            .map_err(|_| "Could not open the approved SSH command channel.")
            .and_then(|channel| {
                direct(channel, cancel).map_err(|_| "The SSH command streams are unavailable.")
            }),
        ExecOpener::Worker(opener) => opener
            .open_exec(command, || gate.requested())
            .map_err(|_| "Could not open the approved isolated SSH command channel.")
            .and_then(|channel| {
                isolated(channel, cancel)
                    .map_err(|_| "The isolated SSH command streams are unavailable.")
            }),
    };
    if cancel.requested() && result.is_err() {
        return Ok(Outcome {
            status: Status::Cancelled,
            stdout: Captured::default(),
            stderr: Captured::default(),
        });
    }
    result.map_err(str::to_owned)
}

pub(crate) fn remote_command(cwd: &str, command: &str) -> Result<String, String> {
    validate_remote_command(cwd, command)?;
    Ok(format!(
        "cd {} && exec /bin/sh -c {}",
        quote(cwd),
        quote(command)
    ))
}

pub(super) fn passwordless_sudo(cwd: &str, command: &str) -> Result<String, String> {
    validate_remote_command(cwd, command)?;
    // Apply the reviewed directory inside sudo, whose policy can change cwd.
    // Failure to enter it must prevent every part of the requested command.
    let payload = format!("exec </dev/null\ncd {} || exit\n{command}", quote(cwd));
    let wire = format!(
        "exec /usr/bin/sudo -k -n -u root -- /bin/sh -c {}",
        quote(&payload)
    );
    if wire.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
        return Err("The effective sudo command exceeds the SSH execution limit.".into());
    }
    Ok(wire)
}

fn validate_remote_command(cwd: &str, command: &str) -> Result<(), String> {
    if !cwd.starts_with('/') || !super::valid_target_label(cwd) || !valid_command(command) {
        return Err("The approved SSH directory or command is unavailable.".into());
    }
    Ok(())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn direct(mut channel: kortty_ssh::ExecLease, cancel: &Cancel) -> io::Result<Outcome> {
    capture::supervise(
        channel.abort_handle(),
        channel.take_reader().ok_or_else(missing_pipe)?,
        channel.take_stderr().ok_or_else(missing_pipe)?,
        channel.take_writer().ok_or_else(missing_pipe)?,
        || channel.exit_code(),
        cancel,
    )
}

pub(super) fn capture_session(
    mut session: kortty_ssh::SshSession,
    cancel: &Cancel,
) -> io::Result<Outcome> {
    let result = capture::supervise(
        session.abort_handle(),
        session.take_reader().ok_or_else(missing_pipe)?,
        session.take_stderr().ok_or_else(missing_pipe)?,
        session.take_writer().ok_or_else(missing_pipe)?,
        || session.exit_code(),
        cancel,
    );
    let _ = session.close();
    result
}

fn isolated(mut channel: ssh_worker::Channel, cancel: &Cancel) -> io::Result<Outcome> {
    let abort = channel.abort_handle();
    let output = channel.take_reader().ok_or_else(missing_pipe)?;
    let stdin = channel.take_writer().ok_or_else(missing_pipe)?;
    thread::scope(|scope| {
        let output = thread::Builder::new()
            .name("kortty-agent-worker-output".into())
            .spawn_scoped(scope, move || capture_tagged(output))?;
        let stdin = match thread::Builder::new()
            .name("kortty-agent-worker-eof".into())
            .spawn_scoped(scope, move || stdin.close_input().is_ok())
        {
            Ok(stdin) => stdin,
            Err(error) => {
                abort.abort();
                return Err(error);
            }
        };
        let stopped = wait(cancel, || output.is_finished() && stdin.is_finished());
        if stopped.is_some() {
            abort.abort();
        }
        let ([stdout, stderr], output_ok) = output.join().map_err(|_| worker_error())?;
        let input_ok = stdin.join().map_err(|_| worker_error())?;
        let status = stopped.unwrap_or_else(|| status(channel.exit_code(), output_ok, input_ok));
        Ok(Outcome {
            status,
            stdout,
            stderr,
        })
    })
}

fn wait(cancel: &Cancel, complete: impl Fn() -> bool) -> Option<Status> {
    let deadline = Instant::now() + COMMAND_LIMIT;
    while !complete() {
        if cancel.requested() {
            return Some(Status::Cancelled);
        }
        if Instant::now() >= deadline {
            return Some(Status::TimedOut);
        }
        thread::sleep(POLL);
    }
    cancel.requested().then_some(Status::Cancelled)
}

fn status(exit: Option<u32>, output_ok: bool, input_ok: bool) -> Status {
    if !output_ok || (exit.is_none() && !input_ok) {
        Status::TransportFailed
    } else {
        exit.map_or(Status::MissingExitCode, Status::Exited)
    }
}

fn capture_tagged(mut reader: impl Read) -> ([Captured; 2], bool) {
    let mut output = [Captured::default(), Captured::default()];
    let mut bytes = Zeroizing::new([0; kortty::ssh_runtime::wire::CHUNK - 3]);
    let mut kind = [0];
    loop {
        match reader.read(&mut kind) {
            Ok(0) => return (output, true),
            Ok(_) if kind[0] <= 1 => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            _ => return (output, false),
        }
        let mut count = [0; 2];
        if reader.read_exact(&mut count).is_err() {
            return (output, false);
        }
        let count = u16::from_be_bytes(count) as usize;
        if count == 0 || count > bytes.len() || reader.read_exact(&mut bytes[..count]).is_err() {
            return (output, false);
        }
        output[kind[0] as usize].push(&bytes[..count]);
    }
}

fn missing_pipe() -> io::Error {
    io::Error::other("SSH command pipe was unavailable")
}

fn worker_error() -> io::Error {
    io::Error::other("SSH command worker stopped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_passwordless_sudo_wrapper_is_fixed_and_bounded() {
        let wire = passwordless_sudo("/srv/my project", "id -u").unwrap();
        assert_eq!(
            wire,
            "exec /usr/bin/sudo -k -n -u root -- /bin/sh -c 'exec </dev/null\ncd '\\''/srv/my project'\\'' || exit\nid -u'"
        );
        assert!(passwordless_sudo("relative", "id -u").is_err());
        assert!(passwordless_sudo("/srv/line\nbreak", "id -u").is_err());
        assert!(passwordless_sudo("/tmp", "printf ***").is_err());
        let path = format!("/{}", "'".repeat(4095));
        assert!(passwordless_sudo(&path, "id -u").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn agent_passwordless_sudo_wrapper_preserves_literal_directory_and_command_arguments() {
        let directory = "/tmp/it's $(exit 77)";
        let command = "printf '%s' \"$(exit 89)\"; exit 91";
        let wire = passwordless_sudo(directory, command).unwrap();
        // Parse the same words without running the wrapper or its payload.
        let capture = format!(
            "set -- {}\nprintf '%s\\n' \"$@\"",
            wire.strip_prefix("exec ").unwrap()
        );
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &capture])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "/usr/bin/sudo\n-k\n-n\n-u\nroot\n--\n/bin/sh\n-c\nexec </dev/null\ncd '/tmp/it'\\''s $(exit 77)' || exit\n{command}\n"
            )
        );
    }
}
