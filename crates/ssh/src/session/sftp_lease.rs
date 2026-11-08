use super::*;

/// Opens SFTP on a specific authenticated terminal connection.
#[derive(Clone)]
pub struct SftpOpener {
    requests: SyncSender<OpenLease>,
    owner: Arc<Shared>,
}

impl SftpOpener {
    pub fn is_closed(&self) -> bool {
        self.owner.closed.load(Ordering::Acquire)
    }

    pub fn open(&self) -> Result<SftpLease, ConnectError> {
        self.open_cancellable(|| false)
    }

    /// Waits for confirmation of the new session and its SFTP subsystem.
    pub fn open_cancellable(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<SftpLease, ConnectError> {
        if self.is_closed() {
            return Err(closed().into());
        }
        let startup = Startup::new(Some(&cancelled));
        startup.check_cancelled()?;
        let (reply, result) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.requests
            .try_send(OpenLease {
                reply,
                abandoned: Arc::clone(&abandoned),
                deadline: startup.deadline,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => lease_busy(),
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
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(closed().into()),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
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

/// Owns one SFTP channel. Dropping it closes only that channel.
pub struct SftpLease {
    abort: SshAbort,
    reader: Option<SshReader>,
    writer: Option<SshWriter>,
}

impl SftpLease {
    pub fn take_reader(&mut self) -> Option<SshReader> {
        self.reader.take()
    }

    pub fn take_writer(&mut self) -> Option<SshWriter> {
        self.writer.take()
    }

    pub fn abort_handle(&self) -> SshAbort {
        self.abort.clone()
    }

    pub fn close(&mut self) {
        self.abort.abort();
    }
}

impl Drop for SftpLease {
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) struct OpenLease {
    reply: SyncSender<Result<SftpLease, ConnectError>>,
    abandoned: Arc<AtomicBool>,
    deadline: Instant,
}

impl OpenLease {
    fn expired(&self) -> bool {
        self.abandoned.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    fn complete(self, result: Result<SftpLease, ConnectError>) {
        let _ = self.reply.try_send(result);
    }
}

pub(super) fn control(owner: &Arc<Shared>) -> (SftpOpener, Receiver<OpenLease>) {
    let (requests, incoming) = mpsc::sync_channel(1);
    (
        SftpOpener {
            requests,
            owner: Arc::clone(owner),
        },
        incoming,
    )
}

pub(super) struct Leases {
    incoming: Receiver<OpenLease>,
    pending: Option<OpenLease>,
    channel: Option<LeaseState>,
}

enum LeaseState {
    Opening(ChanHandle, OpenLease),
    Negotiating(ChanHandle, OpenLease),
    Active(ChannelPump),
    Closing(ChanHandle),
}

impl Leases {
    pub(super) fn new(incoming: Receiver<OpenLease>) -> Self {
        Self {
            incoming,
            pending: None,
            channel: None,
        }
    }

    pub(super) fn wait_time(&self) -> Option<Duration> {
        let opening = self.channel.as_ref().and_then(|state| match state {
            LeaseState::Opening(_, request) | LeaseState::Negotiating(_, request) => {
                Some(request.deadline)
            }
            _ => None,
        });
        opening
            .into_iter()
            .chain(self.pending.iter().map(|request| request.deadline))
            .min()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(super) fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> bool {
        let mut progressed = false;
        if let Some(state) = self.channel.take() {
            let (next, activity) = state.step(runner, owner, buffer, output_drained);
            self.channel = next;
            progressed = activity;
        }
        if self.pending.is_none() {
            self.pending = self.incoming.try_recv().ok();
        }
        let Some(request) = self.pending.take() else {
            return progressed;
        };
        if request.expired() {
            request.complete(Err(lease_timeout()));
            return true;
        }
        match &self.channel {
            Some(LeaseState::Closing(_)) => self.pending = Some(request),
            Some(_) => {
                request.complete(Err(lease_busy().into()));
                progressed = true;
            }
            None => match runner.open_tracked_session() {
                Ok(Some(channel)) => {
                    self.channel = Some(LeaseState::Opening(channel, request));
                    progressed = true;
                }
                Ok(None) => self.pending = Some(request),
                Err(error) => {
                    request.complete(Err(error.into()));
                    progressed = true;
                }
            },
        }
        progressed
    }
}

impl Drop for Leases {
    fn drop(&mut self) {
        if let Some(LeaseState::Active(pump)) = self.channel.take() {
            pump.finish(Err(closed()));
        }
    }
}

impl LeaseState {
    fn step(
        self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> (Option<Self>, bool) {
        let next = match self {
            Self::Opening(channel, request) => {
                if request.expired() {
                    return reject(channel, request, lease_timeout());
                }
                match runner.tracked_session_status(&channel) {
                    Ok(DirectStatus::Opening) => {
                        return (Some(Self::Opening(channel, request)), false);
                    }
                    Ok(DirectStatus::Refused(_)) => {
                        return reject(
                            channel,
                            request,
                            ConnectError::RequestRejected { request: "session" },
                        );
                    }
                    Ok(DirectStatus::Open) => {}
                    Err(error) => return reject(channel, request, error.into()),
                }
                if let Some(error) = invalid_open_data(runner, &channel) {
                    return reject(channel, request, error);
                }
                match runner.client_tracked_subsystem(&channel, "sftp") {
                    Ok(true) => Self::Negotiating(channel, request),
                    Ok(false) => return (Some(Self::Opening(channel, request)), false),
                    Err(error) => return reject(channel, request, error.into()),
                }
            }
            Self::Negotiating(channel, request) => {
                if request.expired() {
                    return reject(channel, request, lease_timeout());
                }
                match runner.client_request_result(&channel) {
                    Ok(Some(true)) => {
                        let (pump, stream) = lease_pair(channel, owner);
                        request.complete(Ok(stream));
                        Self::Active(pump)
                    }
                    Ok(Some(false)) => {
                        return reject(
                            channel,
                            request,
                            ConnectError::RequestRejected {
                                request: "subsystem",
                            },
                        );
                    }
                    Ok(None) => {
                        if let Some(error) = invalid_open_data(runner, &channel) {
                            return reject(channel, request, error);
                        }
                        return (Some(Self::Negotiating(channel, request)), false);
                    }
                    Err(error) => return reject(channel, request, error.into()),
                }
            }
            Self::Active(mut pump) => match pump.step(runner, buffer, output_drained) {
                Ok(PumpStep::Active(activity)) => return (Some(Self::Active(pump)), activity),
                Ok(PumpStep::Done) => Self::Closing(pump.finish(Ok(()))),
                Err(error) => Self::Closing(pump.finish(Err(error))),
            },
            Self::Closing(channel) => {
                return match runner.close_tracked_channel(&channel) {
                    Ok(false)
                    | Err(sunset::Error::BusySend { .. } | sunset::Error::NoRoom { .. }) => {
                        (Some(Self::Closing(channel)), false)
                    }
                    Ok(true) | Err(_) => (None, true),
                };
            }
        };
        (Some(next), true)
    }
}

fn invalid_open_data(runner: &Runner<'_, Client>, channel: &ChanHandle) -> Option<ConnectError> {
    if runner.is_channel_closed(channel) || runner.is_channel_eof(channel) {
        return Some(closed().into());
    }
    runner
        .read_channel_ready()
        .filter(|(ready, _, _)| *ready == channel.num())
        .map(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH peer sent SFTP data before subsystem confirmation",
            )
            .into()
        })
}

fn reject(
    channel: ChanHandle,
    request: OpenLease,
    error: ConnectError,
) -> (Option<LeaseState>, bool) {
    request.complete(Err(error));
    (Some(LeaseState::Closing(channel)), true)
}

fn lease_pair(channel: ChanHandle, owner: &Arc<Shared>) -> (ChannelPump, SftpLease) {
    let shared = Shared::new(Arc::clone(&owner.wake));
    let (input, commands) = mpsc::sync_channel(QUEUE_DEPTH);
    let (stdout, output) = output::queue();
    let stream = SftpLease {
        abort: SshAbort(Arc::clone(&shared)),
        reader: Some(SshReader::new(&shared, output)),
        writer: Some(SshWriter {
            shared: Arc::clone(&shared),
            input,
            input_closed: false,
        }),
    };
    let output = Outputs {
        mode: ChannelMode::SftpLease,
        stdout: Some(stdout),
        stderr: None,
    };
    (ChannelPump::new(channel, shared, commands, output), stream)
}

fn lease_timeout() -> ConnectError {
    io::Error::new(io::ErrorKind::TimedOut, "SFTP channel open timed out").into()
}

fn lease_busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "this SSH terminal already has an SFTP lease",
    )
}
