use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::time::Duration;

use kortty_ssh::{
    Authentication, ConnectError, ConnectOptions, Endpoint, HostKey, KeyboardChallenge,
    KeyboardPrompt, KeyboardResponse, SshTarget, TrustStore, WindowSize,
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"kortssh4";
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

#[derive(Clone, Debug)]
pub enum Open {
    Sftp,
    Direct(Endpoint, SocketAddr),
    Listen(SocketAddr),
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
                    _ => return Err(invalid()),
                },
            ),
            6 => Self::Opened(id, flag(input)?.then(|| recv_address(input)).transpose()?),
            7 => Self::Accepted(id, number(input)?, recv_address(input)?),
            8 => Self::Closed(id, flag(input)?.then(|| text(input, TEXT)).transpose()?),
            9 => Self::EndAck(id),
            10 => Self::Finished(id, flag(input)?.then(|| text(input, TEXT)).transpose()?),
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
    fn worker_login_round_trips_and_rejects_out_of_range_startup_options() {
        let options = ConnectOptions::new(300, 20).unwrap();
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
        for (index, value) in [(9, 0xff), (11, 21)] {
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
