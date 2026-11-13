use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kortty_ssh::{
    ConnectError, DirectChannel, DirectOpener, ExecOpener, RemoteOpener, SftpLease, SftpOpener,
    SshAbort, SshReader, SshSession, SshWriter,
};

use super::mux::{Flow, Mux, PasswordSide};
use super::wire::{Frame, Open};
use super::{closed, lock};

#[path = "server/exec.rs"]
mod exec;
#[path = "server/file_exec.rs"]
mod file_exec;

const ACCEPT_POLL: Duration = Duration::from_millis(100);
// Opening a stream and starting its relay can briefly occupy two tasks.
const MAX_TASKS: usize =
    2 * kortty_ssh::MAX_SESSION_CHANNELS + kortty_ssh::MAX_REMOTE_FORWARD_LISTENERS;

struct Server {
    mux: Mux,
    ssh: Mutex<SshSession>,
    active: Mutex<BTreeMap<u32, SshAbort>>,
    sftp: Option<SftpOpener>,
    exec: Option<ExecOpener>,
    direct: DirectOpener,
    remote: RemoteOpener,
    next_id: AtomicU32,
    tasks: AtomicUsize,
    terminal: bool,
}

pub(super) fn run(
    mut ssh: SshSession,
    input: impl Read + Send + 'static,
    mut output: impl Write,
    terminal: bool,
) -> io::Result<()> {
    let streams = Streams {
        reader: ssh.take_reader().ok_or_else(closed)?,
        writer: ssh.take_writer().ok_or_else(closed)?,
        abort: ssh.abort_handle(),
    };
    let mux = Mux::default();
    let primary = mux.add(0)?;
    let server = Arc::new(Server {
        mux,
        sftp: ssh.sftp_opener(),
        exec: ssh.exec_opener(),
        direct: ssh.direct_opener(),
        remote: ssh.remote_opener(),
        ssh: Mutex::new(ssh),
        active: Mutex::default(),
        next_id: AtomicU32::new(2),
        tasks: AtomicUsize::new(0),
        terminal,
    });
    server.bridge(primary, streams, None)?;
    let inbound = Arc::clone(&server);
    thread::Builder::new()
        .name("kortty-ssh-worker-control".into())
        .spawn(move || inbound.receive(input))?;
    let result = (|| {
        while let Some(frame) = server.mux.next() {
            let finished = matches!(
                frame,
                Frame::End(0, _) | Frame::Close(0, _) | Frame::Finished(0, _)
            );
            frame.send(&mut output)?;
            if finished {
                break;
            }
        }
        Ok(())
    })();
    server.mux.stop();
    lock(&server.ssh).abort_handle().abort();
    result
}

impl Server {
    fn receive(self: Arc<Self>, mut input: impl Read) {
        while let Ok(frame) = Frame::recv(&mut input) {
            let result = match frame {
                Frame::Resize(size) if self.terminal => {
                    lock(&self.ssh).resize(size).or_else(|error| {
                        if error.kind() == io::ErrorKind::BrokenPipe {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    })
                }
                Frame::Open(id, request) if id % 2 == 1 => self.open(id, request),
                Frame::Close(id, error) => {
                    let result = self.mux.receive(Frame::Close(id, error));
                    if let Some(abort) = lock(&self.active).remove(&id) {
                        abort.abort();
                    }
                    result
                }
                other => self.mux.receive(other),
            };
            if result.is_err() {
                break;
            }
        }
        self.mux.stop();
        lock(&self.ssh).abort_handle().abort();
    }

    fn open(self: &Arc<Self>, id: u32, request: Open) -> io::Result<()> {
        let flow = match &request {
            Open::PasswordExec { .. } => self.mux.add_password(id, PasswordSide::Worker),
            Open::PasswordFileExec { .. } => self.mux.add_password_file(id, PasswordSide::Worker),
            _ => self.mux.add(id),
        };
        let flow = match flow {
            Ok(flow) => flow,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return self.mux.control(Frame::Close(id, Some(error.to_string())));
            }
            Err(error) => return Err(error),
        };
        if !self.terminal
            && !matches!(
                request,
                Open::Sftp
                    | Open::FileExec
                    | Open::PasswordFileExec { .. }
                    | Open::AccountLookup { .. }
                    | Open::ArchiveExtract
                    | Open::DraftReplace
                    | Open::FolderUsage(..)
                    | Open::AccountNames(..)
            )
        {
            flow.close(Some(
                "This channel is unavailable on a standalone SFTP connection".into(),
            ));
            return Ok(());
        }
        let active = flow.clone();
        if let Err(error) = self.spawn("kortty-ssh-worker-open", move |server| {
            let result = match request {
                Open::Sftp => server
                    .sftp
                    .as_ref()
                    .ok_or_else(|| ConnectError::from(closed()))
                    .and_then(|opener| opener.open_cancellable(|| !active.is_open()))
                    .and_then(|channel| server.opened(&active, OwnedChannel::Sftp(channel))),
                Open::Direct(endpoint, origin) => server
                    .direct
                    .open_cancellable(endpoint, origin, || !active.is_open())
                    .and_then(|channel| server.opened(&active, OwnedChannel::Direct(channel))),
                Open::Listen(address) => server.listen(&active, address),
                Open::Exec(command) => server.exec(&active, &command),
                Open::PasswordExec { command, nonce } => {
                    server.exec_password(&active, &command, nonce)
                }
                Open::FileExec => server.exec_file(&active),
                Open::PasswordFileExec { nonce } => server.exec_password_file(&active, nonce),
                Open::AccountLookup { kind, name } => {
                    server.exec_account_lookup(&active, kind, &name)
                }
                Open::ArchiveExtract => server.exec_archive_extract(&active),
                Open::DraftReplace => server.exec_draft_replace(&active),
                Open::FolderUsage(paths) => server.exec_folder_usage(&active, &paths),
                Open::AccountNames(query) => server.exec_account_names(&active, &query),
            };
            if let Err(error) = result {
                active.close(Some(error.to_string()));
            }
        }) {
            flow.close(Some(error.to_string()));
        }
        Ok(())
    }

    fn opened(
        self: &Arc<Self>,
        flow: &Flow,
        mut channel: OwnedChannel,
    ) -> Result<(), ConnectError> {
        if !flow.is_open() {
            return Ok(());
        }
        let streams = channel.streams()?;
        self.mux.control(Frame::Opened(flow.id(), None))?;
        self.bridge(flow.clone(), streams, Some(channel))?;
        Ok(())
    }

    fn listen(self: &Arc<Self>, flow: &Flow, address: SocketAddr) -> Result<(), ConnectError> {
        let mut listener = self.remote.bind_cancellable(address, || !flow.is_open())?;
        if flow.is_open() {
            self.mux
                .control(Frame::Opened(flow.id(), Some(listener.remote_addr())))?;
        }
        let mut failure = None;
        while flow.is_open() {
            match listener.accept_timeout(ACCEPT_POLL) {
                Ok(Some(connection)) => {
                    if let Err(error) = self.accept(flow, connection.origin, connection.channel) {
                        failure = Some(error.to_string());
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        let closed = listener.close().err().map(|error| error.to_string());
        let failure = failure.or(closed);
        if flow.is_open() {
            flow.close(failure);
        } else if self.mux.running() {
            self.mux.control(Frame::Closed(flow.id(), failure))?;
        }
        Ok(())
    }

    fn accept(
        self: &Arc<Self>,
        listener: &Flow,
        origin: SocketAddr,
        channel: DirectChannel,
    ) -> io::Result<()> {
        let id = self
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(2))
            .map_err(|_| io::Error::other("SSH worker stream IDs exhausted"))?;
        let flow = match self.mux.add(id) {
            Ok(flow) => flow,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut channel = OwnedChannel::Direct(channel);
        let result = (|| {
            let streams = channel.streams()?;
            self.mux
                .control(Frame::Accepted(listener.id(), id, origin))?;
            self.bridge(flow.clone(), streams, Some(channel))
        })();
        if let Err(error) = result {
            flow.close(Some(error.to_string()));
        }
        Ok(())
    }

    fn bridge(
        self: &Arc<Self>,
        flow: Flow,
        streams: Streams,
        channel: Option<OwnedChannel>,
    ) -> io::Result<()> {
        let id = flow.id();
        {
            let mut active = lock(&self.active);
            if !flow.is_open() {
                return Ok(());
            }
            active.insert(id, streams.abort.clone());
        }
        let result = self.spawn("kortty-ssh-worker-stream", move |server| {
            let _channel = channel;
            let result = server.copy(&flow, streams);
            if let Err(error) = result {
                flow.finish(Some(error.to_string()));
            }
            lock(&server.active).remove(&id);
        });
        if let Err(error) = result {
            if let Some(abort) = lock(&self.active).remove(&id) {
                abort.abort();
            }
            return Err(error);
        }
        Ok(())
    }

    fn spawn(
        self: &Arc<Self>,
        name: &str,
        task: impl FnOnce(Arc<Self>) + Send + 'static,
    ) -> io::Result<()> {
        self.tasks
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_TASKS).then_some(count + 1)
            })
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "SSH worker task limit"))?;
        let slot = TaskSlot(Arc::clone(self));
        thread::Builder::new().name(name.into()).spawn(move || {
            task(Arc::clone(&slot.0));
            drop(slot);
        })?;
        Ok(())
    }

    fn copy(&self, flow: &Flow, streams: Streams) -> io::Result<()> {
        let Streams {
            mut reader,
            mut writer,
            abort,
        } = streams;
        let mut local_read = flow.reader();
        let mut local_write = flow.writer();
        thread::scope(|scope| {
            let upstream = thread::Builder::new()
                .name("kortty-ssh-worker-upstream".into())
                .spawn_scoped(scope, || {
                    let result = io::copy(&mut local_read, &mut writer)
                        .and_then(|_| writer.close_input())
                        .and_then(|()| flow.acknowledge_end());
                    if let Err(error) = &result
                        && !abort.is_closed()
                    {
                        flow.finish(Some(error.to_string()));
                        abort.abort();
                    }
                    result
                })?;
            let downstream = io::copy(&mut reader, &mut local_write).and_then(|_| {
                let code = (flow.id() == 0)
                    .then(|| lock(&self.ssh).exit_code())
                    .flatten();
                local_write.end(code)
            });
            if let Err(error) = &downstream {
                flow.finish(Some(error.to_string()));
                abort.abort();
            } else if flow.id() != 0 {
                // SSH EOF is directional. Full close must also wake an idle
                // local sender without dropping already queued output.
                while !upstream.is_finished() && !abort.is_closed() && flow.is_open() {
                    thread::sleep(ACCEPT_POLL);
                }
                if abort.is_closed() {
                    flow.finish(None);
                }
            }
            let upstream = upstream
                .join()
                .map_err(|_| io::Error::other("SSH worker upstream panicked"))?;
            downstream.and(if abort.is_closed() { Ok(()) } else { upstream })
        })
    }
}

struct TaskSlot(Arc<Server>);

impl Drop for TaskSlot {
    fn drop(&mut self) {
        self.0.tasks.fetch_sub(1, Ordering::Release);
    }
}

enum OwnedChannel {
    Direct(DirectChannel),
    Sftp(SftpLease),
}

impl OwnedChannel {
    fn streams(&mut self) -> io::Result<Streams> {
        let (reader, writer, abort) = match self {
            Self::Direct(channel) => (
                channel.take_reader(),
                channel.take_writer(),
                channel.abort_handle(),
            ),
            Self::Sftp(channel) => (
                channel.take_reader(),
                channel.take_writer(),
                channel.abort_handle(),
            ),
        };
        Ok(Streams {
            reader: reader.ok_or_else(closed)?,
            writer: writer.ok_or_else(closed)?,
            abort,
        })
    }
}

struct Streams {
    reader: SshReader,
    writer: SshWriter,
    abort: SshAbort,
}
