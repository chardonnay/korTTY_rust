//! Connects to a managed runtime without sending bytes until the kernel
//! attributes the accepted connection to the supervised child.

use std::fmt;
use std::io::{self, Read, Write};
#[cfg(not(windows))]
use std::net::Shutdown;
use std::net::{SocketAddr, SocketAddrV4, TcpStream};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, ShutdownHandle, Transport,
};
use ureq::{Agent, Error};

use super::super::connection_config;
use super::process::Sidecar;

const CONNECTION_BUDGET: Duration = Duration::from_secs(1);

pub(super) fn agent(sidecar: &Arc<Sidecar>, timeout: Duration) -> Agent {
    Agent::with_parts(
        connection_config(timeout),
        OwnedEndpoint(Arc::downgrade(sidecar)),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct OwnedEndpoint(Weak<Sidecar>);

impl Connector for OwnedEndpoint {
    type Out = VerifiedSocket;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, Error> {
        let owner = self.0.upgrade().ok_or_else(runtime_stopped)?;
        let address = owner.address();
        if details.uri.scheme_str() != Some("http")
            || details
                .uri
                .authority()
                .and_then(|value| value.as_str().parse().ok())
                != Some(address)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unexpected managed runtime destination",
            )
            .into());
        }
        owner.validate()?;
        if details.timeout.after.is_zero() {
            return Err(Error::Timeout(details.timeout.reason));
        }
        let budget = details
            .timeout
            .not_zero()
            .map_or(CONNECTION_BUDGET, |timeout| {
                (*timeout).min(CONNECTION_BUDGET)
            });
        let deadline = Instant::now() + budget;
        let stream = TcpStream::connect_timeout(&address.into(), budget)
            .map_err(|error| socket_error(error, details.timeout))?;
        let socket = Arc::new(Socket(stream));
        let shutdown: Arc<dyn ShutdownHandle> = socket.clone();
        (details.register_shutdown)(Arc::downgrade(&shutdown))?;
        verify_peer(&socket.0, address, owner.pid(), deadline)?;
        owner.validate()?;
        Ok(Some(VerifiedSocket {
            socket,
            owner: self.0.clone(),
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        }))
    }
}

fn verify_peer(
    socket: &TcpStream,
    address: SocketAddrV4,
    pid: u32,
    deadline: Instant,
) -> io::Result<()> {
    if socket.peer_addr()? != SocketAddr::V4(address) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unexpected managed runtime peer",
        ));
    }
    let SocketAddr::V4(source) = socket.local_addr()? else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Expected an IPv4 loopback connection",
        ));
    };
    loop {
        if kortty_pty::owns_loopback_connection(pid, address, source)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The managed runtime does not own the connected socket",
            ));
        }
        // The TCP handshake can complete just before the server accepts the socket.
        std::thread::sleep(Duration::from_millis(15));
    }
}

struct Socket(TcpStream);

impl ShutdownHandle for Socket {
    fn shutdown(&self) {
        #[cfg(windows)]
        kortty_pty::shutdown_tcp_io(&self.0);
        #[cfg(not(windows))]
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

struct VerifiedSocket {
    socket: Arc<Socket>,
    owner: Weak<Sidecar>,
    buffers: LazyBuffers,
}

impl fmt::Debug for VerifiedSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VerifiedSocket")
    }
}

impl Transport for VerifiedSocket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        let owner = self.owner.upgrade().ok_or_else(runtime_stopped)?;
        owner.validate()?;
        let timeout = timeout.for_write();
        self.socket.0.set_write_timeout(socket_timeout(timeout)?)?;
        (&self.socket.0)
            .write_all(&self.buffers.output()[..amount])
            .map_err(|error| socket_error(error, timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let timeout = timeout.for_read();
        self.socket.0.set_read_timeout(socket_timeout(timeout)?)?;
        let count = (&self.socket.0)
            .read(self.buffers.input_append_buf())
            .map_err(|error| socket_error(error, timeout))?;
        self.buffers.input_appended(count);
        Ok(count != 0)
    }

    fn is_open(&mut self) -> bool {
        // Prove ownership anew for each request rather than pooling the transport.
        false
    }
}

fn socket_timeout(timeout: NextTimeout) -> Result<Option<Duration>, Error> {
    if timeout.after.is_zero() {
        return Err(Error::Timeout(timeout.reason));
    }
    Ok(timeout.not_zero().map(|duration| *duration))
}

fn socket_error(error: io::Error, timeout: NextTimeout) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        Error::Timeout(timeout.reason)
    } else {
        error.into()
    }
}

fn runtime_stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "The managed runtime stopped")
}
