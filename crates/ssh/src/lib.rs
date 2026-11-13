//! Native SSH terminal sessions and endpoint-specific host-key pins.

#![forbid(unsafe_code)]

mod session;
pub mod sftp;
mod trust;

/// Total channel budget for one authenticated desktop SSH connection.
pub const MAX_SESSION_CHANNELS: usize = sunset::config::MAX_CHANNELS;

/// One browser and up to eight transfer leases on a terminal connection.
pub const MAX_SFTP_LEASES: usize = 9;

/// Forwarding shares the connection with the terminal, SFTP, exec and dedicated exec PTY.
pub const MAX_FORWARD_STREAMS: usize = MAX_SESSION_CHANNELS - MAX_SFTP_LEASES - 3;

/// Remote forwarding registrations use no SSH channel until a client connects.
pub const MAX_REMOTE_FORWARD_LISTENERS: usize = 4;

#[cfg(any(windows, target_os = "macos"))]
pub use session::PipeTransport;
pub use session::{
    Authentication, Connect, ConnectError, ConnectOptions, DirectChannel, DirectOpener,
    ExecConnection, ExecLease, ExecOpener, JumpRoute, KeepAlive, KeyboardChallenge, KeyboardPrompt,
    KeyboardResponse, MAX_EXEC_COMMAND_BYTES, RemoteConnection, RemoteForward, RemoteOpener,
    SensitiveExecInput, SensitiveLine, SftpLease, SftpOpener, SshAbort, SshReader, SshSession,
    SshTarget, SshWriter, StartupChannel, WindowSize, connect_tcp_transport,
    connect_tcp_transport_until, validate_private_key,
};
pub use trust::{
    Endpoint, HostKey, HostKeyStatus, ImportConflict, ImportReport, ImportReview, ImportSkipped,
    KnownHostsFile, TrustMode, TrustStore, TrustedHost,
};
