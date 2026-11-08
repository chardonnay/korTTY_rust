use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kortty_ssh::{ConnectError, Endpoint};

use super::mux::{Flow, Mux, Reader, Writer};
use super::wire::{Frame, Open, invalid};
use super::{Abort, NO_EXIT, closed, lock};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(9);
const CANCEL_POLL: Duration = Duration::from_millis(100);
const MAX_PENDING: usize = 32;
const ACCEPT_BACKLOG: usize = 4;

#[derive(Clone)]
pub(crate) struct Opener(Arc<Client>);

struct Client {
    mux: Mux,
    abort: Abort,
    next_id: AtomicU32,
    requests: Mutex<Requests>,
}

#[derive(Default)]
struct Requests {
    pending: BTreeMap<u32, SyncSender<Reply>>,
    listeners: BTreeMap<u32, SyncSender<Accepted>>,
}

enum Reply {
    Opened(Option<SocketAddr>),
    Closed(Option<String>),
}

pub(crate) struct Channel {
    flow: Flow,
    reader: Option<Reader>,
    writer: Option<Writer>,
}

impl Channel {
    fn new(flow: Flow) -> Self {
        Self {
            reader: Some(flow.reader()),
            writer: Some(flow.writer()),
            flow,
        }
    }

    pub(crate) fn take_reader(&mut self) -> Option<Reader> {
        self.reader.take()
    }

    pub(crate) fn take_writer(&mut self) -> Option<Writer> {
        self.writer.take()
    }

    pub(crate) fn abort_handle(&self) -> StreamAbort {
        StreamAbort(self.flow.clone())
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.flow.close(None);
    }
}

#[derive(Clone)]
pub(crate) struct StreamAbort(Flow);

impl StreamAbort {
    pub(crate) fn abort(&self) {
        self.0.close(None);
    }

    pub(crate) fn is_closed(&self) -> bool {
        !self.0.is_open()
    }
}

pub(crate) struct RemoteForward {
    owner: Opener,
    flow: Flow,
    address: SocketAddr,
    incoming: Receiver<Accepted>,
}

pub(crate) struct Accepted {
    pub(crate) origin: SocketAddr,
    pub(crate) channel: Channel,
}

impl RemoteForward {
    pub(crate) fn remote_addr(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn accept_timeout(&self, timeout: Duration) -> io::Result<Option<Accepted>> {
        if !self.flow.is_open() {
            return Err(self.flow.error());
        }
        match self.incoming.recv_timeout(timeout) {
            Ok(accepted) if self.flow.is_open() => Ok(Some(accepted)),
            Err(RecvTimeoutError::Timeout) if self.flow.is_open() => Ok(None),
            _ => Err(self.flow.error()),
        }
    }

    pub(crate) fn close(&mut self) -> Result<(), ConnectError> {
        let owner = &self.owner.0;
        let id = self.flow.id();
        if !self.flow.is_open() {
            return Ok(());
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        {
            let mut requests = lock(&owner.requests);
            requests.listeners.remove(&id);
            if requests.pending.len() >= MAX_PENDING {
                owner.mux.stop();
                owner.abort.abort();
                return Err(request_limit().into());
            }
            requests.pending.insert(id, reply);
        }
        if !self.flow.close(None) {
            lock(&owner.requests).pending.remove(&id);
            return Ok(());
        }
        let result = wait_reply(receiver, || false);
        lock(&owner.requests).pending.remove(&id);
        match result {
            Ok(Reply::Closed(None)) => Ok(()),
            Ok(Reply::Closed(Some(error))) => Err(io::Error::other(error).into()),
            Ok(_) => Err(invalid().into()),
            Err(error) => {
                // A lost cancellation reply must not leave a server listener alive.
                owner.mux.stop();
                owner.abort.abort();
                Err(error.into())
            }
        }
    }
}

impl Drop for RemoteForward {
    fn drop(&mut self) {
        lock(&self.owner.0.requests)
            .listeners
            .remove(&self.flow.id());
        self.flow.close(None);
    }
}

impl Opener {
    pub(super) fn new(mux: Mux, abort: Abort) -> Self {
        Self(Arc::new(Client {
            mux,
            abort,
            next_id: AtomicU32::new(1),
            requests: Mutex::default(),
        }))
    }

    pub(crate) fn is_closed(&self) -> bool {
        !self.0.mux.running() || self.0.abort.is_closed()
    }

    pub(crate) fn open_sftp(&self, cancelled: impl Fn() -> bool) -> Result<Channel, ConnectError> {
        self.open_channel(Open::Sftp, cancelled)
    }

    pub(crate) fn open_direct(
        &self,
        destination: Endpoint,
        origin: SocketAddr,
        cancelled: impl Fn() -> bool,
    ) -> Result<Channel, ConnectError> {
        self.open_channel(Open::Direct(destination, origin), cancelled)
    }

    fn open_channel(
        &self,
        request: Open,
        cancelled: impl Fn() -> bool,
    ) -> Result<Channel, ConnectError> {
        let (flow, address) = self.open(request, None, cancelled)?;
        if address.is_some() {
            flow.close(None);
            return Err(invalid().into());
        }
        Ok(Channel::new(flow))
    }

    pub(crate) fn bind(
        &self,
        address: SocketAddr,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteForward, ConnectError> {
        let (accepted, incoming) = mpsc::sync_channel(ACCEPT_BACKLOG);
        let (flow, address) = self.open(Open::Listen(address), Some(accepted), cancelled)?;
        let Some(address) = address else {
            lock(&self.0.requests).listeners.remove(&flow.id());
            flow.close(None);
            return Err(invalid().into());
        };
        Ok(RemoteForward {
            owner: self.clone(),
            flow,
            address,
            incoming,
        })
    }

    fn open(
        &self,
        request: Open,
        listener: Option<SyncSender<Accepted>>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(Flow, Option<SocketAddr>), ConnectError> {
        if cancelled() {
            return Err(cancelled_error().into());
        }
        let id = self
            .0
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(2))
            .map_err(|_| io::Error::other("SSH worker stream IDs exhausted"))?;
        let flow = self.0.mux.add(id)?;
        let (reply, receiver) = mpsc::sync_channel(1);
        {
            let mut requests = lock(&self.0.requests);
            if requests.pending.len() >= MAX_PENDING {
                // The peer has not seen this ID yet.
                flow.close(None);
                return Err(request_limit().into());
            }
            requests.pending.insert(id, reply);
            if let Some(listener) = listener {
                requests.listeners.insert(id, listener);
            }
        }
        let reply = self
            .0
            .mux
            .control(Frame::Open(id, request))
            .and_then(|()| wait_reply(receiver, cancelled));
        match reply {
            Ok(Reply::Opened(address)) => Ok((flow, address)),
            outcome => {
                let mut requests = lock(&self.0.requests);
                requests.pending.remove(&id);
                requests.listeners.remove(&id);
                flow.close(None);
                match outcome {
                    Ok(Reply::Closed(Some(error))) => Err(io::Error::other(error).into()),
                    Ok(Reply::Closed(None)) => Err(closed().into()),
                    Ok(Reply::Opened(_)) => unreachable!(),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    pub(super) fn receive(&self, frame: Frame) -> io::Result<()> {
        let owner = &self.0;
        match frame {
            Frame::Opened(id, address) => owner.reply(id, Reply::Opened(address)),
            Frame::Closed(id, error) => owner.reply(id, Reply::Closed(error)),
            Frame::Accepted(listener, id, origin) if id != 0 && id.is_multiple_of(2) => {
                let accepted = lock(&owner.requests).listeners.get(&listener).cloned();
                if let Some(accepted) = accepted {
                    match owner.mux.add(id) {
                        Ok(flow) => {
                            let _ = accepted.try_send(Accepted {
                                origin,
                                channel: Channel::new(flow),
                            });
                            return Ok(());
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => return Err(error),
                    }
                }
                owner.mux.control(Frame::Close(id, None))?;
            }
            Frame::Close(id, error) => {
                owner.mux.receive(Frame::Close(id, error.clone()))?;
                lock(&owner.requests).listeners.remove(&id);
                owner.reply(id, Reply::Closed(error));
            }
            Frame::End(0, code) => {
                owner
                    .abort
                    .0
                    .exit_code
                    .store(code.map_or(NO_EXIT, u64::from), Ordering::Release);
                owner.mux.receive(Frame::End(0, code))?;
            }
            other => owner.mux.receive(other)?,
        }
        Ok(())
    }

    pub(super) fn stop(&self) {
        self.0.mux.stop();
        *lock(&self.0.requests) = Requests::default();
    }
}

impl Client {
    fn reply(&self, id: u32, reply: Reply) {
        if let Some(sender) = lock(&self.requests).pending.remove(&id) {
            let _ = sender.try_send(reply);
        }
    }
}

fn wait_reply(receiver: Receiver<Reply>, cancelled: impl Fn() -> bool) -> io::Result<Reply> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        if cancelled() {
            return Err(cancelled_error());
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(CANCEL_POLL);
        if wait.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH worker channel request timed out",
            ));
        }
        match receiver.recv_timeout(wait) {
            Ok(reply) if !cancelled() => return Ok(reply),
            Ok(_) => return Err(cancelled_error()),
            Err(RecvTimeoutError::Disconnected) => return Err(closed()),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn cancelled_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "SSH worker channel request cancelled",
    )
}

fn request_limit() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "SSH worker request limit")
}
