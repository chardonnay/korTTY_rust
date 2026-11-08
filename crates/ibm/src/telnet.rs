use std::io;

use crate::{MAX_RECORD, invalid};

pub(crate) const BINARY: u8 = 0;
pub(crate) const SGA: u8 = 3;
pub(crate) const TERMINAL_TYPE: u8 = 24;
pub(crate) const EOR_OPTION: u8 = 25;
pub(crate) const NEW_ENVIRON: u8 = 39;
pub(crate) const TN3270E: u8 = 40;

const EOR: u8 = 239;
const SE: u8 = 240;
const SB: u8 = 250;
pub(crate) const WILL: u8 = 251;
pub(crate) const WONT: u8 = 252;
pub(crate) const DO: u8 = 253;
pub(crate) const DONT: u8 = 254;
const IAC: u8 = 255;

const MAX_SUBNEGOTIATION: usize = 4096;

#[derive(Default)]
enum State {
    #[default]
    Data,
    Iac,
    Option(u8),
    SubOption,
    SubData(u8),
    SubIac(u8),
}

pub(crate) enum Event {
    Record(Vec<u8>),
    Option(u8, u8),
    Subnegotiation(u8, Vec<u8>),
}

#[derive(Default)]
pub(crate) struct Framing {
    state: State,
    record: Vec<u8>,
    subnegotiation: Vec<u8>,
}

impl Framing {
    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.state, State::Data) && self.record.is_empty()
    }

    pub(crate) fn push(&mut self, byte: u8, ready: bool) -> io::Result<Option<Event>> {
        self.state = match self.state {
            State::Data if byte == IAC => State::Iac,
            State::Data => {
                self.push_record(byte, ready)?;
                State::Data
            }
            State::Iac => match byte {
                IAC => {
                    self.push_record(byte, ready)?;
                    State::Data
                }
                EOR if ready => {
                    self.state = State::Data;
                    return Ok(Some(Event::Record(std::mem::take(&mut self.record))));
                }
                EOR => return Err(invalid("IBM Telnet EOR before negotiation completed")),
                SB => State::SubOption,
                WILL | WONT | DO | DONT => State::Option(byte),
                236..=238 | 241..=249 => State::Data,
                _ => return Err(invalid("invalid IBM Telnet command")),
            },
            State::Option(command) => {
                self.state = State::Data;
                return Ok(Some(Event::Option(command, byte)));
            }
            State::SubOption => State::SubData(byte),
            State::SubData(option) if byte == IAC => State::SubIac(option),
            State::SubData(option) => {
                self.push_subnegotiation(byte)?;
                State::SubData(option)
            }
            State::SubIac(option) => match byte {
                IAC => {
                    self.push_subnegotiation(byte)?;
                    State::SubData(option)
                }
                SE => {
                    self.state = State::Data;
                    return Ok(Some(Event::Subnegotiation(
                        option,
                        std::mem::take(&mut self.subnegotiation),
                    )));
                }
                _ => return Err(invalid("invalid IAC in IBM Telnet subnegotiation")),
            },
        };
        Ok(None)
    }

    fn push_record(&mut self, byte: u8, ready: bool) -> io::Result<()> {
        if !ready {
            return Err(invalid("IBM Telnet data before negotiation completed"));
        }
        if self.record.len() == MAX_RECORD {
            return Err(invalid("IBM Telnet record exceeds the wire limit"));
        }
        self.record.push(byte);
        Ok(())
    }

    fn push_subnegotiation(&mut self, byte: u8) -> io::Result<()> {
        if self.subnegotiation.len() == MAX_SUBNEGOTIATION {
            return Err(invalid("IBM Telnet subnegotiation exceeds the wire limit"));
        }
        self.subnegotiation.push(byte);
        Ok(())
    }
}

pub(crate) fn encode_option(command: u8, option: u8) -> Vec<u8> {
    vec![IAC, command, option]
}

pub(crate) fn encode_subnegotiation(option: u8, bytes: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(bytes.len() + 5);
    wire.extend([IAC, SB, option]);
    append_escaped(&mut wire, bytes);
    wire.extend([IAC, SE]);
    wire
}

pub(crate) fn encode_record(header: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(header.len() + payload.len() + 2);
    append_escaped(&mut wire, header);
    append_escaped(&mut wire, payload);
    wire.extend([IAC, EOR]);
    wire
}

fn append_escaped(wire: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        wire.push(byte);
        if byte == IAC {
            wire.push(IAC);
        }
    }
}
