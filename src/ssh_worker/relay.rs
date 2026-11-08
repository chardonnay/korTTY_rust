//! The desktop retains the only network socket for a confined worker.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

#[cfg(windows)]
use kortty_pty::WindowsRelayIo as Endpoint;
#[cfg(target_os = "macos")]
use macos::Endpoint;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub(super) type Pipe = Endpoint;
#[cfg(target_os = "macos")]
pub(super) type Pipe = std::os::unix::net::UnixStream;

pub(super) struct Relay {
    endpoints: Arc<Endpoints>,
    workers: [Option<JoinHandle<()>>; 2],
}

impl Relay {
    pub(super) fn start(tcp: TcpStream, pipe: Pipe) -> io::Result<Self> {
        tcp.set_nodelay(true)?;
        let tcp = Endpoint::tcp(tcp);
        #[cfg(target_os = "macos")]
        let pipe = Endpoint::Local(pipe);
        let tcp_reader = tcp.reader()?;
        let tcp_writer = tcp.writer()?;
        let pipe_reader = pipe.reader()?;
        let pipe_writer = pipe.writer()?;
        let mut relay = Self {
            endpoints: Arc::new(Endpoints {
                tcp,
                pipe,
                stopped: AtomicBool::new(false),
            }),
            workers: [None, None],
        };
        relay.workers[0] = Some(relay.forward("kortty-ssh-relay-in", tcp_reader, pipe_writer)?);
        relay.workers[1] = Some(relay.forward("kortty-ssh-relay-out", pipe_reader, tcp_writer)?);
        Ok(relay)
    }

    fn forward(
        &self,
        name: &str,
        mut reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        let endpoints = Arc::clone(&self.endpoints);
        thread::Builder::new().name(name.into()).spawn(move || {
            // Both destinations exert backpressure. Each direction copies at
            // most one standard I/O buffer outside the kernel at a time.
            let _ = io::copy(&mut reader, &mut writer);
            endpoints.stop();
        })
    }

    pub(super) fn stop(&self) {
        self.endpoints.stop();
    }

    pub(super) fn is_finished(&self) -> bool {
        self.endpoints.stopped.load(Ordering::Acquire)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop();
        for worker in self.workers.iter_mut().filter_map(Option::take) {
            let _ = worker.join();
        }
    }
}

struct Endpoints {
    tcp: Endpoint,
    pipe: Endpoint,
    stopped: AtomicBool,
}

impl Endpoints {
    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.pipe.close();
            self.tcp.close();
        }
    }
}
