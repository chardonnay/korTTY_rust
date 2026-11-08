//! Native SSH terminal sessions and endpoint-specific host-key pins.

#![forbid(unsafe_code)]

mod session;
pub mod sftp;
mod trust;

#[cfg(any(windows, target_os = "macos"))]
pub use session::PipeTransport;
pub use session::{
    Authentication, Connect, ConnectError, ConnectOptions, DirectChannel, DirectOpener, JumpRoute,
    KeyboardChallenge, KeyboardPrompt, KeyboardResponse, RemoteConnection, RemoteForward,
    RemoteOpener, SftpLease, SftpOpener, SshAbort, SshReader, SshSession, SshTarget, SshWriter,
    StartupChannel, WindowSize, connect_tcp_transport, connect_tcp_transport_until,
};
pub use trust::{
    Endpoint, HostKey, HostKeyStatus, ImportConflict, ImportReport, ImportReview, ImportSkipped,
    KnownHostsFile, TrustMode, TrustStore, TrustedHost,
};
