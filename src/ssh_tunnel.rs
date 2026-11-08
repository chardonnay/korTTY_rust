use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use kortty_ssh::Endpoint;

use crate::ssh_connection::{Abort, Channel, DirectOpener, RemoteOpener};

mod remote;
mod socks;

const MAX_CLIENTS: usize = 3;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum Destination {
    Fixed(Endpoint),
    Remote(Endpoint),
    Socks,
}

impl fmt::Display for Destination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(endpoint) | Self::Remote(endpoint) => {
                if let Ok(address) = endpoint.host().parse::<Ipv6Addr>() {
                    SocketAddr::from((address, endpoint.port())).fmt(formatter)
                } else {
                    write!(formatter, "{}:{}", endpoint.wire_host(), endpoint.port())
                }
            }
            Self::Socks => formatter.write_str("SOCKS"),
        }
    }
}

pub struct Listener {
    address: SocketAddr,
    destination: Destination,
    state: Arc<ListenerState>,
}

#[derive(Default)]
struct ListenerState {
    listening: AtomicBool,
    stopping: AtomicBool,
    finished: AtomicBool,
    failure: Mutex<Option<io::Error>>,
    clients: Mutex<Vec<Weak<Client>>>,
}

struct Client {
    socket: TcpStream,
    cancelled: AtomicBool,
    abort: Mutex<Option<Abort>>,
}

#[derive(Default, Clone)]
pub struct Capacity {
    clients: Arc<AtomicUsize>,
}

struct Slot(Capacity);

impl Capacity {
    fn claim(&self) -> Option<Slot> {
        self.clients
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_CLIENTS).then_some(count + 1)
            })
            .ok()?;
        Some(Slot(self.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::Release);
    }
}

impl Listener {
    #[cfg(all(test, unix))]
    pub(crate) fn bound_address(&self) -> SocketAddr {
        self.address
    }

    pub fn start(
        opener: DirectOpener,
        address: SocketAddr,
        destination: Destination,
        capacity: Capacity,
    ) -> io::Result<(Self, Receiver<()>)> {
        if opener.is_closed() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "SSH session is closed",
            ));
        }
        let socket = TcpListener::bind(address)?;
        let address = socket.local_addr()?;
        socket.set_nonblocking(true)?;
        let route = destination.clone();
        Self::launch(address, destination, true, move |state, _| {
            listen(socket, opener, route, capacity, &state)
        })
    }

    pub fn start_remote(
        opener: RemoteOpener,
        address: SocketAddr,
        target: Endpoint,
        capacity: Capacity,
    ) -> io::Result<(Self, Receiver<()>)> {
        if opener.is_closed() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "SSH session is closed",
            ));
        }
        Self::launch(
            address,
            Destination::Remote(target.clone()),
            false,
            move |state, changes| remote::listen(opener, address, target, capacity, state, changes),
        )
    }

    fn launch(
        address: SocketAddr,
        destination: Destination,
        listening: bool,
        run: impl FnOnce(Arc<ListenerState>, &Sender<()>) -> io::Result<()> + Send + 'static,
    ) -> io::Result<(Self, Receiver<()>)> {
        let state = Arc::new(ListenerState {
            listening: AtomicBool::new(listening),
            ..ListenerState::default()
        });
        let worker = Arc::clone(&state);
        let (changes, updates) = async_channel::bounded(1);
        thread::Builder::new()
            .name("kortty-tunnel".into())
            .spawn(move || {
                if let Err(error) = run(Arc::clone(&worker), &changes) {
                    *worker.failure.lock().unwrap_or_else(|err| err.into_inner()) = Some(error);
                }
                worker.stop();
                worker.finished.store(true, Ordering::Release);
                let _ = changes.try_send(());
            })?;
        Ok((
            Self {
                address,
                destination,
                state,
            },
            updates,
        ))
    }

    pub fn label(&self) -> String {
        let exposed = if self.address.ip().is_loopback() {
            ""
        } else {
            " · reachable from other devices"
        };
        match &self.destination {
            Destination::Remote(_) => {
                format!(
                    "{} on SSH server → {}{exposed}",
                    self.address, self.destination
                )
            }
            _ => format!("{} → {}{exposed}", self.address, self.destination),
        }
    }

    pub fn status(&self) -> String {
        if let Some(error) = &*self
            .state
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            return format!("Listener failed ({error})");
        }
        if self.is_finished() {
            "Stopped".into()
        } else if self.is_stopping() {
            "Stopping…".into()
        } else if self.is_listening() {
            "Listening".into()
        } else {
            "Starting…".into()
        }
    }

    pub fn is_listening(&self) -> bool {
        self.state.listening.load(Ordering::Acquire) && !self.is_stopping()
    }

    pub fn is_stopping(&self) -> bool {
        self.state.is_stopping()
    }

    pub fn is_finished(&self) -> bool {
        self.state.finished.load(Ordering::Acquire)
    }

    pub fn stop(&self) {
        self.state.stop();
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ListenerState {
    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    fn register(&self, client: &Arc<Client>) -> bool {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_stopping() {
            return false;
        }
        clients.retain(|client| client.strong_count() != 0);
        clients.push(Arc::downgrade(client));
        true
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for client in clients.drain(..).filter_map(|client| client.upgrade()) {
            client.stop();
        }
    }
}

impl Client {
    fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            cancelled: AtomicBool::new(false),
            abort: Mutex::new(None),
        }
    }

    fn set_abort(&self, abort: Abort) {
        let mut state = self.abort.lock().unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() {
            abort.abort();
        } else {
            *state = Some(abort);
        }
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.socket.shutdown(Shutdown::Both);
        let mut state = self.abort.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(abort) = state.take() {
            abort.abort();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

fn listen(
    listener: TcpListener,
    opener: DirectOpener,
    route: Destination,
    capacity: Capacity,
    state: &ListenerState,
) -> io::Result<()> {
    while !state.is_stopping() && !opener.is_closed() {
        let (socket, origin) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let Some(slot) = capacity.claim() else {
            // A slow peer cannot create unbounded threads or SSH channels.
            drop(socket);
            thread::sleep(POLL_INTERVAL);
            continue;
        };
        let client = Arc::new(Client::new(socket));
        if !state.register(&client) {
            break;
        }
        let (opener, route) = (opener.clone(), route.clone());
        thread::Builder::new()
            .name("kortty-tunnel-client".into())
            .spawn(move || {
                let _slot = slot;
                let _ = serve(&opener, &route, origin, &client);
            })?;
    }
    Ok(())
}

fn serve(
    opener: &DirectOpener,
    route: &Destination,
    origin: SocketAddr,
    client: &Client,
) -> io::Result<()> {
    let mut socket = client.socket.try_clone()?;
    socket.set_nonblocking(false)?;
    let (destination, socks) = match route {
        Destination::Fixed(endpoint) => (endpoint.clone(), None),
        Destination::Socks => {
            let request = socks::handshake(&mut DeadlineSocket {
                socket: &socket,
                deadline: Instant::now() + HANDSHAKE_TIMEOUT,
            })?;
            (
                Endpoint::new(&request.host, request.port)?,
                Some(request.version),
            )
        }
        Destination::Remote(_) => return Err(io::Error::other("Expected a local SSH listener")),
    };
    let channel = opener.open_cancellable(destination, origin, || client.is_cancelled());
    if let Some(socks) = socks {
        socket.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        socks.reply(&mut socket, channel.is_ok())?;
    }
    relay(channel.map_err(io::Error::other)?, socket, client)
}

fn relay(mut channel: Channel, mut socket: TcpStream, client: &Client) -> io::Result<()> {
    let abort = channel.abort_handle();
    client.set_abort(abort.clone());
    socket.set_read_timeout(Some(POLL_INTERVAL))?;
    socket.set_write_timeout(None)?;
    let mut reader = channel.take_reader().ok_or_else(missing_stream)?;
    let mut writer = channel.take_writer().ok_or_else(missing_stream)?;
    let mut input = RelayInput {
        socket: socket.try_clone()?,
        abort: &abort,
    };
    thread::scope(|scope| {
        let upstream = thread::Builder::new()
            .name("kortty-tunnel-upstream".into())
            .spawn_scoped(scope, || {
                let result = io::copy(&mut input, &mut writer).and_then(|_| {
                    if abort.is_closed() {
                        Ok(())
                    } else {
                        writer.close_input()
                    }
                });
                // A full remote close stops upstream without discarding output
                // already accepted by the downstream reader.
                if abort.is_closed() && !client.is_cancelled() {
                    return Ok(());
                }
                if result.is_err() {
                    client.stop();
                }
                result
            })?;
        let downstream =
            io::copy(&mut reader, &mut socket).and_then(|_| socket.shutdown(Shutdown::Write));
        if downstream.is_err() {
            client.stop();
        }
        let upstream = upstream
            .join()
            .map_err(|_| io::Error::other("SSH tunnel input worker panicked"))?;
        downstream.and(upstream)
    })
}

fn missing_stream() -> io::Error {
    io::Error::other("SSH forwarding channel is missing an I/O stream")
}

struct RelayInput<'a> {
    socket: TcpStream,
    abort: &'a Abort,
}

impl Read for RelayInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.socket.read(output) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if self.abort.is_closed() {
                        return Ok(0);
                    }
                }
                result => return result,
            }
        }
    }
}

struct DeadlineSocket<'a> {
    socket: &'a TcpStream,
    deadline: Instant,
}

impl DeadlineSocket<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "SOCKS handshake timed out"))
    }
}

impl Read for DeadlineSocket<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(output)
    }
}

impl Write for DeadlineSocket<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}
