//! Authentication and framed file exchange on a dedicated non-PTY exec lease.

use std::io;
use std::time::{Duration, Instant};

use kortty_ssh::{ExecLease, SshReader, SshWriter, sftp::MAX_EDIT_BYTES};
use zeroize::Zeroizing;

use super::{ConnectError, Flow, Frame, Server, closed, exec, lock};
use crate::ssh_runtime::{
    mux::Writer,
    password::{self, Prompt, Status},
    password_file::{self, HELLO},
    wire::CHUNK,
};

const POLL: Duration = Duration::from_millis(35);
const OUTPUT_LIMIT: usize = MAX_EDIT_BYTES + 4 * CHUNK;

impl Server {
    pub(super) fn exec_password_file(
        &self,
        flow: &Flow,
        nonce: [u8; 16],
    ) -> Result<(), ConnectError> {
        let opener = self.exec.as_ref().ok_or_else(closed)?;
        let marker = password::marker(nonce);
        let command = password_file::command(Some(&marker))?;
        let deadline = Instant::now() + password_file::DEADLINE;
        let gate = flow.clone();
        let mut lease = opener.open_sensitive_cancellable(&command, move || {
            Instant::now() >= deadline || !gate.password_open()
        })?;
        {
            let mut active = lock(&self.active);
            if !flow.password_open() {
                return Ok(());
            }
            active.insert(flow.id(), lease.abort_handle());
        }
        let result = self
            .mux
            .control(Frame::Opened(flow.id(), None))
            .and_then(
                |()| match authenticate(&mut lease, flow, &marker, deadline) {
                    Ok(mut authenticated) => {
                        flow.ready_password_file()?;
                        while !flow.wait_password_file_ready(POLL)? {
                            check_relay(flow, deadline)?;
                        }
                        exec::relay(
                            flow,
                            &lease,
                            authenticated.input,
                            Some(deadline),
                            |output| {
                                pump_file(
                                    &mut authenticated.stdout,
                                    authenticated.stderr.as_mut(),
                                    output,
                                    flow,
                                    deadline,
                                )
                            },
                        )
                    }
                    Err(status) if flow.password_open() => flow.finish_password(status),
                    Err(_) => Ok(()),
                },
            );
        lock(&self.active).remove(&flow.id());
        result.map_err(Into::into)
    }
}

struct Authenticated {
    stdout: SshReader,
    stderr: Option<SshReader>,
    input: SshWriter,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Authentication {
    Initial,
    Waiting,
    Submitted,
}

fn authenticate(
    lease: &mut ExecLease,
    flow: &Flow,
    marker: &str,
    deadline: Instant,
) -> Result<Authenticated, Status> {
    let mut stdout = lease.take_reader().ok_or(Status::TransportFailed)?;
    let mut stderr = lease.take_stderr();
    let mut input = None;
    let mut prompt = Prompt::new(marker);
    let mut state = Authentication::Initial;
    let mut greeting = Greeting::default();
    let mut buffer = Zeroizing::new([0; CHUNK]);
    loop {
        if !flow.password_open() {
            return Err(Status::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Status::TimedOut);
        }
        if let Some(reader) = &mut stderr {
            match read_available(reader, &mut buffer[..], Duration::ZERO) {
                Ok(Some(0)) => stderr = None,
                Ok(Some(count)) => {
                    let matched = prompt.observe(&buffer[..count]);
                    if state == Authentication::Waiting
                        || (matched && state != Authentication::Initial)
                    {
                        return Err(Status::TransportFailed);
                    }
                    if matched {
                        flow.password_challenge().map_err(|_| Status::Cancelled)?;
                        state = Authentication::Waiting;
                    }
                }
                Ok(None) => {}
                Err(_) => return Err(Status::TransportFailed),
            }
        }
        if let Some(line) = flow.take_password().map_err(|_| Status::Cancelled)? {
            if state != Authentication::Waiting || !flow.password_open() {
                return Err(Status::Cancelled);
            }
            let stdin = lease
                .take_sensitive_exec_input()
                .ok_or(Status::TransportFailed)?;
            let gate = flow.clone();
            input = Some(
                stdin
                    .send_then(line, move || {
                        Instant::now() < deadline && gate.password_open()
                    })
                    .map_err(|_| Status::TransportFailed)?,
            );
            state = Authentication::Submitted;
        }
        // Read at most the remaining greeting bytes. No pre-auth output is
        // forwarded, and the helper waits for the first file request next.
        match read_available(&mut stdout, &mut buffer[..greeting.remaining()], POLL) {
            Ok(Some(0)) => {
                return Err(lease
                    .exit_code()
                    .map_or(Status::MissingExitCode, Status::Exited));
            }
            Ok(Some(count)) if greeting.accept(&buffer[..count])? => {
                if state == Authentication::Waiting {
                    return Err(Status::TransportFailed);
                }
                let input = input
                    .or_else(|| lease.take_writer())
                    .ok_or(Status::TransportFailed)?;
                return Ok(Authenticated {
                    stdout,
                    stderr,
                    input,
                });
            }
            Ok(_) => {}
            Err(_) => return Err(Status::TransportFailed),
        }
    }
}

#[derive(Default)]
struct Greeting(usize);

impl Greeting {
    fn remaining(&self) -> usize {
        HELLO.len() - self.0
    }

    fn accept(&mut self, bytes: &[u8]) -> Result<bool, Status> {
        let end = self.0 + bytes.len();
        if HELLO.get(self.0..end) != Some(bytes) {
            return Err(Status::TransportFailed);
        }
        self.0 = end;
        Ok(self.remaining() == 0)
    }
}

fn read_available(
    reader: &mut SshReader,
    bytes: &mut [u8],
    timeout: Duration,
) -> io::Result<Option<usize>> {
    match reader.read_timeout(bytes, timeout) {
        Ok(count) => Ok(Some(count)),
        Err(error) if retry(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn pump_file(
    stdout: &mut SshReader,
    mut stderr: Option<&mut SshReader>,
    output: &mut Writer,
    flow: &Flow,
    deadline: Instant,
) -> io::Result<()> {
    let mut buffer = Zeroizing::new([0; CHUNK]);
    let mut remaining = OUTPUT_LIMIT;
    let mut stdout_open = true;
    while stdout_open || stderr.is_some() {
        check_relay(flow, deadline)?;
        let stderr_poll = if stdout_open { Duration::ZERO } else { POLL };
        if let Some(reader) = &mut stderr
            && read_available(reader, &mut buffer[..], stderr_poll)? == Some(0)
        {
            stderr = None;
        }
        if !stdout_open {
            continue;
        }
        let Some(count) = read_available(stdout, &mut buffer[3..], POLL)? else {
            continue;
        };
        if count == 0 {
            stdout_open = false;
            continue;
        }
        remaining = remaining
            .checked_sub(count)
            .ok_or_else(|| io::Error::other("Privileged file output exceeds its limit"))?;
        buffer[0] = 0;
        buffer[1..3].copy_from_slice(&(count as u16).to_be_bytes());
        let mut frame = &buffer[..count + 3];
        while !frame.is_empty() {
            check_relay(flow, deadline)?;
            match output.write_timeout(frame, POLL) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => frame = &frame[count..],
                Err(error) if retry(&error) => continue,
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn check_relay(flow: &Flow, deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else if !flow.is_open() {
        Err(closed())
    } else {
        Ok(())
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_worker_greeting_must_be_exact_and_complete() {
        let mut greeting = Greeting::default();
        assert_eq!(greeting.accept(&HELLO[..2]), Ok(false));
        assert_eq!(greeting.accept(&HELLO[2..14]), Ok(false));
        assert_eq!(greeting.accept(&HELLO[14..]), Ok(true));
        assert_eq!(
            Greeting::default().accept(b"untrusted banner"),
            Err(Status::TransportFailed)
        );
        assert_eq!(greeting.accept(b"trailing"), Err(Status::TransportFailed));
    }
}
