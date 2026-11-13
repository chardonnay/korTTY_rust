use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Default)]
pub(crate) struct Link {
    pub(crate) hold_inbound: AtomicBool,
    pub(crate) blackhole: AtomicBool,
    stopped: AtomicBool,
}

impl Link {
    pub(crate) fn run(&self, listener: TcpListener, upstream: SocketAddr) -> io::Result<()> {
        let (client, _) = listener.accept()?;
        let server = TcpStream::connect(upstream)?;
        let client_read = client.try_clone()?;
        let server_read = server.try_clone()?;
        thread::scope(|scope| {
            let outgoing = scope.spawn(|| self.copy(client_read, server, false));
            let incoming = self.copy(server_read, client, true);
            incoming.and(outgoing.join().unwrap())
        })
    }

    fn copy(&self, mut input: TcpStream, mut output: TcpStream, inbound: bool) -> io::Result<()> {
        let result = (|| {
            input.set_read_timeout(Some(Duration::from_millis(50)))?;
            output.set_write_timeout(Some(Duration::from_secs(2)))?;
            let mut buffer = [0; 8192];
            let mut held = Vec::new();
            while !self.stopped.load(Ordering::Acquire) {
                let blocked = self.blackhole.load(Ordering::Acquire);
                let hold = inbound && self.hold_inbound.load(Ordering::Acquire);
                if !hold && !held.is_empty() {
                    if !blocked {
                        output.write_all(&held)?;
                    }
                    held.clear();
                }
                let count = match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !self.blackhole.load(Ordering::Acquire) {
                    if inbound && self.hold_inbound.load(Ordering::Acquire) {
                        if held.len() + count > 32 * 1024 {
                            return Err(io::Error::other(
                                "unexpected traffic during the delayed probe reply",
                            ));
                        }
                        held.extend_from_slice(&buffer[..count]);
                    } else {
                        if !held.is_empty() {
                            output.write_all(&held)?;
                            held.clear();
                        }
                        output.write_all(&buffer[..count])?;
                    }
                }
            }
            Ok(())
        })();
        self.stopped.store(true, Ordering::Release);
        let _ = output.shutdown(Shutdown::Both);
        result
    }
}
