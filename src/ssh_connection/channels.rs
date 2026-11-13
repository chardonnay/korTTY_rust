use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use kortty_ssh::{ConnectError, Endpoint};

use super::Abort;
use crate::ssh_worker;

#[derive(Clone)]
pub(crate) enum SftpOpener {
    Direct(kortty_ssh::SftpOpener),
    Worker(ssh_worker::Opener),
}

impl SftpOpener {
    pub(crate) fn verified_host_key(&self) -> Option<&kortty_ssh::HostKey> {
        match self {
            Self::Direct(opener) => opener.verified_host_key(),
            Self::Worker(opener) => opener.verified_host_key(),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Direct(opener) => opener.is_closed(),
            Self::Worker(opener) => opener.is_closed(),
        }
    }

    pub(crate) fn exec_opener(&self) -> super::ExecOpener {
        match self {
            Self::Direct(opener) => super::ExecOpener::Direct(opener.exec_opener()),
            Self::Worker(opener) => super::ExecOpener::Worker(opener.clone()),
        }
    }

    pub(crate) fn open_cancellable(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<Channel, ConnectError> {
        match self {
            Self::Direct(opener) => opener.open_cancellable(cancelled).map(Channel::Sftp),
            Self::Worker(opener) => opener.open_sftp(cancelled).map(Channel::Worker),
        }
    }
}

#[derive(Clone)]
pub(crate) enum DirectOpener {
    Direct(kortty_ssh::DirectOpener),
    Worker(ssh_worker::Opener),
}

impl DirectOpener {
    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Direct(opener) => opener.is_closed(),
            Self::Worker(opener) => opener.is_closed(),
        }
    }

    pub(crate) fn open_cancellable(
        &self,
        destination: Endpoint,
        origin: SocketAddr,
        cancelled: impl Fn() -> bool,
    ) -> Result<Channel, ConnectError> {
        match self {
            Self::Direct(opener) => opener
                .open_cancellable(destination, origin, cancelled)
                .map(Channel::Direct),
            Self::Worker(opener) => opener
                .open_direct(destination, origin, cancelled)
                .map(Channel::Worker),
        }
    }
}

#[derive(Clone)]
pub(crate) enum RemoteOpener {
    Direct(kortty_ssh::RemoteOpener),
    Worker(ssh_worker::Opener),
}

impl RemoteOpener {
    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Direct(opener) => opener.is_closed(),
            Self::Worker(opener) => opener.is_closed(),
        }
    }

    pub(crate) fn bind(&self, address: SocketAddr) -> Result<RemoteForward, ConnectError> {
        match self {
            Self::Direct(opener) => opener.bind(address).map(RemoteForward::Direct),
            Self::Worker(opener) => opener.bind(address, || false).map(RemoteForward::Worker),
        }
    }
}

pub(crate) enum RemoteForward {
    Direct(kortty_ssh::RemoteForward),
    Worker(ssh_worker::RemoteForward),
}

impl RemoteForward {
    pub(crate) fn remote_addr(&self) -> SocketAddr {
        match self {
            Self::Direct(listener) => listener.remote_addr(),
            Self::Worker(listener) => listener.remote_addr(),
        }
    }

    pub(crate) fn accept_timeout(
        &self,
        timeout: Duration,
    ) -> io::Result<Option<(SocketAddr, Channel)>> {
        match self {
            Self::Direct(listener) => Ok(listener
                .accept_timeout(timeout)?
                .map(|connection| (connection.origin, Channel::Direct(connection.channel)))),
            Self::Worker(listener) => Ok(listener
                .accept_timeout(timeout)?
                .map(|connection| (connection.origin, Channel::Worker(connection.channel)))),
        }
    }

    pub(crate) fn close(&mut self) -> Result<(), ConnectError> {
        match self {
            Self::Direct(listener) => listener.close(),
            Self::Worker(listener) => listener.close(),
        }
    }
}

pub(crate) enum Channel {
    Direct(kortty_ssh::DirectChannel),
    Sftp(kortty_ssh::SftpLease),
    Worker(ssh_worker::Channel),
}

impl Channel {
    pub(crate) fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Direct(channel) => channel.take_reader().map(|reader| Box::new(reader) as _),
            Self::Sftp(channel) => channel.take_reader().map(|reader| Box::new(reader) as _),
            Self::Worker(channel) => channel.take_reader().map(|reader| Box::new(reader) as _),
        }
    }

    pub(crate) fn take_writer(&mut self) -> Option<Writer> {
        match self {
            Self::Direct(channel) => channel.take_writer().map(Writer::Direct),
            Self::Sftp(channel) => channel.take_writer().map(Writer::Direct),
            Self::Worker(channel) => channel.take_writer().map(Writer::Worker),
        }
    }

    pub(crate) fn take_timed_reader(&mut self, timeout: Duration) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Direct(channel) => channel.take_reader().map(TimedRead::Direct),
            Self::Sftp(channel) => channel.take_reader().map(TimedRead::Direct),
            Self::Worker(channel) => channel.take_reader().map(TimedRead::Worker),
        }
        .map(|reader| Box::new(TimedReader { reader, timeout }) as _)
    }

    pub(crate) fn take_timed_writer(&mut self, timeout: Duration) -> Option<Box<dyn Write + Send>> {
        self.take_writer()
            .map(|writer| Box::new(TimedWriter { writer, timeout }) as _)
    }

    pub(crate) fn abort_handle(&self) -> Abort {
        match self {
            Self::Direct(channel) => Abort::Direct(channel.abort_handle()),
            Self::Sftp(channel) => Abort::Direct(channel.abort_handle()),
            Self::Worker(channel) => Abort::Stream(channel.abort_handle()),
        }
    }
}

enum TimedRead {
    Direct(kortty_ssh::SshReader),
    Worker(ssh_worker::Reader),
}

struct TimedReader {
    reader: TimedRead,
    timeout: Duration,
}

impl Read for TimedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match &mut self.reader {
            TimedRead::Direct(reader) => reader.read_timeout(bytes, self.timeout),
            TimedRead::Worker(reader) => reader.read_timeout(bytes, self.timeout),
        }
    }
}

struct TimedWriter {
    writer: Writer,
    timeout: Duration,
}

impl Write for TimedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match &mut self.writer {
            Writer::Worker(writer) => writer.write_timeout(bytes, self.timeout),
            Writer::Direct(writer) => {
                let start = Instant::now();
                loop {
                    match writer.write_nonblocking(bytes) {
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            let left = self.timeout.saturating_sub(start.elapsed());
                            if left.is_zero() {
                                return Err(io::ErrorKind::TimedOut.into());
                            }
                            std::thread::sleep(left.min(Duration::from_millis(10)));
                        }
                        result => return result,
                    }
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Both transports queue and wake immediately on each write.
        Ok(())
    }
}

pub(crate) enum Writer {
    Direct(kortty_ssh::SshWriter),
    Worker(ssh_worker::Writer),
}

impl Writer {
    pub(crate) fn close_input(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(writer) => writer.close_input(),
            Self::Worker(writer) => writer.close_input(),
        }
    }
}

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Direct(writer) => writer.write(bytes),
            Self::Worker(writer) => writer.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(writer) => writer.flush(),
            Self::Worker(writer) => writer.flush(),
        }
    }
}
