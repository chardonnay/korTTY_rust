//! Bounded request/reply exchange on the fixed archive helper exec channel.

use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kortty::ssh_runtime::password::{Prompt, marker};
use kortty_server_archive::draft_replace;
use kortty_server_archive::protocol::{
    self, CreateSpec, CreatedPayload, PrivilegedReply, PrivilegedRequest, Reply, Request,
};
use kortty_ssh::{ExecLease, ExecOpener as DirectExecOpener, SensitiveLine, SshReader, SshWriter};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::ssh_connection::ExecOpener;
use crate::ssh_worker;

const POLL: Duration = Duration::from_millis(25);
const MAX_DIAGNOSTICS: usize = 8 * 1024;
const COPY_POLICY_POLL: Duration = Duration::from_millis(250);
const SUDO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivilegedArchiveFailure {
    BeforeCommit,
    Refused,
    Uncertain,
    CopyFailed,
}

pub(crate) fn exchange_privileged(
    opener: &DirectExecOpener,
    spec: &CreateSpec,
    sudo_password: Option<SensitiveLine>,
    password: Option<&str>,
    copy: Option<&File>,
    stopped: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Clone + Send + Sync + 'static,
) -> Result<(), PrivilegedArchiveFailure> {
    use PrivilegedArchiveFailure::{BeforeCommit, CopyFailed, Refused, Uncertain};

    if (spec.format == protocol::CreateFormat::ZipPassword) != password.is_some()
        || spec.report_payload != copy.is_some()
    {
        return Err(BeforeCommit);
    }
    let password = password
        .map(protocol::creation_password_body)
        .transpose()
        .map_err(|_| BeforeCommit)?;
    let challenge = if sudo_password.is_some() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| BeforeCommit)?;
        Some(marker(nonce))
    } else {
        None
    };
    let command = privileged_command(challenge.as_deref());
    let mut frame = Vec::new();
    protocol::write_privileged_request(
        &mut frame,
        &PrivilegedRequest::Review { spec: spec.clone() },
    )
    .map_err(|_| BeforeCommit)?;
    authorize().map_err(|_| BeforeCommit)?;
    let opening_authorize = authorize.clone();
    let opening_stopped = stopped.clone();
    let stop_opening = move || opening_stopped() || opening_authorize().is_err();
    let mut lease = if sudo_password.is_some() || password.is_some() {
        opener.open_sensitive_cancellable(&command, stop_opening)
    } else {
        opener.open_cancellable(&command, stop_opening)
    }
    .map_err(|_| BeforeCommit)?;
    let mut stdout = lease.take_reader().ok_or(BeforeCommit)?;
    let mut input = if let Some((line, challenge)) = sudo_password.zip(challenge.as_deref()) {
        authenticate_privileged(
            &mut lease,
            &mut stdout,
            line,
            challenge,
            stopped.clone(),
            authorize.clone(),
        )
        .map_err(|_| BeforeCommit)?
    } else {
        // Without a challenge, discard stderr so it cannot backpressure stdout.
        drop(lease.take_stderr());
        lease.take_writer().ok_or(BeforeCommit)?
    };
    let abort = lease.abort_handle();
    let before_commit = || stopped() || authorize().is_err();
    send(
        &before_commit,
        move || abort.abort(),
        || {
            running(&before_commit)?;
            input.write_all(&frame)
        },
    )
    .map_err(|_| BeforeCommit)?;
    let review = match protocol::read_privileged_reply(PrivilegedReader {
        stdout: &mut stdout,
        stopped: &before_commit,
    })
    .map_err(|_| BeforeCommit)?
    {
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
    let check_cancel = stopped.clone();
    let commit_authority = authorize.clone();
    let password_protocol = challenge.is_some();
    let mut submitted = false;
    send(
        &before_commit,
        move || abort.abort(),
        || {
            running(&before_commit)?;
            submitted = true;
            let permitted = move || !check_cancel() && commit_authority().is_ok();
            match password {
                Some(password) => {
                    input.write_all(&frame)?;
                    input.send_sensitive_input(password, protocol::PASSWORD_COMMIT, permitted)
                }
                None if password_protocol => input.send_sensitive_input(
                    Zeroizing::new(frame),
                    protocol::PRIVILEGED_PASSWORD_COMMIT,
                    permitted,
                ),
                None => {
                    input.write_all(&frame)?;
                    input.close_input()
                }
            }
        },
    )
    .map_err(|_| if submitted { Uncertain } else { BeforeCommit })?;
    let mut reader = PrivilegedReader {
        stdout: &mut stdout,
        stopped: &stopped,
    };
    let reply = protocol::read_privileged_reply(&mut reader).map_err(|_| Uncertain)?;
    let finishing = match reply {
        PrivilegedReply::AppliedCopy {
            destination,
            payload,
        } if destination == spec.destination && spec.report_payload => {
            let mut copy = copy.ok_or(CopyFailed)?;
            authorize().map_err(|_| CopyFailed)?;
            let checked_at = Cell::new(Instant::now());
            let stopped_copy = || {
                if stopped() {
                    return true;
                }
                let now = Instant::now();
                if now.duration_since(checked_at.get()) >= COPY_POLICY_POLL {
                    checked_at.set(now);
                    return authorize().is_err();
                }
                false
            };
            let mut copy_reader = PrivilegedReader {
                stdout: &mut *reader.stdout,
                stopped: &stopped_copy,
            };
            receive_archive_copy(&mut copy_reader, &mut copy, payload).map_err(|_| CopyFailed)?;
            CopyFailed
        }
        PrivilegedReply::Applied { destination } if destination == spec.destination => {
            if spec.report_payload {
                require_privileged_eof(&mut reader, || lease.exit_code(), &stopped)
                    .map_err(|_| Uncertain)?;
                return Err(CopyFailed);
            }
            Uncertain
        }
        PrivilegedReply::Failed {
            outcome_unknown: false,
            ..
        } => {
            require_privileged_eof(&mut reader, || lease.exit_code(), &stopped)
                .map_err(|_| Uncertain)?;
            return Err(Refused);
        }
        _ => return Err(Uncertain),
    };
    require_privileged_eof(&mut reader, || lease.exit_code(), &stopped).map_err(|_| finishing)?;
    if spec.report_payload {
        authorize().map_err(|_| CopyFailed)?;
    }
    Ok(())
}

fn privileged_command(challenge: Option<&str>) -> String {
    let mut command = String::from("exec /usr/bin/sudo -k ");
    let argument = if let Some(challenge) = challenge {
        command.push_str("-S -p ");
        crate::scheduler_window::script::shell_quote(&mut command, challenge);
        command.push(' ');
        protocol::PRIVILEGED_PASSWORD_ARGUMENT
    } else {
        command.push_str("-n ");
        protocol::PRIVILEGED_ARGUMENT
    };
    command.push_str("-u root -- ");
    command.push_str(protocol::PRIVILEGED_PATH);
    command.push(' ');
    command.push_str(argument);
    command
}

fn authenticate_privileged(
    lease: &mut ExecLease,
    stdout: &mut SshReader,
    line: SensitiveLine,
    challenge: &str,
    stopped: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Clone + Send + Sync + 'static,
) -> io::Result<SshWriter> {
    let deadline = Instant::now() + SUDO_TIMEOUT;
    let stopped = move || stopped() || Instant::now() >= deadline || authorize().is_err();
    let mut line = Some(line);
    let mut input = None;
    let mut stderr = lease.take_stderr();
    let mut prompt = Prompt::new(challenge);
    let mut diagnostics = Zeroizing::new([0; MAX_DIAGNOSTICS]);
    let mut diagnostic_bytes = 0;
    let expected = protocol::PRIVILEGED_PASSWORD_READY;
    let mut greeting = [0; protocol::PRIVILEGED_PASSWORD_READY.len()];
    let mut offset = 0;
    while offset < greeting.len() {
        running(&stopped)?;
        // NOPASSWD rules can start the helper without requesting the saved password.
        match stdout.read_timeout(&mut greeting[offset..], POLL) {
            Ok(0) => return Err(invalid_output()),
            Ok(count) => {
                offset += count;
                if greeting[..offset] != expected[..offset] {
                    return Err(invalid_output());
                }
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
        let Some(diagnostic) = &mut stderr else {
            continue;
        };
        match diagnostic.read_timeout(&mut diagnostics[..], Duration::ZERO) {
            Ok(0) => stderr = None,
            Ok(count) => {
                diagnostic_bytes += count;
                if diagnostic_bytes > MAX_DIAGNOSTICS {
                    return Err(invalid_output());
                }
                if prompt.observe(&diagnostics[..count]) {
                    running(&stopped)?;
                    let line = line.take().ok_or_else(invalid_output)?;
                    let writer = lease.take_sensitive_exec_input().ok_or_else(closed)?;
                    let abort = lease.abort_handle();
                    let guard = stopped.clone();
                    input = Some(send(
                        &stopped,
                        move || abort.abort(),
                        || writer.send_then(line, move || !guard()),
                    )?);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
    }
    running(&stopped)?;
    input.or_else(|| lease.take_writer()).ok_or_else(closed)
}

fn require_privileged_eof(
    input: &mut impl Read,
    exit_status: impl Fn() -> Option<u32>,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    let mut trailing = [0; 1];
    if input.read(&mut trailing)? != 0 || exit_code(exit_status, stopped)? != 0 {
        return Err(invalid_output());
    }
    Ok(())
}

fn receive_archive_copy(
    input: &mut impl Read,
    output: &mut impl Write,
    payload: CreatedPayload,
) -> io::Result<()> {
    payload.validate()?;
    let mut hash = Sha256::new();
    let mut remaining = payload.bytes;
    let mut buffer = [0; 64 * 1024];
    while remaining != 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        let bytes = &mut buffer[..count];
        input.read_exact(bytes)?;
        hash.update(&*bytes);
        output.write_all(bytes)?;
        remaining -= count as u64;
    }
    if <[u8; 32]>::from(hash.finalize()) != payload.sha256 {
        return Err(invalid_output());
    }
    Ok(())
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

pub(crate) fn exchange_password_creation(
    opener: &ExecOpener,
    request: &Request,
    password: &str,
    cancelled: impl Fn() -> bool + Clone + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Clone + Send + Sync + 'static,
) -> io::Result<Reply> {
    let ExecOpener::Direct(opener) = opener else {
        return Err(io::Error::other(
            "Password ZIP requires the direct SSH route",
        ));
    };
    if !request.needs_password() {
        return Err(io::Error::other(
            "The archive creation does not use a password",
        ));
    }
    let body = protocol::creation_password_body(password)?;
    let mut frame = Vec::new();
    protocol::write_request(&mut frame, request)?;
    let deadline = Instant::now() + Duration::from_secs(1800);
    let greeting_deadline = Instant::now() + Duration::from_secs(30);
    let stopped = move || cancelled() || Instant::now() >= deadline;
    let greeting_stop = stopped.clone();
    let awaiting_greeting = move || greeting_stop() || Instant::now() >= greeting_deadline;

    authorize()?;
    let mut lease = opener
        .open_sensitive_cancellable(protocol::COMMAND, awaiting_greeting.clone())
        .map_err(io::Error::other)?;
    let mut input = lease.take_writer().ok_or_else(closed)?;
    let mut stdout = lease.take_reader().ok_or_else(closed)?;
    drop(lease.take_stderr());
    let abort = lease.abort_handle();
    authorize()?;
    running(&awaiting_greeting)?;
    send(
        &awaiting_greeting,
        move || abort.abort(),
        || input.write_all(&frame),
    )?;
    let mut greeting = Zeroizing::new([0; protocol::PASSWORD_READY.len()]);
    PrivilegedReader {
        stdout: &mut stdout,
        stopped: &awaiting_greeting,
    }
    .read_exact(greeting.as_mut())?;
    if greeting.as_slice() != protocol::PASSWORD_READY {
        return Err(io::Error::other(
            "The server helper did not accept password ZIP",
        ));
    }

    authorize()?;
    running(&stopped)?;
    let abort = lease.abort_handle();
    let authorized = authorize.clone();
    let guard = stopped.clone();
    let result = (|| {
        send(
            &stopped,
            move || abort.abort(),
            || {
                input.send_sensitive_input(body, protocol::PASSWORD_COMMIT, move || {
                    !guard() && authorized().is_ok()
                })
            },
        )?;
        let reader = PrivilegedReader {
            stdout: &mut stdout,
            stopped: &stopped,
        };
        let reply = protocol::read_reply(reader)?;
        if exit_code(|| lease.exit_code(), &stopped)? != 0 {
            return Err(invalid_output());
        }
        Ok(reply)
    })();
    result.map_err(|_: io::Error| io::Error::other(
        "The server archive publication outcome is unknown. Check the reviewed destination before retrying.",
    ))
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

fn send<T>(
    stopped: &(impl Fn() -> bool + Sync),
    abort: impl FnOnce() + Send,
    submit: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
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
