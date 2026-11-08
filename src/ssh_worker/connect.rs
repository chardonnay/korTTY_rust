//! Cancellable desktop TCP setup. Retries finish before any SSH login starts.

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kortty_ssh::{ConnectOptions, Endpoint, connect_tcp_transport_until};

const POLL: Duration = Duration::from_millis(50);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_DNS_WORKERS: usize = 16;
static DNS_WORKERS: AtomicUsize = AtomicUsize::new(0);

pub fn connect_transport(
    endpoint: &Endpoint,
    cancelled: impl Fn() -> bool,
) -> io::Result<TcpStream> {
    connect_transport_with_options(endpoint, ConnectOptions::default(), cancelled)
}

pub fn connect_transport_with_options(
    endpoint: &Endpoint,
    options: ConnectOptions,
    cancelled: impl Fn() -> bool,
) -> io::Result<TcpStream> {
    let attempts = options.attempts().max(1);
    for attempt in 1..=attempts {
        check_cancelled(&cancelled)?;
        let deadline = Instant::now() + options.timeout();
        let result =
            resolve_until(endpoint, deadline, &cancelled).and_then(|(address, additional)| {
                connect_tcp_transport_until(address, &additional, deadline, &cancelled)
            });
        match result {
            Err(error) if attempt < attempts && retryable(&error) => {
                wait_before_retry(&cancelled)?;
            }
            result => {
                check_cancelled(&cancelled)?;
                return result;
            }
        }
    }
    unreachable!("at least one connection attempt")
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::WouldBlock
    )
}

fn wait_before_retry(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    let ready = Instant::now() + RETRY_DELAY;
    while let Some(left) = ready.checked_duration_since(Instant::now()) {
        check_cancelled(cancelled)?;
        thread::sleep(left.min(POLL));
    }
    check_cancelled(cancelled)
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "SSH connection cancelled",
        ));
    }
    Ok(())
}

fn resolve_until(
    endpoint: &Endpoint,
    deadline: Instant,
    cancelled: &impl Fn() -> bool,
) -> io::Result<(SocketAddr, Vec<SocketAddr>)> {
    let host = endpoint.wire_host();
    let port = endpoint.port();
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok((SocketAddr::new(ip, port), Vec::new()));
    }
    // libc/Winsock hostname resolution is blocking. Bound outstanding OS work
    // and let cancellation or the attempt deadline stop waiting for its result.
    let permit = ResolverPermit::acquire()?;
    let host = host.into_owned();
    let (send, receive) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("kortty-ssh-resolve".into())
        .spawn(move || {
            let _permit = permit;
            let _ = send.send(resolve(&host, port));
        })?;
    loop {
        check_cancelled(cancelled)?;
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "SSH hostname resolution timed out")
            })?;
        match receive.recv_timeout(left.min(POLL)) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("SSH hostname resolver stopped"));
            }
        }
    }
}

struct ResolverPermit;

impl ResolverPermit {
    fn acquire() -> io::Result<Self> {
        DNS_WORKERS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < MAX_DNS_WORKERS).then_some(used + 1)
            })
            .map(|_| Self)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "SSH hostname resolver is busy"))
    }
}

impl Drop for ResolverPermit {
    fn drop(&mut self) {
        DNS_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn resolve(host: &str, port: u16) -> io::Result<(SocketAddr, Vec<SocketAddr>)> {
    let mut addresses = (host, port).to_socket_addrs()?;
    let address = addresses
        .next()
        .ok_or_else(|| io::Error::other("SSH hostname did not resolve"))?;
    Ok((address, addresses.take(15).collect()))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn configured_tcp_attempts_reach_a_loopback_listener_after_initial_refusal() {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
        // Winsock can take over two seconds to report a closed loopback port.
        let options = ConnectOptions::new(6, 0).unwrap();
        assert_eq!(
            connect_transport_with_options(&endpoint, options, || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::ConnectionRefused,
        );

        // Keep the listener absent for the first one-second attempt. A kernel
        // SYN retransmission on that first socket must not count as our retry.
        let listener_delay = Duration::from_millis(1250);
        let started = Instant::now();
        let listener = thread::spawn(move || {
            thread::sleep(listener_delay);
            TcpListener::bind(address).unwrap()
        });
        let result =
            connect_transport_with_options(&endpoint, ConnectOptions::new(1, 3).unwrap(), || false);
        let listener = listener.join().unwrap();
        let socket = result.unwrap();
        assert_eq!(socket.peer_addr().unwrap(), listener.local_addr().unwrap());
        assert!(started.elapsed() >= listener_delay);
    }

    #[test]
    fn cancellation_interrupts_a_pending_tcp_attempt_or_retry_pause() {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = Endpoint::new("127.0.0.1", socket.local_addr().unwrap().port()).unwrap();
        drop(socket);
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                cancelled.store(true, Ordering::Release);
            });
            let error = connect_transport_with_options(
                &endpoint,
                ConnectOptions::new(300, 20).unwrap(),
                || cancelled.load(Ordering::Acquire),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        });
        assert!(started.elapsed() < Duration::from_millis(800));
    }
}
