use std::io;

use crate::telnet::{
    BINARY, DO, DONT, EOR_OPTION, NEW_ENVIRON, SGA, TERMINAL_TYPE, TN3270E, WILL, WONT,
    encode_option, encode_subnegotiation,
};
use crate::{DeviceName5250, Profile, environment5250, invalid};

const DEVICE_TYPE: u8 = 2;
const FUNCTIONS: u8 = 3;
const DEVICE_IS: u8 = 4;
const REASON: u8 = 5;
const REJECT: u8 = 6;
const REQUEST: u8 = 7;
const DEVICE_SEND: u8 = 8;
const CONNECT: u8 = 1;
const RESPONSES: u8 = 2;

const TELNET_IS: u8 = 0;
const TELNET_SEND: u8 = 1;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Tn3270eState {
    AwaitDeviceSend,
    AwaitDevice,
    AwaitFunctions { responses: bool, countered: bool },
    Ready { responses: bool },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Mode {
    Tn3270e { responses: bool },
    Tn3270,
    Tn5250,
}

pub(crate) struct Negotiation {
    profile: Profile,
    mode: Option<Mode>,
    local: [bool; 256],
    remote: [bool; 256],
    terminal_type_sent: bool,
    tn3270e: Tn3270eState,
    device_rejection: Option<u8>,
    device_name_5250: Option<DeviceName5250>,
}

impl Negotiation {
    pub(crate) fn new(profile: Profile, device_name_5250: Option<DeviceName5250>) -> Self {
        Self {
            profile,
            mode: None,
            local: [false; 256],
            remote: [false; 256],
            terminal_type_sent: false,
            tn3270e: Tn3270eState::AwaitDeviceSend,
            device_rejection: None,
            device_name_5250,
        }
    }

    pub(crate) fn mode(&self) -> Option<Mode> {
        self.mode
    }

    pub(crate) fn device_rejection(&self) -> Option<io::Error> {
        self.device_rejection.map(|reason| {
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("TN3270E server rejected the device type (reason {reason})"),
            )
        })
    }

    fn update_mode(&mut self) -> io::Result<()> {
        let tn3270e_enabled = self.local[usize::from(TN3270E)];
        let basic_options = self.terminal_type_sent
            && self.local[usize::from(TERMINAL_TYPE)]
            && [BINARY, EOR_OPTION]
                .into_iter()
                .map(usize::from)
                .all(|index| self.local[index] && self.remote[index]);

        // Once selected, the wire format stays fixed. A peer cannot switch
        // the interpretation of an in-flight record by withdrawing options.
        let still_valid = match self.mode {
            Some(Mode::Tn3270e { .. }) => tn3270e_enabled,
            Some(Mode::Tn3270 | Mode::Tn5250) => basic_options,
            None => {
                self.mode = match self.profile {
                    Profile::Tn3270e | Profile::Tn3270eExtended if tn3270e_enabled => {
                        match self.tn3270e {
                            Tn3270eState::Ready { responses } => Some(Mode::Tn3270e { responses }),
                            _ => None,
                        }
                    }
                    Profile::Tn3270e | Profile::Tn3270eExtended => {
                        basic_options.then_some(Mode::Tn3270)
                    }
                    Profile::Tn5250e | Profile::Tn5250eWide => {
                        basic_options.then_some(Mode::Tn5250)
                    }
                };
                true
            }
        };
        if !still_valid {
            return Err(invalid("peer withdrew a required IBM Telnet option"));
        }
        Ok(())
    }

    pub(crate) fn option(&mut self, command: u8, option: u8) -> io::Result<Option<Vec<u8>>> {
        let local_supported = match option {
            BINARY | EOR_OPTION | SGA | TERMINAL_TYPE => true,
            NEW_ENVIRON => self.profile.model_5250().is_some(),
            TN3270E => {
                self.profile.model_5250().is_none()
                    && self.mode != Some(Mode::Tn3270)
                    && self.device_rejection.is_none()
            }
            _ => false,
        };
        let remote_supported = matches!(option, BINARY | EOR_OPTION | SGA);
        let index = usize::from(option);
        let response = match command {
            DO if !local_supported => Some(WONT),
            DO if !self.local[index] => {
                self.local[index] = true;
                Some(WILL)
            }
            WILL if !remote_supported => Some(DONT),
            WILL if !self.remote[index] => {
                self.remote[index] = true;
                Some(DO)
            }
            DONT if self.local[index] => {
                self.local[index] = false;
                if option == TERMINAL_TYPE {
                    self.terminal_type_sent = false;
                } else if option == TN3270E {
                    self.tn3270e = Tn3270eState::AwaitDeviceSend;
                }
                Some(WONT)
            }
            WONT if self.remote[index] => {
                self.remote[index] = false;
                Some(DONT)
            }
            _ => None,
        };
        self.update_mode()?;
        Ok(response.map(|response| encode_option(response, option)))
    }

    pub(crate) fn subnegotiation(
        &mut self,
        option: u8,
        data: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        if !self.local[usize::from(option)] {
            return Ok(None);
        }
        let reply = match (self.profile, option) {
            (Profile::Tn3270e | Profile::Tn3270eExtended, TN3270E) => {
                self.tn3270e_subnegotiation(data)?
            }
            (profile, TERMINAL_TYPE) if data == [TELNET_SEND] => {
                self.terminal_type_sent = true;
                let mut reply = vec![TELNET_IS];
                reply.extend_from_slice(profile.terminal_type().as_bytes());
                Some(encode_subnegotiation(TERMINAL_TYPE, &reply))
            }
            (_, TERMINAL_TYPE) => return Err(invalid("invalid IBM terminal type request")),
            (profile, NEW_ENVIRON) if profile.model_5250().is_some() => {
                let reply = environment5250::reply(data, self.device_name_5250.as_ref())?;
                Some(encode_subnegotiation(NEW_ENVIRON, &reply))
            }
            _ => None,
        };
        self.update_mode()?;
        Ok(reply)
    }

    fn tn3270e_subnegotiation(&mut self, data: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let device_type = self.profile.terminal_type().as_bytes();
        let reply = match (self.tn3270e, data) {
            (Tn3270eState::AwaitDeviceSend, [DEVICE_SEND, DEVICE_TYPE]) => {
                self.tn3270e = Tn3270eState::AwaitDevice;
                let mut reply = vec![DEVICE_TYPE, REQUEST];
                reply.extend_from_slice(device_type);
                reply
            }
            (Tn3270eState::AwaitDevice, [DEVICE_TYPE, DEVICE_IS, device @ ..])
                if is_requested_device(device_type, device) =>
            {
                let responses = self.profile == Profile::Tn3270eExtended;
                self.tn3270e = Tn3270eState::AwaitFunctions {
                    responses,
                    countered: false,
                };
                functions(REQUEST, responses)
            }
            (Tn3270eState::AwaitDevice, [DEVICE_TYPE, REJECT, REASON, reason]) => {
                self.local[usize::from(TN3270E)] = false;
                self.device_rejection = Some(*reason);
                return Ok(Some(encode_option(WONT, TN3270E)));
            }
            (
                Tn3270eState::AwaitFunctions { responses, .. },
                [FUNCTIONS, DEVICE_IS, agreed @ ..],
            ) if agreed == function_list(responses) => {
                self.tn3270e = Tn3270eState::Ready { responses };
                return Ok(None);
            }
            (
                Tn3270eState::AwaitFunctions {
                    responses: proposed,
                    countered,
                },
                [FUNCTIONS, REQUEST, offered @ ..],
            ) => {
                let responses = proposed && offered.contains(&RESPONSES);
                if offered == function_list(responses) {
                    self.tn3270e = Tn3270eState::Ready { responses };
                    functions(DEVICE_IS, responses)
                } else if !countered || proposed != responses {
                    self.tn3270e = Tn3270eState::AwaitFunctions {
                        responses,
                        countered: true,
                    };
                    functions(REQUEST, responses)
                } else {
                    return Err(invalid(
                        "TN3270E server repeatedly requested unsupported functions",
                    ));
                }
            }
            _ => return Err(invalid("unexpected TN3270E subnegotiation")),
        };
        Ok(Some(encode_subnegotiation(TN3270E, &reply)))
    }
}

fn function_list(responses: bool) -> &'static [u8] {
    if responses { &[RESPONSES] } else { &[] }
}

fn functions(command: u8, responses: bool) -> Vec<u8> {
    let mut bytes = vec![FUNCTIONS, command];
    bytes.extend_from_slice(function_list(responses));
    bytes
}

fn is_requested_device(requested: &[u8], bytes: &[u8]) -> bool {
    bytes.strip_prefix(requested).is_some_and(|rest| {
        matches!(rest, [CONNECT, name @ ..] if !name.is_empty() && name.iter().all(u8::is_ascii_graphic))
    })
}
