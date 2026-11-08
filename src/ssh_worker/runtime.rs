//! Shared worker protocol and child process entry point.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader};
use std::net::TcpStream;
use std::sync::{Mutex, MutexGuard};
use std::thread;

#[cfg(target_os = "macos")]
use kortty_pty::MacWorker;
use kortty_pty::ParentExit;
#[cfg(windows)]
use kortty_pty::WindowsWorker;
#[cfg(not(any(windows, target_os = "macos")))]
use kortty_pty::WorkerSandbox;
#[cfg(any(windows, target_os = "macos"))]
use kortty_ssh::PipeTransport;
use kortty_ssh::{ConnectError, ConnectOptions, SshSession, SshTarget, StartupChannel, TrustStore};

#[path = "connect.rs"]
mod connect;
pub use connect::{connect_transport, connect_transport_with_options};

#[path = "mux.rs"]
pub mod mux;
#[path = "server.rs"]
mod server;
#[path = "wire.rs"]
pub mod wire;

pub const ARGUMENT: &str = "--internal-ssh-worker";

pub fn descriptor(mut args: impl Iterator<Item = OsString>) -> io::Result<Option<u32>> {
    let Some(value) = args.next() else {
        return Ok(None);
    };
    if args.next().is_some() {
        return Err(wire::invalid());
    }
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .map(Some)
        .ok_or_else(wire::invalid)
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "SSH worker closed")
}

pub fn dial(
    (target, jump): (SshTarget, Option<SshTarget>),
    trust: TrustStore,
    channel: StartupChannel<'_>,
    options: ConnectOptions,
    tcp: Option<TcpStream>,
    cancelled: impl Fn() -> bool,
) -> Result<SshSession, ConnectError> {
    let tcp = match tcp {
        Some(tcp) => tcp,
        None => connect_transport_with_options(
            &jump.as_ref().unwrap_or(&target).endpoint,
            options,
            &cancelled,
        )?,
    };
    SshSession::connect_preconnected_with_options(
        tcp,
        (target, jump.into_iter().collect()),
        trust,
        channel,
        options,
        cancelled,
    )
}

/// Worker entry point. No desktop state, listeners, credentials in argv or profile I/O.
fn input_pipe() -> io::Result<File> {
    // Avoid keeping credentials in the global stdin buffer. The login and
    // authentication phases each have exclusive access to the inherited pipe.
    #[cfg(unix)]
    let handle = {
        use std::os::fd::AsFd as _;
        io::stdin().as_fd().try_clone_to_owned()?
    };
    #[cfg(windows)]
    let handle = {
        use std::os::windows::io::AsHandle as _;
        io::stdin().as_handle().try_clone_to_owned()?
    };
    Ok(File::from(handle))
}

pub fn serve(descriptor: Option<u32>) -> io::Result<()> {
    let mut output = io::stdout();
    #[cfg(windows)]
    let enter = WindowsWorker::enter;
    #[cfg(target_os = "macos")]
    let enter = MacWorker::enter;
    #[cfg(not(any(windows, target_os = "macos")))]
    let enter = WorkerSandbox::enter;
    let transport = match descriptor.map(enter).transpose() {
        Ok(transport) => transport,
        Err(error) => return wire::send(&mut output, wire::Message::Failed(error.into())),
    };
    let parent = match ParentExit::watch() {
        Ok(parent) => parent,
        Err(error) => return wire::send(&mut output, wire::Message::Failed(error.into())),
    };
    thread::Builder::new()
        .name("kortty-ssh-parent".into())
        .spawn(move || {
            let result = parent.wait();
            std::process::exit(i32::from(result.is_err()));
        })?;
    let mut pipe = input_pipe()?;
    // The authentication callback can run on another thread before this
    // process starts streaming. Keep both standard handles unlocked here.
    if transport.is_some() {
        wire::send(&mut output, wire::Message::SandboxReady)?;
    }
    #[cfg(any(windows, target_os = "macos"))]
    let transport = transport
        .map(|stream| {
            wire::recv_relay_origin(&mut pipe).map(|origin| PipeTransport::new(stream, origin))
        })
        .transpose()?;
    let wire::Login {
        target,
        jump,
        trust,
        term,
        size,
        sftp,
        options,
    } = wire::recv_login(&mut pipe)?;
    let channel = if sftp {
        StartupChannel::Subsystem("sftp")
    } else {
        StartupChannel::Terminal { term: &term, size }
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let connection = dial((target, jump), trust, channel, options, transport, || false);
    #[cfg(any(windows, target_os = "macos"))]
    let connection = if let Some(transport) = transport {
        SshSession::connect_pipe_with_options(
            transport,
            (target, jump.into_iter().collect()),
            trust,
            channel,
            options,
            || false,
        )
    } else {
        dial((target, jump), trust, channel, options, None, || false)
    };
    let ssh = match connection {
        Ok(ssh) => ssh,
        Err(error) => return wire::send(&mut output, wire::Message::Failed(error)),
    };
    wire::send(&mut output, wire::Message::Ready)?;
    server::run(ssh, BufReader::new(pipe), output.lock(), !sftp)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    use kortty_ssh::{Authentication, Endpoint, WindowSize};

    use super::*;

    #[test]
    fn direct_ssh_handshake_uses_the_requested_timeout_without_a_second_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = Endpoint::new("127.0.0.1", listener.local_addr().unwrap().port()).unwrap();
        let incoming = listener.try_clone().unwrap();
        // Test double: a real TCP peer that receives the SSH identification and
        // withholds its own identification to exercise startup timeout.
        let peer = thread::spawn(move || {
            let (mut socket, _) = incoming.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut seen = Vec::new();
            socket.read_to_end(&mut seen).unwrap();
            seen
        });
        let target = SshTarget {
            endpoint,
            username: "fixture".into(),
            authentication: Authentication::password("test fixture".into()),
        };
        let started = Instant::now();
        let result = dial(
            (target, None),
            TrustStore::from_pins(Vec::new()).unwrap(),
            StartupChannel::Terminal {
                term: "xterm",
                size: WindowSize {
                    columns: 80,
                    rows: 24,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            ConnectOptions::new(1, 3).unwrap(),
            None,
            || false,
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
        );
        assert!(
            (Duration::from_millis(900)..Duration::from_secs(4)).contains(&elapsed),
            "{elapsed:?}"
        );
        let bytes = peer.join().unwrap();
        assert!(bytes.starts_with(b"SSH-2.0-"), "{bytes:?}");
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }
}
