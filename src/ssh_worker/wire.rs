use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::time::Duration;

use kortty_ssh::{
    Authentication, ConnectError, ConnectOptions, Endpoint, HostKey, KeepAlive, KeyboardChallenge,
    KeyboardPrompt, KeyboardResponse, SshTarget, TrustStore, WindowSize,
};
use zeroize::Zeroizing;

use super::account_lookup::{self, DisplayQuery, Kind};
use super::folder_usage;

const MAGIC: &[u8; 8] = b"kortssh5";
const TEXT: usize = 4096;
const KEY: usize = 1024 * 1024;
const MAX_PROMPTS: usize = 16;
const ANSWER_BYTES: usize = 16 * 1024;
const PROMPT_MILLIS: u32 = 120_000;
pub const CHUNK: usize = 8192;

pub(super) struct Login {
    pub target: SshTarget,
    pub jump: Option<SshTarget>,
    pub trust: TrustStore,
    pub term: String,
    pub size: WindowSize,
    pub sftp: bool,
    pub options: ConnectOptions,
}

pub enum Message {
    Ready,
    SandboxReady,
    Failed(ConnectError),
    Challenge(KeyboardChallenge),
}

// Relayed transports carry the desktop's TCP origin before the login message.
#[cfg(any(windows, target_os = "macos", test))]
pub fn send_relay_origin(out: &mut impl Write, origin: SocketAddr) -> io::Result<()> {
    send_address(out, origin)
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn recv_relay_origin(input: &mut impl Read) -> io::Result<SocketAddr> {
    let origin = recv_address(input)?;
    if origin.ip().is_unspecified() || origin.port() == 0 {
        return Err(invalid());
    }
    Ok(origin)
}

pub fn send_login_header(
    out: &mut impl Write,
    pins: &[(Endpoint, HostKey)],
    term: &str,
    size: WindowSize,
    sftp: bool,
    options: ConnectOptions,
) -> io::Result<()> {
    out.write_all(MAGIC)?;
    out.write_all(&[u8::from(sftp)])?;
    out.write_all(&options.seconds().to_be_bytes())?;
    out.write_all(&[options.attempts()])?;
    let keepalive = options.keepalive();
    out.write_all(&[keepalive.map_or(0, |value| if value.enabled() { 2 } else { 1 })])?;
    if let Some(keepalive) = keepalive {
        out.write_all(&keepalive.seconds().to_be_bytes())?;
    }
    blob(out, term.as_bytes(), TEXT)?;
    send_size(out, size)?;
    out.write_all(&[pins.len().try_into().map_err(|_| invalid())?])?;
    for (endpoint, key) in pins {
        send_endpoint(out, endpoint)?;
        blob(out, key.to_openssh()?.as_bytes(), CHUNK)?;
    }
    Ok(())
}

pub(super) fn recv_login(input: &mut impl Read) -> io::Result<Login> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid());
    }
    let sftp = flag(input)?;
    let mut seconds = [0; 2];
    input.read_exact(&mut seconds)?;
    let options =
        ConnectOptions::new(u16::from_be_bytes(seconds), byte(input)?).map_err(|_| invalid())?;
    let keepalive = match byte(input)? {
        0 => None,
        kind @ (1 | 2) => {
            input.read_exact(&mut seconds)?;
            Some(KeepAlive::new(kind == 2, u16::from_be_bytes(seconds)).map_err(|_| invalid())?)
        }
        _ => return Err(invalid()),
    };
    let options = options.with_keepalive(keepalive);
    let term = text(input, TEXT)?;
    let size = recv_size(input)?;
    let pins = byte(input)?;
    if pins > 2 {
        return Err(invalid());
    }
    let pins = (0..pins)
        .map(|_| {
            Ok((
                recv_endpoint(input)?,
                HostKey::from_openssh(&text(input, CHUNK)?)?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let target = recv_target(input)?;
    let jump = flag(input)?.then(|| recv_target(input)).transpose()?;
    if pins.iter().any(|(endpoint, _)| {
        *endpoint != target.endpoint
            && !jump.as_ref().is_some_and(|jump| *endpoint == jump.endpoint)
    }) {
        return Err(invalid());
    }
    Ok(Login {
        target,
        jump,
        trust: TrustStore::from_pins(pins)?,
        term,
        size,
        sftp,
        options,
    })
}

pub enum Credential<'a> {
    Password(&'a str),
    PrivateKey(&'a [u8]),
    KeyWithPassphrase(&'a [u8], &'a str),
    KeyboardInteractive,
}

pub fn send_target(
    out: &mut impl Write,
    endpoint: &Endpoint,
    username: &str,
    follow_up: bool,
    credential: Credential<'_>,
) -> io::Result<()> {
    send_endpoint(out, endpoint)?;
    blob(out, username.as_bytes(), TEXT)?;
    out.write_all(&[u8::from(follow_up)])?;
    match credential {
        Credential::KeyboardInteractive if !follow_up => out.write_all(&[3]),
        Credential::KeyboardInteractive => Err(invalid()),
        Credential::Password(password) => {
            out.write_all(&[0])?;
            blob(out, password.as_bytes(), TEXT)
        }
        Credential::PrivateKey(key) => {
            out.write_all(&[1])?;
            blob(out, key, KEY)
        }
        Credential::KeyWithPassphrase(key, password) => {
            out.write_all(&[2])?;
            blob(out, key, KEY)?;
            blob(out, password.as_bytes(), TEXT)
        }
    }
}

fn recv_target(input: &mut impl Read) -> io::Result<SshTarget> {
    let endpoint = recv_endpoint(input)?;
    let username = text(input, TEXT)?;
    let follow_up = flag(input)?;
    let authentication = match byte(input)? {
        0 => Authentication::password(take_secret_text(input)?),
        1 => Authentication::private_key(std::mem::take(&mut *recv_blob(input, KEY)?)),
        2 => {
            let mut key = recv_blob(input, KEY)?;
            let passphrase = take_secret_text(input)?;
            Authentication::private_key_with_passphrase(std::mem::take(&mut *key), passphrase)
        }
        3 if !follow_up => Authentication::keyboard_interactive(worker_answers),
        _ => return Err(invalid()),
    };
    let authentication = if follow_up {
        authentication.with_keyboard_interactive(worker_answers)
    } else {
        authentication
    };
    Ok(SshTarget {
        endpoint,
        username,
        authentication,
    })
}

fn worker_answers(challenge: &KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> {
    let mut input = super::input_pipe()?;
    write_challenge(&mut io::stdout().lock(), challenge)?;
    recv_answers(&mut input, challenge.prompts.len())
}

fn write_challenge(out: &mut impl Write, challenge: &KeyboardChallenge) -> io::Result<()> {
    let millis = u32::try_from(challenge.response_timeout.as_millis()).map_err(|_| invalid())?;
    if challenge.prompts.len() > MAX_PROMPTS || millis == 0 || millis > PROMPT_MILLIS {
        return Err(invalid());
    }
    out.write_all(&[7])?;
    send_endpoint(out, &challenge.endpoint)?;
    blob(out, challenge.username.as_bytes(), TEXT)?;
    blob(out, challenge.name.as_bytes(), TEXT)?;
    blob(out, challenge.instruction.as_bytes(), TEXT)?;
    out.write_all(&millis.to_be_bytes())?;
    out.write_all(&[challenge.prompts.len() as u8])?;
    for prompt in &challenge.prompts {
        out.write_all(&[u8::from(prompt.echo)])?;
        blob(out, prompt.text.as_bytes(), TEXT)?;
    }
    out.flush()
}

fn recv_challenge(input: &mut impl Read) -> io::Result<KeyboardChallenge> {
    let endpoint = recv_endpoint(input)?;
    let username = text(input, TEXT)?;
    let name = text(input, TEXT)?;
    let instruction = text(input, TEXT)?;
    let millis = number(input)?;
    let count = usize::from(byte(input)?);
    if count > MAX_PROMPTS || millis == 0 || millis > PROMPT_MILLIS {
        return Err(invalid());
    }
    let prompts = (0..count)
        .map(|_| {
            Ok(KeyboardPrompt {
                echo: flag(input)?,
                text: text(input, TEXT)?,
            })
        })
        .collect::<io::Result<_>>()?;
    Ok(KeyboardChallenge {
        endpoint,
        username,
        name,
        instruction,
        prompts,
        response_timeout: Duration::from_millis(u64::from(millis)),
    })
}

pub fn send_answers(
    out: &mut impl Write,
    expected: usize,
    answers: &[KeyboardResponse],
) -> io::Result<()> {
    if answers.len() != expected
        || expected > MAX_PROMPTS
        || answers.iter().any(|answer| answer.as_str().len() > TEXT)
        || answers
            .iter()
            .map(|answer| answer.as_str().len())
            .sum::<usize>()
            > ANSWER_BYTES
    {
        return Err(invalid());
    }
    out.write_all(&[answers.len() as u8])?;
    for answer in answers {
        blob(out, answer.as_str().as_bytes(), TEXT)?;
    }
    out.flush()
}

fn recv_answers(input: &mut impl Read, expected: usize) -> io::Result<Vec<KeyboardResponse>> {
    if usize::from(byte(input)?) != expected || expected > MAX_PROMPTS {
        return Err(invalid());
    }
    let mut total = 0;
    (0..expected)
        .map(|_| {
            let answer = KeyboardResponse::new(take_secret_text(input)?);
            total += answer.as_str().len();
            if total > ANSWER_BYTES {
                return Err(invalid());
            }
            Ok(answer)
        })
        .collect()
}

fn take_secret_text(input: &mut impl Read) -> io::Result<String> {
    let mut value = recv_blob(input, TEXT)?;
    if std::str::from_utf8(&value).is_err() {
        return Err(invalid());
    }
    String::from_utf8(std::mem::take(&mut *value)).map_err(|_| invalid())
}

fn send_endpoint(out: &mut impl Write, endpoint: &Endpoint) -> io::Result<()> {
    blob(out, endpoint.wire_host().as_bytes(), TEXT)?;
    out.write_all(&endpoint.port().to_be_bytes())
}

fn recv_endpoint(input: &mut impl Read) -> io::Result<Endpoint> {
    let host = text(input, TEXT)?;
    let mut port = [0; 2];
    input.read_exact(&mut port)?;
    Endpoint::new(&host, u16::from_be_bytes(port))
}

pub fn send(out: &mut impl Write, message: Message) -> io::Result<()> {
    match message {
        Message::Ready => out.write_all(&[0])?,
        Message::SandboxReady => out.write_all(&[8])?,
        Message::Challenge(challenge) => return write_challenge(out, &challenge),
        Message::Failed(ConnectError::UnknownHostKey { endpoint, offered }) => {
            out.write_all(&[4])?;
            send_endpoint(out, &endpoint)?;
            blob(out, offered.to_openssh()?.as_bytes(), CHUNK)?;
        }
        Message::Failed(ConnectError::ChangedHostKey {
            endpoint,
            expected,
            offered,
        }) => {
            out.write_all(&[5])?;
            send_endpoint(out, &endpoint)?;
            blob(out, expected.to_openssh()?.as_bytes(), CHUNK)?;
            blob(out, offered.to_openssh()?.as_bytes(), CHUNK)?;
        }
        Message::Failed(error) => {
            let code = if matches!(&error, ConnectError::Io(error) if error.kind() == io::ErrorKind::TimedOut)
            {
                9
            } else {
                6
            };
            out.write_all(&[code])?;
            let message = error.to_string();
            let end = message.floor_char_boundary(message.len().min(TEXT));
            blob(out, &message.as_bytes()[..end], TEXT)?;
        }
    }
    out.flush()
}

pub fn recv(input: &mut impl Read) -> io::Result<Message> {
    Ok(match byte(input)? {
        0 => Message::Ready,
        4 => Message::Failed(ConnectError::UnknownHostKey {
            endpoint: recv_endpoint(input)?,
            offered: HostKey::from_openssh(&text(input, CHUNK)?)?,
        }),
        5 => Message::Failed(ConnectError::ChangedHostKey {
            endpoint: recv_endpoint(input)?,
            expected: HostKey::from_openssh(&text(input, CHUNK)?)?,
            offered: HostKey::from_openssh(&text(input, CHUNK)?)?,
        }),
        6 => Message::Failed(io::Error::other(text(input, TEXT)?).into()),
        7 => Message::Challenge(recv_challenge(input)?),
        8 => Message::SandboxReady,
        9 => Message::Failed(io::Error::new(io::ErrorKind::TimedOut, text(input, TEXT)?).into()),
        _ => return Err(invalid()),
    })
}

fn send_size(out: &mut impl Write, size: WindowSize) -> io::Result<()> {
    for value in [size.columns, size.rows, size.pixel_width, size.pixel_height] {
        out.write_all(&value.to_be_bytes())?;
    }
    Ok(())
}

fn recv_size(input: &mut impl Read) -> io::Result<WindowSize> {
    Ok(WindowSize {
        columns: number(input)?,
        rows: number(input)?,
        pixel_width: number(input)?,
        pixel_height: number(input)?,
    })
}

fn blob(out: &mut impl Write, bytes: &[u8], limit: usize) -> io::Result<()> {
    if bytes.len() > limit {
        return Err(invalid());
    }
    out.write_all(&(bytes.len() as u32).to_be_bytes())?;
    out.write_all(bytes)
}

fn recv_blob(input: &mut impl Read, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let length = number(input)? as usize;
    if length > limit {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn recv_folder_usage(input: &mut impl Read) -> io::Result<Vec<Vec<u8>>> {
    let count = number(input)? as usize;
    if !(1..=folder_usage::MAX_PATHS).contains(&count) {
        return Err(invalid());
    }
    let mut paths = Vec::with_capacity(count);
    let mut remaining = kortty_ssh::MAX_EXEC_COMMAND_BYTES;
    for _ in 0..count {
        let mut path = recv_blob(input, remaining)?;
        remaining -= path.len();
        paths.push(std::mem::take(&mut *path));
    }
    folder_usage::command(&paths).map_err(|_| invalid())?;
    Ok(paths)
}

fn recv_account_names(input: &mut impl Read) -> io::Result<DisplayQuery> {
    let users = usize::from(byte(input)?);
    let groups = usize::from(byte(input)?);
    if !(1..=account_lookup::DISPLAY_ID_LIMIT).contains(&(users + groups)) {
        return Err(invalid());
    }
    let users = (0..users)
        .map(|_| number(input))
        .collect::<io::Result<_>>()?;
    let groups = (0..groups)
        .map(|_| number(input))
        .collect::<io::Result<_>>()?;
    DisplayQuery::new(users, groups).map_err(|_| invalid())
}

fn text(input: &mut impl Read, limit: usize) -> io::Result<String> {
    String::from_utf8(std::mem::take(&mut *recv_blob(input, limit)?)).map_err(|_| invalid())
}

fn number(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn byte(input: &mut impl Read) -> io::Result<u8> {
    let mut byte = [0; 1];
    input.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn flag(input: &mut impl Read) -> io::Result<bool> {
    match byte(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}

pub fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid SSH worker message")
}

#[derive(Clone)]
pub enum Open {
    Sftp,
    Direct(Endpoint, SocketAddr),
    Listen(SocketAddr),
    Exec(String),
    PasswordExec { command: String, nonce: [u8; 16] },
    PasswordFileExec { nonce: [u8; 16] },
    FileExec,
    AccountLookup { kind: Kind, name: String },
    ArchiveExtract,
    FolderUsage(Vec<Vec<u8>>),
    AccountNames(DisplayQuery),
    DraftReplace,
}

impl std::fmt::Debug for Open {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Sftp => "Sftp",
            Self::Direct(..) => "Direct",
            Self::Listen(..) => "Listen",
            Self::Exec(..) => "Exec",
            Self::PasswordExec { .. } => "PasswordExec",
            Self::PasswordFileExec { .. } => "PasswordFileExec",
            Self::FileExec => "FileExec",
            Self::AccountLookup { .. } => "AccountLookup",
            Self::ArchiveExtract => "ArchiveExtract",
            Self::FolderUsage(..) => "FolderUsage",
            Self::AccountNames(..) => "AccountNames",
            Self::DraftReplace => "DraftReplace",
        })
    }
}

#[derive(Debug)]
pub enum Frame {
    Resize(WindowSize),
    Data(u32, Vec<u8>),
    Credit(u32, u8),
    End(u32, Option<u32>),
    Close(u32, Option<String>),
    Open(u32, Open),
    Opened(u32, Option<SocketAddr>),
    Accepted(u32, u32, SocketAddr),
    Closed(u32, Option<String>),
    EndAck(u32),
    Finished(u32, Option<String>),
    Password(u32, PasswordFrame),
}

#[derive(Debug)]
pub enum PasswordFrame {
    Challenge,
    Input(kortty_ssh::SensitiveLine, Option<PasswordWriteGuard>),
    Staged,
    Commit(Option<PasswordWriteGuard>),
    Result(super::password::Status),
    Ready,
}

pub struct PasswordWriteGuard {
    pub(super) lane: std::sync::Weak<()>,
    pub(super) permitted: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
}

impl std::fmt::Debug for PasswordWriteGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SSH password write guard")
    }
}

impl PasswordFrame {
    fn send(self, id: u32, out: &mut impl Write) -> io::Result<()> {
        let guard = match &self {
            Self::Input(_, guard) | Self::Commit(guard) => guard.as_ref(),
            _ => None,
        };
        if guard.is_some_and(|guard| guard.lane.strong_count() == 0 || !(guard.permitted)()) {
            return Ok(());
        }
        header(out, 11, id)?;
        match self {
            Self::Challenge => out.write_all(&[0]),
            Self::Input(line, _) => {
                out.write_all(&[1])?;
                line.write_wire(out)
            }
            Self::Staged => out.write_all(&[2]),
            Self::Commit(_) => out.write_all(&[3]),
            Self::Result(status) => {
                use super::password::Status;
                let tag = match status {
                    Status::Exited(_) => 0,
                    Status::Cancelled => 1,
                    Status::TimedOut => 2,
                    Status::TransportFailed => 3,
                    Status::MissingExitCode => 4,
                };
                out.write_all(&[4, tag])?;
                if let Status::Exited(code) = status {
                    out.write_all(&code.to_be_bytes())?;
                }
                Ok(())
            }
            Self::Ready => out.write_all(&[5]),
        }
    }

    fn recv(input: &mut impl Read) -> io::Result<Self> {
        use super::password::Status;
        Ok(match byte(input)? {
            0 => Self::Challenge,
            1 => Self::Input(kortty_ssh::SensitiveLine::read_wire(input)?, None),
            2 => Self::Staged,
            3 => Self::Commit(None),
            4 => Self::Result(match byte(input)? {
                0 => Status::Exited(number(input)?),
                1 => Status::Cancelled,
                2 => Status::TimedOut,
                3 => Status::TransportFailed,
                4 => Status::MissingExitCode,
                _ => return Err(invalid()),
            }),
            5 => Self::Ready,
            _ => return Err(invalid()),
        })
    }
}

impl Frame {
    pub fn send(self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Resize(size) => {
                out.write_all(&[0])?;
                send_size(out, size)?;
            }
            Self::Data(id, bytes) => {
                header(out, 1, id)?;
                blob(out, &bytes, CHUNK)?;
            }
            Self::Credit(id, count) => {
                header(out, 2, id)?;
                out.write_all(&[count])?;
            }
            Self::End(id, code) => {
                header(out, 3, id)?;
                out.write_all(&[u8::from(code.is_some())])?;
                if let Some(code) = code {
                    out.write_all(&code.to_be_bytes())?;
                }
            }
            Self::Close(id, error) => {
                header(out, 4, id)?;
                error_text(out, error)?;
            }
            Self::Open(id, request) => {
                if let Open::AccountLookup { name, .. } = &request
                    && !account_lookup::valid_name(name)
                {
                    return Err(invalid());
                }
                if let Open::FolderUsage(paths) = &request {
                    folder_usage::command(paths).map_err(|_| invalid())?;
                }
                header(out, 5, id)?;
                match request {
                    Open::Sftp => out.write_all(&[0])?,
                    Open::Direct(endpoint, origin) => {
                        out.write_all(&[1])?;
                        send_endpoint(out, &endpoint)?;
                        send_address(out, origin)?;
                    }
                    Open::Listen(bind) => {
                        out.write_all(&[2])?;
                        send_address(out, bind)?;
                    }
                    Open::Exec(command) => {
                        out.write_all(&[3])?;
                        blob(out, command.as_bytes(), kortty_ssh::MAX_EXEC_COMMAND_BYTES)?;
                    }
                    Open::PasswordExec { command, nonce } => {
                        out.write_all(&[4])?;
                        blob(out, command.as_bytes(), kortty_ssh::MAX_EXEC_COMMAND_BYTES)?;
                        out.write_all(&nonce)?;
                    }
                    Open::PasswordFileExec { nonce } => {
                        out.write_all(&[5])?;
                        out.write_all(&nonce)?;
                    }
                    Open::FileExec => out.write_all(&[6])?,
                    Open::AccountLookup { kind, name } => {
                        let kind = match kind {
                            Kind::User => 0,
                            Kind::Group => 1,
                        };
                        out.write_all(&[7, kind])?;
                        blob(out, name.as_bytes(), account_lookup::INPUT_LIMIT)?;
                    }
                    Open::ArchiveExtract => out.write_all(&[8])?,
                    Open::FolderUsage(paths) => {
                        out.write_all(&[9])?;
                        out.write_all(&(paths.len() as u32).to_be_bytes())?;
                        for path in paths {
                            blob(out, &path, kortty_ssh::MAX_EXEC_COMMAND_BYTES)?;
                        }
                    }
                    Open::AccountNames(query) => {
                        out.write_all(&[
                            10,
                            query.users().len() as u8,
                            query.groups().len() as u8,
                        ])?;
                        for id in query.users().iter().chain(query.groups()) {
                            out.write_all(&id.to_be_bytes())?;
                        }
                    }
                    Open::DraftReplace => out.write_all(&[11])?,
                }
            }
            Self::Opened(id, address) => {
                header(out, 6, id)?;
                out.write_all(&[u8::from(address.is_some())])?;
                if let Some(address) = address {
                    send_address(out, address)?;
                }
            }
            Self::Accepted(listener, id, origin) => {
                header(out, 7, listener)?;
                out.write_all(&id.to_be_bytes())?;
                send_address(out, origin)?;
            }
            Self::Closed(id, error) => {
                header(out, 8, id)?;
                error_text(out, error)?;
            }
            Self::EndAck(id) => header(out, 9, id)?,
            Self::Finished(id, error) => {
                header(out, 10, id)?;
                error_text(out, error)?;
            }
            Self::Password(id, frame) => frame.send(id, out)?,
        }
        out.flush()
    }

    pub fn recv(input: &mut impl Read) -> io::Result<Self> {
        let kind = byte(input)?;
        if kind == 0 {
            return Ok(Self::Resize(recv_size(input)?));
        }
        let id = number(input)?;
        Ok(match kind {
            1 => {
                let mut bytes = recv_blob(input, CHUNK)?;
                if bytes.is_empty() {
                    return Err(invalid());
                }
                Self::Data(id, std::mem::take(&mut *bytes))
            }
            2 => Self::Credit(id, byte(input)?),
            3 => Self::End(id, flag(input)?.then(|| number(input)).transpose()?),
            4 => Self::Close(id, flag(input)?.then(|| text(input, TEXT)).transpose()?),
            5 => Self::Open(
                id,
                match byte(input)? {
                    0 => Open::Sftp,
                    1 => Open::Direct(recv_endpoint(input)?, recv_address(input)?),
                    2 => Open::Listen(recv_address(input)?),
                    3 => Open::Exec(text(input, kortty_ssh::MAX_EXEC_COMMAND_BYTES)?),
                    4 => {
                        let command = text(input, kortty_ssh::MAX_EXEC_COMMAND_BYTES)?;
                        let mut nonce = [0; 16];
                        input.read_exact(&mut nonce)?;
                        Open::PasswordExec { command, nonce }
                    }
                    5 => {
                        let mut nonce = [0; 16];
                        input.read_exact(&mut nonce)?;
                        Open::PasswordFileExec { nonce }
                    }
                    6 => Open::FileExec,
                    7 => {
                        let kind = match byte(input)? {
                            0 => Kind::User,
                            1 => Kind::Group,
                            _ => return Err(invalid()),
                        };
                        let name = text(input, account_lookup::INPUT_LIMIT)?;
                        if !account_lookup::valid_name(&name) {
                            return Err(invalid());
                        }
                        Open::AccountLookup { kind, name }
                    }
                    8 => Open::ArchiveExtract,
                    9 => Open::FolderUsage(recv_folder_usage(input)?),
                    10 => Open::AccountNames(recv_account_names(input)?),
                    11 => Open::DraftReplace,
                    _ => return Err(invalid()),
                },
            ),
            6 => Self::Opened(id, flag(input)?.then(|| recv_address(input)).transpose()?),
            7 => Self::Accepted(id, number(input)?, recv_address(input)?),
            8 => Self::Closed(id, flag(input)?.then(|| text(input, TEXT)).transpose()?),
            9 => Self::EndAck(id),
            10 => Self::Finished(id, flag(input)?.then(|| text(input, TEXT)).transpose()?),
            11 => Self::Password(id, PasswordFrame::recv(input)?),
            _ => return Err(invalid()),
        })
    }
}

fn header(out: &mut impl Write, kind: u8, id: u32) -> io::Result<()> {
    out.write_all(&[kind])?;
    out.write_all(&id.to_be_bytes())
}

fn error_text(out: &mut impl Write, error: Option<String>) -> io::Result<()> {
    out.write_all(&[u8::from(error.is_some())])?;
    if let Some(error) = error {
        let end = error.floor_char_boundary(error.len().min(TEXT));
        blob(out, &error.as_bytes()[..end], TEXT)?;
    }
    Ok(())
}

fn send_address(out: &mut impl Write, address: SocketAddr) -> io::Result<()> {
    blob(out, address.to_string().as_bytes(), 128)
}

fn recv_address(input: &mut impl Read) -> io::Result<SocketAddr> {
    text(input, 128)?.parse().map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    use super::*;

    #[test]
    fn fixed_file_open_frames_have_no_command_or_body() {
        let nonce = [37; 16];
        let mut bytes = Vec::new();
        Frame::Open(3, Open::FileExec).send(&mut bytes).unwrap();
        Frame::Open(5, Open::PasswordFileExec { nonce })
            .send(&mut bytes)
            .unwrap();
        Frame::Password(5, PasswordFrame::Ready)
            .send(&mut bytes)
            .unwrap();
        assert_eq!(bytes.len(), 6 + 22 + 6);
        let mut input = bytes.as_slice();
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(3, Open::FileExec))
        ));
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(5, Open::PasswordFileExec { nonce: decoded })) if decoded == nonce
        ));
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Password(5, PasswordFrame::Ready))
        ));
        assert!(input.is_empty());
    }

    #[test]
    fn archive_extract_open_has_only_its_fixed_tag() {
        let mut bytes = Vec::new();
        Frame::Open(3, Open::ArchiveExtract)
            .send(&mut bytes)
            .unwrap();
        assert_eq!(bytes, [5, 0, 0, 0, 3, 8]);
        let mut input = bytes.as_slice();
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(3, Open::ArchiveExtract))
        ));
        assert!(input.is_empty());
    }

    #[test]
    fn draft_replace_open_has_only_its_fixed_tag() {
        let mut bytes = Vec::new();
        Frame::Open(3, Open::DraftReplace).send(&mut bytes).unwrap();
        assert_eq!(bytes, [5, 0, 0, 0, 3, 11]);
        let mut input = bytes.as_slice();
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(3, Open::DraftReplace))
        ));
        assert!(input.is_empty());
    }

    #[test]
    fn folder_usage_open_carries_only_ordered_paths() {
        let paths = vec![b"/one/quoted' path".to_vec(), "/two/ü".as_bytes().to_vec()];
        let mut bytes = Vec::new();
        Frame::Open(3, Open::FolderUsage(paths.clone()))
            .send(&mut bytes)
            .unwrap();
        assert_eq!(&bytes[..10], [5, 0, 0, 0, 3, 9, 0, 0, 0, 2]);
        assert!(!bytes.windows(2).any(|bytes| bytes == b"du"));
        let mut input = bytes.as_slice();
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(3, Open::FolderUsage(decoded))) if decoded == paths
        ));
        assert!(input.is_empty());
    }

    #[test]
    fn folder_usage_open_rejects_unrepresentable_or_oversized_paths() {
        let limit = kortty_ssh::MAX_EXEC_COMMAND_BYTES;
        for paths in [
            Vec::new(),
            vec![b"/valid".to_vec(); folder_usage::MAX_PATHS + 1],
            vec![b"du -sk /".to_vec()],
            vec![b"/one\n/two".to_vec()],
            vec![b"/invalid/\xff".to_vec()],
            vec![format!("/{}", "'".repeat(limit / 4)).into_bytes()],
            vec![format!("/{}", "a".repeat(limit / 2)).into_bytes(); 2],
        ] {
            let mut encoded = Vec::new();
            assert!(
                Frame::Open(3, Open::FolderUsage(paths.clone()))
                    .send(&mut encoded)
                    .is_err()
            );
            assert!(encoded.is_empty());

            // Simulate a peer bypassing the sender's validation.
            let mut bytes = vec![5, 0, 0, 0, 3, 9];
            bytes.extend_from_slice(&(paths.len() as u32).to_be_bytes());
            for path in paths {
                blob(&mut bytes, &path, limit).unwrap();
            }
            assert!(Frame::recv(&mut bytes.as_slice()).is_err());
        }
    }

    #[test]
    fn account_display_lookup_wire_round_trips_numeric_ids_and_rejects_unbounded_input() {
        let query = DisplayQuery::new(vec![0, 1000], vec![2000]).unwrap();
        let mut bytes = Vec::new();
        Frame::Open(3, Open::AccountNames(query.clone()))
            .send(&mut bytes)
            .unwrap();
        assert_eq!(
            bytes,
            [
                5, 0, 0, 0, 3, 10, 2, 1, 0, 0, 0, 0, 0, 0, 3, 232, 0, 0, 7, 208
            ]
        );
        let mut input = bytes.as_slice();
        assert!(matches!(
            Frame::recv(&mut input),
            Ok(Frame::Open(3, Open::AccountNames(decoded))) if decoded == query
        ));
        assert!(input.is_empty());
        for invalid in [
            vec![5, 0, 0, 0, 3, 10, 0, 0],
            vec![5, 0, 0, 0, 3, 10, 64, 1],
            vec![5, 0, 0, 0, 3, 10, 1, 0, 255, 255, 255, 255],
            vec![5, 0, 0, 0, 3, 10, 2, 0, 0, 0, 0, 2, 0, 0, 0, 1],
        ] {
            assert!(Frame::recv(&mut invalid.as_slice()).is_err());
        }
    }

    #[test]
    fn account_lookup_open_carries_only_the_database_and_name() {
        for (kind, name) in [(Kind::User, "service$"), (Kind::Group, "work-team@example")] {
            let mut bytes = Vec::new();
            Frame::Open(
                3,
                Open::AccountLookup {
                    kind,
                    name: name.to_owned(),
                },
            )
            .send(&mut bytes)
            .unwrap();
            assert_eq!(bytes.len(), 11 + name.len());
            let mut input = bytes.as_slice();
            assert!(matches!(
                Frame::recv(&mut input),
                Ok(Frame::Open(3, Open::AccountLookup { kind: decoded, name: received }))
                    if decoded == kind && received == name
            ));
            assert!(input.is_empty());
        }
    }

    #[test]
    fn account_lookup_open_rejects_invalid_names_and_database_tags() {
        for name in ["", "123", "-option", "account'command", "é"] {
            let mut encoded = Vec::new();
            let request = Open::AccountLookup {
                kind: Kind::User,
                name: name.to_owned(),
            };
            assert!(Frame::Open(3, request).send(&mut encoded).is_err());
            assert!(encoded.is_empty());

            // Simulate an untrusted sender that bypasses Frame::send validation.
            let mut bytes = vec![5, 0, 0, 0, 3, 7, 0];
            blob(&mut bytes, name.as_bytes(), account_lookup::INPUT_LIMIT).unwrap();
            assert!(Frame::recv(&mut bytes.as_slice()).is_err());
        }
        assert!(Frame::recv(&mut [5, 0, 0, 0, 3, 7, 2].as_slice()).is_err());
        assert!(Frame::recv(&mut [5, 0, 0, 0, 3, 7, 0, 0, 0, 0, 65].as_slice()).is_err());
    }

    #[test]
    fn worker_login_round_trips_and_rejects_out_of_range_startup_options() {
        let options = ConnectOptions::new(300, 20)
            .unwrap()
            .with_keepalive(Some(KeepAlive::new(false, 600).unwrap()));
        let mut bytes = Vec::new();
        let size = WindowSize {
            columns: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        };
        send_login_header(&mut bytes, &[], "xterm", size, false, options).unwrap();
        let target = Endpoint::new("fixture.test", 22).unwrap();
        send_target(
            &mut bytes,
            &target,
            "user",
            false,
            Credential::KeyboardInteractive,
        )
        .unwrap();
        bytes.push(0);
        assert_eq!(recv_login(&mut bytes.as_slice()).unwrap().options, options);
        for (index, value) in [(9, 0xff), (11, 21), (12, 3), (13, 0xff)] {
            let mut invalid = bytes.clone();
            invalid[index] = value;
            assert!(
                matches!(recv_login(&mut invalid.as_slice()), Err(error) if error.kind() == io::ErrorKind::InvalidData)
            );
        }
        let mut result = Vec::new();
        send(
            &mut result,
            Message::Failed(io::Error::new(io::ErrorKind::TimedOut, "fixture deadline").into()),
        )
        .unwrap();
        assert!(
            matches!(recv(&mut result.as_slice()).unwrap(), Message::Failed(ConnectError::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn relay_origin_round_trips_before_login() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_, desktop_address) = listener.accept().unwrap();
        assert_eq!(tcp.local_addr().unwrap(), desktop_address);

        for origin in [desktop_address, "[fe80::1%4]:39000".parse().unwrap()] {
            let mut bytes = Vec::new();
            send_relay_origin(&mut bytes, origin).unwrap();
            bytes.extend_from_slice(MAGIC);
            let mut input = bytes.as_slice();
            assert_eq!(recv_relay_origin(&mut input).unwrap(), origin);
            assert_eq!(input, MAGIC);
        }
    }

    #[test]
    fn relay_origin_rejects_unspecified_or_invalid_addresses() {
        for address in ["0.0.0.0:39000", "[::]:39000", "127.0.0.1:0", "invalid"] {
            let mut bytes = Vec::new();
            blob(&mut bytes, address.as_bytes(), 128).unwrap();
            bytes.extend_from_slice(MAGIC);
            let mut input = bytes.as_slice();
            assert_eq!(
                recv_relay_origin(&mut input).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input, MAGIC);
        }
    }
}
