#[cfg(not(feature = "larger"))]
pub const DEFAULT_WINDOW: usize = 1000;
#[cfg(not(feature = "larger"))]
pub const DEFAULT_MAX_PACKET: usize = 1000;

// Desktop channels can keep useful bandwidth across normal network latency.
// The maximum data packet fits the 35,000-byte allocation below with overhead.
#[cfg(feature = "larger")]
pub const DEFAULT_WINDOW: usize = 256 * 1024;
#[cfg(feature = "larger")]
pub const DEFAULT_MAX_PACKET: usize = 32 * 1024;

/// Maximum SSH packet size, from RFC4253.
///
/// Used to size buffers when using `alloc`
#[allow(unused)]
pub(crate) const SSH_MAX_PACKET: usize = 35000;

#[cfg(not(feature = "larger"))]
pub const MAX_CHANNELS: usize = 4;

// Reserve room for independent desktop SFTP sessions and a dedicated exec PTY.
#[cfg(feature = "larger")]
pub const MAX_CHANNELS: usize = 25;

// Enough for longest 23 of "screen.konsole-256color" on my system
// Unsure if this is specified somewhere
pub const MAX_TERM: usize = 32;

pub const DEFAULT_TERM: &str = "xterm";

pub const RSA_DEFAULT_KEYSIZE: usize = 2048;
pub const RSA_MIN_KEYSIZE: usize = 2048;
pub const RSA_MAX_KEYSIZE: usize = 8192;

/// Maximum username for client or server
///
/// 31 is the limit for various Linux APIs like wtmp
/// A larger limit can be set with `larger` crate feature
#[cfg(not(feature = "larger"))]
pub const MAX_USERNAME: usize = 31;

/// Maximum username for client or server
///
/// 31 is the limit for various Linux APIs like wtmp
#[cfg(feature = "larger")]
pub const MAX_USERNAME: usize = 256;

// TODO: server auth timeout/tries
