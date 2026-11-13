//! Isolation for a separate worker that needs only an approved TCP transport.

use std::io;
use std::net::TcpStream;
use std::process::Command;

/// An SSH worker sandbox backed by Linux Landlock and seccomp.
pub struct WorkerSandbox;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{MacWorker, MacWorkerProcess};
#[cfg(windows)]
mod relay_io;
#[cfg(windows)]
pub use relay_io::{WindowsRelayIo, WindowsRelayReader, WindowsRelayWriter};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::with_private_pipe;
#[cfg(windows)]
pub use windows::{WindowsCli, WindowsWorker, WindowsWorkerProcess};

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
))]
use crate::linux_sandbox::worker as platform;

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
)))]
mod platform {
    use super::*;

    #[cfg(not(windows))]
    pub(super) fn check_available() -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn attach(_command: &mut Command, _tcp: TcpStream) -> io::Result<u32> {
        Err(unsupported())
    }

    pub(super) fn enter(_descriptor: u32) -> io::Result<TcpStream> {
        Err(unsupported())
    }

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "no OS sandbox for the SSH worker",
        )
    }
}

impl WorkerSandbox {
    /// Checks the platform prerequisites. Each worker must still attest its sandbox.
    pub fn check_available() -> io::Result<()> {
        #[cfg(windows)]
        return windows::check_available();
        #[cfg(not(windows))]
        platform::check_available()
    }

    /// Gives a trusted child executable one already-connected TCP capability.
    /// Pass the returned descriptor number to `enter` in that child. Drop the
    /// command after spawning so the parent does not keep a duplicate socket.
    /// Configure standard input and output as pipes, and reserve stderr for
    /// the read end of the [`crate::ParentExit`] lifetime pipe.
    pub fn attach(command: &mut Command, tcp: TcpStream) -> io::Result<u32> {
        platform::attach(command, tcp)
    }

    /// Irreversibly confines this freshly started, single-threaded child before
    /// it handles IPC or SSH input. On failure the child must terminate.
    /// This does not use any paths or keys from the desktop profile.
    pub fn enter(descriptor: u32) -> io::Result<TcpStream> {
        platform::enter(descriptor)
    }
}
