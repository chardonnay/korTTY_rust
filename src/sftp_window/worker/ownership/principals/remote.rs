//! Resolves one user or group on the already authenticated SFTP connection.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kortty::ssh_runtime::account_lookup::{
    DEADLINE, DISPLAY_DEADLINE, DISPLAY_OUTPUT_LIMIT, DisplayNames, DisplayQuery,
    OUTPUT_LIMIT as MAX_OUTPUT, command,
};
use kortty_ssh::{ConnectError, HostKey, SshReader};
use zeroize::Zeroizing;

use crate::sftp_window::worker::{Failure, Result};
use crate::{ssh_connection::ExecOpener, ssh_worker};

use super::{Kind, valid_name};

const POLL: Duration = Duration::from_millis(20);

pub(in crate::sftp_window::worker) fn lookup_unavailable() -> io::Error {
    io::Error::other("A verified live SSH command channel is required for server account lookup.")
}

pub(in crate::sftp_window::worker) fn resolve_for_peer(
    opener: Option<&ExecOpener>,
    peer: &HostKey,
    kind: Kind,
    name: &str,
    wait_for_session: bool,
    guard: &impl Fn() -> Result<()>,
) -> Result<u32> {
    let opener = opener
        .filter(|opener| !opener.is_closed() && opener.verified_host_key() == Some(peer))
        .ok_or_else(lookup_unavailable)?;
    resolve(opener, kind, name, wait_for_session, guard)
}

#[derive(Clone)]
struct Gate {
    deadline: Instant,
    stopped: Arc<AtomicBool>,
    wait_for_session: bool,
}

impl Gate {
    fn timeout(&self) -> io::Result<Duration> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Server account lookup cancelled.",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Server account lookup timed out. Enter a numeric UID or GID.",
            ));
        }
        Ok(POLL.min(remaining))
    }

    fn open<T>(
        &self,
        mut attempt: impl FnMut() -> std::result::Result<T, ConnectError>,
    ) -> std::result::Result<T, ConnectError> {
        loop {
            self.timeout()?;
            match attempt() {
                Err(error) if self.wait_for_session && session_slot_unavailable(&error) => {
                    // A cancelled advisory measurement can still own the
                    // server's last session slot while its close is processed.
                    thread::sleep(self.timeout()?);
                }
                result => return result,
            }
        }
    }
}

fn session_slot_unavailable(error: &ConnectError) -> bool {
    const LIMIT: &str = "this SSH connection has reached its channel limit for this request";
    match error {
        ConnectError::RequestRejected { request: "session" } => true,
        ConnectError::Io(error) if error.kind() == io::ErrorKind::WouldBlock => {
            error.to_string() == LIMIT
        }
        // The isolated worker reports its channel-open errors over IPC.
        ConnectError::Io(error) if error.kind() == io::ErrorKind::Other => matches!(
            error.to_string().as_str(),
            LIMIT | "SSH server refused the session request"
        ),
        _ => false,
    }
}

pub(in crate::sftp_window::worker) fn resolve(
    opener: &ExecOpener,
    kind: Kind,
    name: &str,
    wait_for_session: bool,
    guard: &impl Fn() -> Result<()>,
) -> Result<u32> {
    guard()?;
    if opener.is_closed() || opener.verified_host_key().is_none() {
        return Err(io::Error::other(
            "A verified live SSH connection is required for server account lookup.",
        )
        .into());
    }
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The server account name is invalid.",
        )
        .into());
    }
    let gate = Gate {
        deadline: Instant::now() + DEADLINE,
        stopped: Arc::new(AtomicBool::new(false)),
        wait_for_session,
    };
    let (exit, bytes) = thread::scope(|scope| -> Result<_> {
        let runner_gate = gate.clone();
        let runner = thread::Builder::new()
            .name("kortty-sftp-account-lookup".into())
            .spawn_scoped(scope, move || {
                run(opener, Query::Exact(kind, name), &runner_gate)
            })?;
        let permitted = loop {
            match guard().and_then(|()| gate.timeout().map_err(Failure::from)) {
                Ok(_) if runner.is_finished() => break Ok(()),
                Ok(wait) => thread::sleep(wait),
                Err(error) => {
                    gate.stopped.store(true, Ordering::Release);
                    break Err(error);
                }
            }
        };
        let output = runner
            .join()
            .map_err(|_| unavailable("the lookup worker stopped"))?;
        permitted?;
        output
    })?;
    guard()?;
    match exit {
        0 => parse(kind, name, &bytes),
        2 if bytes.is_empty() => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "The named server account was not found. Enter a numeric UID or GID.",
        )
        .into()),
        _ => Err(unavailable("the command failed")),
    }
}

pub(in crate::sftp_window) fn display_names(
    opener: &ExecOpener,
    query: &DisplayQuery,
    stopped: Arc<AtomicBool>,
) -> Option<DisplayNames> {
    if opener.is_closed() || opener.verified_host_key().is_none() {
        return None;
    }
    let gate = Gate {
        deadline: Instant::now() + DISPLAY_DEADLINE,
        stopped,
        wait_for_session: false,
    };
    let (exit, bytes) = run(opener, Query::Display(query), &gate).ok()?;
    gate.timeout().ok()?;
    if exit != 0 || opener.is_closed() {
        return None;
    }
    query.parse(&bytes)
}

#[derive(Clone, Copy)]
enum Query<'a> {
    Exact(Kind, &'a str),
    Display(&'a DisplayQuery),
}

fn run(opener: &ExecOpener, query: Query<'_>, gate: &Gate) -> Result<(u32, Zeroizing<Vec<u8>>)> {
    gate.timeout()?;
    let limit = match query {
        Query::Exact(..) => MAX_OUTPUT,
        Query::Display(_) => DISPLAY_OUTPUT_LIMIT,
    };
    match opener {
        ExecOpener::Direct(opener) => {
            let command = match query {
                Query::Exact(kind, name) => command(kind, name)?,
                Query::Display(query) => query.command(),
            };
            let mut channel = gate
                .open(|| {
                    let opening = gate.clone();
                    let before_exec = gate.clone();
                    opener.open_guarded(
                        &command,
                        move || opening.timeout().is_err(),
                        move || before_exec.timeout().map(|_| ()),
                    )
                })
                .map_err(|_| unavailable("could not open the SSH command"))?;
            let mut stdout = channel.take_reader().ok_or_else(missing_stream)?;
            let mut stderr = channel.take_stderr().ok_or_else(missing_stream)?;
            let bytes = collect_direct(&mut stdout, &mut stderr, gate, limit)?;
            let exit = loop {
                if let Some(exit) = channel.exit_code() {
                    break exit;
                }
                thread::sleep(gate.timeout()?);
            };
            Ok((exit, bytes))
        }
        ExecOpener::Worker(opener) => {
            let mut channel = gate
                .open(|| match query {
                    Query::Exact(kind, name) => {
                        opener.open_account_lookup(kind, name, || gate.timeout().is_err())
                    }
                    Query::Display(query) => {
                        opener.open_account_names(query, || gate.timeout().is_err())
                    }
                })
                .map_err(|_| unavailable("could not open the isolated SSH command"))?;
            let mut reader = channel.take_reader().ok_or_else(missing_stream)?;
            let bytes = collect_tagged(&mut reader, gate, limit)?;
            let exit = channel
                .exit_code()
                .ok_or_else(|| unavailable("no command exit status"))?;
            Ok((exit, bytes))
        }
    }
}

fn collect_direct(
    stdout: &mut SshReader,
    stderr: &mut SshReader,
    gate: &Gate,
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut streams = [Some(stdout), Some(stderr)];
    let mut output = Zeroizing::new(Vec::new());
    let mut buffer = Zeroizing::new([0; 512]);
    while streams.iter().any(Option::is_some) {
        for (kind, stream) in streams.iter_mut().enumerate() {
            let Some(reader) = stream else { continue };
            match reader.read_timeout(&mut buffer[..], gate.timeout()?) {
                Ok(0) => *stream = None,
                Ok(count) if kind == 0 && count <= limit - output.len() => {
                    output.extend_from_slice(&buffer[..count]);
                }
                Ok(_) if kind == 1 => return Err(unavailable("the command wrote to stderr")),
                Ok(_) => return Err(unavailable("the output exceeded its size limit")),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(unavailable("the command output stream failed")),
            }
        }
    }
    Ok(output)
}

fn collect_tagged(
    reader: &mut ssh_worker::Reader,
    gate: &Gate,
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut output = Zeroizing::new(Vec::new());
    let mut header = [0; 3];
    while read_tagged(reader, &mut header, gate, true)? {
        let count = usize::from(u16::from_be_bytes([header[1], header[2]]));
        if header[0] == 1 {
            return Err(unavailable("the command wrote to stderr"));
        }
        if header[0] != 0 || count == 0 || count > limit - output.len() {
            return Err(unavailable("invalid or oversized command output"));
        }
        let start = output.len();
        output.resize(start + count, 0);
        read_tagged(reader, &mut output[start..], gate, false)?;
    }
    Ok(output)
}

fn read_tagged(
    reader: &mut ssh_worker::Reader,
    mut bytes: &mut [u8],
    gate: &Gate,
    mut may_end: bool,
) -> Result<bool> {
    while !bytes.is_empty() {
        match reader.read_timeout(bytes, gate.timeout()?) {
            Ok(0) if may_end => return Ok(false),
            Ok(0) => return Err(unavailable("the isolated output ended mid-record")),
            Ok(count) => {
                bytes = &mut bytes[count..];
                may_end = false;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(unavailable("the isolated output stream failed")),
        }
    }
    Ok(true)
}

fn parse(kind: Kind, name: &str, output: &[u8]) -> Result<u32> {
    if output.len() > MAX_OUTPUT {
        return Err(invalid_record());
    }
    let record = output.strip_suffix(b"\n").ok_or_else(invalid_record)?;
    if record.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
        return Err(invalid_record());
    }
    let mut fields = record.split(|byte| *byte == b':');
    if fields.next() != Some(name.as_bytes()) || fields.next().is_none() {
        return Err(invalid_record());
    }
    let id = fields.next().ok_or_else(invalid_record)?;
    let remaining = match kind {
        Kind::User => 4,
        Kind::Group => 1,
    };
    if fields.count() != remaining || id.is_empty() || !id.iter().all(u8::is_ascii_digit) {
        return Err(invalid_record());
    }
    std::str::from_utf8(id)
        .ok()
        .and_then(|id| id.parse::<u32>().ok())
        .filter(|&id| id != u32::MAX)
        .ok_or_else(invalid_record)
}

fn missing_stream() -> Failure {
    unavailable("the command output stream is unavailable")
}

fn invalid_record() -> Failure {
    unavailable("the command returned no exact account record")
}

fn unavailable(reason: &str) -> Failure {
    io::Error::other(format!(
        "Could not verify a server account by name ({reason}). Enter a numeric UID or GID."
    ))
    .into()
}

#[cfg(all(test, target_os = "linux"))]
mod openssh;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_principal_command_has_one_fixed_database_and_exact_account_argument() {
        assert_eq!(
            command(Kind::User, "service$").unwrap(),
            "LC_ALL=C exec /usr/bin/getent -- passwd 'service$' </dev/null"
        );
        assert_eq!(
            command(Kind::Group, "work-team@example").unwrap(),
            "LC_ALL=C exec /usr/bin/getent -- group 'work-team@example' </dev/null"
        );
        for invalid in [
            "",
            "1000",
            "-option",
            "name suffix",
            "quoted'name",
            "name\n",
        ] {
            assert!(command(Kind::User, invalid).is_err());
        }
    }

    #[test]
    fn remote_principal_parser_requires_one_exact_record_with_a_usable_numeric_id() {
        let passwd = b"operator:x:1000:2000:Operator:/home/operator:/bin/sh\n";
        let group = b"operators:x:2000:operator,service\n";
        assert_eq!(parse(Kind::User, "operator", passwd).unwrap(), 1000);
        assert_eq!(parse(Kind::Group, "operators", group).unwrap(), 2000);
        assert!(parse(Kind::Group, "operator", passwd).is_err());
        assert!(parse(Kind::User, "operator", group).is_err());
        for invalid in [
            b"Operator:x:1000:2000:Operator:/home/operator:/bin/sh\n".as_slice(),
            b"operator:x:1000:2000:Operator:/home/operator:/bin/sh",
            b"operator:x:4294967295:2000:Operator:/home/operator:/bin/sh\n",
            b"operator:x:4294967296:2000:Operator:/home/operator:/bin/sh\n",
            b"operator:x:+1000:2000:Operator:/home/operator:/bin/sh\n",
            b"operator:x:1000:2000:Operator:/home/operator:/bin/sh\r\n",
            b"operator:x:1000:2000:Op\0erator:/home/operator:/bin/sh\n",
        ] {
            assert!(parse(Kind::User, "operator", invalid).is_err());
        }
        let multiple = [passwd.as_slice(), passwd].concat();
        assert!(parse(Kind::User, "operator", &multiple).is_err());
        let oversized = vec![b'x'; MAX_OUTPUT + 1];
        assert!(parse(Kind::User, "operator", &oversized).is_err());
    }

    #[test]
    fn remote_principal_gate_stops_after_deadline_or_cancellation() {
        let gate = Gate {
            deadline: Instant::now(),
            stopped: Arc::new(AtomicBool::new(false)),
            wait_for_session: false,
        };
        assert_eq!(gate.timeout().unwrap_err().kind(), io::ErrorKind::TimedOut);
        gate.stopped.store(true, Ordering::Release);
        assert_eq!(
            gate.timeout().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn remote_principal_wait_retries_only_an_unavailable_session_slot() {
        let gate = Gate {
            deadline: Instant::now() + Duration::from_secs(2),
            stopped: Arc::new(AtomicBool::new(false)),
            wait_for_session: true,
        };
        for refusal in [
            ConnectError::RequestRejected { request: "session" },
            io::Error::other("SSH server refused the session request").into(),
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "this SSH connection has reached its channel limit for this request",
            )
            .into(),
            io::Error::other("this SSH connection has reached its channel limit for this request")
                .into(),
        ] {
            let mut first = Some(refusal);
            assert_eq!(gate.open(|| first.take().map_or(Ok(42), Err)).unwrap(), 42);
        }
        assert!(matches!(
            gate.open::<()>(|| Err(ConnectError::RequestRejected { request: "exec" })),
            Err(ConnectError::RequestRejected { request: "exec" })
        ));
        assert!(matches!(
            gate.open::<()>(|| Err(io::Error::from(io::ErrorKind::WouldBlock).into())),
            Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        let ordinary = Gate {
            wait_for_session: false,
            ..gate
        };
        assert!(matches!(
            ordinary.open::<()>(|| Err(ConnectError::RequestRejected { request: "session" })),
            Err(ConnectError::RequestRejected { request: "session" })
        ));
    }
}
