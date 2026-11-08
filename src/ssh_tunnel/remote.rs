use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Instant;

use async_channel::Sender;
use kortty_ssh::Endpoint;

use crate::ssh_connection::{Channel, RemoteForward, RemoteOpener};

use super::{Capacity, Client, HANDSHAKE_TIMEOUT, ListenerState, POLL_INTERVAL, relay};

const MAX_RESOLVED_ADDRESSES: usize = 16;

pub(super) fn listen(
    opener: RemoteOpener,
    address: SocketAddr,
    target: Endpoint,
    capacity: Capacity,
    state: Arc<ListenerState>,
    changes: &Sender<()>,
) -> io::Result<()> {
    let targets: Arc<[SocketAddr]> = (target.wire_host().as_ref(), target.port())
        .to_socket_addrs()?
        .take(MAX_RESOLVED_ADDRESSES)
        .collect();
    if targets.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "local forwarding target did not resolve",
        ));
    }
    if state.is_stopping() {
        return Ok(());
    }
    // Once admitted, wait for the server reply so a stop can cancel the actual registration.
    let mut listener = match opener.bind(address) {
        Ok(listener) => listener,
        Err(_) if state.is_stopping() => return Ok(()),
        Err(error) => return Err(io::Error::other(error)),
    };
    if listener.remote_addr() != address {
        let _ = listener.close();
        return Err(io::Error::other(
            "SSH server returned an unexpected bind address",
        ));
    }
    if state.is_stopping() {
        return listener.close().map_err(io::Error::other);
    }
    state.listening.store(true, Ordering::Release);
    let _ = changes.try_send(());
    let result = accept(&opener, &listener, &state, &targets, &capacity);
    let closed = listener.close().map_err(io::Error::other);
    result.and(closed)
}

fn accept(
    opener: &RemoteOpener,
    listener: &RemoteForward,
    state: &Arc<ListenerState>,
    targets: &Arc<[SocketAddr]>,
    capacity: &Capacity,
) -> io::Result<()> {
    while !state.is_stopping() && !opener.is_closed() {
        let Some((_, channel)) = listener.accept_timeout(POLL_INTERVAL)? else {
            continue;
        };
        let Some(slot) = capacity.claim() else {
            // Dropping the channel closes this connection only.
            continue;
        };
        let state = Arc::clone(state);
        let targets = Arc::clone(targets);
        thread::Builder::new()
            .name("kortty-tunnel-client".into())
            .spawn(move || {
                let _slot = slot;
                let _ = serve(channel, &targets, &state);
            })?;
    }
    Ok(())
}

fn serve(channel: Channel, targets: &[SocketAddr], state: &ListenerState) -> io::Result<()> {
    let abort = channel.abort_handle();
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut error = io::Error::from(io::ErrorKind::TimedOut);
    for address in targets {
        if state.is_stopping() || abort.is_closed() {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(address, remaining) {
            Ok(socket) => {
                let client = Arc::new(Client::new(socket.try_clone()?));
                if state.register(&client) {
                    return relay(channel, socket, &client);
                }
                return Ok(());
            }
            Err(failed) => error = failed,
        }
    }
    Err(error)
}
