use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;

pub(super) enum Endpoint {
    Tcp(TcpStream),
    Local(UnixStream),
}

impl Endpoint {
    pub(super) fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    pub(super) fn reader(&self) -> io::Result<Self> {
        self.try_clone()
    }

    pub(super) fn writer(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(socket) => socket.try_clone().map(Self::Tcp),
            Self::Local(socket) => socket.try_clone().map(Self::Local),
        }
    }

    pub(super) fn close(&self) {
        let _ = match self {
            Self::Tcp(socket) => socket.shutdown(Shutdown::Both),
            Self::Local(socket) => socket.shutdown(Shutdown::Both),
        };
    }
}

impl Read for Endpoint {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(bytes),
            Self::Local(socket) => socket.read(bytes),
        }
    }
}

impl Write for Endpoint {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.write(bytes),
            Self::Local(socket) => socket.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
