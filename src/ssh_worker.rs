//! The SSH transport can run in a dedicated child process.
use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

#[cfg(target_os = "macos")]
use kortty_pty::{MacWorker, MacWorkerProcess};
use kortty_pty::{ParentExit, ParentGuard, WorkerSandbox};
#[cfg(windows)]
use kortty_pty::{WindowsWorker, WindowsWorkerProcess};
use kortty_ssh::{
    ConnectError, Endpoint, KeyboardChallenge, KeyboardResponse, TrustStore, WindowSize,
};

use crate::{session_security::IsolationLevel, ssh_connection, ssh_dialog::Request};

#[cfg(test)]
mod smoke;

mod client;
mod login;
pub(crate) mod monitor;
#[cfg(any(windows, target_os = "macos"))]
mod relay;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, unix))]
pub(crate) use tests::{
    Fixture as OpenSshFixture, dial as dial_test_executable, executable as test_executable,
};

pub(crate) use client::{Channel, Opener, RemoteForward, StreamAbort};
use kortty::ssh_runtime::{ARGUMENT, closed, lock, mux, wire};
pub(crate) use mux::{Reader, Writer};
use wire::Frame;

const NO_EXIT: u64 = u64::MAX;
#[cfg(windows)]
const SANDBOX_BACKEND: &str = "LPAC + Job";
#[cfg(target_os = "macos")]
const SANDBOX_BACKEND: &str = "App Sandbox";
#[cfg(not(any(windows, target_os = "macos")))]
const SANDBOX_BACKEND: &str = "landlock + seccomp";

pub(crate) fn sandbox_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        #[cfg(target_os = "macos")]
        return std::env::current_exe()
            .and_then(|path| MacWorker::check_available(&path))
            .is_ok();
        #[cfg(not(target_os = "macos"))]
        WorkerSandbox::check_available().is_ok()
    })
}

type PromptHandler<'a> = dyn FnMut(&KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> + 'a;

#[derive(Clone, Default)]
pub(crate) struct DialCancellation(Arc<Mutex<Pending>>);

impl DialCancellation {
    pub(crate) fn for_session(display_name: String) -> Self {
        Self(Arc::new(Mutex::new(Pending {
            display_name: Some(display_name),
            ..Pending::default()
        })))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        lock(&self.0).cancelled
    }

    pub(crate) fn worker_exited(&self) -> bool {
        let active = lock(&self.0).active.clone();
        active.is_some_and(|abort| {
            // A keyboard-interactive worker can be waiting on its control
            // pipe even after the peer has disconnected from the relay.
            #[cfg(any(windows, target_os = "macos"))]
            if lock(&abort.0.relay)
                .as_ref()
                .is_some_and(relay::Relay::is_finished)
            {
                return true;
            }
            abort.is_closed()
        })
    }
}

#[derive(Default)]
struct Pending {
    cancelled: bool,
    active: Option<Abort>,
    display_name: Option<String>,
}

/// Only the login dialog owns this guard. A completed dial disarms the child
/// before sending its result back to the desktop.
pub(crate) struct CancelOnDrop(pub DialCancellation);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let active = {
            let mut pending = lock(&self.0.0);
            pending.cancelled = true;
            pending.active.take()
        };
        if let Some(active) = active {
            active.abort();
        }
    }
}

#[derive(Clone)]
pub(crate) struct Abort(Arc<Process>);

struct Process {
    child: Mutex<Option<WorkerChild>>,
    monitor: Option<monitor::Registration>,
    _lifetime: Option<ParentGuard>,
    #[cfg(any(windows, target_os = "macos"))]
    relay: Mutex<Option<relay::Relay>>,
    closed: AtomicBool,
    exit_code: AtomicU64,
}

impl Abort {
    pub(crate) fn abort(&self) {
        self.0.closed.store(true, Ordering::Release);
        #[cfg(any(windows, target_os = "macos"))]
        self.0.stop_relay();
        if let Some(child) = lock(&self.0.child).as_mut() {
            let _ = child.kill();
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        if self.0.closed.load(Ordering::Acquire) {
            return true;
        }
        let exited = lock(&self.0.child)
            .as_mut()
            .is_none_or(|child| child.exited().unwrap_or(false));
        if exited {
            self.0.closed.store(true, Ordering::Release);
        }
        exited
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.reap();
    }
}

impl Process {
    fn reap(&self) -> io::Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        let relay = lock(&self.relay).take();
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(relay) = &relay {
            relay.stop();
        }
        let child = lock(&self.child).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            child.wait()?;
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn stop_relay(&self) {
        if let Some(relay) = lock(&self.relay).as_ref() {
            relay.stop();
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn install_relay(&self, relay: relay::Relay) -> io::Result<()> {
        let mut active = lock(&self.relay);
        if self.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        *active = Some(relay);
        Ok(())
    }
}

enum WorkerChild {
    Standard(Child),
    #[cfg(windows)]
    Confined(WindowsWorkerProcess),
    #[cfg(target_os = "macos")]
    Confined(MacWorkerProcess),
}

impl WorkerChild {
    fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            #[cfg(any(windows, target_os = "macos"))]
            Self::Confined(child) => child.kill(),
        }
    }

    fn wait(&mut self) -> io::Result<()> {
        match self {
            Self::Standard(child) => child.wait().map(|_| ()),
            #[cfg(windows)]
            Self::Confined(child) => child.wait().map(|_| ()),
            #[cfg(target_os = "macos")]
            Self::Confined(child) => child.wait(),
        }
    }

    fn exited(&mut self) -> io::Result<bool> {
        match self {
            Self::Standard(child) => child.try_wait().map(|status| status.is_some()),
            #[cfg(any(windows, target_os = "macos"))]
            Self::Confined(child) => child.exited(),
        }
    }

    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            #[cfg(any(windows, target_os = "macos"))]
            Self::Confined(child) => child.id(),
        }
    }

    fn usage(&mut self) -> kortty_pty::ProcessUsage {
        match self {
            Self::Standard(child) => kortty_pty::child_usage(child),
            #[cfg(any(windows, target_os = "macos"))]
            Self::Confined(child) => child.usage(),
        }
    }
}

type WorkerInput = Box<dyn Write + Send>;
type WorkerOutput = BufReader<Box<dyn Read + Send>>;

struct WorkerSpawn {
    child: WorkerChild,
    lifetime: Option<ParentGuard>,
    input: WorkerInput,
    output: WorkerOutput,
    #[cfg(any(windows, target_os = "macos"))]
    relay_pipe: Option<relay::Pipe>,
}

impl WorkerSpawn {
    fn standard(executable: &Path, transport: Option<TcpStream>) -> io::Result<Self> {
        let mut command = Command::new(executable);
        command
            .arg(ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let lifetime = ParentExit::attach(&mut command)?;
        if let Some(tcp) = transport {
            let descriptor = WorkerSandbox::attach(&mut command, tcp)?;
            command.arg(descriptor.to_string());
        }
        let mut child = command.spawn()?;
        drop(command);
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("SSH worker control pipes missing"));
        };
        Ok(Self {
            child: WorkerChild::Standard(child),
            lifetime: Some(lifetime),
            input: Box::new(input),
            output: BufReader::new(Box::new(output)),
            #[cfg(any(windows, target_os = "macos"))]
            relay_pipe: None,
        })
    }

    #[cfg(windows)]
    fn confined(executable: &Path) -> io::Result<Self> {
        if !executable.is_absolute() || !executable.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid desktop executable for SSH sandbox",
            ));
        }
        let helper = executable.with_file_name("kortty-ssh-worker.exe");
        if !helper.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "bundled SSH sandbox worker is missing",
            ));
        }
        let worker = WindowsWorker::prepare(&helper, ARGUMENT)?;
        Ok(Self {
            child: WorkerChild::Confined(worker.process),
            lifetime: None,
            input: Box::new(worker.input),
            output: BufReader::new(Box::new(worker.output)),
            relay_pipe: Some(worker.ciphertext),
        })
    }

    #[cfg(target_os = "macos")]
    fn confined(executable: &Path) -> io::Result<Self> {
        let worker = MacWorker::prepare(executable, ARGUMENT, &crate::profile::root()?)?;
        Ok(Self {
            child: WorkerChild::Confined(worker.process),
            lifetime: None,
            input: Box::new(worker.input),
            output: BufReader::new(Box::new(worker.output)),
            relay_pipe: Some(worker.ciphertext),
        })
    }
}

pub(crate) struct Session {
    abort: Abort,
    mux: mux::Mux,
    opener: Opener,
    terminal: bool,
    sandbox: bool,
    reader: Option<Reader>,
    writer: Option<Writer>,
    sender: Option<JoinHandle<()>>,
    receiver: Option<JoinHandle<()>>,
}

impl Session {
    pub(crate) fn dial(
        request: Request,
        size: WindowSize,
        sftp: bool,
        cancellation: &DialCancellation,
        prompts: &mut PromptHandler<'_>,
    ) -> Result<Self, ConnectError> {
        Self::dial_executable(
            request,
            size,
            sftp,
            cancellation,
            &std::env::current_exe()?,
            &ssh_connection::trust_store()?,
            Some(prompts),
        )
    }

    fn dial_executable(
        request: Request,
        size: WindowSize,
        sftp: bool,
        cancellation: &DialCancellation,
        executable: &Path,
        trust: &TrustStore,
        prompts: Option<&mut PromptHandler<'_>>,
    ) -> Result<Self, ConnectError> {
        let endpoints = std::iter::once(&request.target)
            .chain(request.jump.iter())
            .map(|login| Endpoint::new(&login.endpoint.host, login.endpoint.port))
            .collect::<io::Result<Vec<_>>>()?;
        let pins = trust.selected_pins(&endpoints)?;
        let sandbox = request.isolation == IsolationLevel::Sandbox;
        if sandbox {
            #[cfg(target_os = "macos")]
            MacWorker::check_available(executable)?;
            #[cfg(not(target_os = "macos"))]
            WorkerSandbox::check_available()?;
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let transport = if sandbox {
            let first = endpoints.last().ok_or_else(wire::invalid)?;
            Some(ssh_connection::connect_transport_with_options(
                first,
                request.options,
                || cancellation.is_cancelled(),
            )?)
        } else {
            None
        };
        let cancelled = || io::Error::new(io::ErrorKind::Interrupted, "SSH worker start cancelled");
        if cancellation.is_cancelled() {
            return Err(cancelled().into());
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let spawned = WorkerSpawn::standard(executable, transport)?;
        #[cfg(any(windows, target_os = "macos"))]
        let spawned = if sandbox {
            WorkerSpawn::confined(executable)?
        } else {
            WorkerSpawn::standard(executable, None)?
        };
        let WorkerSpawn {
            child,
            lifetime,
            input,
            mut output,
            #[cfg(any(windows, target_os = "macos"))]
            relay_pipe,
        } = spawned;
        let display_name = lock(&cancellation.0).display_name.clone();
        let monitor = display_name.map(|name| monitor::Registration::new(name, child.id()));
        let abort = Abort(Arc::new(Process {
            child: Mutex::new(Some(child)),
            monitor,
            _lifetime: lifetime,
            #[cfg(any(windows, target_os = "macos"))]
            relay: Mutex::new(None),
            closed: AtomicBool::new(false),
            exit_code: AtomicU64::new(NO_EXIT),
        }));
        let mut pending = lock(&cancellation.0);
        if pending.cancelled {
            drop(pending);
            abort.abort();
            return Err(cancelled().into());
        }
        pending.active = Some(abort.clone());
        drop(pending);
        monitor::register(&abort);

        #[cfg(any(windows, target_os = "macos"))]
        let mut input = input;
        let result = (|| {
            if sandbox {
                sandbox_ready(&mut output, &abort)?;
                #[cfg(target_os = "macos")]
                match lock(&abort.0.child).as_mut() {
                    Some(WorkerChild::Confined(child)) => child.verify()?,
                    _ => return Err(wire::invalid().into()),
                }
                #[cfg(any(windows, target_os = "macos"))]
                {
                    let pipe = relay_pipe.ok_or_else(wire::invalid)?;
                    let first = endpoints.last().ok_or_else(wire::invalid)?;
                    let tcp = ssh_connection::connect_transport_with_options(
                        first,
                        request.options,
                        || cancellation.is_cancelled(),
                    )?;
                    let origin = tcp.local_addr()?;
                    abort.0.install_relay(relay::Relay::start(tcp, pipe)?)?;
                    wire::send_relay_origin(&mut input, origin)?;
                }
                if let Some(registration) = &abort.0.monitor {
                    registration.sandbox_verified();
                }
            }
            if cancellation.is_cancelled() {
                return Err(cancelled().into());
            }
            Self::connect_child((input, output), abort, request, &pins, size, sftp, prompts)
        })();
        let (active, was_cancelled) = {
            let mut pending = lock(&cancellation.0);
            (pending.active.take(), pending.cancelled)
        };
        drop(active);
        if was_cancelled {
            return Err(cancelled().into());
        }
        result
    }

    fn connect_child(
        (mut input, mut output): (WorkerInput, WorkerOutput),
        abort: Abort,
        request: Request,
        pins: &[(Endpoint, kortty_ssh::HostKey)],
        size: WindowSize,
        sftp: bool,
        mut prompts: Option<&mut PromptHandler<'_>>,
    ) -> Result<Self, ConnectError> {
        let sandbox = request.isolation == IsolationLevel::Sandbox;
        let route = std::iter::once(&request.target)
            .chain(request.jump.iter())
            .map(|login| {
                Ok((
                    Endpoint::new(&login.endpoint.host, login.endpoint.port)?,
                    login.endpoint.username.clone(),
                    login.keyboard_options().enabled(),
                ))
            })
            .collect::<io::Result<Vec<_>>>()?;
        login::send(&mut input, request, pins, size, sftp)?;
        let mut rounds = 0;
        loop {
            match wire::recv(&mut output)? {
                wire::Message::Ready => break,
                wire::Message::SandboxReady => return Err(wire::invalid().into()),
                wire::Message::Challenge(challenge) => {
                    let approved = route.iter().any(|(endpoint, username, enabled)| {
                        *enabled
                            && *endpoint == challenge.endpoint
                            && *username == challenge.username
                    });
                    if !approved
                        || !pins
                            .iter()
                            .any(|(endpoint, _)| *endpoint == challenge.endpoint)
                        || rounds == 8 * route.len()
                    {
                        return Err(wire::invalid().into());
                    }
                    rounds += 1;
                    let handler = prompts.as_deref_mut().ok_or_else(wire::invalid)?;
                    let answers = handler(&challenge)?;
                    wire::send_answers(&mut input, challenge.prompts.len(), &answers)?;
                }
                wire::Message::Failed(error) => {
                    if let ConnectError::UnknownHostKey { endpoint, .. }
                    | ConnectError::ChangedHostKey { endpoint, .. } = &error
                        && !route.iter().any(|(approved, _, _)| approved == endpoint)
                    {
                        return Err(wire::invalid().into());
                    }
                    return Err(error);
                }
            }
        }
        let mux = mux::Mux::default();
        let primary = mux.add(0)?;
        let opener = Opener::new(mux.clone(), abort.clone());
        let out_mux = mux.clone();
        let out_opener = opener.clone();
        let out_abort = abort.clone();
        let sender = thread::Builder::new()
            .name("kortty-ssh-worker-write".into())
            .spawn(move || {
                while let Some(frame) = out_mux.next() {
                    if frame.send(&mut input).is_err() {
                        out_opener.stop();
                        out_abort.abort();
                        break;
                    }
                }
            })?;
        let in_opener = opener.clone();
        let in_abort = abort.clone();
        let receiver = thread::Builder::new()
            .name("kortty-ssh-worker-read".into())
            .spawn(move || {
                loop {
                    match Frame::recv(&mut output) {
                        Ok(frame) => {
                            if in_opener.receive(frame).is_err() {
                                in_abort.abort();
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        _ => {
                            in_abort.abort();
                            break;
                        }
                    }
                }
                in_opener.stop();
                in_abort.0.closed.store(true, Ordering::Release);
                #[cfg(windows)]
                in_abort.0.stop_relay();
            });
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(error) => {
                opener.stop();
                abort.abort();
                let _ = sender.join();
                return Err(error.into());
            }
        };
        Ok(Self {
            sandbox,
            reader: Some(primary.reader()),
            writer: Some(primary.writer()),
            abort,
            mux,
            opener,
            terminal: !sftp,
            sender: Some(sender),
            receiver: Some(receiver),
        })
    }

    pub(crate) fn sandbox_backend(&self) -> Option<&'static str> {
        self.sandbox.then_some(SANDBOX_BACKEND)
    }

    pub(crate) fn take_reader(&mut self) -> Option<Reader> {
        self.reader.take()
    }

    pub(crate) fn take_writer(&mut self) -> Option<Writer> {
        self.writer.take()
    }

    pub(crate) fn opener(&self) -> Option<Opener> {
        self.terminal.then(|| self.opener.clone())
    }

    pub(crate) fn resize(&self, size: WindowSize) -> io::Result<()> {
        self.mux.resize(size)
    }

    pub(crate) fn exit_code(&self) -> Option<u32> {
        self.abort
            .0
            .exit_code
            .load(Ordering::Acquire)
            .try_into()
            .ok()
    }

    pub(crate) fn abort_handle(&self) -> Abort {
        self.abort.clone()
    }

    pub(crate) fn close(&mut self) -> io::Result<()> {
        self.opener.stop();
        let result = self.abort.0.reap();
        self.abort.0.closed.store(true, Ordering::Release);
        // Kill and reap before joining, releasing any blocked pipe read/write.
        for worker in [self.sender.take(), self.receiver.take()]
            .into_iter()
            .flatten()
        {
            let _ = worker.join();
        }
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn sandbox_ready(output: &mut WorkerOutput, _abort: &Abort) -> Result<(), ConnectError> {
    #[cfg(any(windows, target_os = "macos"))]
    let response = {
        use std::sync::mpsc;
        use std::time::Duration;

        let (ready, deadline) = mpsc::channel();
        let abort = _abort.clone();
        let timer = thread::Builder::new()
            .name("kortty-ssh-sandbox-startup".into())
            .spawn(move || {
                let expired = deadline.recv_timeout(Duration::from_secs(5))
                    == Err(mpsc::RecvTimeoutError::Timeout);
                if expired {
                    abort.abort();
                }
                expired
            })?;
        let response = wire::recv(output);
        let _ = ready.send(());
        if timer.join().unwrap_or(true) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH sandbox did not confirm startup",
            )
            .into());
        }
        response?
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let response = wire::recv(output)?;

    match response {
        wire::Message::SandboxReady => Ok(()),
        wire::Message::Failed(error) => Err(error),
        _ => Err(wire::invalid().into()),
    }
}
