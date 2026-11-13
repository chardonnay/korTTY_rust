//! Bounded request/reply exchange on the fixed archive helper exec channel.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kortty_server_archive::draft_replace;
use kortty_server_archive::protocol::{
    self, CreateSpec, PrivilegedReply, PrivilegedRequest, Reply, Request,
};
use kortty_ssh::{ExecOpener as DirectExecOpener, SshReader};

use crate::ssh_connection::ExecOpener;
use crate::ssh_worker;

const POLL: Duration = Duration::from_millis(25);
const MAX_DIAGNOSTICS: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivilegedArchiveFailure {
    BeforeCommit,
    Refused,
    Uncertain,
}

pub(crate) fn exchange_privileged(
    opener: &DirectExecOpener,
    spec: &CreateSpec,
    stopped: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()>,
) -> Result<(), PrivilegedArchiveFailure> {
    use PrivilegedArchiveFailure::{BeforeCommit, Refused, Uncertain};

    let command = format!(
        "exec /usr/bin/sudo -k -n -u root -- {} {}",
        protocol::PRIVILEGED_PATH,
        protocol::PRIVILEGED_ARGUMENT,
    );
    let mut frame = Vec::new();
    protocol::write_privileged_request(
        &mut frame,
        &PrivilegedRequest::Review { spec: spec.clone() },
    )
    .map_err(|_| BeforeCommit)?;
    authorize().map_err(|_| BeforeCommit)?;
    let mut lease = opener
        .open_cancellable(&command, stopped.clone())
        .map_err(|_| BeforeCommit)?;
    let mut input = lease.take_writer().ok_or(BeforeCommit)?;
    let mut stdout = lease.take_reader().ok_or(BeforeCommit)?;
    // Close the diagnostic reader so unexpected stderr cannot backpressure stdout.
    drop(lease.take_stderr());
    let abort = lease.abort_handle();
    authorize().map_err(|_| BeforeCommit)?;
    running(&stopped).map_err(|_| BeforeCommit)?;
    send(&stopped, move || abort.abort(), || input.write_all(&frame)).map_err(|_| BeforeCommit)?;
    let mut reader = PrivilegedReader {
        stdout: &mut stdout,
        stopped: &stopped,
    };
    let review = match protocol::read_privileged_reply(&mut reader).map_err(|_| BeforeCommit)? {
        PrivilegedReply::Review { review } => review,
        _ => return Err(Refused),
    };
    review.validate_for(spec).map_err(|_| BeforeCommit)?;
    frame.clear();
    protocol::write_privileged_request(
        &mut frame,
        &PrivilegedRequest::Commit {
            token: review.token,
        },
    )
    .map_err(|_| BeforeCommit)?;
    authorize().map_err(|_| BeforeCommit)?;
    running(&stopped).map_err(|_| BeforeCommit)?;
    let abort = lease.abort_handle();
    send(
        &stopped,
        move || abort.abort(),
        || {
            input.write_all(&frame)?;
            input.close_input()
        },
    )
    .map_err(|_| Uncertain)?;
    let reply = protocol::read_privileged_reply(&mut reader).map_err(|_| Uncertain)?;
    let mut trailing = [0; 1];
    if reader.read(&mut trailing).map_err(|_| Uncertain)? != 0
        || exit_code(|| lease.exit_code(), &stopped).map_err(|_| Uncertain)? != 0
    {
        return Err(Uncertain);
    }
    match reply {
        PrivilegedReply::Applied { destination } if destination == spec.destination => Ok(()),
        PrivilegedReply::Failed {
            outcome_unknown: false,
            ..
        } => Err(Refused),
        _ => Err(Uncertain),
    }
}

struct PrivilegedReader<'a, F> {
    stdout: &'a mut SshReader,
    stopped: &'a F,
}

impl<F: Fn() -> bool> Read for PrivilegedReader<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            // Read::read_exact retries Interrupted, so turn revocation into an error it returns.
            running(self.stopped).map_err(io::Error::other)?;
            match self.stdout.read_timeout(bytes, POLL) {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                result => return result,
            }
        }
    }
}

pub(crate) fn exchange(
    opener: &ExecOpener,
    request: &Request,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<Reply> {
    let applying = request.publishes();
    let mut frame = Vec::new();
    protocol::write_request(&mut frame, request)?;
    exchange_fixed(
        opener,
        Helper::Extract,
        &frame,
        applying,
        cancelled,
        authorize,
        |output, code| output.reply(code),
    )
}

pub(crate) fn exchange_draft_replace(
    opener: &ExecOpener,
    request: &draft_replace::Request,
    draft: &[u8],
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<draft_replace::Reply> {
    let mut frame = Vec::new();
    draft_replace::write_request(&mut frame, request, draft)?;
    exchange_fixed(
        opener,
        Helper::DraftReplace,
        &frame,
        matches!(request, draft_replace::Request::Apply { .. }),
        cancelled,
        authorize,
        |output, code| match code {
            0 if output.diagnostics == 0 => draft_replace::read_reply(output.data.as_slice()),
            126 | 127 if output.data.is_empty() => Err(io::Error::other(
                "The server draft replacement helper could not start. Check that /usr/local/bin/kortty-server-archive is installed and up to date.",
            )),
            _ => Err(invalid_output()),
        },
    )
}

#[derive(Clone, Copy)]
enum Helper {
    Extract,
    DraftReplace,
}

#[allow(clippy::too_many_arguments)]
fn exchange_fixed<T>(
    opener: &ExecOpener,
    helper: Helper,
    frame: &[u8],
    applying: bool,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()>,
    decode: impl FnOnce(Output, u32) -> io::Result<T>,
) -> io::Result<T> {
    let (command, timeout, unknown) = match helper {
        Helper::Extract => (
            protocol::COMMAND,
            if applying { 1800 } else { 300 },
            "The server archive publication outcome is unknown. Check the reviewed destination before retrying.",
        ),
        Helper::DraftReplace => (
            draft_replace::COMMAND,
            100,
            "The server file replacement outcome is unknown. Check the reviewed destination before retrying.",
        ),
    };
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let stopped = move || cancelled() || Instant::now() >= deadline;

    let mut output = Output::default();
    let mut submitted = false;
    let result = (|| {
        let code = match opener {
            ExecOpener::Direct(opener) => {
                let mut lease = opener
                    .open_cancellable(command, stopped.clone())
                    .map_err(io::Error::other)?;
                let mut input = lease.take_writer().ok_or_else(closed)?;
                let mut stdout = lease.take_reader().ok_or_else(closed)?;
                let mut stderr = lease.take_stderr().ok_or_else(closed)?;
                let abort = lease.abort_handle();
                authorize()?;
                running(&stopped)?;
                send(
                    &stopped,
                    move || abort.abort(),
                    || {
                        submitted = matches!(helper, Helper::Extract);
                        input.write_all(frame)?;
                        if matches!(helper, Helper::DraftReplace) {
                            authorize()?;
                            running(&stopped)?;
                        }
                        submitted = true;
                        input.close_input()
                    },
                )?;
                read_direct(&mut stdout, &mut stderr, &mut output, &stopped)?;
                exit_code(|| lease.exit_code(), &stopped)?
            }
            ExecOpener::Worker(opener) => {
                let mut channel = match helper {
                    Helper::Extract => opener.open_archive_extract(&stopped),
                    Helper::DraftReplace => opener.open_draft_replace(&stopped),
                }
                .map_err(io::Error::other)?;
                let mut input = channel.take_writer().ok_or_else(closed)?;
                let mut reader = channel.take_reader().ok_or_else(closed)?;
                let abort = channel.abort_handle();
                authorize()?;
                running(&stopped)?;
                send(
                    &stopped,
                    move || abort.abort(),
                    || {
                        submitted = matches!(helper, Helper::Extract);
                        input.write_all(frame)?;
                        if matches!(helper, Helper::DraftReplace) {
                            authorize()?;
                            running(&stopped)?;
                        }
                        submitted = true;
                        input.close_input()
                    },
                )?;
                read_tagged(
                    PollReader {
                        reader: &mut reader,
                        stopped: &stopped,
                    },
                    &mut output,
                )?;
                exit_code(|| channel.exit_code(), &stopped)?
            }
        };
        decode(output, code)
    })();
    result.map_err(|error| {
        if applying && submitted {
            io::Error::new(error.kind(), format!("{error} · {unknown}"))
        } else {
            error
        }
    })
}

fn send(
    stopped: &(impl Fn() -> bool + Sync),
    abort: impl FnOnce() + Send,
    submit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    // Both writers can block until the original exec lane accepts EOF.
    let complete = AtomicBool::new(false);
    thread::scope(|scope| {
        let watchdog = thread::Builder::new()
            .name("kortty-server-archive-submit".into())
            .spawn_scoped(scope, || {
                while !complete.load(Ordering::Acquire) {
                    if stopped() {
                        abort();
                        break;
                    }
                    thread::park_timeout(POLL);
                }
            })?;
        let result = submit();
        complete.store(true, Ordering::Release);
        watchdog.thread().unpark();
        watchdog.join().map_err(|_| closed())?;
        result
    })
}

fn read_direct(
    stdout: &mut kortty_ssh::SshReader,
    stderr: &mut kortty_ssh::SshReader,
    output: &mut Output,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    let mut streams = [Some(stdout), Some(stderr)];
    let mut buffer = [0; 8192];
    while streams.iter().any(Option::is_some) {
        for (tag, stream) in streams.iter_mut().enumerate() {
            let Some(reader) = stream else { continue };
            running(stopped)?;
            match reader.read_timeout(&mut buffer, POLL) {
                Ok(0) => *stream = None,
                Ok(count) => output.push(tag as u8, &buffer[..count])?,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn read_tagged(mut input: impl Read, output: &mut Output) -> io::Result<()> {
    let mut header = [0; 3];
    let mut buffer = [0; 8192];
    while input.read(&mut header[..1])? != 0 {
        input.read_exact(&mut header[1..])?;
        let mut remaining = usize::from(u16::from_be_bytes([header[1], header[2]]));
        if remaining == 0 || header[0] > 1 {
            return Err(invalid_output());
        }
        while remaining != 0 {
            let count = remaining.min(buffer.len());
            input.read_exact(&mut buffer[..count])?;
            output.push(header[0], &buffer[..count])?;
            remaining -= count;
        }
    }
    Ok(())
}

struct PollReader<'a, F> {
    reader: &'a mut ssh_worker::Reader,
    stopped: &'a F,
}

impl<F: Fn() -> bool> Read for PollReader<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            // Read::read_exact retries Interrupted, including mid-record.
            running(self.stopped).map_err(io::Error::other)?;
            match self.reader.read_timeout(bytes, POLL) {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
                result => return result,
            }
        }
    }
}

#[derive(Default)]
struct Output {
    data: Vec<u8>,
    diagnostics: usize,
}

impl Output {
    fn push(&mut self, tag: u8, bytes: &[u8]) -> io::Result<()> {
        match tag {
            0 if bytes.len() <= protocol::MAX_RESPONSE_BYTES + 4 - self.data.len() => {
                self.data.extend_from_slice(bytes);
            }
            1 if bytes.len() <= MAX_DIAGNOSTICS - self.diagnostics => {
                self.diagnostics += bytes.len();
            }
            _ => return Err(invalid_output()),
        }
        Ok(())
    }

    fn reply(self, code: u32) -> io::Result<Reply> {
        match code {
            0 if self.diagnostics == 0 => protocol::read_reply(self.data.as_slice()),
            126 | 127 if self.data.is_empty() => Err(io::Error::other(
                "The native server archive helper could not start. Check that /usr/local/bin/kortty-server-archive is installed and executable.",
            )),
            _ => Err(invalid_output()),
        }
    }
}

fn exit_code(code: impl Fn() -> Option<u32>, stopped: &impl Fn() -> bool) -> io::Result<u32> {
    loop {
        if let Some(code) = code() {
            return Ok(code);
        }
        running(stopped)?;
        thread::sleep(POLL);
    }
}

fn running(stopped: &impl Fn() -> bool) -> io::Result<()> {
    if stopped() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Server helper operation stopped",
        ));
    }
    Ok(())
}

fn closed() -> io::Error {
    io::Error::other("The server archive helper channel closed")
}

fn invalid_output() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The server archive helper returned unexpected or oversized output",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_helper_accepts_a_split_reply_and_refuses_invalid_tagged_output() {
        let mut frame = Vec::new();
        protocol::write_reply(
            &mut frame,
            &Reply::Applied {
                destination: "/srv/out".into(),
            },
        )
        .unwrap();
        let tagged: Vec<u8> = frame
            .chunks(11)
            .flat_map(|chunk| {
                let length = (chunk.len() as u16).to_be_bytes();
                [0, length[0], length[1]]
                    .into_iter()
                    .chain(chunk.iter().copied())
            })
            .collect();
        let mut output = Output::default();
        read_tagged(tagged.as_slice(), &mut output).unwrap();
        assert!(
            matches!(output.reply(0).unwrap(), Reply::Applied { destination } if destination == "/srv/out")
        );

        for bytes in [&tagged[..tagged.len() - 1], &[2, 0, 1, 0], &[0, 0, 0]] {
            assert!(read_tagged(bytes, &mut Output::default()).is_err());
        }
        let mut diagnostics = Output::default();
        assert!(diagnostics.push(1, &[0; MAX_DIAGNOSTICS + 1]).is_err());
    }
}
