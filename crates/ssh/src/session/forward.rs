use std::sync::mpsc::RecvTimeoutError;

use crate::MAX_FORWARD_STREAMS;

use super::*;

/// A direct-tcpip opener tied to an already verified SSH connection.
#[derive(Clone)]
pub struct DirectOpener {
    requests: SyncSender<OpenForward>,
    owner: Arc<Shared>,
}

/// One bidirectional direct-tcpip channel on an existing connection.
pub struct DirectChannel {
    shared: Arc<Shared>,
    reader: Option<SshReader>,
    writer: Option<SshWriter>,
}

impl DirectChannel {
    pub fn take_reader(&mut self) -> Option<SshReader> {
        self.reader.take()
    }

    pub fn take_writer(&mut self) -> Option<SshWriter> {
        self.writer.take()
    }

    pub fn abort_handle(&self) -> SshAbort {
        SshAbort(Arc::clone(&self.shared))
    }

    /// Closes this channel without closing the SSH connection or other channels.
    pub fn close(&mut self) {
        self.abort_handle().abort();
    }
}

impl Drop for DirectChannel {
    fn drop(&mut self) {
        self.close();
    }
}

impl DirectOpener {
    pub fn is_closed(&self) -> bool {
        self.owner.closed.load(Ordering::Acquire)
    }

    /// Waits for the server to confirm the direct-tcpip channel.
    pub fn open(
        &self,
        destination: Endpoint,
        origin: SocketAddr,
    ) -> Result<DirectChannel, ConnectError> {
        self.open_cancellable(destination, origin, || false)
    }

    /// Opens a direct channel with a cooperative cancellation check.
    pub fn open_cancellable(
        &self,
        destination: Endpoint,
        origin: SocketAddr,
        cancelled: impl Fn() -> bool,
    ) -> Result<DirectChannel, ConnectError> {
        if self.is_closed() {
            return Err(closed().into());
        }
        let startup = Startup::new(Some(&cancelled));
        startup.check_cancelled()?;
        let (reply, result) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.requests
            .try_send(OpenForward {
                destination,
                origin,
                reply,
                abandoned: Arc::clone(&abandoned),
                deadline: startup.deadline,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "SSH forward queue is full")
                }
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
                            startup.check_cancelled()?;
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

pub(super) struct OpenForward {
    destination: Endpoint,
    origin: SocketAddr,
    reply: SyncSender<Result<DirectChannel, ConnectError>>,
    abandoned: Arc<AtomicBool>,
    deadline: Instant,
}

impl OpenForward {
    fn expired(&self) -> bool {
        self.abandoned.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    fn fail(self, error: ConnectError) {
        let _ = self.reply.try_send(Err(error));
    }
}

pub(super) fn control(owner: &Arc<Shared>) -> (DirectOpener, Receiver<OpenForward>) {
    let (requests, incoming) = mpsc::sync_channel(sunset::config::MAX_CHANNELS);
    let opener = DirectOpener {
        requests,
        owner: Arc::clone(owner),
    };
    (opener, incoming)
}

enum Forward {
    Opening(ChanHandle, OpenForward),
    Active(ChannelPump),
    Closing(ChanHandle),
}

pub(super) struct Forwards {
    incoming: Receiver<OpenForward>,
    pending: Option<OpenForward>,
    channels: Vec<Forward>,
    limit: usize,
}

impl Forwards {
    pub(super) fn new(incoming: Receiver<OpenForward>, transfer_helper: bool) -> Self {
        Self {
            incoming,
            pending: None,
            channels: Vec::new(),
            limit: MAX_FORWARD_STREAMS - usize::from(transfer_helper),
        }
    }

    pub(super) fn wait_time(&self) -> Option<Duration> {
        self.channels
            .iter()
            .filter_map(|state| match state {
                Forward::Opening(_, request) => Some(request.deadline),
                _ => None,
            })
            .chain(self.pending.iter().map(|request| request.deadline))
            .min()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(super) fn accept_inbound(
        &mut self,
        channel: ChanHandle,
        owner: &Arc<Shared>,
    ) -> DirectChannel {
        let (pump, stream) = direct_pair(channel, owner);
        self.channels.push(Forward::Active(pump));
        stream
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.channels.len() < self.limit
    }

    pub(super) fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> io::Result<bool> {
        let mut progressed = false;
        for index in (0..self.channels.len()).rev() {
            let state = self.channels.swap_remove(index);
            let (next, activity) = state.step(runner, owner, buffer, output_drained)?;
            self.channels.extend(next);
            progressed |= activity;
        }

        if self.pending.is_none() {
            self.pending = self.incoming.try_recv().ok();
        }
        if let Some(request) = self.pending.take() {
            if request.expired() {
                request.fail(forward_timeout());
                return Ok(true);
            }
            let open = if self.has_capacity() {
                open_direct_channel(runner, &request.destination, request.origin)
            } else {
                Err(sunset::Error::NoChannels)
            };
            match open {
                Ok(Some(channel)) => {
                    self.channels.push(Forward::Opening(channel, request));
                    progressed = true;
                }
                Ok(None) => self.pending = Some(request),
                Err(sunset::Error::NoChannels)
                    if self
                        .channels
                        .iter()
                        .any(|channel| matches!(channel, Forward::Closing(_))) =>
                {
                    self.pending = Some(request);
                }
                Err(sunset::Error::NoChannels) => {
                    request.fail(
                        io::Error::new(io::ErrorKind::WouldBlock, "SSH forwarding slots are full")
                            .into(),
                    );
                    progressed = true;
                }
                Err(error) => {
                    request.fail(error.into());
                    progressed = true;
                }
            }
        }
        Ok(progressed)
    }
}

impl Drop for Forwards {
    fn drop(&mut self) {
        for state in self.channels.drain(..) {
            if let Forward::Active(pump) = state {
                pump.finish(Err(closed()));
            }
        }
    }
}

impl Forward {
    fn step(
        self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> io::Result<(Option<Self>, bool)> {
        let next = match self {
            Self::Opening(channel, request) => {
                if request.expired() {
                    request.fail(forward_timeout());
                    Self::Closing(channel)
                } else {
                    match runner.direct_status(&channel).map_err(io::Error::other)? {
                        DirectStatus::Opening => {
                            return Ok((Some(Self::Opening(channel, request)), false));
                        }
                        DirectStatus::Refused(reason) => {
                            request.fail(
                                io::Error::new(
                                    io::ErrorKind::ConnectionRefused,
                                    format!("SSH server refused TCP forwarding (reason {reason})"),
                                )
                                .into(),
                            );
                            Self::Closing(channel)
                        }
                        DirectStatus::Open => {
                            let (pump, stream) = direct_pair(channel, owner);
                            let _ = request.reply.try_send(Ok(stream));
                            Self::Active(pump)
                        }
                    }
                }
            }
            Self::Active(mut pump) => match pump.step(runner, buffer, output_drained) {
                Ok(PumpStep::Active(activity)) => return Ok((Some(Self::Active(pump)), activity)),
                Ok(PumpStep::Done) => Self::Closing(pump.finish(Ok(()))),
                Err(error) => Self::Closing(pump.finish(Err(error))),
            },
            Self::Closing(channel) => {
                return match runner.close_direct(&channel) {
                    Ok(true) => Ok((None, true)),
                    Ok(false) | Err(sunset::Error::BusySend { .. }) => {
                        Ok((Some(Self::Closing(channel)), false))
                    }
                    Err(error) => Err(io::Error::other(error)),
                };
            }
        };
        Ok((Some(next), true))
    }
}

fn direct_pair(channel: ChanHandle, owner: &Arc<Shared>) -> (ChannelPump, DirectChannel) {
    let shared = Shared::new(Arc::clone(&owner.wake));
    let (input, commands) = mpsc::sync_channel(QUEUE_DEPTH);
    let (stdout, output) = output::queue();
    let stream = DirectChannel {
        shared: Arc::clone(&shared),
        reader: Some(SshReader::new(&shared, output)),
        writer: Some(SshWriter {
            shared: Arc::clone(&shared),
            input,
            input_closed: false,
        }),
    };
    let output = Outputs {
        mode: ChannelMode::Direct,
        stdout: Some(stdout),
        stderr: None,
    };
    let pump = ChannelPump::new(channel, shared, commands, output);
    (pump, stream)
}

fn forward_timeout() -> ConnectError {
    io::Error::new(io::ErrorKind::TimedOut, "SSH direct channel timed out").into()
}
