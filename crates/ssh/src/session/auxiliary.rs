//! Independent bounded session channels on an authenticated terminal transport.

use super::*;

pub const MAX_EXEC_COMMAND_BYTES: usize = 28 * 1024;
const EXEC_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
type ExecAuthorization = Box<dyn FnOnce() -> io::Result<()> + Send>;

#[derive(Clone)]
struct Opener {
    requests: SyncSender<Open>,
    owner: Arc<Shared>,
}

/// Opens SFTP on a specific authenticated terminal connection.
#[derive(Clone)]
pub struct SftpOpener(Opener, Opener);

impl SftpOpener {
    pub fn verified_host_key(&self) -> Option<&HostKey> {
        self.0.owner.host_key.get()
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Opens commands on the exact same authenticated connection as this SFTP handle.
    pub fn exec_opener(&self) -> ExecOpener {
        ExecOpener(self.0.clone(), self.1.clone())
    }

    pub fn open(&self) -> Result<SftpLease, ConnectError> {
        self.open_cancellable(|| false)
    }

    pub fn open_cancellable(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<SftpLease, ConnectError> {
        self.0.open(Request::Sftp, &cancelled)
    }
}

/// Opens independently owned command channels on an authenticated connection.
#[derive(Clone)]
pub struct ExecOpener(Opener, Opener);

impl ExecOpener {
    pub fn verified_host_key(&self) -> Option<&HostKey> {
        self.0.owner.host_key.get()
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Compares the authenticated connection behind two exec handles.
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0.owner, &other.0.owner)
    }

    pub fn open_cancellable(
        &self,
        command: &str,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<ExecLease, ConnectError> {
        self.open_exec(command, ExecMode::Plain, cancelled, None)
    }

    /// Authorizes once on the I/O worker, immediately before requesting exec.
    pub fn open_guarded(
        &self,
        command: &str,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
        before_exec: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> Result<ExecLease, ConnectError> {
        self.open_exec(
            command,
            ExecMode::Plain,
            cancelled,
            Some(Box::new(before_exec)),
        )
    }

    /// Uses a wiping output queue for a command that can echo a credential.
    pub fn open_sensitive_cancellable(
        &self,
        command: &str,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<ExecLease, ConnectError> {
        self.open_exec(command, ExecMode::Sensitive, cancelled, None)
    }

    /// Opens a private command channel after the server confirms PTY allocation.
    ///
    /// Output is merged and wiped on consumption. The lease can hand off one
    /// sensitive input line to a subsequent stream without sending EOF.
    pub fn open_pty_cancellable(
        &self,
        command: &str,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<ExecLease, ConnectError> {
        self.open_exec(command, ExecMode::Pty, cancelled, None)
    }

    fn open_exec(
        &self,
        command: &str,
        mode: ExecMode,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
        before_exec: Option<ExecAuthorization>,
    ) -> Result<ExecLease, ConnectError> {
        if command.trim().is_empty()
            || command.len() > MAX_EXEC_COMMAND_BYTES
            || command.contains('\0')
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid SSH command").into());
        }
        let cancelled = Arc::new(cancelled);
        let wait = Arc::clone(&cancelled);
        let opener = if mode == ExecMode::Pty {
            &self.1
        } else {
            &self.0
        };
        opener.open(
            Request::Exec {
                command: command.into(),
                cancelled,
                mode,
                before_exec,
            },
            &|| wait(),
        )
    }
}

impl Opener {
    fn is_closed(&self) -> bool {
        self.owner.closed.load(Ordering::Acquire)
    }

    fn open(
        &self,
        request: Request,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SessionLease, ConnectError> {
        if self.is_closed() {
            return Err(closed().into());
        }
        let startup = if matches!(request, Request::Exec { .. }) {
            Startup::with_timeout(EXEC_OPEN_TIMEOUT, Some(cancelled))
        } else {
            Startup::new(Some(cancelled))
        };
        startup.check_cancelled()?;
        let (reply, result) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.requests
            .try_send(Open {
                request,
                reply,
                abandoned: Arc::clone(&abandoned),
                deadline: startup.deadline,
                status: None,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => busy(),
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

pub type SftpLease = SessionLease;
pub type ExecLease = SessionLease;

/// Dropping a lease closes only the channel it owns.
pub struct SessionLease {
    abort: SshAbort,
    mode: ChannelMode,
    stdout: Option<SshReader>,
    stderr: Option<SshReader>,
    writer: Option<SshWriter>,
}

impl SessionLease {
    pub fn take_reader(&mut self) -> Option<SshReader> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<SshReader> {
        self.stderr.take()
    }

    pub fn take_writer(&mut self) -> Option<SshWriter> {
        self.writer.take()
    }

    /// Takes the one-use credential input of an exec lease.
    pub fn take_sensitive_exec_input(&mut self) -> Option<super::SensitiveExecInput> {
        if !matches!(
            self.mode,
            ChannelMode::Exec | ChannelMode::SensitiveExec | ChannelMode::SensitiveExecPty
        ) {
            return None;
        }
        self.take_writer().map(super::SensitiveExecInput::new)
    }

    pub fn abort_handle(&self) -> SshAbort {
        self.abort.clone()
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.abort
            .0
            .exit_code
            .load(Ordering::Acquire)
            .try_into()
            .ok()
    }

    pub fn close(&mut self) {
        self.abort.abort();
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecMode {
    Plain,
    Sensitive,
    Pty,
}

enum Request {
    Sftp,
    Exec {
        command: String,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
        mode: ExecMode,
        before_exec: Option<ExecAuthorization>,
    },
}

impl Request {
    fn authorize(&mut self) -> io::Result<bool> {
        if let Self::Exec { before_exec, .. } = self
            && let Some(authorize) = before_exec.take()
        {
            authorize()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Sftp => "subsystem",
            Self::Exec { .. } => "exec",
        }
    }

    fn needs_pty(&self) -> bool {
        matches!(
            self,
            Self::Exec {
                mode: ExecMode::Pty,
                ..
            }
        )
    }

    fn send(
        &self,
        runner: &mut Runner<'_, Client>,
        channel: &ChanHandle,
    ) -> Result<bool, ConnectError> {
        Ok(match self {
            Self::Sftp => runner.client_tracked_subsystem(channel, "sftp")?,
            Self::Exec {
                mode: ExecMode::Pty,
                ..
            } => runner.client_tracked_pty(channel, private_pty()?)?,
            Self::Exec { command, .. } => runner.client_tracked_exec(channel, command)?,
        })
    }

    fn send_exec_after_pty(
        &self,
        runner: &mut Runner<'_, Client>,
        channel: &ChanHandle,
    ) -> sunset::Result<bool> {
        let Self::Exec { command, .. } = self else {
            unreachable!("only an exec channel requests a PTY");
        };
        runner.client_exec_after_pty(channel, command)
    }

    fn channel_mode(&self) -> ChannelMode {
        match self {
            Self::Sftp => ChannelMode::SftpLease,
            Self::Exec { mode, .. } => match mode {
                ExecMode::Plain => ChannelMode::Exec,
                ExecMode::Sensitive => ChannelMode::SensitiveExec,
                ExecMode::Pty => ChannelMode::SensitiveExecPty,
            },
        }
    }
}

fn private_pty() -> io::Result<Pty> {
    let mut pty = terminal_request(
        "dumb",
        WindowSize {
            columns: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
    )?;
    // RFC 4254 terminal modes: ICANON, ECHO, ECHONL, OPOST.
    for opcode in [51, 53, 56, 70] {
        pty.modes
            .push(sunset::ModePair { opcode, arg: 0 })
            .expect("four PTY modes fit in the request");
    }
    Ok(pty)
}

struct Open {
    request: Request,
    reply: SyncSender<Result<SessionLease, ConnectError>>,
    abandoned: Arc<AtomicBool>,
    deadline: Instant,
    status: Option<u32>,
}

impl Open {
    fn expired(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
            || matches!(&self.request, Request::Exec { cancelled, .. } if cancelled())
    }

    fn complete(self, result: Result<SessionLease, ConnectError>) {
        let _ = self.reply.try_send(result);
    }
}

pub(super) fn control(
    owner: &Arc<Shared>,
    transfer_helper: bool,
) -> (SftpOpener, ExecOpener, Channels) {
    let (sftp, sftp_pool) = Pool::control(owner, crate::MAX_SFTP_LEASES);
    let (exec, exec_pool) = Pool::control(owner, 1 + usize::from(transfer_helper));
    let (pty, pty_pool) = Pool::control(owner, 1);
    (
        SftpOpener(sftp, pty.clone()),
        ExecOpener(exec, pty),
        Channels([sftp_pool, exec_pool, pty_pool]),
    )
}

pub(super) struct Channels([Pool; 3]);

impl Channels {
    pub(super) fn wait_time(&self) -> Option<Duration> {
        self.0.iter().filter_map(Pool::wait_time).min()
    }

    pub(super) fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> bool {
        let mut progressed = false;
        for pool in &mut self.0 {
            progressed |= pool.step(runner, owner, buffer, output_drained);
        }
        progressed
    }

    pub(super) fn exit_status(&mut self, channel: sunset::ChanNum, status: u32) {
        for channel_state in self.0.iter_mut().flat_map(|pool| pool.channels.iter_mut()) {
            match channel_state {
                Some(State::Active(pump)) if pump.channel.num() == channel => {
                    pump.shared
                        .exit_code
                        .store(u64::from(status), Ordering::Release);
                }
                Some(State::Negotiating(handle, request, _)) if handle.num() == channel => {
                    request.status = Some(status);
                }
                _ => {}
            }
        }
    }
}

struct Pool {
    incoming: Receiver<Open>,
    pending: Option<Open>,
    channels: Box<[Option<State>]>,
}

enum State {
    Opening(ChanHandle, Open),
    Negotiating(ChanHandle, Open, Confirmation),
    Active(ChannelPump),
    Closing(ChanHandle),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    Pty,
    Command,
}

impl Pool {
    fn control(owner: &Arc<Shared>, capacity: usize) -> (Opener, Self) {
        let (requests, incoming) = mpsc::sync_channel(capacity);
        (
            Opener {
                requests,
                owner: Arc::clone(owner),
            },
            Self {
                incoming,
                pending: None,
                channels: std::iter::repeat_with(|| None).take(capacity).collect(),
            },
        )
    }

    fn wait_time(&self) -> Option<Duration> {
        let opening = self
            .channels
            .iter()
            .flatten()
            .filter_map(|state| match state {
                State::Opening(_, request) | State::Negotiating(_, request, _) => {
                    Some(request.deadline)
                }
                _ => None,
            });
        opening
            .chain(self.pending.iter().map(|request| request.deadline))
            .min()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> bool {
        let mut progressed = false;
        for channel in &mut self.channels {
            if let Some(state) = channel.take() {
                let (next, activity) = state.step(runner, owner, buffer, output_drained);
                *channel = next;
                progressed |= activity;
            }
        }
        if self.pending.is_none() {
            self.pending = self.incoming.try_recv().ok();
        }
        let Some(request) = self.pending.take() else {
            return progressed;
        };
        if request.expired() {
            request.complete(Err(timeout()));
            return true;
        }
        if let Some(slot) = self.channels.iter_mut().find(|channel| channel.is_none()) {
            match runner.open_tracked_session() {
                Ok(Some(channel)) => {
                    *slot = Some(State::Opening(channel, request));
                    progressed = true;
                }
                Ok(None) => self.pending = Some(request),
                Err(error) => {
                    request.complete(Err(error.into()));
                    progressed = true;
                }
            }
        } else if self
            .channels
            .iter()
            .any(|channel| matches!(channel, Some(State::Closing(_))))
        {
            self.pending = Some(request);
        } else {
            request.complete(Err(busy().into()));
            progressed = true;
        }
        progressed
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        for channel in &mut self.channels {
            if let Some(State::Active(pump)) = channel.take() {
                pump.finish(Err(closed()));
            }
        }
    }
}

impl State {
    fn step(
        self,
        runner: &mut Runner<'_, Client>,
        owner: &Arc<Shared>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> (Option<Self>, bool) {
        let next = match self {
            Self::Opening(channel, mut request) => {
                if request.expired() {
                    return reject(channel, request, timeout());
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
                if let Some(error) = premature_data(runner, &channel) {
                    return reject(channel, request, error);
                }
                if request.expired() {
                    return reject(channel, request, timeout());
                }
                let authorized_now = match request.request.authorize() {
                    Ok(authorized) => authorized,
                    Err(error) => return reject(channel, request, error.into()),
                };
                if request.expired() {
                    return reject(channel, request, timeout());
                }
                match request.request.send(runner, &channel) {
                    Ok(true) => {
                        let confirmation = if request.request.needs_pty() {
                            Confirmation::Pty
                        } else {
                            Confirmation::Command
                        };
                        Self::Negotiating(channel, request, confirmation)
                    }
                    Ok(false) if authorized_now => {
                        let error = io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "SSH could not dispatch the command immediately after authorization",
                        );
                        return reject(channel, request, error.into());
                    }
                    Ok(false) => return (Some(Self::Opening(channel, request)), false),
                    Err(error) => return reject(channel, request, error),
                }
            }
            Self::Negotiating(channel, request, confirmation) => {
                if request.expired() {
                    return reject(channel, request, timeout());
                }
                match runner.client_request_result(&channel) {
                    Ok(Some(true)) if confirmation == Confirmation::Pty => {
                        if let Some(error) = premature_data(runner, &channel) {
                            return reject(channel, request, error);
                        }
                        match request.request.send_exec_after_pty(runner, &channel) {
                            Ok(true) => Self::Negotiating(channel, request, Confirmation::Command),
                            Ok(false) => {
                                return (
                                    Some(Self::Negotiating(channel, request, confirmation)),
                                    false,
                                );
                            }
                            Err(error) => return reject(channel, request, error.into()),
                        }
                    }
                    Ok(Some(true)) => {
                        let (pump, lease) = lease_pair(channel, owner, &request);
                        request.complete(Ok(lease));
                        Self::Active(pump)
                    }
                    Ok(Some(false)) => {
                        let name = if confirmation == Confirmation::Pty {
                            "pty-req"
                        } else {
                            request.request.name()
                        };
                        return reject(
                            channel,
                            request,
                            ConnectError::RequestRejected { request: name },
                        );
                    }
                    Ok(None) => {
                        if let Some(error) = premature_data(runner, &channel) {
                            return reject(channel, request, error);
                        }
                        return (
                            Some(Self::Negotiating(channel, request, confirmation)),
                            false,
                        );
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

fn premature_data(runner: &Runner<'_, Client>, channel: &ChanHandle) -> Option<ConnectError> {
    if runner.is_channel_closed(channel) || runner.is_channel_eof(channel) {
        return Some(closed().into());
    }
    runner
        .read_channel_ready()
        .filter(|(ready, _, _)| *ready == channel.num())
        .map(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH peer sent data before confirming the channel request",
            )
            .into()
        })
}

fn reject(channel: ChanHandle, request: Open, error: ConnectError) -> (Option<State>, bool) {
    request.complete(Err(error));
    (Some(State::Closing(channel)), true)
}

fn lease_pair(
    channel: ChanHandle,
    owner: &Arc<Shared>,
    request: &Open,
) -> (ChannelPump, SessionLease) {
    let shared = Shared::new(Arc::clone(&owner.wake));
    let (input, commands) = mpsc::sync_channel(QUEUE_DEPTH);
    let mode = request.request.channel_mode();
    let sensitive = matches!(
        mode,
        ChannelMode::SensitiveExec | ChannelMode::SensitiveExecPty
    );
    let (stdout, output) = output::queue_with_wipe(sensitive);
    let (stderr, errors) = if matches!(mode, ChannelMode::Exec | ChannelMode::SensitiveExec) {
        let (stderr, errors) = output::queue_with_wipe(sensitive);
        (Some(stderr), Some(SshReader::new(&shared, errors)))
    } else {
        (None, None)
    };
    if let Some(status) = request.status {
        shared.exit_code.store(u64::from(status), Ordering::Release);
    }
    let stream = SessionLease {
        abort: SshAbort(Arc::clone(&shared)),
        mode,
        stdout: Some(SshReader::new(&shared, output)),
        stderr: errors,
        writer: Some(SshWriter {
            shared: Arc::clone(&shared),
            input,
            input_closed: false,
        }),
    };
    let outputs = Outputs {
        mode,
        stdout: Some(stdout),
        stderr,
    };
    (ChannelPump::new(channel, shared, commands, outputs), stream)
}

fn timeout() -> ConnectError {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "SSH side channel open timed out or was cancelled",
    )
    .into()
}

fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "this SSH connection has reached its channel limit for this request",
    )
}
