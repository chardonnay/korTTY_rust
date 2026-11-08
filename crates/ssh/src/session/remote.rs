use std::net::IpAddr;
use std::sync::mpsc::RecvTimeoutError;

use sunset::GlobalReply;
use sunset::event::ForwardedTcpipOpen;

use super::forward::Forwards;
use super::*;

const MAX_REMOTE_LISTENERS: usize = 4;
const BIND: &str = "tcpip-forward";
const CANCEL: &str = "cancel-tcpip-forward";

/// Registers remote listeners on an authenticated SSH connection.
#[derive(Clone)]
pub struct RemoteOpener {
    requests: SyncSender<BindRequest>,
    owner: Arc<Shared>,
}

/// One confirmed remote listener. Dropping it revokes local access and requests cancellation.
pub struct RemoteForward {
    address: SocketAddr,
    control: Arc<ListenerControl>,
    incoming: Receiver<RemoteConnection>,
    cancelled: Option<Receiver<Result<(), ConnectError>>>,
}

/// A TCP connection accepted by the server on this remote listener.
pub struct RemoteConnection {
    /// TCP peer address reported by the SSH server.
    pub origin: SocketAddr,
    pub channel: DirectChannel,
}

impl RemoteOpener {
    pub fn is_closed(&self) -> bool {
        self.owner.closed.load(Ordering::Acquire)
    }

    /// Confirms an explicit numeric bind. Port zero requests a server-assigned port.
    pub fn bind(&self, bind: SocketAddr) -> Result<RemoteForward, ConnectError> {
        self.bind_cancellable(bind, || false)
    }

    pub fn bind_cancellable(
        &self,
        bind: SocketAddr,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteForward, ConnectError> {
        if matches!(bind, SocketAddr::V6(address) if address.scope_id() != 0) {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "scoped SSH remote bind").into(),
            );
        }
        if self.is_closed() {
            return Err(closed().into());
        }
        let startup = Startup::new(Some(&cancelled));
        startup.check_cancelled()?;
        let (reply, result) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.requests
            .try_send(BindRequest {
                address: SocketAddr::new(bind.ip(), bind.port()),
                deadline: startup.deadline,
                reply,
                abandoned: Arc::clone(&abandoned),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "SSH remote listener queue is full",
                ),
                TrySendError::Disconnected(_) => closed(),
            })?;
        let outcome = self
            .owner
            .wake
            .wake()
            .map_err(ConnectError::from)
            .and_then(|()| {
                loop {
                    match result.recv_timeout(startup.wait_time()?) {
                        Ok(result) => {
                            startup.remaining()?;
                            return result;
                        }
                        Err(RecvTimeoutError::Disconnected) => return Err(closed().into()),
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            });
        if outcome.is_err() {
            abandoned.store(true, Ordering::Release);
            let _ = self.owner.wake.wake();
        }
        outcome
    }
}

impl RemoteForward {
    pub fn remote_addr(&self) -> SocketAddr {
        self.address
    }

    /// Returns one forwarded stream or `None` when the local wait expires.
    pub fn accept_timeout(&self, timeout: Duration) -> io::Result<Option<RemoteConnection>> {
        if self.control.is_closing() {
            return Err(closed());
        }
        let result = self.incoming.recv_timeout(timeout);
        if self.control.is_closing() {
            return Err(closed());
        }
        match result {
            Ok(connection) => Ok(Some(connection)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(closed()),
        }
    }

    /// Revokes this listener and its streams and waits for the server's cancellation reply.
    pub fn close(&mut self) -> Result<(), ConnectError> {
        self.control.close();
        let Some(cancelled) = self.cancelled.take() else {
            return Ok(());
        };
        match cancelled.recv_timeout(CONNECT_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Disconnected) => Err(closed().into()),
            Err(RecvTimeoutError::Timeout) => {
                // A missing cancellation response must release the server's listener.
                SshAbort(Arc::clone(&self.control.owner)).abort();
                Err(remote_timeout().into())
            }
        }
    }
}

impl Drop for RemoteForward {
    fn drop(&mut self) {
        self.control.close();
    }
}

struct ListenerControl {
    closing: AtomicBool,
    owner: Arc<Shared>,
}

impl ListenerControl {
    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire) || self.owner.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        let _ = self.owner.wake.wake();
    }
}

pub(super) struct BindRequest {
    address: SocketAddr,
    deadline: Instant,
    reply: SyncSender<Result<RemoteForward, ConnectError>>,
    abandoned: Arc<AtomicBool>,
}

impl BindRequest {
    fn expired(&self) -> bool {
        self.abandoned.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
}

pub(super) fn control(owner: &Arc<Shared>) -> (RemoteOpener, Receiver<BindRequest>) {
    let (requests, incoming) = mpsc::sync_channel(MAX_REMOTE_LISTENERS);
    let opener = RemoteOpener {
        requests,
        owner: Arc::clone(owner),
    };
    (opener, incoming)
}

struct Registration {
    address: SocketAddr,
    control: Arc<ListenerControl>,
    incoming: SyncSender<RemoteConnection>,
    cancelled: SyncSender<Result<(), ConnectError>>,
    channels: Vec<SshAbort>,
    cancel_deadline: Option<Instant>,
}

impl Registration {
    fn update(&mut self) {
        self.channels.retain(|channel| !channel.is_closed());
        if self.control.is_closing() && self.cancel_deadline.is_none() {
            self.cancel_deadline = Some(Instant::now() + CONNECT_TIMEOUT);
            for channel in self.channels.drain(..) {
                channel.abort();
            }
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.control.close();
        for channel in self.channels.drain(..) {
            channel.abort();
        }
    }
}

enum Pending {
    Bind(BindRequest),
    Cancel(Registration, Instant),
}

impl Pending {
    fn deadline(&self) -> Instant {
        match self {
            Self::Bind(bind) => bind.deadline,
            Self::Cancel(_, deadline) => *deadline,
        }
    }
}

pub(super) struct Remotes {
    requests: Receiver<BindRequest>,
    ready: Option<BindRequest>,
    pending: Option<Pending>,
    listeners: Vec<Registration>,
}

impl Remotes {
    pub(super) fn new(requests: Receiver<BindRequest>) -> Self {
        Self {
            requests,
            ready: None,
            pending: None,
            listeners: Vec::new(),
        }
    }

    pub(super) fn wait_time(&self) -> Option<Duration> {
        self.pending
            .iter()
            .map(Pending::deadline)
            .chain(self.ready.iter().map(|request| request.deadline))
            .chain(
                self.listeners
                    .iter()
                    .filter_map(|listener| listener.cancel_deadline),
            )
            .min()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(super) fn accept(
        &mut self,
        request: ForwardedTcpipOpen<'_, '_>,
        forwards: &mut Forwards,
    ) -> sunset::Result<()> {
        let addresses = request.addresses()?;
        let destination = numeric_address(addresses.address, addresses.port);
        let origin = numeric_address(addresses.origin, addresses.origin_port);
        let Some((destination, origin)) = destination.zip(origin) else {
            return request.reject();
        };
        let Some(listener) = self.authorized_listener(destination) else {
            return request.reject();
        };
        let channel = forwards.accept_inbound(request.accept()?, &listener.control.owner);
        let abort = channel.abort_handle();
        if listener
            .incoming
            .try_send(RemoteConnection { origin, channel })
            .is_ok()
        {
            listener.channels.retain(|channel| !channel.is_closed());
            listener.channels.push(abort);
        }
        Ok(())
    }

    fn authorized_listener(&mut self, address: SocketAddr) -> Option<&mut Registration> {
        self.listeners
            .iter_mut()
            .find(|listener| listener.address == address && !listener.control.is_closing())
    }

    pub(super) fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
    ) -> io::Result<bool> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline())
        {
            // A late reply cannot safely be matched to any subsequent request.
            return Err(remote_timeout());
        }
        let replied = if let Some(reply) = runner.take_global_reply() {
            let pending = self.pending.take().ok_or_else(invalid_reply)?;
            self.complete(pending, reply, owner)?;
            true
        } else {
            false
        };
        for listener in &mut self.listeners {
            listener.update();
        }
        if self.ready.as_ref().is_some_and(BindRequest::expired) {
            if let Some(bind) = self.ready.take() {
                let _ = bind.reply.try_send(Err(remote_timeout().into()));
            }
            return Ok(true);
        }
        let closing = self
            .listeners
            .iter()
            .enumerate()
            .find_map(|(index, listener)| {
                listener.cancel_deadline.map(|deadline| (index, deadline))
            });
        if let Some((index, deadline)) = closing {
            if Instant::now() >= deadline {
                return Err(remote_timeout());
            }
            if self.pending.is_none() && send_request(runner, self.listeners[index].address, true)?
            {
                self.pending = Some(Pending::Cancel(self.listeners.swap_remove(index), deadline));
                return Ok(true);
            }
            return Ok(replied);
        }
        if self.pending.is_some() {
            return Ok(replied);
        }
        self.start_bind(runner).map(|started| replied || started)
    }

    fn start_bind(&mut self, runner: &mut Runner<'_, Client>) -> io::Result<bool> {
        let Some(bind) = self.ready.take().or_else(|| self.requests.try_recv().ok()) else {
            return Ok(false);
        };
        if bind.expired() {
            let _ = bind.reply.try_send(Err(remote_timeout().into()));
        } else if self.listeners.len() >= MAX_REMOTE_LISTENERS {
            let error = io::Error::new(
                io::ErrorKind::WouldBlock,
                "SSH remote listener limit reached",
            );
            let _ = bind.reply.try_send(Err(error.into()));
        } else if self
            .listeners
            .iter()
            .any(|listener| listener.address == bind.address)
        {
            let error = io::Error::new(
                io::ErrorKind::AddrInUse,
                "SSH remote address already registered",
            );
            let _ = bind.reply.try_send(Err(error.into()));
        } else if send_request(runner, bind.address, false)? {
            self.pending = Some(Pending::Bind(bind));
        } else {
            self.ready = Some(bind);
            return Ok(false);
        }
        Ok(true)
    }

    fn complete(
        &mut self,
        pending: Pending,
        reply: GlobalReply,
        owner: &Arc<Shared>,
    ) -> io::Result<()> {
        match (pending, reply) {
            (Pending::Bind(bind), GlobalReply::Accepted(port)) => {
                let address = confirmed_address(bind.address, port).ok_or_else(invalid_reply)?;
                if self
                    .listeners
                    .iter()
                    .any(|listener| listener.address == address)
                {
                    return Err(invalid_reply());
                }
                let (incoming, connections) = mpsc::sync_channel(sunset::config::MAX_CHANNELS);
                let (cancelled, cancel_result) = mpsc::sync_channel(1);
                let control = Arc::new(ListenerControl {
                    closing: AtomicBool::new(bind.expired()),
                    owner: Arc::clone(owner),
                });
                self.listeners.push(Registration {
                    address,
                    control: Arc::clone(&control),
                    incoming,
                    cancelled,
                    channels: Vec::new(),
                    cancel_deadline: None,
                });
                let _ = bind.reply.try_send(Ok(RemoteForward {
                    address,
                    control,
                    incoming: connections,
                    cancelled: Some(cancel_result),
                }));
                Ok(())
            }
            (Pending::Bind(bind), GlobalReply::Refused) => {
                let _ = bind
                    .reply
                    .try_send(Err(ConnectError::RequestRejected { request: BIND }));
                Ok(())
            }
            (Pending::Cancel(listener, _), GlobalReply::Accepted(None)) => {
                let _ = listener.cancelled.try_send(Ok(()));
                Ok(())
            }
            (Pending::Cancel(listener, _), GlobalReply::Refused) => {
                let _ = listener
                    .cancelled
                    .try_send(Err(ConnectError::RequestRejected { request: CANCEL }));
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SSH server refused listener cancellation",
                ))
            }
            (Pending::Cancel(_, _), GlobalReply::Accepted(Some(_))) => Err(invalid_reply()),
        }
    }
}

fn send_request(
    runner: &mut Runner<'_, Client>,
    address: SocketAddr,
    cancel: bool,
) -> io::Result<bool> {
    runner
        .request_remote_forward(&address.ip().to_string(), address.port(), cancel)
        .map_err(io::Error::other)
}

fn confirmed_address(requested: SocketAddr, reply: Option<u32>) -> Option<SocketAddr> {
    match (requested.port(), reply) {
        (0, Some(port)) => Some(SocketAddr::new(requested.ip(), usable_port(port)?)),
        (port, None) if port != 0 => Some(requested),
        _ => None,
    }
}

fn numeric_address(address: sunset::TextString<'_>, port: u32) -> Option<SocketAddr> {
    // Long, non-ASCII and non-numeric fields cannot match an explicitly registered bind.
    let address = address.to_ascii().ok()?;
    if address.len() > 45 {
        return None;
    }
    Some(SocketAddr::new(
        address.parse::<IpAddr>().ok()?,
        usable_port(port)?,
    ))
}

fn usable_port(port: u32) -> Option<u16> {
    u16::try_from(port).ok().filter(|port| *port != 0)
}

fn remote_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "SSH remote listener request timed out",
    )
}

fn invalid_reply() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid SSH remote listener reply",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_numeric_channel_addresses_and_global_reply_shape() {
        let assigned = "127.0.0.1:49152".parse().unwrap();
        let fixed = "[::1]:49152".parse().unwrap();
        let dynamic = "127.0.0.1:0".parse().unwrap();
        assert_eq!(numeric_address("127.0.0.1".into(), 49152), Some(assigned));
        assert_eq!(
            numeric_address("0:0:0:0:0:0:0:1".into(), 49152),
            Some(fixed)
        );
        for invalid in ["localhost", "[::1]", "fe80::1%lo0", "127.0.0.1\0"] {
            assert_eq!(numeric_address(invalid.into(), 49152), None);
        }
        assert_eq!(numeric_address(sunset::TextString(&[0xff]), 49152), None);
        assert_eq!(numeric_address("127.0.0.1".into(), 0), None);
        assert_eq!(numeric_address("127.0.0.1".into(), 65536), None);
        assert_eq!(confirmed_address(dynamic, Some(49152)), Some(assigned));
        assert_eq!(confirmed_address(fixed, None), Some(fixed));
        assert_eq!(confirmed_address(dynamic, None), None);
        assert_eq!(confirmed_address(dynamic, Some(0)), None);
        assert_eq!(confirmed_address(dynamic, Some(65536)), None);
        assert_eq!(confirmed_address(fixed, Some(49152)), None);
    }

    #[test]
    fn accepts_only_the_active_confirmed_bind() {
        let poll = Poll::new().unwrap();
        let owner = Shared::new(Arc::new(Waker::new(poll.registry(), CONTROL).unwrap()));
        let (_opener, requests) = control(&owner);
        let mut remotes = Remotes::new(requests);
        let wanted = "127.0.0.1:49152".parse().unwrap();
        assert!(remotes.authorized_listener(wanted).is_none());

        let (reply, result) = mpsc::sync_channel(1);
        let bind = BindRequest {
            address: "127.0.0.1:0".parse().unwrap(),
            deadline: Instant::now() + CONNECT_TIMEOUT,
            reply,
            abandoned: Arc::new(AtomicBool::new(false)),
        };
        // Test double for the peer's confirmed global reply.
        remotes
            .complete(
                Pending::Bind(bind),
                GlobalReply::Accepted(Some(49152)),
                &owner,
            )
            .unwrap();
        let listener = result.try_recv().unwrap().unwrap();
        assert!(remotes.authorized_listener(wanted).is_some());
        for other in ["127.0.0.1:49153", "127.0.0.2:49152", "[::1]:49152"] {
            assert!(
                remotes
                    .authorized_listener(other.parse().unwrap())
                    .is_none()
            );
        }
        drop(listener);
        assert!(remotes.authorized_listener(wanted).is_none());
    }
}
