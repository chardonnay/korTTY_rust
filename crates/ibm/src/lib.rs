//! Bounded IBM Telnet transport and 3270 and 5250 display cores.

mod ebcdic;
mod environment5250;
mod negotiation;
pub mod screen3270;
pub mod screen5250;
mod telnet;
mod tn3270e;

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub use environment5250::DeviceName5250;
use negotiation::{Mode, Negotiation};
use screen3270::Error3270;
use screen5250::Model5250;
use telnet::{Event, Framing};
use tn3270e::{HEADER_LEN, ResponseRequest};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const NEGOTIATION_POLL: Duration = Duration::from_millis(50);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RECORD: usize = u16::MAX as usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    /// IBM-3278-2 with RFC 2355 basic TN3270E or RFC 1576 traditional framing.
    Tn3270e,
    /// IBM-3278-2-E with extended 3270 data-stream capabilities and display RESPONSES.
    Tn3270eExtended,
    /// RFC 1205 display mode, IBM-3179-2 with RFC 4777 display settings.
    Tn5250e,
    /// RFC 1205 display mode, IBM-3180-2 with alternate 132×27 presentation.
    Tn5250eWide,
}

impl Profile {
    /// Name used in Telnet TERMINAL-TYPE or TN3270E DEVICE-TYPE negotiation.
    pub const fn terminal_type(self) -> &'static str {
        match self {
            Self::Tn3270e => "IBM-3278-2",
            Self::Tn3270eExtended => "IBM-3278-2-E",
            Self::Tn5250e => Model5250::Ibm3179Model2.terminal_type(),
            Self::Tn5250eWide => Model5250::Ibm3180Model2.terminal_type(),
        }
    }

    /// Returns the display model used by the 5250 presentation core.
    pub const fn model_5250(self) -> Option<Model5250> {
        match self {
            Self::Tn3270e | Self::Tn3270eExtended => None,
            Self::Tn5250e => Some(Model5250::Ibm3179Model2),
            Self::Tn5250eWide => Some(Model5250::Ibm3180Model2),
        }
    }
}

/// An explicit decision about exposing Telnet traffic on the network.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Plaintext {
    #[default]
    Denied,
    /// Permits all negotiation and subsequent traffic without encryption.
    ExplicitlyAllowed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tn3270eData {
    Display,
    Nvt,
}

/// One Telnet EOR-delimited record, without Telnet escaping or the EOR marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// 3270 data for either traditional TN3270 or TN3270E framing.
    Tn3270e {
        kind: Tn3270eData,
        /// Data without the five-byte header when enhanced framing is active.
        payload: Vec<u8>,
    },
    /// Complete 5250 logical record including the GDS header.
    Tn5250e(Vec<u8>),
}

/// One negotiated IBM Telnet connection.
///
/// Operations use blocking TCP. A session worker should own the connection.
pub struct Client {
    stream: BufReader<TcpStream>,
    framing: Framing,
    negotiation: Negotiation,
    pending_response: Option<ResponseRequest>,
    next_sequence: u16,
    active: bool,
}

impl Client {
    /// Checks the plaintext decision before opening a TCP connection and then
    /// waits for the selected profile's required negotiation.
    pub fn connect(
        address: SocketAddr,
        profile: Profile,
        plaintext: Plaintext,
    ) -> io::Result<Self> {
        Self::connect_cancellable(address, profile, plaintext, CONNECT_TIMEOUT, || false)
    }

    /// Connects with a caller-selected TCP dial timeout and polls cancellation
    /// throughout Telnet negotiation. A dial in progress is bounded by its timeout.
    pub fn connect_cancellable(
        address: SocketAddr,
        profile: Profile,
        plaintext: Plaintext,
        dial_timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Self> {
        Self::connect_cancellable_with_device_name(
            address,
            profile,
            plaintext,
            None,
            dial_timeout,
            cancelled,
        )
    }

    /// Connects as [`Self::connect_cancellable`] with an optional TN5250 device name.
    /// The name is sent only if the peer negotiates and requests it.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] if a 5250 device name is supplied
    /// for a different profile, and otherwise propagates connection errors.
    pub fn connect_cancellable_with_device_name(
        address: SocketAddr,
        profile: Profile,
        plaintext: Plaintext,
        device_name: Option<DeviceName5250>,
        dial_timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Self> {
        if plaintext != Plaintext::ExplicitlyAllowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IBM Telnet requires an explicit plaintext grant",
            ));
        }
        if device_name.is_some() && profile.model_5250().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "5250 device names require a TN5250 profile",
            ));
        }

        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let stream = TcpStream::connect_timeout(&address, dial_timeout)?;
        stream.set_nodelay(true)?;
        let mut client = Self {
            stream: BufReader::new(stream),
            framing: Framing::default(),
            negotiation: Negotiation::new(profile, device_name),
            pending_response: None,
            next_sequence: 0,
            active: true,
        };
        let deadline = Instant::now() + NEGOTIATION_TIMEOUT;
        while client.negotiation.mode().is_none() {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let poll = (Instant::now() + NEGOTIATION_POLL).min(deadline);
            match client.read_event(poll) {
                Ok(Some(event)) => {
                    if cancelled() {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    let write_deadline = (Instant::now() + WRITE_TIMEOUT).min(deadline);
                    client.handle_event(event, write_deadline)?;
                }
                Ok(None) => {
                    return Err(client.negotiation.device_rejection().unwrap_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "IBM negotiation ended early")
                    }));
                }
                Err(error)
                    if error.kind() == io::ErrorKind::TimedOut && Instant::now() < deadline => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    return Err(client.negotiation.device_rejection().unwrap_or(error));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(client)
    }

    /// Reads one record, or returns `None` for a clean peer shutdown. A read
    /// timeout preserves partial input so that a session worker can retry.
    /// After processing a 3270 record, call [`Self::complete_3270_record`]
    /// before any subsequent read or send. An outstanding requested response
    /// makes these operations return `InvalidInput` until completion.
    pub fn read_record(&mut self, timeout: Duration) -> io::Result<Option<Record>> {
        self.ensure_ready()?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid read timeout"))?;
        loop {
            let event = match self.read_event(deadline) {
                Ok(Some(event)) => event,
                Ok(None) => {
                    self.close();
                    return Ok(None);
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(error),
                Err(error) => {
                    self.close();
                    return Err(error);
                }
            };
            match self.handle_event(event, deadline) {
                Ok(Some(record)) => return Ok(Some(record)),
                Ok(None) => {}
                Err(error) => {
                    self.close();
                    return Err(error);
                }
            }
        }
    }

    /// Sends a record for the negotiated profile. Invalid caller data is
    /// rejected before writing. A failed socket write closes the connection.
    pub fn send_record(&mut self, record: &Record) -> io::Result<()> {
        self.ensure_ready()?;
        let mode = self.wire_mode()?;
        let sequence = matches!(
            (mode, record),
            (
                Mode::Tn3270e { responses: true },
                Record::Tn3270e {
                    kind: Tn3270eData::Display,
                    ..
                }
            )
        )
        .then_some(self.next_sequence);
        let wire = record.encode(mode, sequence.unwrap_or_default())?;
        self.write_record(&wire)?;
        if let Some(sequence) = sequence {
            self.next_sequence = tn3270e::next_sequence(sequence);
        }
        Ok(())
    }

    /// Completes processing of the last received 3270 record and sends its
    /// TN3270E response if requested. Returns whether a response was sent.
    /// Pass the presentation core's actual result before sending any data it
    /// produced. With no pending request this returns `Ok(false)`.
    pub fn complete_3270_record(&mut self, result: Result<(), Error3270>) -> io::Result<bool> {
        self.ensure_active()?;
        let Some(wire) = self
            .pending_response
            .take()
            .and_then(|request| request.reply(result))
        else {
            return Ok(false);
        };
        self.write_record(&wire)?;
        Ok(true)
    }

    /// Whether this live connection agreed to RFC 2355 display RESPONSES.
    pub fn tn3270e_responses_enabled(&self) -> bool {
        self.active
            && matches!(
                self.negotiation.mode(),
                Some(Mode::Tn3270e { responses: true })
            )
    }

    fn write_record(&mut self, wire: &[u8]) -> io::Result<()> {
        let result = self.write(wire, Instant::now() + WRITE_TIMEOUT);
        if result.is_err() {
            self.close();
        }
        result
    }

    fn read_event(&mut self, deadline: Instant) -> io::Result<Option<Event>> {
        loop {
            if self.stream.buffer().is_empty() {
                self.stream
                    .get_ref()
                    .set_read_timeout(Some(remaining(deadline)?))?;
                match self.stream.fill_buf() {
                    Ok([]) if self.framing.is_idle() => return Ok(None),
                    Ok([]) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "partial IBM Telnet frame",
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, error));
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(&byte) = self.stream.buffer().first() {
                self.stream.consume(1);
                if let Some(event) = self.framing.push(byte, self.negotiation.mode().is_some())? {
                    return Ok(Some(event));
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event, deadline: Instant) -> io::Result<Option<Record>> {
        let reply = match event {
            Event::Record(bytes) => {
                let (record, response) = Record::decode(self.wire_mode()?, bytes)?;
                self.pending_response = response;
                return Ok(Some(record));
            }
            Event::Option(command, option) => self.negotiation.option(command, option)?,
            Event::Subnegotiation(option, data) => {
                self.negotiation.subnegotiation(option, &data)?
            }
        };
        if let Some(reply) = reply {
            self.write(&reply, deadline)?;
        }
        Ok(None)
    }

    fn write(&mut self, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        let stream = self.stream.get_mut();
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, error));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn ensure_active(&self) -> io::Result<()> {
        if self.active {
            Ok(())
        } else {
            Err(io::ErrorKind::NotConnected.into())
        }
    }

    fn ensure_ready(&self) -> io::Result<()> {
        self.ensure_active()?;
        if self.pending_response.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "complete the pending TN3270E record before the next read or send",
            ));
        }
        Ok(())
    }

    fn wire_mode(&self) -> io::Result<Mode> {
        self.negotiation
            .mode()
            .ok_or_else(|| invalid("IBM negotiation is incomplete"))
    }

    fn close(&mut self) {
        self.active = false;
        let _ = self.stream.get_ref().shutdown(Shutdown::Both);
    }
}

impl Record {
    fn decode(mode: Mode, mut bytes: Vec<u8>) -> io::Result<(Self, Option<ResponseRequest>)> {
        match mode {
            Mode::Tn3270e { responses } => {
                let (kind, response) = tn3270e::decode_header(&bytes, responses)?;
                bytes.drain(..HEADER_LEN);
                Ok((
                    Self::Tn3270e {
                        kind,
                        payload: bytes,
                    },
                    response,
                ))
            }
            Mode::Tn3270 if !bytes.is_empty() => Ok((
                Self::Tn3270e {
                    kind: Tn3270eData::Display,
                    payload: bytes,
                },
                None,
            )),
            Mode::Tn3270 => Err(invalid("empty TN3270 record")),
            Mode::Tn5250 if valid_gds(&bytes) => Ok((Self::Tn5250e(bytes), None)),
            Mode::Tn5250 => Err(invalid("invalid 5250 GDS header")),
        }
    }

    fn encode(&self, mode: Mode, sequence: u16) -> io::Result<Vec<u8>> {
        match (mode, self) {
            (Mode::Tn3270e { .. }, Self::Tn3270e { kind, payload })
                if payload.len() <= MAX_RECORD - HEADER_LEN =>
            {
                Ok(telnet::encode_record(
                    &tn3270e::display_header(*kind, sequence),
                    payload,
                ))
            }
            (
                Mode::Tn3270,
                Self::Tn3270e {
                    kind: Tn3270eData::Display,
                    payload,
                },
            ) if !payload.is_empty() && payload.len() <= MAX_RECORD => {
                Ok(telnet::encode_record(&[], payload))
            }
            (Mode::Tn5250, Self::Tn5250e(bytes)) if valid_gds(bytes) => {
                Ok(telnet::encode_record(&[], bytes))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "record does not fit the negotiated IBM profile",
            )),
        }
    }
}

fn valid_gds(bytes: &[u8]) -> bool {
    if let Some((
        &[
            length_high,
            length_low,
            0x12,
            0xa0,
            _,
            _,
            variable_header,
            _,
            _,
            _,
        ],
        _,
    )) = bytes.split_first_chunk::<10>()
    {
        let length = usize::from(u16::from_be_bytes([length_high, length_low]));
        length == bytes.len() && variable_header >= 4 && usize::from(variable_header) + 6 <= length
    } else {
        false
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
