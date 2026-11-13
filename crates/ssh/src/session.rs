use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::ops::Range;
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ed25519::pkcs8::spki::der::{Decode as _, pem};
use ed25519::pkcs8::{ALGORITHM_OID as ED25519_OID, KeypairBytes, PrivateKeyInfo};
use mio::net::TcpStream as MioTcpStream;
#[cfg(target_os = "macos")]
use mio::net::UnixStream as MioRelay;
#[cfg(windows)]
use mio::windows::NamedPipe as MioRelay;
use mio::{Events, Interest, Poll, Token, Waker};
use ssh_key::private::KeypairData;
use ssh_key::{Algorithm, EcdsaCurve, Kdf, PrivateKey};
use sunset::event::CheckHostkey;
use sunset::packets::WinChange;
use sunset::{
    ChanData, ChanHandle, CliEvent, CliSessionExit, Client, DirectStatus, Event, Pty, Runner,
    SessionCommand, SignKey,
};
use zeroize::Zeroizing;

use crate::{Endpoint, HostKey, HostKeyStatus, TrustMode, TrustStore};

mod auxiliary;
mod forward;
mod interactive;
mod liveness;
mod output;
mod private_key;
mod remote;
mod sensitive;
pub use auxiliary::{ExecLease, ExecOpener, MAX_EXEC_COMMAND_BYTES, SftpLease, SftpOpener};
pub use forward::{DirectChannel, DirectOpener};
use interactive::InteractiveAuth;
pub use interactive::{KeyboardChallenge, KeyboardPrompt, KeyboardResponse};
pub use remote::{RemoteConnection, RemoteForward, RemoteOpener};
pub use sensitive::{SensitiveExecInput, SensitiveLine};

const CONNECT_TIMEOUT_SECONDS: u16 = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(CONNECT_TIMEOUT_SECONDS as u64);
const STARTUP_CANCEL_POLL: Duration = Duration::from_millis(250);
const CHUNK_BYTES: usize = 8192;
const QUEUE_DEPTH: usize = 16;
const TRANSPORT: Token = Token(0);
const CONTROL: Token = Token(1);
const NO_EXIT_CODE: u64 = u64::MAX;
const MAX_PRIVATE_KEY_BYTES: usize = 1024 * 1024;
const MAX_KEY_PASSPHRASE_BYTES: usize = 1024;
const MAX_OPENSSH_BCRYPT_ROUNDS: u32 = 128;

/// Configured SSH transport keepalive. The saved interval is retained while disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeepAlive {
    enabled: bool,
    seconds: u16,
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self {
            enabled: true,
            seconds: 60,
        }
    }
}

impl KeepAlive {
    pub const MIN_SECONDS: u16 = 5;
    pub const MAX_SECONDS: u16 = 600;

    pub fn new(enabled: bool, seconds: u16) -> io::Result<Self> {
        if !(Self::MIN_SECONDS..=Self::MAX_SECONDS).contains(&seconds) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH keepalive requires 5 to 600 seconds",
            ));
        }
        Ok(Self { enabled, seconds })
    }

    pub fn enabled(self) -> bool {
        self.enabled
    }

    pub fn seconds(self) -> u16 {
        self.seconds
    }
}

/// Bounded desktop SSH connection policy. Attempts count TCP connections only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectOptions {
    seconds: u16,
    attempts: u8,
    keepalive: Option<KeepAlive>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            seconds: CONNECT_TIMEOUT_SECONDS,
            attempts: 0,
            keepalive: None,
        }
    }
}

impl ConnectOptions {
    pub const QUICK: Self = Self {
        seconds: 10,
        attempts: 0,
        keepalive: None,
    };

    pub fn new(seconds: u16, attempts: u8) -> io::Result<Self> {
        if !(1..=300).contains(&seconds) || attempts > 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH timeout or attempt count is out of range",
            ));
        }
        Ok(Self {
            seconds,
            attempts,
            keepalive: None,
        })
    }

    pub fn seconds(self) -> u16 {
        self.seconds
    }

    pub fn attempts(self) -> u8 {
        self.attempts
    }

    pub fn timeout(self) -> Duration {
        Duration::from_secs(self.seconds.into())
    }

    pub fn keepalive(self) -> Option<KeepAlive> {
        self.keepalive
    }

    pub fn with_keepalive(mut self, keepalive: Option<KeepAlive>) -> Self {
        self.keepalive = keepalive;
        self
    }
}

/// The first channel requested after SSH verification and authentication.
pub enum StartupChannel<'a> {
    Terminal {
        term: &'a str,
        size: WindowSize,
    },
    Subsystem(&'a str),
    /// An SFTP session that reserves an additional exec lease for its transfer helper.
    SftpWithTransferHelper,
    Exec {
        command: &'a str,
        pty: Option<(&'a str, WindowSize)>,
    },
}

impl<'a> StartupChannel<'a> {
    fn request(self) -> io::Result<OpenRequest<'a>> {
        match self {
            Self::Terminal { term, size } => Ok(OpenRequest::Terminal(Box::new(terminal_request(
                term, size,
            )?))),
            Self::Subsystem(subsystem) => {
                validate_subsystem(subsystem)?;
                Ok(OpenRequest::Subsystem(subsystem))
            }
            Self::SftpWithTransferHelper => Ok(OpenRequest::SftpWithTransferHelper),
            Self::Exec { command, pty } => exec_request(command, pty),
        }
    }
}

/// A connected local relay endpoint and the desktop TCP stream's local address.
#[cfg(any(windows, target_os = "macos"))]
pub struct PipeTransport {
    #[cfg(windows)]
    pipe: mio::windows::NamedPipe,
    #[cfg(target_os = "macos")]
    pipe: UnixStream,
    origin: SocketAddr,
}

#[cfg(any(windows, target_os = "macos"))]
impl PipeTransport {
    /// Pairs a connected Unix stream or an unregistered, overlapped duplex
    /// named pipe with the relay's TCP origin.
    /// Obtain `origin` from `TcpStream::local_addr` on the trusted relay side.
    pub fn new(
        #[cfg(windows)] pipe: mio::windows::NamedPipe,
        #[cfg(target_os = "macos")] pipe: UnixStream,
        origin: SocketAddr,
    ) -> Self {
        Self { pipe, origin }
    }
}

/// One credential for SSH user authentication.
pub struct Authentication {
    credential: Option<Credential>,
    interactive: Option<InteractiveAuth>,
}

enum Credential {
    Password(Zeroizing<String>),
    PrivateKey(KeyCredential),
}

struct KeyCredential {
    bytes: Zeroizing<Vec<u8>>,
    passphrase: Option<Zeroizing<String>>,
}

impl Authentication {
    /// Supplies an SSH password. The owned buffer is cleared on drop.
    pub fn password(password: String) -> Self {
        Self {
            credential: Some(Credential::Password(Zeroizing::new(password))),
            interactive: None,
        }
    }

    /// Supplies an unencrypted Ed25519, ECDSA, or RSA private key.
    ///
    /// Accepts OpenSSH and PKCS#8 PEM, SEC1 for named ECDSA curves, and RSA PKCS#1 PEM.
    pub fn private_key(key: Vec<u8>) -> Self {
        Self {
            credential: Some(Credential::PrivateKey(KeyCredential {
                bytes: Zeroizing::new(key),
                passphrase: None,
            })),
            interactive: None,
        }
    }

    /// Supplies an OpenSSH or PBES2 PKCS#8 key and its local decryption passphrase.
    pub fn private_key_with_passphrase(key: Vec<u8>, passphrase: String) -> Self {
        Self {
            credential: Some(Credential::PrivateKey(KeyCredential {
                bytes: Zeroizing::new(key),
                passphrase: Some(Zeroizing::new(passphrase)),
            })),
            interactive: None,
        }
    }

    /// Enables SSH keyboard-interactive as the sole explicitly supplied credential.
    /// See [Self::with_keyboard_interactive] for the callback contract.
    pub fn keyboard_interactive(
        handler: impl FnMut(&KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> + Send + 'static,
    ) -> Self {
        Self {
            credential: None,
            interactive: Some(InteractiveAuth::new(handler)),
        }
    }

    /// Allows an additional keyboard-interactive method after or instead of the primary credential.
    ///
    /// The callback runs on a separate thread only after host verification. Return
    /// exactly one answer per prompt in order. An error cancels authentication.
    /// A server notice may contain no prompts. Acknowledge it with an empty vector.
    /// Honor the challenge timeout in the callback to release any prompt resources
    /// when the SSH session stops waiting for an answer.
    pub fn with_keyboard_interactive(
        mut self,
        handler: impl FnMut(&KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> + Send + 'static,
    ) -> Self {
        self.interactive = Some(InteractiveAuth::new(handler));
        self
    }
}

impl KeyCredential {
    fn signing_key(&self) -> Result<SignKey, ConnectError> {
        Self::parse_signing_key(
            self.bytes.as_slice(),
            self.passphrase
                .as_ref()
                .map(|passphrase| passphrase.as_str()),
        )
    }

    fn parse_signing_key(bytes: &[u8], passphrase: Option<&str>) -> Result<SignKey, ConnectError> {
        let trimmed = bytes.trim_ascii_start();
        if trimmed.starts_with(b"-----BEGIN PRIVATE KEY-----")
            || trimmed.starts_with(b"-----BEGIN EC PRIVATE KEY-----")
            || trimmed.starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
            || trimmed.starts_with(b"-----BEGIN ENCRYPTED PRIVATE KEY-----")
        {
            return Self::pem_signing_key(trimmed, passphrase);
        }
        let key = PrivateKey::from_openssh(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid private key, expected OpenSSH or supported PEM",
            )
        })?;
        if !matches!(
            key.algorithm(),
            Algorithm::Ed25519 | Algorithm::Ecdsa { .. } | Algorithm::Rsa { .. }
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SSH requires an Ed25519, NIST ECDSA, or RSA private key",
            )
            .into());
        }
        if !key.is_encrypted() {
            return Self::openssh_signing_key(&key);
        }
        let passphrase = passphrase
            .filter(|passphrase| !passphrase.is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "encrypted OpenSSH key requires a passphrase",
                )
            })?;
        if !matches!(key.kdf(), Kdf::Bcrypt { salt, rounds }
            if (16..=64).contains(&salt.len()) && (1..=MAX_OPENSSH_BCRYPT_ROUNDS).contains(rounds))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OpenSSH key KDF parameters exceed supported bounds",
            )
            .into());
        }
        let decrypted = key.decrypt(passphrase.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "incorrect OpenSSH key passphrase or damaged key",
            )
        })?;
        Self::openssh_signing_key(&decrypted)
    }

    fn pem_signing_key(bytes: &[u8], passphrase: Option<&str>) -> Result<SignKey, ConnectError> {
        let invalid = private_key::invalid_pem;
        let mut decoder = pem::Decoder::new(bytes).map_err(|_| invalid())?;
        let mut decoded = Zeroizing::new(vec![0; decoder.remaining_len()]);
        let der = decoder.decode(&mut decoded).map_err(|_| invalid())?;
        match decoder.type_label() {
            "EC PRIVATE KEY" => SignKey::ecdsa_sec1(der).map_err(|_| invalid().into()),
            "RSA PRIVATE KEY" => private_key::rsa_pkcs1(der, None),
            "PRIVATE KEY" => Self::pkcs8_signing_key(der),
            "ENCRYPTED PRIVATE KEY" => {
                let passphrase = passphrase
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "encrypted PKCS#8 key requires a passphrase",
                        )
                    })?;
                private_key::with_decrypted_pkcs8(
                    der,
                    passphrase.as_bytes(),
                    Self::pkcs8_signing_key,
                )
            }
            _ => Err(invalid().into()),
        }
    }

    fn pkcs8_signing_key(der: &[u8]) -> Result<SignKey, ConnectError> {
        let invalid = private_key::invalid_pem;
        let info = PrivateKeyInfo::from_der(der).map_err(|_| invalid())?;
        if info.algorithm.oid == pkcs1::ALGORITHM_OID {
            return private_key::rsa_pkcs8(info);
        }
        if info.algorithm.oid != ED25519_OID {
            return SignKey::ecdsa_pkcs8(info).map_err(|_| invalid().into());
        }
        let pair = KeypairBytes::try_from(info).map_err(|_| invalid())?;
        let key = sunset::ed25519_dalek::SigningKey::from_bytes(&pair.secret_key);
        if pair
            .public_key
            .is_some_and(|public| public.as_ref() != key.verifying_key().as_bytes())
        {
            return Err(invalid().into());
        }
        Ok(SignKey::Ed25519(key))
    }

    fn openssh_signing_key(key: &PrivateKey) -> Result<SignKey, ConnectError> {
        match key.key_data() {
            KeypairData::Ed25519(pair) => {
                let secret = Zeroizing::new(pair.private.to_bytes());
                let signer = sunset::ed25519_dalek::SigningKey::from_bytes(&secret);
                if pair.public.as_ref() != signer.verifying_key().as_bytes() {
                    return Err(sunset::Error::BadKey.into());
                }
                Ok(SignKey::Ed25519(signer))
            }
            KeypairData::Ecdsa(pair) => {
                let curve = match pair.curve() {
                    EcdsaCurve::NistP256 => sunset::EcdsaCurve::NistP256,
                    EcdsaCurve::NistP384 => sunset::EcdsaCurve::NistP384,
                    EcdsaCurve::NistP521 => sunset::EcdsaCurve::NistP521,
                };
                Ok(SignKey::ecdsa_private_key(
                    curve,
                    pair.private_key_bytes(),
                    pair.public_key_bytes(),
                )?)
            }
            KeypairData::Rsa(pair) => private_key::rsa_openssh(pair),
            _ => Err(sunset::Error::BadKey.into()),
        }
    }
}

/// Validates a private SSH key and its optional passphrase without a network connection.
///
/// Uses the same parser and size limits as session authentication. Callers importing
/// encrypted keys should run this on a worker because local key derivation can be slow.
pub fn validate_private_key(bytes: &[u8], passphrase: Option<&str>) -> Result<(), ConnectError> {
    validate_private_key_size(bytes, passphrase)?;
    KeyCredential::parse_signing_key(bytes, passphrase).map(drop)
}

/// The address to dial, the identity to verify, and the requested remote terminal.
pub struct Connect {
    pub address: SocketAddr,
    pub endpoint: Endpoint,
    pub username: String,
    pub authentication: Authentication,
    pub term: String,
    pub size: WindowSize,
}

/// Credentials and host-key identity for one SSH endpoint.
pub struct SshTarget {
    pub endpoint: Endpoint,
    pub username: String,
    pub authentication: Authentication,
}

/// A route through one or more verified jump hosts.
///
/// Only the first hop uses the local socket addresses. Later names are
/// resolved by the preceding SSH peer.
pub struct JumpRoute {
    pub address: SocketAddr,
    pub additional: Vec<SocketAddr>,
    pub jumps: Vec<SshTarget>,
    pub target: SshTarget,
}

/// The terminal size in cells and pixels. Unknown pixel dimensions may be zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowSize {
    pub columns: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl WindowSize {
    fn validate(self) -> io::Result<()> {
        if self.columns == 0 || self.rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH terminal requires nonzero rows and columns",
            ));
        }
        Ok(())
    }

    fn winchange(self) -> WinChange {
        WinChange {
            cols: self.columns,
            rows: self.rows,
            width: self.pixel_width,
            height: self.pixel_height,
        }
    }
}

/// A failed connection attempt. Unknown and changed identities precede user authentication.
#[derive(Debug)]
pub enum ConnectError {
    UnknownHostKey {
        endpoint: Endpoint,
        offered: HostKey,
    },
    ChangedHostKey {
        endpoint: Endpoint,
        expected: HostKey,
        offered: HostKey,
    },
    RequestRejected {
        request: &'static str,
    },
    Io(io::Error),
    Protocol(sunset::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownHostKey { endpoint, offered } => {
                write!(
                    f,
                    "Unknown SSH host key for {endpoint} ({})",
                    offered.fingerprint()
                )
            }
            Self::ChangedHostKey {
                endpoint,
                expected,
                offered,
            } => write!(
                f,
                "SSH host key changed for {endpoint} (expected {}, received {})",
                expected.fingerprint(),
                offered.fingerprint()
            ),
            Self::RequestRejected { request } => {
                write!(f, "SSH server refused the {request} request")
            }
            Self::Io(error) => error.fmt(f),
            Self::Protocol(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<sunset::Error> for ConnectError {
    fn from(error: sunset::Error) -> Self {
        Self::Protocol(error)
    }
}

/// An authenticated SSH shell, command, or subsystem channel.
///
/// The protocol is driven on one I/O worker. One reader and one writer can
/// block independently. Closing the session wakes both of them.
pub struct SshSession {
    shared: Arc<Shared>,
    reader: Option<SshReader>,
    stderr: Option<SshReader>,
    writer: Option<SshWriter>,
    worker: Option<JoinHandle<()>>,
    mode: ChannelMode,
    forwarding: DirectOpener,
    remote_forwarding: RemoteOpener,
    sftp: Option<SftpOpener>,
    exec: Option<ExecOpener>,
}

/// An authenticated SSH transport with no shell, PTY or initial channel.
pub struct ExecConnection(SshSession);

impl ExecConnection {
    /// Uses the supplied connected TCP stream and strict pins for every hop.
    /// The authentication guard runs before disclosing identity or credentials.
    pub fn connect_preconnected(
        tcp: TcpStream,
        (target, jumps): (SshTarget, Vec<SshTarget>),
        trust: TrustStore,
        options: ConnectOptions,
        cancelled: impl Fn() -> bool,
        before_authentication: impl Fn() -> io::Result<()>,
    ) -> Result<Self, ConnectError> {
        SshSession::start_preconnected_with_startup(
            tcp,
            target,
            jumps,
            trust.strict(),
            OpenRequest::ExecOnly,
            Startup {
                before_authentication: Some(&before_authentication),
                ..Startup::configured(options, &cancelled)
            },
        )
        .map(Self)
    }

    pub fn exec_opener(&self) -> Option<ExecOpener> {
        self.0.exec_opener()
    }
}

struct Shared {
    wake: Arc<Waker>,
    host_key: OnceLock<HostKey>,
    closed: AtomicBool,
    exit_code: AtomicU64,
    resize: Mutex<Option<WindowSize>>,
    error: Mutex<Option<Arc<io::Error>>>,
    downstream: OnceLock<Arc<Waker>>,
    consumed_output: AtomicUsize,
}

struct NotifyClosed(Arc<Shared>);

impl Drop for NotifyClosed {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.notify_downstream();
    }
}

impl Shared {
    fn new(wake: Arc<Waker>) -> Arc<Self> {
        Arc::new(Self {
            wake,
            host_key: OnceLock::new(),
            closed: AtomicBool::new(false),
            exit_code: AtomicU64::new(NO_EXIT_CODE),
            resize: Mutex::new(None),
            error: Mutex::new(None),
            downstream: OnceLock::new(),
            consumed_output: AtomicUsize::new(0),
        })
    }

    fn notify_downstream(&self) {
        if let Some(wake) = self.downstream.get() {
            let _ = wake.wake();
        }
    }

    fn consume_output(&self, bytes: usize) {
        if bytes != 0 && self.consumed_output.fetch_add(bytes, Ordering::AcqRel) == 0 {
            let _ = self.wake.wake();
        }
    }

    fn finish(&self, result: io::Result<()>) {
        if let Err(error) = result
            && !self.closed.load(Ordering::Acquire)
        {
            *lock(&self.error) = Some(Arc::new(error));
        }
        self.closed.store(true, Ordering::Release);
    }
}

/// Stops a running SSH channel and wakes its I/O endpoints from another thread.
#[derive(Clone)]
pub struct SshAbort(Arc<Shared>);

impl SshAbort {
    /// True after a full local or remote close, independent of receive EOF.
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }

    pub fn abort(&self) {
        if !self.0.closed.swap(true, Ordering::AcqRel) {
            let _ = self.0.wake.wake();
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ChannelMode {
    Terminal,
    Exec,
    SensitiveExec,
    ExecPty,
    ExecOnly,
    Subsystem,
    SftpLease,
    Direct,
}

enum OpenRequest<'a> {
    Terminal(Box<Pty>),
    Exec {
        command: &'a str,
        pty: Option<Box<Pty>>,
    },
    ExecOnly,
    Subsystem(&'a str),
    SftpWithTransferHelper,
    Direct {
        target: &'a Endpoint,
        origin: SocketAddr,
    },
}

struct UserAuth<'a> {
    username: &'a str,
    authentication: Authentication,
}

impl OpenRequest<'_> {
    fn mode(&self) -> ChannelMode {
        match self {
            Self::Terminal(_) => ChannelMode::Terminal,
            Self::Exec { pty: Some(_), .. } => ChannelMode::ExecPty,
            Self::Exec { pty: None, .. } => ChannelMode::Exec,
            Self::ExecOnly => ChannelMode::ExecOnly,
            Self::Subsystem(_) | Self::SftpWithTransferHelper => ChannelMode::Subsystem,
            Self::Direct { .. } => ChannelMode::Direct,
        }
    }
}

impl SshSession {
    /// Connects, verifies the host key before sending credentials, authenticates,
    /// and requests the remote PTY and shell.
    ///
    /// The server must negotiate strict key exchange and present a supported
    /// host key. The initial network handshake has an eight-second deadline.
    /// PTY and shell requests are queued without remote acknowledgments.
    pub fn connect(config: Connect, trust: TrustStore) -> Result<Self, ConnectError> {
        Self::connect_addresses(config, trust, &[])
    }

    /// Tries the primary address and then additional addresses in order.
    ///
    /// Only TCP connection failures advance to another address. Once TCP
    /// connects, host-key or authentication failures terminate the attempt.
    /// The same endpoint identity and deadline apply to all addresses.
    pub fn connect_addresses(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
    ) -> Result<Self, ConnectError> {
        let pty = terminal_request(&config.term, config.size)?;
        Self::connect_channel(
            config,
            trust,
            additional,
            OpenRequest::Terminal(Box::new(pty)),
            None,
        )
    }

    /// Opens and confirms a named subsystem over an SSH session channel.
    ///
    /// The channel has no PTY. Its reader contains only the subsystem's standard output.
    pub fn connect_subsystem(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        subsystem: &str,
    ) -> Result<Self, ConnectError> {
        validate_subsystem(subsystem)?;
        Self::connect_channel(
            config,
            trust,
            additional,
            OpenRequest::Subsystem(subsystem),
            None,
        )
    }

    /// Opens a TCP channel over a dedicated authenticated SSH connection.
    pub fn connect_direct(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        target: &Endpoint,
        origin: SocketAddr,
    ) -> Result<Self, ConnectError> {
        Self::connect_channel(
            config,
            trust,
            additional,
            OpenRequest::Direct { target, origin },
            None,
        )
    }

    /// Opens a remote shell through a chain of strictly verified jump hosts.
    pub fn connect_via(
        route: JumpRoute,
        trust: TrustStore,
        term: String,
        size: WindowSize,
    ) -> Result<Self, ConnectError> {
        let pty = terminal_request(&term, size)?;
        Self::connect_route(route, trust, OpenRequest::Terminal(Box::new(pty)))
    }

    /// Opens a named subsystem through strictly verified jump hosts.
    pub fn connect_subsystem_via(
        route: JumpRoute,
        trust: TrustStore,
        subsystem: &str,
    ) -> Result<Self, ConnectError> {
        validate_subsystem(subsystem)?;
        Self::connect_route(route, trust, OpenRequest::Subsystem(subsystem))
    }

    /// Starts a remote shell on a caller-provided connected TCP stream.
    /// The stream reaches the first jump, or the target for a direct connection.
    /// All later hops open within the preceding verified SSH transport.
    pub fn connect_preconnected(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        term: String,
        size: WindowSize,
    ) -> Result<Self, ConnectError> {
        let pty = terminal_request(&term, size)?;
        Self::start_preconnected(
            tcp,
            target,
            jumps,
            trust,
            OpenRequest::Terminal(Box::new(pty)),
            None,
        )
    }

    /// Like [Self::connect_preconnected], checking cancellation during each hop.
    pub fn connect_preconnected_cancellable(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        term: String,
        size: WindowSize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        let pty = terminal_request(&term, size)?;
        Self::start_preconnected(
            tcp,
            target,
            jumps,
            trust,
            OpenRequest::Terminal(Box::new(pty)),
            Some(&cancelled),
        )
    }

    /// Opens and confirms a subsystem on a caller-provided connected TCP stream.
    pub fn connect_subsystem_preconnected(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        subsystem: &str,
    ) -> Result<Self, ConnectError> {
        validate_subsystem(subsystem)?;
        Self::start_preconnected(
            tcp,
            target,
            jumps,
            trust,
            OpenRequest::Subsystem(subsystem),
            None,
        )
    }

    /// Like [Self::connect_subsystem_preconnected], checking cancellation during each hop.
    pub fn connect_subsystem_preconnected_cancellable(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        subsystem: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        validate_subsystem(subsystem)?;
        Self::start_preconnected(
            tcp,
            target,
            jumps,
            trust,
            OpenRequest::Subsystem(subsystem),
            Some(&cancelled),
        )
    }

    /// Opens a desktop shell or subsystem with a caller-selected SSH startup timeout.
    /// The caller establishes TCP; this method performs no retries.
    pub fn connect_preconnected_with_options(
        tcp: TcpStream,
        (target, jumps): (SshTarget, Vec<SshTarget>),
        trust: TrustStore,
        channel: StartupChannel<'_>,
        options: ConnectOptions,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        Self::start_preconnected_with_startup(
            tcp,
            target,
            jumps,
            trust,
            channel.request()?,
            Startup::configured(options, &cancelled),
        )
    }

    /// Rechecks the caller's authority before authentication and channel startup.
    pub fn connect_preconnected_guarded(
        tcp: TcpStream,
        (target, jumps): (SshTarget, Vec<SshTarget>),
        trust: TrustStore,
        channel: StartupChannel<'_>,
        cancelled: impl Fn() -> bool,
        authorize: impl Fn() -> io::Result<()>,
    ) -> Result<Self, ConnectError> {
        Self::start_preconnected_with_startup(
            tcp,
            target,
            jumps,
            trust,
            channel.request()?,
            Startup {
                before_authentication: Some(&authorize),
                ..Startup::new(Some(&cancelled))
            },
        )
    }

    /// Starts a shell over a caller-owned, connected local relay endpoint.
    ///
    /// On Windows use a fresh, unregistered duplex byte-mode pipe opened for
    /// overlapped I/O. On macOS use a connected Unix stream. The trusted relay
    /// must connect only to the first jump or, for a
    /// direct connection, to the target. All SSH host keys and credentials are
    /// handled by the same route as for connected TCP. The TCP origin is used
    /// for the jump's `direct-tcpip` channel metadata.
    ///
    /// # Errors
    /// Returns an error if the route is invalid or cancelled, the pipe fails,
    /// or the SSH peer cannot be verified, authenticated or opened.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn connect_pipe_cancellable(
        transport: PipeTransport,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        term: String,
        size: WindowSize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        let pty = terminal_request(&term, size)?;
        Self::start_pipe(
            transport,
            target,
            jumps,
            trust,
            OpenRequest::Terminal(Box::new(pty)),
            Startup::new(Some(&cancelled)),
        )
    }

    /// Like [Self::connect_pipe_cancellable], opening and confirming a subsystem
    /// instead of a shell.
    ///
    /// # Errors
    /// Returns the same route errors, or an error for an invalid or rejected
    /// subsystem request.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn connect_subsystem_pipe_cancellable(
        transport: PipeTransport,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        subsystem: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        validate_subsystem(subsystem)?;
        Self::start_pipe(
            transport,
            target,
            jumps,
            trust,
            OpenRequest::Subsystem(subsystem),
            Startup::new(Some(&cancelled)),
        )
    }

    /// Opens a desktop shell or subsystem over an established local relay.
    /// The timeout applies to SSH startup at each hop; no retry occurs here.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn connect_pipe_with_options(
        transport: PipeTransport,
        (target, jumps): (SshTarget, Vec<SshTarget>),
        trust: TrustStore,
        channel: StartupChannel<'_>,
        options: ConnectOptions,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        Self::start_pipe(
            transport,
            target,
            jumps,
            trust,
            channel.request()?,
            Startup::configured(options, &cancelled),
        )
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn start_pipe(
        transport: PipeTransport,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        request: OpenRequest<'_>,
        startup: Startup<'_>,
    ) -> Result<Self, ConnectError> {
        startup.check_cancelled()?;
        validate_route(&jumps, &target)?;
        let origin = transport.origin;
        Self::start_transport_route(
            Wire::local_pipe(transport)?,
            origin,
            target,
            jumps,
            trust,
            request,
            startup,
        )
    }

    fn start_preconnected(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        request: OpenRequest<'_>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<Self, ConnectError> {
        Self::start_preconnected_with_startup(
            tcp,
            target,
            jumps,
            trust,
            request,
            Startup::new(cancelled),
        )
    }

    fn start_preconnected_with_startup(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        request: OpenRequest<'_>,
        startup: Startup<'_>,
    ) -> Result<Self, ConnectError> {
        startup.check_cancelled()?;
        validate_route(&jumps, &target)?;
        let first = jumps.first().unwrap_or(&target);
        if tcp.peer_addr()?.port() != first.endpoint.port() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connected SSH transport port differs from the first endpoint",
            )
            .into());
        }
        Self::start_route(tcp, target, jumps, trust, request, startup)
    }

    fn connect_route(
        route: JumpRoute,
        trust: TrustStore,
        request: OpenRequest<'_>,
    ) -> Result<Self, ConnectError> {
        if route.jumps.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH route requires at least one jump host",
            )
            .into());
        }
        validate_route(&route.jumps, &route.target)?;
        let startup = Startup::new(None);
        let tcp = connect_tcp(route.address, &route.additional, startup)?;
        Self::start_route(tcp, route.target, route.jumps, trust, request, startup)
    }

    fn start_route(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        request: OpenRequest<'_>,
        startup: Startup<'_>,
    ) -> Result<Self, ConnectError> {
        let origin = tcp.local_addr()?;
        Self::start_transport_route(
            Wire::new(tcp)?,
            origin,
            target,
            jumps,
            trust,
            request,
            startup,
        )
    }

    fn start_transport_route(
        (mut wire, mut wake): (Wire, Arc<Waker>),
        origin: SocketAddr,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        request: OpenRequest<'_>,
        mut startup: Startup<'_>,
    ) -> Result<Self, ConnectError> {
        let trust = if jumps.is_empty() {
            trust
        } else {
            trust.strict()
        };
        let mut route_targets = jumps.into_iter().chain(std::iter::once(target));
        let mut target = route_targets
            .next()
            .ok_or_else(|| io::Error::other("SSH route is empty"))?;
        for next in route_targets {
            let link = Self::start_wire(
                wire,
                wake,
                target,
                trust.clone(),
                OpenRequest::Direct {
                    target: &next.endpoint,
                    origin,
                },
                Startup {
                    keepalive: None,
                    ..startup
                },
            )?;
            (wire, wake) = Wire::forwarded(link)?;
            target = next;
            startup.deadline = Instant::now() + startup.timeout;
        }
        Self::start_wire(wire, wake, target, trust, request, startup)
    }

    /// Starts a command on the verified remote server without allocating a PTY.
    ///
    /// Consume standard output and standard error concurrently. Both use
    /// bounded buffers and share the SSH receive window.
    pub fn connect_exec(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        command: &str,
    ) -> Result<Self, ConnectError> {
        Self::connect_exec_with(config, trust, additional, command, false, None)
    }

    /// Like [Self::connect_exec], with a cooperative startup cancellation check.
    ///
    /// Cancellation prevents further handshake I/O once observed, including
    /// sending the command request. For a running command, use [Self::abort_handle].
    pub fn connect_exec_cancellable(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        command: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        Self::connect_exec_with(config, trust, additional, command, false, Some(&cancelled))
    }

    /// Starts a command only after the server confirms the requested PTY.
    /// Read the combined terminal output with [Self::take_reader].
    pub fn connect_exec_pty_cancellable(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        command: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        Self::connect_exec_with(config, trust, additional, command, true, Some(&cancelled))
    }

    /// Starts a command over connected TCP and optional authenticated SSH jumps.
    /// A supplied PTY must be confirmed by the final server before exec.
    pub fn connect_exec_preconnected_cancellable(
        tcp: TcpStream,
        target: SshTarget,
        jumps: Vec<SshTarget>,
        trust: TrustStore,
        command: &str,
        pty: Option<(&str, WindowSize)>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, ConnectError> {
        Self::start_preconnected(
            tcp,
            target,
            jumps,
            trust,
            exec_request(command, pty)?,
            Some(&cancelled),
        )
    }

    fn connect_exec_with(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        command: &str,
        with_pty: bool,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<Self, ConnectError> {
        let request = exec_request(command, with_pty.then_some((&config.term, config.size)))?;
        Self::connect_channel(config, trust, additional, request, cancelled)
    }

    fn connect_channel(
        config: Connect,
        trust: TrustStore,
        additional: &[SocketAddr],
        request: OpenRequest<'_>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<Self, ConnectError> {
        let target = SshTarget {
            endpoint: config.endpoint,
            username: config.username,
            authentication: config.authentication,
        };
        validate_target(&target)?;
        let startup = Startup::new(cancelled);
        let tcp = connect_tcp(config.address, additional, startup)?;
        startup.check_cancelled()?;
        let (wire, wake) = Wire::new(tcp)?;
        Self::start_wire(wire, wake, target, trust, request, startup)
    }

    fn start_wire(
        mut wire: Wire,
        wake: Arc<Waker>,
        target: SshTarget,
        trust: TrustStore,
        request: OpenRequest<'_>,
        startup: Startup<'_>,
    ) -> Result<Self, ConnectError> {
        let mode = request.mode();
        let transfer_helper = matches!(&request, OpenRequest::SftpWithTransferHelper);
        let auxiliary_channels = mode == ChannelMode::Terminal
            || transfer_helper
            || matches!(&request, OpenRequest::Subsystem("sftp"));
        let exec_only = mode == ChannelMode::ExecOnly;
        let shared = Shared::new(wake);
        let (input, commands) = mpsc::sync_channel(QUEUE_DEPTH);
        let (stdout, output) = output::queue();
        let (stderr, error_output) = if mode == ChannelMode::Exec {
            let (send, receive) = output::queue();
            (Some(send), Some(receive))
        } else {
            (None, None)
        };
        let outputs = Outputs {
            mode,
            stdout: Some(stdout),
            stderr,
        };
        let (channel, host_key) = wire.handshake(
            &target.endpoint,
            UserAuth {
                username: &target.username,
                authentication: target.authentication,
            },
            request,
            &trust,
            &outputs,
            startup,
        )?;
        let _ = shared.host_key.set(host_key);
        startup.check_cancelled()?;
        wire.heartbeat = startup
            .keepalive
            .filter(|config| config.enabled())
            .map(|config| {
                Heartbeat::new(
                    Duration::from_secs(u64::from(config.seconds())),
                    Instant::now(),
                )
            });
        wire.liveness = auxiliary_channels.then(|| liveness::Probe::new(Instant::now()));
        let (forwarding, requests) = forward::control(&shared);
        let (remote_forwarding, remote_requests) = remote::control(&shared);
        let (sftp, exec, side_channels) = auxiliary::control(&shared, transfer_helper);
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("kortty-ssh-io".into())
            .spawn(move || {
                let _closed = NotifyClosed(Arc::clone(&worker_shared));
                let mut pump = channel.map(|channel| {
                    ChannelPump::new(channel, Arc::clone(&worker_shared), commands, outputs)
                });
                let result = drive_connection(
                    &mut wire,
                    (&worker_shared, pump.as_mut()),
                    &target.endpoint,
                    &trust,
                    (requests, transfer_helper),
                    remote_requests,
                    side_channels,
                );
                if let Some(pump) = pump {
                    pump.finish(result);
                } else {
                    worker_shared.finish(result);
                }
            })?;
        Ok(Self {
            reader: (!exec_only).then(|| SshReader::new(&shared, output)),
            stderr: error_output.map(|output| SshReader::new(&shared, output)),
            writer: (!exec_only).then_some(SshWriter {
                shared: Arc::clone(&shared),
                input,
                input_closed: false,
            }),
            shared,
            worker: Some(worker),
            mode,
            forwarding,
            remote_forwarding,
            sftp: auxiliary_channels.then_some(sftp),
            exec: (auxiliary_channels || exec_only).then_some(exec),
        })
    }

    /// Returns an opener tied to this already authenticated SSH connection.
    pub fn direct_opener(&self) -> DirectOpener {
        self.forwarding.clone()
    }

    /// The peer key verified during the authenticated connection setup.
    pub fn verified_host_key(&self) -> Option<&HostKey> {
        self.shared.host_key.get()
    }

    /// Returns an SFTP opener tied to this authenticated terminal or SFTP session.
    pub fn sftp_opener(&self) -> Option<SftpOpener> {
        self.sftp
            .as_ref()
            .filter(|opener| !opener.is_closed())
            .cloned()
    }

    /// Returns a non-PTY command opener bound to this authenticated terminal or SFTP session.
    pub fn exec_opener(&self) -> Option<ExecOpener> {
        self.exec
            .as_ref()
            .filter(|opener| !opener.is_closed())
            .cloned()
    }

    /// Returns a remote listener controller tied to this authenticated connection.
    pub fn remote_opener(&self) -> RemoteOpener {
        self.remote_forwarding.clone()
    }

    pub fn take_reader(&mut self) -> Option<SshReader> {
        self.reader.take()
    }

    /// Takes the separate stderr reader of an exec channel.
    pub fn take_stderr(&mut self) -> Option<SshReader> {
        self.stderr.take()
    }

    pub fn take_writer(&mut self) -> Option<SshWriter> {
        self.writer.take()
    }

    /// Takes the one-use input of a standalone non-PTY exec channel.
    pub fn take_sensitive_exec_input(&mut self) -> Option<SensitiveExecInput> {
        if self.mode != ChannelMode::Exec {
            return None;
        }
        self.take_writer().map(SensitiveExecInput::new)
    }

    /// Schedules the latest terminal size for the remote SSH channel.
    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        if self.mode != ChannelMode::Terminal {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SSH channel has no PTY",
            ));
        }
        size.validate()?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        *lock(&self.shared.resize) = Some(size);
        self.shared.wake.wake()
    }

    /// Returns the status sent by the remote process, if it supplied one.
    pub fn exit_code(&self) -> Option<u32> {
        self.shared
            .exit_code
            .load(Ordering::Acquire)
            .try_into()
            .ok()
    }

    pub fn abort_handle(&self) -> SshAbort {
        SshAbort(Arc::clone(&self.shared))
    }

    /// Stops the transport and waits for its I/O worker to exit.
    pub fn close(&mut self) -> io::Result<()> {
        if let Some(worker) = self.worker.take() {
            self.abort_handle().abort();
            worker
                .join()
                .map_err(|_| io::Error::other("SSH I/O worker panicked"))?;
        }
        Ok(())
    }
}

fn exec_request<'a>(
    command: &'a str,
    pty: Option<(&str, WindowSize)>,
) -> io::Result<OpenRequest<'a>> {
    if command.is_empty() || command.len() > 16 * 1024 || command.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH command length or NUL",
        ));
    }
    let pty = pty
        .map(|(term, size)| terminal_request(term, size).map(Box::new))
        .transpose()?;
    Ok(OpenRequest::Exec { command, pty })
}

fn terminal_request(term: &str, size: WindowSize) -> io::Result<Pty> {
    size.validate()?;
    if term.is_empty()
        || !term
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH terminal name",
        ));
    }
    Ok(Pty {
        term: term.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "SSH terminal name is too long")
        })?,
        cols: size.columns,
        rows: size.rows,
        width: size.pixel_width,
        height: size.pixel_height,
        modes: Default::default(),
    })
}

fn validate_target(target: &SshTarget) -> io::Result<()> {
    if target.username.is_empty()
        || target.username.len() > 255
        || target.username.chars().any(char::is_control)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH username",
        ));
    }
    match &target.authentication.credential {
        Some(Credential::Password(password)) if password.len() > 1024 => {
            Err(oversized_credential())
        }
        Some(Credential::PrivateKey(key)) => validate_private_key_size(
            key.bytes.as_slice(),
            key.passphrase
                .as_ref()
                .map(|passphrase| passphrase.as_str()),
        ),
        _ => Ok(()),
    }
}

fn validate_private_key_size(bytes: &[u8], passphrase: Option<&str>) -> io::Result<()> {
    if bytes.len() > MAX_PRIVATE_KEY_BYTES
        || passphrase.is_some_and(|passphrase| passphrase.len() > MAX_KEY_PASSPHRASE_BYTES)
    {
        return Err(oversized_credential());
    }
    Ok(())
}

fn oversized_credential() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "SSH credential is too large")
}

fn validate_route(jumps: &[SshTarget], target: &SshTarget) -> io::Result<()> {
    if jumps.len() > 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH route supports at most four jump hosts",
        ));
    }
    for endpoint in jumps.iter().chain(std::iter::once(target)) {
        validate_target(endpoint)?;
    }
    Ok(())
}

fn validate_subsystem(subsystem: &str) -> io::Result<()> {
    if subsystem.is_empty()
        || subsystem.len() > 128
        || !subsystem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH subsystem name",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Startup<'a> {
    deadline: Instant,
    timeout: Duration,
    cancelled: Option<&'a dyn Fn() -> bool>,
    before_authentication: Option<&'a dyn Fn() -> io::Result<()>>,
    keepalive: Option<KeepAlive>,
}

impl<'a> Startup<'a> {
    fn new(cancelled: Option<&'a dyn Fn() -> bool>) -> Self {
        Self::with_timeout(CONNECT_TIMEOUT, cancelled)
    }

    fn with_timeout(timeout: Duration, cancelled: Option<&'a dyn Fn() -> bool>) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            timeout,
            cancelled,
            before_authentication: None,
            keepalive: None,
        }
    }

    fn configured(options: ConnectOptions, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            keepalive: options.keepalive(),
            ..Self::with_timeout(options.timeout(), Some(cancelled))
        }
    }

    fn check_cancelled(self) -> io::Result<()> {
        if self.cancelled.is_some_and(|cancelled| cancelled()) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "SSH connection cancelled",
            ));
        }
        Ok(())
    }

    fn authorize_authentication(self) -> io::Result<()> {
        if let Some(authorize) = self.before_authentication {
            authorize()?;
            self.remaining()?;
        }
        Ok(())
    }

    fn remaining(self) -> io::Result<Duration> {
        self.check_cancelled()?;
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "SSH connection timed out"))
    }

    fn wait_time(self) -> io::Result<Duration> {
        let remaining = self.remaining()?;
        Ok(if self.cancelled.is_some() {
            remaining.min(STARTUP_CANCEL_POLL)
        } else {
            remaining
        })
    }
}

/// Connects TCP only, leaving host-key verification and SSH in the caller's worker.
/// Resolved address candidates share one eight-second connection deadline.
pub fn connect_tcp_transport(
    primary: SocketAddr,
    additional: &[SocketAddr],
    cancelled: impl Fn() -> bool,
) -> io::Result<TcpStream> {
    connect_tcp_transport_until(
        primary,
        additional,
        Instant::now() + CONNECT_TIMEOUT,
        cancelled,
    )
}

/// Connects to one of the supplied TCP addresses before an absolute deadline.
pub fn connect_tcp_transport_until(
    primary: SocketAddr,
    additional: &[SocketAddr],
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> io::Result<TcpStream> {
    let startup = Startup {
        deadline,
        timeout: CONNECT_TIMEOUT,
        cancelled: Some(&cancelled),
        before_authentication: None,
        keepalive: None,
    };
    let tcp = connect_tcp(primary, additional, startup)?;
    startup.remaining()?;
    Ok(tcp)
}

fn connect_tcp(
    primary: SocketAddr,
    additional: &[SocketAddr],
    startup: Startup<'_>,
) -> io::Result<TcpStream> {
    let mut last_error = io::Error::from(io::ErrorKind::TimedOut);
    for (index, address) in std::iter::once(primary)
        .chain(additional.iter().copied())
        .enumerate()
    {
        startup.check_cancelled()?;
        let Some(remaining) = startup.deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let attempts = u32::try_from(additional.len() - index + 1).unwrap_or(u32::MAX);
        // Keep time for later A or AAAA records if the first address stalls.
        let timeout = (remaining / attempts)
            .max(Duration::from_millis(100))
            .min(remaining);
        if timeout.is_zero() {
            break;
        }
        let attempt = Startup {
            deadline: Instant::now() + timeout,
            ..startup
        };
        match connect_address(address, attempt) {
            Ok(tcp) => return Ok(tcp),
            Err(error) => last_error = error,
        }
    }
    startup.check_cancelled()?;
    Err(last_error)
}

fn connect_address(address: SocketAddr, startup: Startup<'_>) -> io::Result<TcpStream> {
    let mut socket = MioTcpStream::connect(address)?;
    let mut poll = Poll::new()?;
    poll.registry()
        .register(&mut socket, TRANSPORT, Interest::WRITABLE)?;
    let mut events = Events::with_capacity(1);
    loop {
        let wait = startup.wait_time()?;
        match poll.poll(&mut events, Some(wait)) {
            Ok(()) if !events.is_empty() => {}
            Ok(()) => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        if let Some(error) = socket.take_error()? {
            return Err(error);
        }
        match socket.peer_addr() {
            // A loopback connect can be reported as established when the kernel
            // selects the destination port as its own ephemeral source port.
            Ok(peer) if peer == socket.local_addr()? => {
                return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
            }
            Ok(_) => break,
            Err(error) if connect_pending(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    startup.remaining()?;
    poll.registry().deregister(&mut socket)?;
    let socket = TcpStream::from(socket);
    socket.set_nonblocking(false)?;
    Ok(socket)
}

fn connect_pending(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::NotConnected | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    // Stable std does not yet expose ErrorKind::InProgress. The desktop ABIs
    // define EINPROGRESS in errno.h, or WSAEINPROGRESS in winsock2.h.
    match error.raw_os_error() {
        #[cfg(target_os = "macos")]
        Some(36) => true,
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        Some(115) => true,
        #[cfg(windows)]
        Some(10036) => true,
        _ => false,
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Blocking output from one SSH channel stream.
pub struct SshReader {
    shared: Arc<Shared>,
    output: output::OutputReceiver,
    reported_error: bool,
}

enum ReadWait {
    Blocking,
    Nonblocking,
    Timeout(Duration),
}

impl SshReader {
    fn new(shared: &Arc<Shared>, output: output::OutputReceiver) -> Self {
        Self {
            shared: Arc::clone(shared),
            output,
            reported_error: false,
        }
    }

    /// Waits up to `timeout` for output if no bytes are buffered.
    pub fn read_timeout(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.read_inner(bytes, ReadWait::Timeout(timeout))
    }

    fn read_inner(&mut self, bytes: &mut [u8], wait: ReadWait) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = self.output.read(bytes, wait)?;
        if count == 0
            && !self.reported_error
            && let Some(error) = lock(&self.shared.error).clone()
        {
            self.reported_error = true;
            return Err(io::Error::new(error.kind(), error));
        }
        self.shared.consume_output(count);
        Ok(count)
    }
}

impl Drop for SshReader {
    fn drop(&mut self) {
        self.shared.consume_output(self.output.close());
    }
}

impl Read for SshReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.read_inner(bytes, ReadWait::Blocking)
    }
}

/// Blocking SSH channel input with bounded buffering and window backpressure.
pub struct SshWriter {
    shared: Arc<Shared>,
    input: SyncSender<Command>,
    input_closed: bool,
}

enum Command {
    Data(InputBytes),
    Flush(SyncSender<()>),
    Eof(SyncSender<()>),
}

enum InputBytes {
    Stream(Vec<u8>),
    Sensitive(SensitiveLine, Option<Box<dyn Fn() -> bool + Send + Sync>>),
}

impl AsRef<[u8]> for InputBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Stream(bytes) => bytes,
            Self::Sensitive(line, _) => line.as_bytes(),
        }
    }
}

impl InputBytes {
    fn permitted(&self) -> bool {
        match self {
            Self::Sensitive(_, Some(guard)) => guard(),
            _ => true,
        }
    }
}

impl SshWriter {
    /// Sends SSH EOF after pending input, leaving remote output readable.
    pub fn close_input(&mut self) -> io::Result<()> {
        if !self.input_closed {
            let (acknowledge, sent) = mpsc::sync_channel(1);
            self.queue(Command::Eof(acknowledge))?;
            self.input_closed = true;
            sent.recv().map_err(|_| closed())?;
        }
        Ok(())
    }

    fn queue(&self, command: Command) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        self.input.send(command).map_err(|_| closed())?;
        self.shared.wake.wake()
    }

    /// Queues input without waiting for space in the transport buffer.
    pub fn write_nonblocking(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = CHUNK_BYTES.min(bytes.len());
        if count == 0 {
            return Ok(0);
        }
        if self.input_closed || self.shared.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        self.input
            .try_send(Command::Data(InputBytes::Stream(bytes[..count].to_vec())))
            .map_err(|error| match error {
                TrySendError::Full(_) => io::ErrorKind::WouldBlock.into(),
                TrySendError::Disconnected(_) => closed(),
            })?;
        self.shared.wake.wake()?;
        Ok(count)
    }
}

impl Write for SshWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = CHUNK_BYTES.min(bytes.len());
        if count == 0 {
            return Ok(0);
        }
        if self.input_closed {
            return Err(closed());
        }
        self.queue(Command::Data(InputBytes::Stream(bytes[..count].to_vec())))?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let (acknowledge, drained) = mpsc::sync_channel(1);
        self.queue(Command::Flush(acknowledge))?;
        drained.recv().map_err(|_| closed())
    }
}

enum WireTransport {
    Tcp(mio::net::TcpStream),
    #[cfg(any(windows, target_os = "macos"))]
    LocalPipe(MioRelay),
    Forwarded(Box<ForwardedWire>),
}

struct ForwardedWire {
    _owner: SshSession,
    reader: SshReader,
    writer: SshWriter,
}

impl WireTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(bytes),
            #[cfg(any(windows, target_os = "macos"))]
            Self::LocalPipe(pipe) => pipe.read(bytes),
            Self::Forwarded(link) => link.reader.read_inner(bytes, ReadWait::Nonblocking),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.write(bytes),
            #[cfg(any(windows, target_os = "macos"))]
            Self::LocalPipe(pipe) => pipe.write(bytes),
            Self::Forwarded(link) => link.writer.write_nonblocking(bytes),
        }
    }
}

struct Wire {
    transport: WireTransport,
    poll: Poll,
    events: Events,
    runner: Runner<'static, Client>,
    incoming: [u8; CHUNK_BYTES],
    unread: Range<usize>,
    eof: bool,
    heartbeat: Option<Heartbeat>,
    liveness: Option<liveness::Probe>,
    inbound_activity: u64,
}

impl Wire {
    fn new(socket: TcpStream) -> io::Result<(Self, Arc<Waker>)> {
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;
        let mut socket = mio::net::TcpStream::from_std(socket);
        let poll = Poll::new()?;
        poll.registry().register(
            &mut socket,
            TRANSPORT,
            Interest::READABLE | Interest::WRITABLE,
        )?;
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL)?);
        Ok((Self::with_transport(WireTransport::Tcp(socket), poll), wake))
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn local_pipe(transport: PipeTransport) -> io::Result<(Self, Arc<Waker>)> {
        let pipe = transport.pipe;
        #[cfg(target_os = "macos")]
        let pipe = {
            pipe.set_nonblocking(true)?;
            MioRelay::from_std(pipe)
        };
        let mut pipe = pipe;
        let poll = Poll::new()?;
        poll.registry().register(
            &mut pipe,
            TRANSPORT,
            Interest::READABLE | Interest::WRITABLE,
        )?;
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL)?);
        Ok((
            Self::with_transport(WireTransport::LocalPipe(pipe), poll),
            wake,
        ))
    }

    fn forwarded(mut owner: SshSession) -> io::Result<(Self, Arc<Waker>)> {
        let reader = owner
            .take_reader()
            .ok_or_else(|| io::Error::other("SSH forward reader unavailable"))?;
        let writer = owner
            .take_writer()
            .ok_or_else(|| io::Error::other("SSH forward writer unavailable"))?;
        let poll = Poll::new()?;
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL)?);
        owner
            .shared
            .downstream
            .set(Arc::clone(&wake))
            .map_err(|_| io::Error::other("SSH forward already has a downstream transport"))?;
        let transport = WireTransport::Forwarded(Box::new(ForwardedWire {
            _owner: owner,
            reader,
            writer,
        }));
        Ok((Self::with_transport(transport, poll), wake))
    }

    fn with_transport(transport: WireTransport, poll: Poll) -> Self {
        Self {
            transport,
            poll,
            events: Events::with_capacity(8),
            runner: Runner::new_owned(),
            incoming: [0; CHUNK_BYTES],
            unread: 0..0,
            eof: false,
            heartbeat: None,
            liveness: None,
            inbound_activity: 0,
        }
    }

    fn handshake(
        &mut self,
        endpoint: &Endpoint,
        user: UserAuth<'_>,
        start: OpenRequest<'_>,
        trust: &TrustStore,
        output: &Outputs,
        mut startup: Startup<'_>,
    ) -> Result<(Option<ChanHandle>, HostKey), ConnectError> {
        startup.check_cancelled()?;
        if let Some(algorithm) = trust.pinned_algorithm(endpoint)? {
            self.runner.prefer_host_key(&algorithm)?;
        }
        let Authentication {
            mut credential,
            mut interactive,
        } = user.authentication;
        self.runner.client_auth_methods(
            matches!(credential, Some(Credential::PrivateKey(_))),
            matches!(credential, Some(Credential::Password(_))),
            interactive.is_some(),
        )?;
        let mut channel = None;
        let mut host_key = None;
        let mut authenticated = false;
        let mut start = Some(start);
        let mut awaiting_reply = None;
        let mut command_after_pty = None;
        let mut early_data = [0; CHUNK_BYTES];
        loop {
            startup.remaining()?;
            let progressed = match self.runner.progress()? {
                Event::Cli(CliEvent::Hostkey(request)) => {
                    host_key = Some(verify_host(request, endpoint, trust)?);
                    true
                }
                Event::Cli(
                    CliEvent::Username(_)
                    | CliEvent::Pubkey(_)
                    | CliEvent::AgentSign(_)
                    | CliEvent::Password(_)
                    | CliEvent::KeyboardChallenge(_)
                    | CliEvent::Authenticated
                    | CliEvent::SessionOpened(_),
                ) if host_key.is_none() => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "SSH authentication requires a verified host key",
                    )
                    .into());
                }
                Event::Cli(CliEvent::Username(request)) => {
                    startup.authorize_authentication()?;
                    request.username(user.username)?;
                    true
                }
                Event::Cli(CliEvent::Pubkey(request)) => {
                    if let Some(Credential::PrivateKey(key)) = credential.as_ref() {
                        startup.remaining()?;
                        let signing_key = key.signing_key()?;
                        startup.remaining()?;
                        startup.authorize_authentication()?;
                        if startup.before_authentication.is_some() {
                            request.pubkey_deferred(signing_key)?;
                        } else {
                            request.pubkey(signing_key)?;
                        }
                        credential = None;
                    } else {
                        request.skip()?;
                    }
                    true
                }
                Event::Cli(CliEvent::AgentSign(request))
                    if startup.before_authentication.is_some() =>
                {
                    startup.authorize_authentication()?;
                    let signature = request.sign_private()?;
                    startup.remaining()?;
                    request.signed(&signature)?;
                    true
                }
                Event::Cli(CliEvent::Password(request)) => {
                    if let Some(Credential::Password(password)) = credential.take() {
                        startup.authorize_authentication()?;
                        request.password(password.as_str())?;
                    } else {
                        request.skip()?;
                    }
                    true
                }
                Event::Cli(CliEvent::KeyboardChallenge(request)) => {
                    let handler = interactive.as_mut().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "SSH keyboard-interactive was not enabled",
                        )
                    })?;
                    let responses = handler.respond(
                        endpoint,
                        user.username,
                        request.challenge()?,
                        &mut startup,
                    )?;
                    let answers: Vec<_> = responses.iter().map(KeyboardResponse::as_str).collect();
                    startup.authorize_authentication()?;
                    request.respond(&answers)?;
                    true
                }
                Event::Cli(CliEvent::Authenticated) => {
                    if authenticated {
                        return Err(
                            io::Error::other("duplicate SSH authentication completion").into()
                        );
                    }
                    authenticated = true;
                    credential = None;
                    interactive = None;
                    if matches!(start, Some(OpenRequest::ExecOnly)) {
                        return host_key.map(|key| (None, key)).ok_or_else(|| {
                            io::Error::other("SSH authentication has no host key").into()
                        });
                    }
                    true
                }
                Event::Cli(CliEvent::SessionOpened(mut request)) => {
                    startup.check_cancelled()?;
                    startup.authorize_authentication()?;
                    if channel
                        .as_ref()
                        .is_none_or(|channel: &ChanHandle| channel.num() != request.channel())
                    {
                        return Err(io::Error::other("unexpected SSH session channel").into());
                    }
                    match start
                        .take()
                        .ok_or_else(|| io::Error::other("SSH session already opened"))?
                    {
                        OpenRequest::Terminal(pty) => {
                            request.pty(*pty)?;
                            request.cmd(&SessionCommand::<&str>::Shell)?;
                            return channel.map(Some).zip(host_key).ok_or_else(|| {
                                io::Error::other("SSH session did not open").into()
                            });
                        }
                        OpenRequest::Exec { command, pty } => {
                            if let Some(pty) = pty {
                                request.pty_confirmed(*pty)?;
                                command_after_pty = Some(command);
                                awaiting_reply = Some("pty-req");
                            } else {
                                request.cmd_confirmed(&SessionCommand::Exec(command))?;
                                awaiting_reply = Some("exec");
                            }
                        }
                        OpenRequest::Subsystem(name) => {
                            request.cmd_confirmed(&SessionCommand::Subsystem(name))?;
                            awaiting_reply = Some("subsystem");
                        }
                        OpenRequest::SftpWithTransferHelper => {
                            request.cmd_confirmed(&SessionCommand::Subsystem("sftp"))?;
                            awaiting_reply = Some("subsystem");
                        }
                        OpenRequest::Direct { .. } | OpenRequest::ExecOnly => {
                            return Err(io::Error::other(
                                "unexpected session opened for SSH direct channel",
                            )
                            .into());
                        }
                    }
                    true
                }
                Event::Cli(CliEvent::Banner(_) | CliEvent::PollAgain) | Event::Progressed => true,
                Event::Cli(CliEvent::ForwardedTcpip(request)) => {
                    request.reject()?;
                    true
                }
                Event::None => false,
                Event::Cli(CliEvent::Defunct | CliEvent::SessionExit(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "SSH server disconnected during setup",
                    )
                    .into());
                }
                Event::Cli(CliEvent::AgentSign(_)) | Event::Serv(_) => {
                    return Err(io::Error::other("unexpected SSH setup event").into());
                }
            };
            if authenticated && channel.is_none() {
                channel = match start.as_ref() {
                    Some(OpenRequest::Direct { target, origin }) => {
                        open_direct_channel(&mut self.runner, target, *origin)?
                    }
                    _ => Some(self.runner.open_client_session()?),
                };
            }
            if let Some(handle) = channel.as_ref() {
                if matches!(start, Some(OpenRequest::Direct { .. })) {
                    match self.runner.direct_status(handle)? {
                        DirectStatus::Open => {
                            return channel.map(Some).zip(host_key).ok_or_else(|| {
                                io::Error::other("SSH direct channel did not open").into()
                            });
                        }
                        DirectStatus::Refused(_) => {
                            return Err(ConnectError::RequestRejected {
                                request: "direct-tcpip",
                            });
                        }
                        DirectStatus::Opening => {}
                    }
                } else {
                    let result = self.runner.client_request_result(handle)?;
                    if let Some(request) = awaiting_reply {
                        match result {
                            Some(true) => {
                                if let Some(command) = command_after_pty {
                                    startup.check_cancelled()?;
                                    startup.authorize_authentication()?;
                                    if self.runner.client_exec_after_pty(handle, command)? {
                                        command_after_pty = None;
                                        awaiting_reply = Some("exec");
                                        continue;
                                    }
                                } else {
                                    return channel.map(Some).zip(host_key).ok_or_else(|| {
                                        io::Error::other("SSH session did not open").into()
                                    });
                                }
                            }
                            Some(false) => return Err(ConnectError::RequestRejected { request }),
                            None if self.runner.is_channel_closed(handle) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "SSH channel closed before request confirmation",
                                )
                                .into());
                            }
                            None => {
                                if self.runner.read_channel_ready().is_some() {
                                    let (count, kind) = self
                                        .runner
                                        .read_channel_either_deferred(handle, &mut early_data);
                                    if count != 0 {
                                        let discarded = output.send(kind, &early_data[..count])?;
                                        self.runner.release_channel_window(handle, discarded)?;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            startup.check_cancelled()?;
            if !self.exchange()? && !progressed {
                self.wait(Some(startup.wait_time()?))?;
            }
        }
    }

    fn exchange(&mut self) -> Result<bool, ConnectError> {
        let mut progress = false;
        while self.runner.is_output_pending() {
            let bytes = self.runner.output_buf();
            match self.transport.write(bytes) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => {
                    self.runner.consume_output(count);
                    progress = true;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        if !self.eof && self.runner.is_input_ready() {
            if self.unread.is_empty() {
                match self.transport.read(&mut self.incoming) {
                    Ok(0) => {
                        self.eof = true;
                        self.runner.close_input();
                        return Ok(true);
                    }
                    Ok(count) => {
                        self.inbound_activity = self.inbound_activity.wrapping_add(1);
                        self.unread = 0..count;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(progress),
                    Err(error) => return Err(error.into()),
                }
            }
            let count = self.runner.input(&self.incoming[self.unread.clone()])?;
            self.unread.start += count;
            progress |= count != 0;
        }
        Ok(progress)
    }

    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self.poll.poll(&mut self.events, timeout) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(()),
            result => result,
        }
    }
}

fn open_direct_channel(
    runner: &mut Runner<'_, Client>,
    target: &Endpoint,
    origin: SocketAddr,
) -> sunset::Result<Option<ChanHandle>> {
    runner.open_direct_tcpip(
        target.wire_host().as_ref(),
        target.port(),
        &origin.ip().to_string(),
        origin.port(),
    )
}

fn verify_host(
    request: CheckHostkey<'_, '_>,
    endpoint: &Endpoint,
    trust: &TrustStore,
) -> Result<HostKey, ConnectError> {
    let offered = HostKey::try_from(&request.hostkey()?)?;
    let status = trust.check(endpoint, &offered)?;
    if let HostKeyStatus::Changed { expected } = status {
        return Err(changed(endpoint, expected, offered));
    }
    // Sunset verifies the KEX signature in accept(). The pump cannot send
    // credentials or advance auth until this function has also checked the pin.
    request.accept()?;
    if status == HostKeyStatus::Unknown {
        match trust.mode() {
            TrustMode::Strict => {
                return Err(ConnectError::UnknownHostKey {
                    endpoint: endpoint.clone(),
                    offered,
                });
            }
            TrustMode::AcceptNew => {
                if let HostKeyStatus::Changed { expected } = trust.remember(endpoint, &offered)? {
                    return Err(changed(endpoint, expected, offered));
                }
            }
        }
    }
    Ok(offered)
}

fn changed(endpoint: &Endpoint, expected: HostKey, offered: HostKey) -> ConnectError {
    ConnectError::ChangedHostKey {
        endpoint: endpoint.clone(),
        expected,
        offered,
    }
}

struct Outputs {
    mode: ChannelMode,
    stdout: Option<output::OutputSender>,
    stderr: Option<output::OutputSender>,
}

impl Outputs {
    /// Delivers bytes and returns the number intentionally discarded.
    fn send(&self, kind: ChanData, data: &[u8]) -> io::Result<usize> {
        let stream = match (kind, self.mode) {
            (ChanData::Normal, _)
            | (ChanData::Stderr, ChannelMode::Terminal | ChannelMode::ExecPty) => {
                self.stdout.as_ref()
            }
            (ChanData::Stderr, ChannelMode::Exec | ChannelMode::SensitiveExec) => {
                self.stderr.as_ref()
            }
            (
                ChanData::Stderr,
                ChannelMode::Subsystem
                | ChannelMode::SftpLease
                | ChannelMode::Direct
                | ChannelMode::ExecOnly,
            ) => None,
        };
        let Some(stream) = stream else {
            return Ok(data.len());
        };
        match stream.send(data) {
            Ok(()) => Ok(0),
            Err(error)
                if matches!(self.mode, ChannelMode::Exec | ChannelMode::SensitiveExec)
                    && error.kind() == io::ErrorKind::BrokenPipe =>
            {
                Ok(data.len())
            }
            Err(error) => Err(error),
        }
    }
}

struct ChannelPump {
    channel: ChanHandle,
    shared: Arc<Shared>,
    input: Receiver<Command>,
    output: Outputs,
    pending_input: Option<Command>,
    input_offset: usize,
    wrote_input: bool,
    pending_size: Option<WindowSize>,
}

enum PumpStep {
    Active(bool),
    Done,
}

struct Heartbeat {
    interval: Duration,
    deadline: Instant,
}

impl Heartbeat {
    fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval,
            deadline: now + interval,
        }
    }

    fn defer(&mut self, now: Instant) {
        self.deadline = now + self.interval;
    }

    fn due(&mut self, now: Instant) -> bool {
        if now < self.deadline {
            return false;
        }
        self.defer(now);
        true
    }

    fn wait_time(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

#[cfg(test)]
#[test]
fn keepalive_deadline_defers_for_real_traffic_and_never_catches_up() {
    let start = Instant::now();
    let interval = Duration::from_secs(5);
    let mut heartbeat = Heartbeat::new(interval, start);
    let traffic = start + Duration::from_secs(4);
    heartbeat.defer(traffic);
    assert!(!heartbeat.due(start + interval));
    assert!(heartbeat.due(traffic + interval));
    let after_pause = start + Duration::from_secs(60);
    assert!(heartbeat.due(after_pause));
    assert!(!heartbeat.due(after_pause));
    assert!(heartbeat.due(after_pause + interval));
    assert!(KeepAlive::new(true, 4).is_err());
    assert!(KeepAlive::new(false, 601).is_err());
}

impl ChannelPump {
    fn new(
        channel: ChanHandle,
        shared: Arc<Shared>,
        input: Receiver<Command>,
        output: Outputs,
    ) -> Self {
        Self {
            channel,
            shared,
            input,
            output,
            pending_input: None,
            input_offset: 0,
            wrote_input: false,
            pending_size: None,
        }
    }

    fn step(
        &mut self,
        runner: &mut Runner<'_, Client>,
        buffer: &mut [u8],
        output_drained: bool,
    ) -> io::Result<PumpStep> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Ok(PumpStep::Done);
        }
        let mut progressed = false;
        let consumed = self.shared.consumed_output.swap(0, Ordering::AcqRel);
        runner
            .release_channel_window(&self.channel, consumed)
            .map_err(io::Error::other)?;
        if runner
            .read_channel_ready()
            .is_some_and(|(channel, _, _)| channel == self.channel.num())
        {
            let sensitive = self.output.mode == ChannelMode::SensitiveExec;
            let (count, kind) = if sensitive {
                runner.read_sensitive_channel_either_deferred(&self.channel, buffer)
            } else {
                runner.read_channel_either_deferred(&self.channel, buffer)
            };
            if self.output.mode == ChannelMode::SftpLease
                && kind == ChanData::Normal
                && !self.wrote_input
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SFTP peer sent data before the client request",
                ));
            }
            let sent = self.output.send(kind, &buffer[..count]);
            if sensitive {
                use zeroize::Zeroize as _;
                buffer[..count].zeroize();
            }
            let discarded = match sent {
                Ok(discarded) => discarded,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    return Ok(PumpStep::Done);
                }
                Err(error) => return Err(error),
            };
            runner
                .release_channel_window(&self.channel, discarded)
                .map_err(io::Error::other)?;
            self.shared.notify_downstream();
            progressed = true;
        }
        if runner.is_channel_closed(&self.channel)
            || (self.output.mode == ChannelMode::SftpLease && runner.is_channel_eof(&self.channel))
        {
            return Ok(PumpStep::Done);
        }
        if self.output.mode == ChannelMode::Direct
            && runner.is_channel_eof(&self.channel)
            && self.output.stdout.take().is_some()
        {
            self.shared.notify_downstream();
            progressed = true;
        }
        if let Some(size) = lock(&self.shared.resize).take() {
            self.pending_size = Some(size);
        }
        if let Some(size) = self.pending_size {
            match runner.term_window_change(&self.channel, &size.winchange()) {
                Ok(()) => {
                    self.pending_size = None;
                    progressed = true;
                }
                Err(sunset::Error::NoRoom { .. }) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        if self.pending_input.is_none() {
            self.pending_input = self.input.try_recv().ok();
            self.input_offset = 0;
            if self.pending_input.is_some() {
                self.shared.notify_downstream();
            }
        }
        match &self.pending_input {
            Some(Command::Data(data)) => {
                if !data.permitted() {
                    return Ok(PumpStep::Done);
                }
                let data = data.as_ref();
                let count = runner
                    .write_channel(&self.channel, ChanData::Normal, &data[self.input_offset..])
                    .map_err(io::Error::other)?;
                self.input_offset += count;
                self.wrote_input |= count != 0;
                progressed |= count != 0;
                if self.input_offset == data.len() {
                    self.pending_input = None;
                }
            }
            Some(Command::Flush(acknowledge)) if output_drained => {
                let _ = acknowledge.try_send(());
                self.pending_input = None;
                progressed = true;
            }
            Some(Command::Eof(acknowledge)) => match runner.channel_send_eof(&self.channel) {
                Ok(true) => {
                    let _ = acknowledge.try_send(());
                    self.pending_input = None;
                    progressed = true;
                }
                Ok(false) | Err(sunset::Error::NoRoom { .. }) => {}
                Err(error) => return Err(io::Error::other(error)),
            },
            _ => {}
        }
        Ok(PumpStep::Active(progressed))
    }

    fn finish(self, result: io::Result<()>) -> ChanHandle {
        let Self {
            channel,
            shared,
            input,
            output,
            ..
        } = self;
        shared.finish(result);
        drop(input);
        drop(output);
        shared.notify_downstream();
        channel
    }
}

fn drive_connection(
    wire: &mut Wire,
    (shared, mut main): (&Arc<Shared>, Option<&mut ChannelPump>),
    endpoint: &Endpoint,
    trust: &TrustStore,
    (requests, transfer_helper): (Receiver<forward::OpenForward>, bool),
    remote_requests: Receiver<remote::BindRequest>,
    mut side_channels: auxiliary::Channels,
) -> io::Result<()> {
    let mut buffer = [0; CHUNK_BYTES];
    let mut forwards = forward::Forwards::new(requests, transfer_helper);
    let mut remotes = remote::Remotes::new(remote_requests);
    loop {
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        let output_drained = !wire.runner.is_output_pending();
        let mut progressed = match wire.runner.progress().map_err(io::Error::other)? {
            Event::Cli(CliEvent::Hostkey(request)) => {
                let key = verify_host(request, endpoint, trust).map_err(io::Error::other)?;
                if let Some(initial) = shared.host_key.get()
                    && initial != &key
                {
                    return Err(io::Error::other("SSH peer key changed during the session"));
                }
                true
            }
            Event::Cli(CliEvent::SessionExit(CliSessionExit::Status { channel, status })) => {
                if main
                    .as_ref()
                    .is_some_and(|main| channel == main.channel.num())
                {
                    shared.exit_code.store(u64::from(status), Ordering::Release);
                } else {
                    side_channels.exit_status(channel, status);
                }
                true
            }
            Event::Cli(CliEvent::ForwardedTcpip(request)) => {
                remotes
                    .accept(request, &mut forwards)
                    .map_err(io::Error::other)?;
                true
            }
            Event::Cli(CliEvent::Defunct) if main.is_none() => {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            Event::Cli(CliEvent::Defunct) | Event::None => false,
            Event::Cli(CliEvent::Banner(_) | CliEvent::PollAgain | CliEvent::SessionExit(_))
            | Event::Progressed => true,
            Event::Cli(_) | Event::Serv(_) => {
                return Err(io::Error::other("unexpected SSH channel event"));
            }
        };
        if let Some(main) = main.as_mut() {
            match main.step(&mut wire.runner, &mut buffer, output_drained)? {
                PumpStep::Active(made_progress) => progressed |= made_progress,
                PumpStep::Done => {
                    if !wire.eof {
                        let _ = wire.exchange();
                    }
                    break;
                }
            }
        }
        progressed |= side_channels.step(&mut wire.runner, shared, &mut buffer, output_drained);
        progressed |= forwards.step(&mut wire.runner, shared, &mut buffer, output_drained)?;
        let mut liveness_progressed = wire
            .liveness
            .as_mut()
            .is_some_and(|probe| probe.receive(&mut wire.runner, Instant::now()));
        let overdue = wire.liveness.as_ref().is_some_and(liveness::Probe::overdue);
        progressed |= remotes.step(&mut wire.runner, shared, overdue)?;
        let exchanged = wire.exchange().map_err(io::Error::other)?;
        let liveness_now = Instant::now();
        if let Some(probe) = &mut wire.liveness {
            liveness_progressed |= probe.progress(
                &mut wire.runner,
                liveness_now,
                wire.inbound_activity,
                !wire.eof && wire.unread.is_empty(),
            )?;
        }
        // Application/channel traffic postpones the idle heartbeat. The liveness
        // probe runs independently and must not suppress the user's heartbeat.
        if progressed && let Some(heartbeat) = &mut wire.heartbeat {
            heartbeat.defer(liveness_now);
        }
        if exchanged || progressed || liveness_progressed {
            continue;
        }
        if let Some(heartbeat) = &mut wire.heartbeat
            && heartbeat.due(Instant::now())
            && !wire.eof
            && wire.unread.is_empty()
            && wire.runner.read_channel_ready().is_none()
            && wire.runner.try_send_keepalive().map_err(io::Error::other)?
        {
            continue;
        }
        wire.wait(
            forwards
                .wait_time()
                .into_iter()
                .chain(remotes.wait_time())
                .chain(side_channels.wait_time())
                .chain(wire.heartbeat.as_ref().map(Heartbeat::wait_time))
                .chain(
                    wire.liveness
                        .as_ref()
                        .and_then(|probe| probe.wait_time(liveness_now)),
                )
                .min(),
        )?;
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "SSH session is closed")
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod local_pipe_tests {
    #[cfg(windows)]
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader};

    use super::*;

    const ORIGIN: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 3022);

    #[cfg(windows)]
    fn connected_pipe() -> (PipeTransport, File) {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let name = format!(
            r"\\.\pipe\kortty-ssh-test-{}-{:032x}",
            std::process::id(),
            u128::from_ne_bytes(nonce),
        );
        let pipe = mio::windows::NamedPipe::new(&name).unwrap();
        let peer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        (PipeTransport::new(pipe, ORIGIN), peer)
    }

    #[cfg(target_os = "macos")]
    fn connected_pipe() -> (PipeTransport, UnixStream) {
        let (pipe, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (PipeTransport::new(pipe, ORIGIN), peer)
    }

    fn target() -> SshTarget {
        SshTarget {
            endpoint: Endpoint::new("localhost", 22).unwrap(),
            username: "test-only".into(),
            authentication: Authentication::password("unused".into()),
        }
    }

    #[test]
    fn local_pipe_exchanges_ssh_identification_and_kex_before_eof() {
        let (pipe, peer) = connected_pipe();
        let peer = thread::spawn(move || {
            // A test peer stops before host-key verification or authentication.
            let mut peer = BufReader::new(peer);
            let mut ident = Vec::new();
            peer.read_until(b'\n', &mut ident).unwrap();
            assert!(ident.starts_with(b"SSH-2.0-"));
            peer.get_mut().write_all(b"SSH-2.0-test-pipe").unwrap();
            peer.get_mut().write_all(b"\r\n").unwrap();
            let mut prefix = [0; 6];
            peer.read_exact(&mut prefix).unwrap();
            assert_eq!(prefix[5], 20); // SSH_MSG_KEXINIT
        });
        let result = SshSession::connect_pipe_cancellable(
            pipe,
            target(),
            Vec::new(),
            TrustStore::from_pins(Vec::new()).unwrap(),
            "xterm-256color".into(),
            WindowSize {
                columns: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            || false,
        );
        assert!(
            matches!(
                result,
                Err(ConnectError::Io(error)) if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                )
            ),
            "expected the peer's pipe closure during SSH setup"
        );
        peer.join().unwrap();
    }

    #[test]
    fn local_pipe_cancels_a_stalled_subsystem_handshake() {
        let (pipe, _peer) = connected_pipe();
        let deadline = Instant::now() + Duration::from_millis(100);
        let result = SshSession::connect_subsystem_pipe_cancellable(
            pipe,
            target(),
            Vec::new(),
            TrustStore::from_pins(Vec::new()).unwrap(),
            "sftp",
            || Instant::now() >= deadline,
        );
        assert!(matches!(
            result,
            Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::Interrupted
        ));
    }
}
