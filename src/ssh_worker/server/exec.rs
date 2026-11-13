use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use kortty_ssh::{ExecLease, SshWriter};
use zeroize::Zeroizing;

use super::{
    ACCEPT_POLL, ConnectError, Flow, Frame, Server, SshReader, closed, file_exec, lock, thread,
};
use crate::ssh_runtime::account_lookup::{self, DisplayQuery, Kind};
use crate::ssh_runtime::folder_usage;
use crate::ssh_runtime::mux::Writer;
use crate::ssh_runtime::password::{self, Answer, Status};
use crate::ssh_runtime::wire::CHUNK;

impl Server {
    pub(super) fn exec(&self, flow: &Flow, command: &str) -> Result<(), ConnectError> {
        self.exec_command(flow, command, None)
    }

    pub(super) fn exec_file(&self, flow: &Flow) -> Result<(), ConnectError> {
        let command = crate::ssh_runtime::password_file::command(None)?;
        let deadline = Instant::now() + crate::ssh_runtime::password_file::DEADLINE;
        self.exec_command(flow, &command, Some(deadline))
    }

    pub(super) fn exec_archive_extract(&self, flow: &Flow) -> Result<(), ConnectError> {
        self.exec_verified_helper(flow, kortty_server_archive::protocol::COMMAND)
    }

    pub(super) fn exec_draft_replace(&self, flow: &Flow) -> Result<(), ConnectError> {
        self.exec_verified_helper(flow, kortty_server_archive::draft_replace::COMMAND)
    }

    fn exec_verified_helper(&self, flow: &Flow, command: &str) -> Result<(), ConnectError> {
        if !self
            .exec
            .as_ref()
            .is_some_and(|opener| opener.verified_host_key().is_some())
        {
            return Err(closed().into());
        }
        self.exec_command(flow, command, None)
    }

    pub(super) fn exec_account_lookup(
        &self,
        flow: &Flow,
        kind: Kind,
        name: &str,
    ) -> Result<(), ConnectError> {
        self.exec_read_only(
            flow,
            &account_lookup::command(kind, name)?,
            account_lookup::DEADLINE,
            account_lookup::OUTPUT_LIMIT,
        )
    }

    pub(super) fn exec_folder_usage(
        &self,
        flow: &Flow,
        paths: &[Vec<u8>],
    ) -> Result<(), ConnectError> {
        self.exec_read_only(
            flow,
            &folder_usage::command(paths)?,
            folder_usage::DEADLINE,
            folder_usage::OUTPUT_LIMIT,
        )
    }

    pub(super) fn exec_account_names(
        &self,
        flow: &Flow,
        query: &DisplayQuery,
    ) -> Result<(), ConnectError> {
        self.exec_read_only(
            flow,
            &query.command(),
            account_lookup::DISPLAY_DEADLINE,
            account_lookup::DISPLAY_OUTPUT_LIMIT,
        )
    }

    fn exec_read_only(
        &self,
        flow: &Flow,
        command: &str,
        duration: Duration,
        output_limit: usize,
    ) -> Result<(), ConnectError> {
        let opener = self
            .exec
            .as_ref()
            .filter(|opener| opener.verified_host_key().is_some())
            .ok_or_else(closed)?;
        let deadline = Instant::now() + duration;
        let gate = flow.clone();
        let before_exec = flow.clone();
        let mut lease = opener.open_guarded(
            command,
            move || relay_time_left(&gate, deadline).is_err(),
            move || relay_time_left(&before_exec, deadline).map(|_| ()),
        )?;
        let mut stdout = lease.take_reader().ok_or_else(closed)?;
        let mut stderr = lease.take_stderr().ok_or_else(closed)?;
        {
            let mut active = lock(&self.active);
            if !flow.is_open() {
                return Ok(());
            }
            active.insert(flow.id(), lease.abort_handle());
        }
        // This request accepts no desktop input and cannot forward it to the remote shell.
        let result = self.mux.control(Frame::Opened(flow.id(), None)).map(|()| {
            let mut output = flow.writer();
            let result = pump(
                &mut stdout,
                &mut stderr,
                &mut output,
                Some((flow, deadline, output_limit)),
            )
            .and_then(|()| exit_code(flow, &lease, Some(deadline)))
            .and_then(|code| output.end(code));
            flow.finish(result.err().map(|error| error.to_string()));
        });
        lock(&self.active).remove(&flow.id());
        result.map_err(Into::into)
    }

    fn exec_command(
        &self,
        flow: &Flow,
        command: &str,
        deadline: Option<Instant>,
    ) -> Result<(), ConnectError> {
        let opener = self.exec.as_ref().ok_or_else(closed)?;
        let gate = flow.clone();
        let mut lease = opener.open_cancellable(command, move || {
            !gate.is_open() || deadline.is_some_and(|end| Instant::now() >= end)
        })?;
        let abort = lease.abort_handle();
        let mut stdout = lease.take_reader().ok_or_else(closed)?;
        let mut stderr = lease.take_stderr().ok_or_else(closed)?;
        let input = lease.take_writer().ok_or_else(closed)?;
        {
            let mut active = lock(&self.active);
            if !flow.is_open() {
                return Ok(());
            }
            active.insert(flow.id(), abort.clone());
        }
        let result = self
            .mux
            .control(Frame::Opened(flow.id(), None))
            .and_then(|()| {
                relay(flow, &lease, input, deadline, |output| match deadline {
                    Some(end) => {
                        file_exec::pump_file(&mut stdout, Some(&mut stderr), output, flow, end)
                    }
                    None => pump(&mut stdout, &mut stderr, output, None),
                })
            });
        lock(&self.active).remove(&flow.id());
        // After Opened, the lane publishes EOF or failure after buffered output.
        // Before Opened the client still needs the request error.
        if result.is_err() && flow.is_open() {
            result?;
        }
        Ok(())
    }

    pub(super) fn exec_password(
        &self,
        flow: &Flow,
        command: &str,
        nonce: [u8; 16],
    ) -> Result<(), ConnectError> {
        let opener = self.exec.as_ref().ok_or_else(closed)?;
        let gate = flow.clone();
        let lease = opener.open_sensitive_cancellable(command, move || !gate.password_open())?;
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
            .and_then(|()| {
                let gate = flow.clone();
                let status = password::supervise(
                    lease,
                    &password::marker(nonce),
                    move || gate.password_open(),
                    || match flow.take_password() {
                        Ok(Some(line)) => Answer::Line(line),
                        Ok(None) => Answer::Pending,
                        Err(_) => Answer::Closed,
                    },
                    || flow.password_challenge().is_ok(),
                )
                .unwrap_or(Status::TransportFailed);
                if flow.password_open() {
                    flow.finish_password(status)
                } else {
                    Ok(())
                }
            });
        lock(&self.active).remove(&flow.id());
        result.map_err(Into::into)
    }
}

pub(super) fn relay(
    flow: &Flow,
    lease: &ExecLease,
    mut input: SshWriter,
    deadline: Option<Instant>,
    pump: impl FnOnce(&mut Writer) -> io::Result<()>,
) -> io::Result<()> {
    let abort = lease.abort_handle();
    thread::scope(|scope| {
        let upstream = thread::Builder::new()
            .name("kortty-ssh-exec-input".into())
            .spawn_scoped(scope, || {
                let result = io::copy(&mut flow.reader(), &mut input)
                    .and_then(|_| input.close_input())
                    .and_then(|()| flow.acknowledge_end());
                if result.is_err() && !abort.is_closed() {
                    abort.abort();
                }
            })?;
        let mut output = flow.writer();
        let result = pump(&mut output)
            .and_then(|()| exit_code(flow, lease, deadline))
            .and_then(|code| output.end(code));
        if result.is_err() {
            abort.abort();
            flow.finish(Some("SSH command output transport ended".into()));
        } else {
            flow.finish(None);
        }
        let _ = upstream.join();
        result
    })
}

fn exit_code(flow: &Flow, lease: &ExecLease, deadline: Option<Instant>) -> io::Result<Option<u32>> {
    let Some(deadline) = deadline else {
        return Ok(lease.exit_code());
    };
    let abort = lease.abort_handle();
    loop {
        if let Some(code) = lease.exit_code() {
            return Ok(Some(code));
        }
        if abort.is_closed() {
            return Ok(lease.exit_code());
        }
        thread::sleep(ACCEPT_POLL.min(relay_time_left(flow, deadline)?));
    }
}

fn pump(
    stdout: &mut SshReader,
    stderr: &mut SshReader,
    output: &mut Writer,
    bound: Option<(&Flow, Instant, usize)>,
) -> io::Result<()> {
    let mut buffer = Zeroizing::new([0; CHUNK]);
    let mut streams = [Some(stdout), Some(stderr)];
    let mut remaining = bound.map_or(0, |(_, _, limit)| limit);
    while streams.iter().any(Option::is_some) {
        for (kind, stream) in streams.iter_mut().enumerate() {
            let Some(reader) = stream else {
                continue;
            };
            let poll = match bound {
                Some((flow, deadline, _)) => relay_time_left(flow, deadline)?.min(ACCEPT_POLL),
                None => ACCEPT_POLL,
            };
            match reader.read_timeout(&mut buffer[3..], poll) {
                Ok(0) => *stream = None,
                Ok(count) => {
                    buffer[0] = kind as u8;
                    buffer[1..3].copy_from_slice(&(count as u16).to_be_bytes());
                    let frame = &buffer[..count + 3];
                    if let Some((flow, deadline, _)) = bound {
                        remaining = remaining.checked_sub(count).ok_or_else(|| {
                            io::Error::other("SSH read-only command output exceeds its limit")
                        })?;
                        // Each tagged frame fits in one worker write.
                        if output.write_timeout(frame, relay_time_left(flow, deadline)?)?
                            != frame.len()
                        {
                            return Err(io::ErrorKind::WriteZero.into());
                        }
                    } else {
                        output.write_all(frame)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn relay_time_left(flow: &Flow, deadline: Instant) -> io::Result<Duration> {
    if !flow.is_open() {
        return Err(closed());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(remaining)
}
