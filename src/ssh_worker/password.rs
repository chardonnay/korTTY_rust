//! A status-only supervisor for a single authenticated password exec lease.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

use kortty_ssh::{ExecLease, SensitiveLine, SshReader};
use zeroize::{Zeroize, Zeroizing};

const POLL: Duration = Duration::from_millis(35);
const CHALLENGE_LIMIT: Duration = Duration::from_secs(30);
const ANSWER_LIMIT: Duration = Duration::from_secs(60);
const COMMAND_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Exited(u32),
    Cancelled,
    TimedOut,
    TransportFailed,
    MissingExitCode,
}

pub enum Answer {
    Pending,
    Closed,
    Line(SensitiveLine),
}

pub fn marker(nonce: [u8; 16]) -> String {
    format!("[kortty-sudo-{:032x}] ", u128::from_be_bytes(nonce))
}

pub fn supervise(
    mut lease: ExecLease,
    marker: &str,
    permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
    mut answer: impl FnMut() -> Answer,
    challenge: impl FnOnce() -> bool,
) -> Result<Status, &'static str> {
    let abort = lease.abort_handle();
    let mut stdout = lease.take_reader().ok_or("SSH stdout is unavailable.")?;
    let mut stderr = lease.take_stderr().ok_or("SSH stderr is unavailable.")?;
    let mut stdin = lease.take_sensitive_exec_input();
    let mut challenge = Some(challenge);
    let mut prompt = Prompt::new(marker);
    let started = Instant::now();
    let mut deadline = started + CHALLENGE_LIMIT;
    let mut waiting = false;
    let mut submitted = false;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut buffer = Zeroizing::new([0; 8192]);
    thread::scope(|scope| {
        let mut writer: Option<thread::ScopedJoinHandle<'_, io::Result<()>>> = None;
        let result = (|| {
            loop {
                if !permitted() {
                    return Ok(Status::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Ok(Status::TimedOut);
                }
                if stdout_open && let Some(count) = read(&mut stdout, &mut buffer[..])? {
                    stdout_open = count != 0;
                    buffer[..count].zeroize();
                }
                if stderr_open && let Some(count) = read(&mut stderr, &mut buffer[..])? {
                    stderr_open = count != 0;
                    if count != 0 && waiting && !submitted {
                        return Err("Unexpected SSH output followed the sudo challenge.");
                    }
                    if let Some(notify) = challenge.take() {
                        if prompt.observe(&buffer[..count]) {
                            if !notify() {
                                return Ok(Status::Cancelled);
                            }
                            waiting = true;
                            deadline = (Instant::now() + ANSWER_LIMIT).min(started + COMMAND_LIMIT);
                        } else {
                            challenge = Some(notify);
                        }
                    }
                    buffer[..count].zeroize();
                }
                if !stdout_open && !stderr_open {
                    return Ok(lease
                        .exit_code()
                        .map_or(Status::MissingExitCode, Status::Exited));
                }
                if !submitted {
                    match answer() {
                        Answer::Line(line) if waiting && permitted() => {
                            let input = stdin.take().ok_or("SSH sudo input is unavailable.")?;
                            let permitted = permitted.clone();
                            writer = Some(
                                thread::Builder::new()
                                    .name("kortty-agent-sudo-input".into())
                                    .spawn_scoped(scope, move || input.send_if(line, permitted))
                                    .map_err(|_| "Could not start the owned SSH input worker.")?,
                            );
                            submitted = true;
                            deadline = started + COMMAND_LIMIT;
                        }
                        Answer::Line(_) => {
                            return Err("The sudo password request is no longer valid.");
                        }
                        Answer::Closed => return Ok(Status::Cancelled),
                        Answer::Pending => {}
                    }
                }
                if writer.as_ref().is_some_and(|writer| writer.is_finished())
                    && let Some(writer) = writer.take()
                    && !writer.join().is_ok_and(|result| result.is_ok())
                {
                    if let Some(code) = lease.exit_code() {
                        return Ok(Status::Exited(code));
                    }
                    return Err("The owned SSH sudo input could not be completed.");
                }
            }
        })();
        abort.abort();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        result
    })
}

fn read(reader: &mut SshReader, bytes: &mut [u8]) -> Result<Option<usize>, &'static str> {
    match reader.read_timeout(bytes, POLL) {
        Ok(count) => Ok(Some(count)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err("The owned SSH sudo output is unavailable."),
    }
}

pub(super) struct Prompt<'a> {
    marker: &'a [u8],
    matched: usize,
    line_start: bool,
}

impl<'a> Prompt<'a> {
    pub(super) fn new(marker: &'a str) -> Self {
        Self {
            marker: marker.as_bytes(),
            matched: 0,
            line_start: true,
        }
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        for (index, &byte) in bytes.iter().enumerate() {
            if (self.line_start || self.matched != 0) && self.marker[self.matched] == byte {
                self.line_start = false;
                self.matched += 1;
                if self.matched == self.marker.len() {
                    self.matched = 0;
                    if index + 1 == bytes.len() {
                        return true;
                    }
                }
            } else {
                self.matched = 0;
                self.line_start = byte == b'\n';
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_sudo_challenge_requires_a_matching_stderr_line_boundary() {
        let mut prompt = Prompt::new("[fixture] ");
        assert!(!prompt.observe(b"trace [fixture] \n[wrong] \n"));
        assert!(!prompt.observe(b"[fixture] wrong suffix\n"));
        assert!(!prompt.observe(b"[fix"));
        assert!(prompt.observe(b"ture] "));
    }
}
