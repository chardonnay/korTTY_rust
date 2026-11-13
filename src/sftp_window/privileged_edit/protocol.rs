//! Bounded file exchange on a dedicated exec channel of an authenticated SSH connection.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kortty::ssh_runtime::{mux::PasswordFileEvent, password, password_file};
use kortty_ssh::{ExecLease, SensitiveLine, SshReader, SshWriter, sftp::MAX_EDIT_BYTES};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{ssh_connection::ExecOpener, ssh_worker};

mod transfer;
mod tree;
mod tree_download;
pub(crate) use transfer::{MAX_TRANSFER_BYTES, download, upload};
pub(crate) use tree::{TreeUpload, inspect_tree, upload_tree};
pub(crate) use tree_download::{download_tree, preview_tree_download};

const MAX_HEADER: usize = 8192;
const MAX_PATH: usize = 2048;
const POLL: Duration = Duration::from_millis(40);
const CHUNK: usize = kortty::ssh_runtime::wire::CHUNK;

pub enum Auth {
    Passwordless,
    Password(SensitiveLine),
}

#[derive(Clone)]
pub struct Document {
    opener: ExecOpener,
    path: String,
    text: Arc<str>,
    version: String,
}

impl Document {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn text(&self) -> &Arc<str> {
        &self.text
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivilegedDocument")
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Blocked,
    Conflict,
    Cancelled,
    Uncertain,
    Failed,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Blocked => "The SSH route, sudo authentication, file or remote helper is unavailable for privileged editing.",
            Self::Conflict => "The remote file or its directory changed. Reopen it before uploading.",
            Self::Cancelled => "The privileged edit was cancelled before publication.",
            Self::Uncertain => "The server may have published the edit. Reopen and verify the file before uploading again.",
            Self::Failed => "The privileged file exchange failed before publication.",
        })
    }
}

impl std::error::Error for Failure {}

pub fn valid_path(path: &str) -> bool {
    if path.len() > MAX_PATH || path.chars().any(char::is_control) {
        return false;
    }
    let Some(parts) = path.strip_prefix('/') else {
        return false;
    };
    parts.split('/').count() <= 64
        && parts
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && part.len() <= 255)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ProtectedOperation {
    Mkdir,
    Chmod { mode: u16 },
    Chown { uid: Option<u32>, gid: Option<u32> },
    Copy { to: String },
    CopyTree { to: String },
    Rename { to: String },
    Delete { recursive: bool },
}

impl ProtectedOperation {
    pub(crate) fn valid_for(&self, path: &str) -> bool {
        if !valid_path(path) {
            return false;
        }
        match self {
            Self::Mkdir => true,
            Self::Chmod { mode } => *mode <= 0o777,
            Self::Chown { uid, gid } => {
                (uid.is_some() || gid.is_some()) && *uid != Some(u32::MAX) && *gid != Some(u32::MAX)
            }
            Self::Delete { .. } => true,
            Self::Copy { to } | Self::CopyTree { to } | Self::Rename { to } => {
                to != path
                    && valid_path(to)
                    && (!matches!(self, Self::CopyTree { .. })
                        || !to
                            .strip_prefix(path)
                            .is_some_and(|rest| rest.starts_with('/')))
                    && serde_json::to_vec(&Request::FileAction { path, action: self })
                        .is_ok_and(|header| header.len() <= MAX_HEADER)
            }
        }
    }

    pub(crate) fn destination(&self) -> Option<&str> {
        match self {
            Self::Copy { to } | Self::CopyTree { to } | Self::Rename { to } => Some(to),
            _ => None,
        }
    }
}

pub(crate) fn apply_file_action(
    opener: ExecOpener,
    path: &str,
    action: &ProtectedOperation,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
    commit_allowed: impl FnOnce() -> bool,
) -> Result<(), Failure> {
    if !action.valid_for(path) {
        return Err(Failure::Blocked);
    }
    let mut connection = Connection::open(&opener, auth, allowed)?;
    connection.send(&Request::FileAction { path, action })?;
    let token = match connection.response()? {
        Response::Ready { token } if lowercase_hex(&token, 32) => token,
        response => return Err(connection.rejection(response, Failure::Failed)),
    };
    if !commit_allowed() {
        return Err(Failure::Blocked);
    }
    connection.gate.check().map_err(|_| connection.failure())?;
    connection
        .write_all(token.as_bytes())
        .map_err(|_| Failure::Uncertain)?;
    match connection.response().map_err(|_| Failure::Uncertain)? {
        Response::FileActionApplied => {}
        response => return Err(connection.rejection(response, Failure::Uncertain)),
    }
    connection.finish().map_err(|_| Failure::Uncertain)
}

pub fn read(
    opener: ExecOpener,
    path: &str,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<Document, Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let mut connection = Connection::open(&opener, auth, allowed)?;
    connection.send(&Request::Read { path })?;
    let header = match connection.response()? {
        Response::Read(header) if header.valid() => header,
        response => return Err(connection.rejection(response, Failure::Failed)),
    };
    let mut bytes = Zeroizing::new(vec![0; header.size]);
    connection
        .read_exact(&mut bytes)
        .map_err(|_| connection.failure())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Failed)?;
    if !valid_text(text) || hash(text.as_bytes())? != header.sha256 {
        return Err(Failure::Failed);
    }
    connection.finish().map_err(|_| connection.failure())?;
    Ok(Document {
        opener,
        path: path.into(),
        text: Arc::from(text),
        version: header.version,
    })
}

pub fn write(
    opener: ExecOpener,
    opened: &Document,
    text: Arc<str>,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<Document, Failure> {
    if !opener.same_connection(&opened.opener) || !valid_text(&text) {
        return Err(Failure::Blocked);
    }
    let sha256 = hash(text.as_bytes())?;
    let mut connection = Connection::open(&opener, auth, allowed)?;
    connection.send(&Request::Write {
        path: &opened.path,
        size: text.len(),
        sha256: &sha256,
        version: &opened.version,
    })?;
    match connection.response()? {
        Response::Accept => {}
        response => return Err(connection.rejection(response, Failure::Failed)),
    }
    connection
        .write_all(text.as_bytes())
        .map_err(|_| connection.failure())?;
    let token = match connection.response()? {
        Response::Ready { token } if lowercase_hex(&token, 32) => token,
        response => return Err(connection.rejection(response, Failure::Failed)),
    };
    connection.gate.check().map_err(|_| connection.failure())?;
    // Queuing the reviewed token is the publication boundary. A lost
    // acknowledgement after this point leaves the outcome uncertain.
    connection
        .write_all(token.as_bytes())
        .map_err(|_| Failure::Uncertain)?;
    let header = match connection.response().map_err(|_| Failure::Uncertain)? {
        Response::Written(header)
            if header.valid() && header.size == text.len() && header.sha256 == sha256 =>
        {
            header
        }
        response => return Err(connection.rejection(response, Failure::Uncertain)),
    };
    connection.finish().map_err(|_| Failure::Uncertain)?;
    Ok(Document {
        opener,
        path: opened.path.clone(),
        text,
        version: header.version,
    })
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request<'a> {
    Read {
        path: &'a str,
    },
    Write {
        path: &'a str,
        size: usize,
        sha256: &'a str,
        version: &'a str,
    },
    FileAction {
        path: &'a str,
        action: &'a ProtectedOperation,
    },
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Hello {
        protocol: u8,
    },
    Read(Body),
    Accept,
    Ready {
        token: String,
    },
    Written(Body),
    FileActionApplied,
    Download {
        size: u64,
    },
    Downloaded {
        size: u64,
        sha256: String,
    },
    Created {
        size: u64,
        sha256: String,
    },
    TreeTarget {
        witness: String,
        token: Option<String>,
    },
    TreeCreated {
        entries: usize,
        size: u64,
    },
    TreeManifest {
        witness: String,
        entries: usize,
        size: u64,
    },
    TreeDirectory {
        path: String,
    },
    TreeFile {
        path: String,
        size: u64,
        sha256: String,
    },
    TreeRead {
        witness: String,
    },
    Blocked,
    Conflict,
    Failed,
    Uncertain,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    size: usize,
    sha256: String,
    version: String,
}

impl Body {
    fn valid(&self) -> bool {
        self.size <= MAX_EDIT_BYTES
            && lowercase_hex(&self.sha256, 64)
            && lowercase_hex(&self.version, 64)
    }
}

fn decode(reader: &mut impl Read) -> io::Result<Response> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if !(1..=MAX_HEADER).contains(&size) {
        return Err(invalid());
    }
    let mut frame = Zeroizing::new(vec![0; size]);
    reader.read_exact(&mut frame)?;
    serde_json::from_slice(&frame).map_err(|_| invalid())
}

fn valid_text(text: &str) -> bool {
    text.len() <= MAX_EDIT_BYTES && !text.as_bytes().contains(&0)
}

fn lowercase_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash(bytes: &[u8]) -> Result<String, Failure> {
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(|_| Failure::Failed)?;
    hash.update(bytes);
    digest_hex(hash.finalize().as_ref())
}

fn digest_hex(bytes: &[u8]) -> Result<String, Failure> {
    use fmt::Write as _;
    let mut value = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        write!(value, "{byte:02x}").map_err(|_| Failure::Failed)?;
    }
    Ok(value)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid privileged file protocol",
    )
}

#[derive(Clone)]
struct Gate {
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    deadline: Instant,
}

impl Gate {
    fn running(&self) -> bool {
        (self.allowed)() && Instant::now() < self.deadline
    }

    fn check(&self) -> io::Result<()> {
        if !self.running() {
            return Err(io::Error::other("Privileged file exchange ended"));
        }
        Ok(())
    }
}

struct Connection {
    gate: Gate,
    channel: Channel,
}

enum Channel {
    Direct(Box<Direct>),
    Worker(Box<Worker>),
}

struct Worker {
    channel: ssh_worker::Channel,
    output: Tagged,
    input: ssh_worker::Writer,
}

struct Direct {
    lease: ExecLease,
    output: SshReader,
    stderr: Option<SshReader>,
    input: Option<SshWriter>,
    challenge: Option<Challenge>,
}

struct Challenge {
    prompt: Prompt,
    line: Option<SensitiveLine>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Helper {
    Edit,
    Transfer,
}

impl Connection {
    fn open(
        opener: &ExecOpener,
        auth: Auth,
        allowed: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Failure> {
        Self::open_for(opener, auth, allowed, Helper::Edit)
    }

    fn open_for(
        opener: &ExecOpener,
        auth: Auth,
        allowed: impl Fn() -> bool + Send + Sync + 'static,
        helper: Helper,
    ) -> Result<Self, Failure> {
        let gate = Gate {
            allowed: Arc::new(allowed),
            deadline: Instant::now() + password_file::DEADLINE,
        };
        if !gate.running() {
            return Err(Failure::Cancelled);
        }
        if opener.is_closed() || opener.verified_host_key().is_none() {
            return Err(Failure::Blocked);
        }
        let opening = gate.clone();
        let (channel, greeted) = match opener {
            ExecOpener::Direct(opener) => {
                let challenge = match auth {
                    Auth::Passwordless => None,
                    Auth::Password(line) => Some(Challenge {
                        prompt: Prompt::new(password::marker(random_nonce()?)),
                        line: Some(line),
                    }),
                };
                let marker = challenge
                    .as_ref()
                    .map(|challenge| challenge.prompt.marker.as_str());
                let command = match helper {
                    Helper::Edit => command(marker),
                    Helper::Transfer => {
                        password_file::transfer_command(marker).map_err(|_| Failure::Blocked)
                    }
                }?;
                let mut lease = if challenge.is_some() {
                    opener.open_sensitive_cancellable(&command, move || !opening.running())
                } else {
                    opener.open_cancellable(&command, move || !opening.running())
                }
                .map_err(|_| blocked_or_cancelled(&gate))?;
                let output = lease.take_reader().ok_or(Failure::Blocked)?;
                let stderr = lease.take_stderr();
                (
                    Channel::Direct(Box::new(Direct {
                        lease,
                        output,
                        stderr,
                        input: None,
                        challenge,
                    })),
                    false,
                )
            }
            ExecOpener::Worker(opener) => {
                if helper == Helper::Transfer {
                    return Err(Failure::Blocked);
                }
                let (mut channel, greeted) = match auth {
                    Auth::Passwordless => (
                        opener
                            .open_file_exec(move || !opening.running())
                            .map_err(|_| blocked_or_cancelled(&gate))?,
                        false,
                    ),
                    Auth::Password(line) => (worker_password(opener, line, &gate)?, true),
                };
                let output = Tagged::new(channel.take_reader().ok_or(Failure::Blocked)?);
                let input = channel.take_writer().ok_or(Failure::Blocked)?;
                (
                    Channel::Worker(Box::new(Worker {
                        channel,
                        output,
                        input,
                    })),
                    greeted,
                )
            }
        };
        let mut connection = Self { gate, channel };
        if !greeted && !matches!(decode(&mut connection), Ok(Response::Hello { protocol: 1 })) {
            return Err(blocked_or_cancelled(&connection.gate));
        }
        if let Channel::Direct(direct) = &mut connection.channel {
            direct.authenticated()?;
        }
        if helper == Helper::Transfer {
            connection.gate.deadline = Instant::now() + password_file::TRANSFER_DEADLINE;
        }
        Ok(connection)
    }

    fn send(&mut self, request: &impl Serialize) -> Result<(), Failure> {
        let bytes = serde_json::to_vec(request).map_err(|_| Failure::Failed)?;
        if bytes.len() > MAX_HEADER {
            return Err(Failure::Blocked);
        }
        self.write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|()| self.write_all(&bytes))
            .map_err(|_| self.failure())
    }

    fn response(&mut self) -> Result<Response, Failure> {
        decode(self).map_err(|_| self.failure())
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.read(&mut [0])? != 0 {
            return Err(invalid());
        }
        loop {
            self.gate.check()?;
            let status = match &self.channel {
                Channel::Direct(direct) => direct.lease.exit_code(),
                Channel::Worker(worker) => worker.channel.exit_code(),
            };
            if let Some(status) = status {
                return if status == 0 { Ok(()) } else { Err(invalid()) };
            }
            thread::sleep(POLL);
        }
    }

    fn rejection(&mut self, response: Response, fallback: Failure) -> Failure {
        let rejection = match response {
            Response::Blocked => Failure::Blocked,
            Response::Conflict => Failure::Conflict,
            Response::Failed => Failure::Failed,
            Response::Uncertain => Failure::Uncertain,
            _ => return fallback,
        };
        if self.finish().is_ok() {
            rejection
        } else {
            fallback
        }
    }

    fn failure(&self) -> Failure {
        if (self.gate.allowed)() {
            Failure::Failed
        } else {
            Failure::Cancelled
        }
    }
}

fn random_nonce() -> Result<[u8; 16], Failure> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| Failure::Failed)?;
    Ok(nonce)
}

fn worker_password(
    opener: &ssh_worker::Opener,
    line: SensitiveLine,
    gate: &Gate,
) -> Result<ssh_worker::Channel, Failure> {
    let mut line = Some(line);
    let opening = gate.clone();
    let channel = opener
        .open_password_file_exec(random_nonce()?, move || !opening.running())
        .map_err(|_| blocked_or_cancelled(gate))?;
    while gate.running() {
        match channel.next(POLL).map_err(|_| blocked_or_cancelled(gate))? {
            Some(PasswordFileEvent::Challenge) => {
                let line = line.take().ok_or(Failure::Blocked)?;
                let permitted = gate.clone();
                channel
                    .stage(line, move || permitted.running())
                    .map_err(|_| blocked_or_cancelled(gate))?;
            }
            Some(PasswordFileEvent::Staged) => {
                let permitted = gate.clone();
                channel
                    .commit(move || permitted.running())
                    .map_err(|_| blocked_or_cancelled(gate))?;
            }
            Some(PasswordFileEvent::Ready) => {
                gate.check().map_err(|_| blocked_or_cancelled(gate))?;
                return channel.into_channel().map_err(|_| Failure::Blocked);
            }
            Some(PasswordFileEvent::Finished(_)) => return Err(blocked_or_cancelled(gate)),
            None => {}
        }
    }
    Err(blocked_or_cancelled(gate))
}

fn blocked_or_cancelled(gate: &Gate) -> Failure {
    if (gate.allowed)() {
        Failure::Blocked
    } else {
        Failure::Cancelled
    }
}

fn command(marker: Option<&str>) -> Result<String, Failure> {
    password_file::command(marker).map_err(|_| Failure::Blocked)
}

impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.gate.check()?;
            let result = match &mut self.channel {
                Channel::Direct(direct) => {
                    direct.stderr(&self.gate)?;
                    direct.output.read_timeout(bytes, POLL)
                }
                Channel::Worker(worker) => worker.output.read_stdout(bytes, &self.gate),
            };
            match result {
                Err(error) if retry(&error) => {}
                result => return result,
            }
        }
    }
}

impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.gate.check()?;
            let result = match &mut self.channel {
                Channel::Direct(direct) => {
                    direct.stderr(&self.gate)?;
                    direct
                        .input
                        .as_mut()
                        .ok_or_else(invalid)?
                        .write_nonblocking(bytes)
                }
                Channel::Worker(worker) => worker.input.write_timeout(bytes, POLL),
            };
            match result {
                Err(error) if retry(&error) => thread::sleep(POLL),
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Both lanes queue ordered binary data directly. Waiting for an unbounded
        // explicit transport flush would defeat the operation deadline.
        self.gate.check()
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl Direct {
    fn authenticated(&mut self) -> Result<(), Failure> {
        if let Some(challenge) = &mut self.challenge {
            challenge.line = None;
        }
        if self.input.is_none() {
            self.input = self.lease.take_writer();
        }
        self.input.as_ref().map(|_| ()).ok_or(Failure::Blocked)
    }

    fn stderr(&mut self, gate: &Gate) -> io::Result<()> {
        let Some(stderr) = &mut self.stderr else {
            return Ok(());
        };
        let mut bytes = Zeroizing::new([0; CHUNK]);
        let count = match stderr.read_timeout(&mut bytes[..], Duration::ZERO) {
            Ok(0) => {
                self.stderr = None;
                return Ok(());
            }
            Ok(count) => count,
            Err(error) if retry(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let Some(challenge) = &mut self.challenge else {
            return Ok(());
        };
        match challenge.prompt.observe(&bytes[..count]) {
            0 => Ok(()),
            1 => {
                gate.check()?;
                let line = challenge.line.take().ok_or_else(invalid)?;
                let input = self.lease.take_sensitive_exec_input().ok_or_else(invalid)?;
                let gate = gate.clone();
                self.input = Some(input.send_then(line, move || gate.running())?);
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}

struct Prompt {
    marker: String,
    matched: usize,
    line_start: bool,
}

impl Prompt {
    fn new(marker: String) -> Self {
        Self {
            marker,
            matched: 0,
            line_start: true,
        }
    }

    fn observe(&mut self, bytes: &[u8]) -> usize {
        let marker = self.marker.as_bytes();
        let mut hits = 0;
        for byte in bytes {
            if (self.line_start || self.matched > 0) && *byte == marker[self.matched] {
                self.line_start = false;
                self.matched += 1;
                if self.matched == marker.len() {
                    self.matched = 0;
                    hits += 1;
                }
            } else {
                self.matched = 0;
                self.line_start = *byte == b'\n';
            }
        }
        hits
    }
}

struct Tagged {
    input: ssh_worker::Reader,
    prefix: [u8; 3],
    offset: usize,
    remaining: usize,
}

impl Tagged {
    fn new(input: ssh_worker::Reader) -> Self {
        Self {
            input,
            prefix: [0; 3],
            offset: 0,
            remaining: 0,
        }
    }

    fn read_stdout(&mut self, bytes: &mut [u8], gate: &Gate) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut discard = Zeroizing::new([0; CHUNK]);
        loop {
            gate.check()?;
            if self.remaining == 0 {
                let count = self
                    .input
                    .read_timeout(&mut self.prefix[self.offset..], POLL)?;
                if count == 0 {
                    return if self.offset == 0 {
                        Ok(0)
                    } else {
                        Err(invalid())
                    };
                }
                self.offset += count;
                if self.offset < self.prefix.len() {
                    continue;
                }
                self.offset = 0;
                self.remaining = usize::from(u16::from_be_bytes([self.prefix[1], self.prefix[2]]));
                if self.prefix[0] > 1 || !(1..=CHUNK - 3).contains(&self.remaining) {
                    return Err(invalid());
                }
            }
            let limit = self.remaining.min(bytes.len());
            let is_stdout = self.prefix[0] == 0;
            let count = if is_stdout {
                self.input.read_timeout(&mut bytes[..limit], POLL)?
            } else {
                self.input.read_timeout(&mut discard[..limit], POLL)?
            };
            if count == 0 {
                return Err(invalid());
            }
            self.remaining -= count;
            if is_stdout {
                return Ok(count);
            }
        }
    }
}

#[cfg(test)]
mod tests;
