//! Password sudo on a private terminal bound to the reviewed SSH or local target.

use std::fmt::Write as _;
use std::io;
use std::thread;
use std::time::Instant;

use kortty::ssh_runtime::{
    mux::PasswordFileEvent,
    password::{Answer, Status as AuthStatus},
    sudo_pty,
};
use kortty_ssh::{ExecLease, SensitiveLine, SshReader, SshWriter};
use zeroize::{Zeroize, Zeroizing};

use crate::{ssh_connection::ExecOpener, ssh_worker};

#[cfg(unix)]
use super::super::LocalDirectory;
use super::super::runner::{COMMAND_LIMIT, Cancel, POLL, Status};
use super::{quote, validate_remote_command};

pub(crate) const AUTHENTICATION_REJECTED: &str =
    "Sudo authentication failed. No approved command was run.";

#[cfg(unix)]
mod local;

#[cfg(unix)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum LocalRoute {
    Native,
    #[cfg(target_os = "linux")]
    FlatpakHost,
}

#[cfg(unix)]
impl LocalRoute {
    pub(crate) fn available(self) -> bool {
        match self {
            Self::Native => {
                #[cfg(target_os = "linux")]
                if kortty_pty::flatpak::running() {
                    return false;
                }
                std::path::Path::new("/usr/bin/sudo").is_file()
            }
            #[cfg(target_os = "linux")]
            Self::FlatpakHost => kortty_pty::flatpak::running(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Endpoint {
    Ssh(ExecOpener),
    #[cfg(unix)]
    Local(LocalDirectory, LocalRoute),
}

impl Endpoint {
    pub(crate) fn is_local(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self, Self::Local(..))
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(crate) fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ssh(left), Self::Ssh(right)) => left.same_connection(right),
            #[cfg(unix)]
            (Self::Local(left, left_route), Self::Local(right, right_route)) => {
                left_route == right_route && left == right
            }
            #[cfg(unix)]
            _ => false,
        }
    }
}

pub(crate) struct Command {
    wire: String,
}

impl Command {
    pub(crate) fn new(directory: &str, command: &str, local: bool) -> Result<Self, String> {
        validate_remote_command(directory, command)?;
        let directory = quote(directory);
        let payload = if local {
            format!("exec </dev/null\n[ \"$(/bin/pwd -P)\" = {directory} ] || exit 97\n{command}")
        } else {
            format!("exec </dev/null\ncd {directory} || exit\n{command}")
        };
        let wire = format!(
            "exec /usr/bin/sudo -n -u root -- /bin/sh -c {}",
            quote(&payload)
        );
        if wire.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
            return Err("The effective sudo command exceeds the SSH execution limit.".into());
        }
        Ok(Self { wire })
    }

    pub(crate) fn wire(&self) -> &str {
        &self.wire
    }

    pub(crate) fn run_with_terminal(
        self,
        endpoint: Endpoint,
        terminal: Option<Terminal>,
        cancel: Cancel,
        authorized: impl Fn() -> bool + Clone + Send + Sync + 'static,
        answers: async_channel::Receiver<SensitiveLine>,
        challenge: impl FnOnce() -> bool,
    ) -> Result<(Status, Option<Terminal>), &'static str> {
        let permitted = move || !cancel.requested() && authorized();
        if !permitted() {
            return Ok((Status::Cancelled, None));
        }
        let mut terminal = match terminal {
            Some(terminal) if endpoint.matches(&terminal.endpoint) => terminal,
            Some(_) => return Err("The sudo terminal belongs to a different target."),
            None => {
                let Some(terminal) =
                    Terminal::authenticate(endpoint, permitted.clone(), answers, challenge)?
                else {
                    return Ok((Status::Cancelled, None));
                };
                terminal
            }
        };
        let request = new_token()?;
        let status = terminal.execute(&request, &self.wire, &permitted);
        // Stop on a failed or expired ticket. Never reopen or reauthenticate in this run.
        Ok((status, (status == Status::Exited(0)).then_some(terminal)))
    }
}

pub(crate) struct Terminal {
    endpoint: Endpoint,
    streams: Streams,
}

enum Streams {
    Direct {
        _lease: ExecLease,
        reader: SshReader,
        writer: SshWriter,
    },
    Worker {
        _channel: ssh_worker::Channel,
        reader: ssh_worker::Reader,
        writer: ssh_worker::Writer,
    },
    #[cfg(unix)]
    Local(Box<local::Streams>),
}

impl Terminal {
    pub(crate) fn authenticate(
        endpoint: Endpoint,
        permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
        answers: async_channel::Receiver<SensitiveLine>,
        challenge: impl FnOnce() -> bool,
    ) -> Result<Option<Self>, &'static str> {
        if !permitted() {
            return Ok(None);
        }
        match Self::open(endpoint, permitted, answers, challenge) {
            Ok(terminal) => Ok(Some(terminal)),
            Err(Status::Cancelled) => Ok(None),
            Err(Status::TimedOut) => {
                Err("Timed out while validating sudo in the private terminal.")
            }
            Err(Status::Exited(124)) => Err("Could not allocate a sudo pseudoterminal."),
            Err(Status::Exited(125)) => Err("Could not disable echo on the sudo pseudoterminal."),
            Err(Status::Exited(_)) => Err(AUTHENTICATION_REJECTED),
            Err(_) => Err("Could not validate sudo in the private terminal."),
        }
    }

    fn open(
        endpoint: Endpoint,
        permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
        answers: async_channel::Receiver<SensitiveLine>,
        challenge: impl FnOnce() -> bool,
    ) -> Result<Self, Status> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| Status::TransportFailed)?;
        let streams = match &endpoint {
            Endpoint::Ssh(ExecOpener::Direct(direct)) => {
                let opening = permitted.clone();
                let mut lease = direct
                    .open_pty_cancellable(&sudo_pty::command(nonce), move || !opening())
                    .map_err(|_| stopped_or_failed(&permitted))?;
                let sudo_pty::Authenticated { reader, writer } = sudo_pty::authenticate(
                    &mut lease,
                    nonce,
                    permitted,
                    || match answers.try_recv() {
                        Ok(line) => Answer::Line(line),
                        Err(async_channel::TryRecvError::Closed) => Answer::Closed,
                        Err(async_channel::TryRecvError::Empty) => Answer::Pending,
                    },
                    challenge,
                )
                .map_err(auth_status)?;
                Streams::Direct {
                    _lease: lease,
                    reader,
                    writer,
                }
            }
            Endpoint::Ssh(ExecOpener::Worker(worker)) => {
                let mut channel = open_worker(worker, nonce, permitted, answers, challenge)?;
                let reader = channel.take_reader().ok_or(Status::TransportFailed)?;
                let writer = channel.take_writer().ok_or(Status::TransportFailed)?;
                Streams::Worker {
                    _channel: channel,
                    reader,
                    writer,
                }
            }
            #[cfg(unix)]
            Endpoint::Local(directory, route) => Streams::Local(Box::new(local::open(
                directory, *route, nonce, permitted, answers, challenge,
            )?)),
        };
        Ok(Self { endpoint, streams })
    }

    fn execute(&mut self, token: &str, wire: &str, permitted: &impl Fn() -> bool) -> Status {
        let mut reply = sudo_pty::Reply::new(token);
        let frame = command_frame(token, wire);
        let mut remaining = frame.as_bytes();
        let mut buffer = Zeroizing::new([0; 8192]);
        let deadline = Instant::now() + COMMAND_LIMIT;
        loop {
            if !permitted() {
                return Status::Cancelled;
            }
            if Instant::now() >= deadline {
                return Status::TimedOut;
            }
            if !remaining.is_empty() {
                match self.streams.write(remaining) {
                    Ok(0) => return Status::TransportFailed,
                    Ok(count) => remaining = &remaining[count..],
                    Err(error) if retry(&error) => {}
                    Err(_) => return stopped_or_failed(permitted),
                }
                if !remaining.is_empty() {
                    continue;
                }
            }
            match self.streams.read(&mut buffer[..]) {
                Ok(0) => return Status::MissingExitCode,
                Ok(count) => {
                    let result = reply.observe(&buffer[..count]);
                    buffer[..count].zeroize();
                    match result {
                        Ok(Some(code)) => return Status::Exited(code),
                        Ok(None) => {}
                        Err(_) => return Status::TransportFailed,
                    }
                }
                Err(error) if retry(&error) => {}
                Err(_) => return stopped_or_failed(permitted),
            }
        }
    }
}

impl Streams {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Direct { writer, .. } => {
                let result = writer.write_nonblocking(bytes);
                if matches!(&result, Err(error) if error.kind() == io::ErrorKind::WouldBlock) {
                    thread::sleep(POLL);
                }
                result
            }
            Self::Worker { writer, .. } => writer.write_timeout(bytes, POLL),
            #[cfg(unix)]
            Self::Local(streams) => streams.write(bytes),
        }
    }

    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Direct { reader, .. } => reader.read_timeout(bytes, POLL),
            Self::Worker { reader, .. } => reader.read_timeout(bytes, POLL),
            #[cfg(unix)]
            Self::Local(streams) => streams.read(bytes),
        }
    }
}

fn open_worker(
    opener: &ssh_worker::Opener,
    nonce: [u8; 16],
    permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
    answers: async_channel::Receiver<SensitiveLine>,
    challenge: impl FnOnce() -> bool,
) -> Result<ssh_worker::Channel, Status> {
    let channel = opener
        .open_sudo_pty(nonce, || !permitted())
        .map_err(|_| stopped_or_failed(&permitted))?;
    let mut challenge = Some(challenge);
    let mut submitted = false;
    loop {
        if !permitted() {
            return Err(Status::Cancelled);
        }
        match channel
            .next(POLL)
            .map_err(|_| stopped_or_failed(&permitted))?
        {
            Some(PasswordFileEvent::Challenge) => {
                if !challenge.take().is_some_and(|notify| notify()) {
                    return Err(Status::Cancelled);
                }
            }
            Some(PasswordFileEvent::Staged) => {
                channel
                    .commit(permitted.clone())
                    .map_err(|_| stopped_or_failed(&permitted))?;
            }
            Some(PasswordFileEvent::Ready) => {
                return channel.into_channel().map_err(|_| Status::TransportFailed);
            }
            Some(PasswordFileEvent::Finished(status)) => return Err(auth_status(status)),
            None => {}
        }
        if !submitted {
            match answers.try_recv() {
                Ok(line) if challenge.is_none() => {
                    channel
                        .stage(line, permitted.clone())
                        .map_err(|_| stopped_or_failed(&permitted))?;
                    submitted = true;
                }
                Ok(_) | Err(async_channel::TryRecvError::Closed) => return Err(Status::Cancelled),
                Err(async_channel::TryRecvError::Empty) => {}
            }
        }
    }
}

fn auth_status(status: AuthStatus) -> Status {
    match status {
        AuthStatus::Exited(0) | AuthStatus::MissingExitCode => Status::MissingExitCode,
        AuthStatus::Exited(code) => Status::Exited(code),
        AuthStatus::Cancelled => Status::Cancelled,
        AuthStatus::TimedOut => Status::TimedOut,
        AuthStatus::TransportFailed => Status::TransportFailed,
    }
}

fn stopped_or_failed(permitted: &impl Fn() -> bool) -> Status {
    if permitted() {
        Status::TransportFailed
    } else {
        Status::Cancelled
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

fn new_token() -> Result<String, &'static str> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| "Could not create a sudo terminal request.")?;
    Ok(sudo_pty::token(nonce))
}

fn command_frame(token: &str, command: &str) -> String {
    let mut frame = String::with_capacity(token.len() + 2 + command.len() * 5);
    frame.push_str(token);
    frame.push(' ');
    for byte in command.bytes() {
        write!(frame, "\\0{byte:03o}").expect("writing to a String cannot fail");
    }
    frame.push('\n');
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_sudo_reviews_only_the_noninteractive_command_and_quoted_directory() {
        let command = Command::new("/tmp/it's here", "printf '%s' '$literal'", false).unwrap();
        assert!(
            command
                .wire()
                .starts_with("exec /usr/bin/sudo -n -u root -- /bin/sh -c 'exec </dev/null\ncd ")
        );
        assert!(!command.wire().contains("-k"));
        assert!(!command.wire().contains("-S"));
        assert!(Command::new("relative", "id", false).is_err());
        assert!(Command::new("/tmp", "echo ***", false).is_err());
    }

    #[test]
    fn sudo_terminal_command_frame_encodes_every_byte() {
        assert_eq!(command_frame("token", "a\n'"), "token \\0141\\0012\\0047\n");
    }
}
