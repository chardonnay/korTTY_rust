//! Windows TCP transport for cancellable AI requests, with ureq's native TLS layer.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::unversioned::resolver::Resolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NativeTlsConnector, NextTimeout,
    ShutdownHandle, Transport,
};
use ureq::{Agent, Error, config::Config};

pub(super) fn agent(config: Config, resolver: impl Resolver) -> Agent {
    Agent::with_parts(
        config,
        WindowsTcp.chain(NativeTlsConnector::default()),
        resolver,
    )
}

#[derive(Debug)]
struct WindowsTcp;

impl Connector for WindowsTcp {
    type Out = SocketTransport;

    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, Error> {
        let start = Instant::now();
        let budget = remaining(details.timeout, start)?;
        let mut attempts_left = details.addrs.len();
        let mut last_error = Error::HostNotFound;
        for address in &details.addrs {
            let result = if let Some(budget) = budget {
                let left = budget.saturating_sub(start.elapsed());
                if left.is_zero() {
                    return Err(Error::Timeout(details.timeout.reason));
                }
                let attempt = (left / attempts_left as u32).max(Duration::from_millis(1));
                TcpStream::connect_timeout(address, attempt)
            } else {
                TcpStream::connect(address)
            };
            match result {
                Ok(stream) => {
                    if details.config.no_delay() {
                        stream.set_nodelay(true)?;
                    }
                    let socket = Arc::new(Socket(stream));
                    let shutdown: Arc<dyn ShutdownHandle> = socket.clone();
                    (details.register_shutdown)(Arc::downgrade(&shutdown))?;
                    return Ok(Some(SocketTransport {
                        socket,
                        buffers: LazyBuffers::new(
                            details.config.input_buffer_size(),
                            details.config.output_buffer_size(),
                        ),
                    }));
                }
                Err(error) => last_error = io_error(error, details.timeout),
            }
            attempts_left -= 1;
        }
        Err(last_error)
    }
}

struct Socket(TcpStream);

impl ShutdownHandle for Socket {
    fn shutdown(&self) {
        kortty_pty::shutdown_tcp_io(&self.0);
    }
}

struct SocketTransport {
    socket: Arc<Socket>,
    buffers: LazyBuffers,
}

impl fmt::Debug for SocketTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WindowsTcpTransport")
    }
}

impl Transport for SocketTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        let timeout = timeout.for_write();
        let start = Instant::now();
        let mut socket = &self.socket.0;
        let mut data = &self.buffers.output()[..amount];
        while !data.is_empty() {
            socket.set_write_timeout(remaining(timeout, start)?)?;
            let count = socket
                .write(data)
                .map_err(|error| io_error(error, timeout))?;
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero).into());
            }
            data = &data[count..];
        }
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let timeout = timeout.for_read();
        let socket = &self.socket.0;
        socket.set_read_timeout(remaining(timeout, Instant::now())?)?;
        let count = (&self.socket.0)
            .read(self.buffers.input_append_buf())
            .map_err(|error| io_error(error, timeout))?;
        self.buffers.input_appended(count);
        Ok(count != 0)
    }

    fn is_open(&mut self) -> bool {
        let socket = &self.socket.0;
        if socket.set_nonblocking(true).is_err() {
            return false;
        }
        let result = socket.peek(&mut [0]);
        socket.set_nonblocking(false).is_ok()
            && result.is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
    }
}

fn remaining(timeout: NextTimeout, start: Instant) -> Result<Option<Duration>, Error> {
    if timeout.after.is_zero() {
        return Err(Error::Timeout(timeout.reason));
    }
    timeout
        .not_zero()
        .map(|budget| {
            let left = budget.saturating_sub(start.elapsed());
            if left.is_zero() {
                Err(Error::Timeout(timeout.reason))
            } else {
                Ok(left.max(Duration::from_millis(1)))
            }
        })
        .transpose()
}

fn io_error(error: io::Error, timeout: NextTimeout) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        Error::Timeout(timeout.reason)
    } else {
        Error::Io(error)
    }
}
