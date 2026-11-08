//! RFC 2355 §§8.1 and 10.4 response handling for a display client.

use std::io;

use crate::{Tn3270eData, invalid, screen3270::Error3270, telnet};

pub(crate) const HEADER_LEN: usize = 5;
const DISPLAY: u8 = 0;
const RESPONSE: u8 = 2;
const NVT: u8 = 5;
const REQUEST: u8 = 6;
const MAX_SEQUENCE: u16 = 0x7fff;

pub(crate) struct ResponseRequest {
    sequence: [u8; 2],
    always: bool,
}

impl ResponseRequest {
    pub(crate) fn reply(self, result: Result<(), Error3270>) -> Option<Vec<u8>> {
        let (flag, status) = match result {
            Ok(()) if self.always => (0, 0),
            Ok(()) => return None,
            Err(
                Error3270::MissingCommand
                | Error3270::UnsupportedCommand(_)
                | Error3270::UnsupportedStructuredField(_),
            ) => (1, 0),
            Err(_) => (1, 2),
        };
        let [high, low] = self.sequence;
        Some(telnet::encode_record(
            &[RESPONSE, 0, flag, high, low],
            &[status],
        ))
    }
}

pub(crate) fn display_header(kind: Tn3270eData, sequence: u16) -> [u8; HEADER_LEN] {
    let data_type = match kind {
        Tn3270eData::Display => DISPLAY,
        Tn3270eData::Nvt => NVT,
    };
    let [high, low] = sequence.to_be_bytes();
    [data_type, 0, 0, high, low]
}

pub(crate) fn next_sequence(sequence: u16) -> u16 {
    (sequence + 1) & MAX_SEQUENCE
}

pub(crate) fn decode_header(
    bytes: &[u8],
    responses: bool,
) -> io::Result<(Tn3270eData, Option<ResponseRequest>)> {
    let Some((&[data_type, _, flag, high, low], _)) = bytes.split_first_chunk::<HEADER_LEN>()
    else {
        return Err(invalid("truncated TN3270E header"));
    };
    match data_type {
        DISPLAY if responses => {
            let sequence = [high, low];
            if u16::from_be_bytes(sequence) > MAX_SEQUENCE || flag > 2 {
                return Err(invalid("invalid TN3270E display response flag or sequence"));
            }
            Ok((
                Tn3270eData::Display,
                (flag != 0).then_some(ResponseRequest {
                    sequence,
                    always: flag == 2,
                }),
            ))
        }
        DISPLAY if flag == 0 => Ok((Tn3270eData::Display, None)),
        NVT => Ok((Tn3270eData::Nvt, None)),
        // This client always sends NO-RESPONSE. ERR-COND-CLEARED only flows
        // from a printer client to its server, so neither is valid inbound.
        RESPONSE if responses => Err(invalid("unsolicited TN3270E response")),
        REQUEST if responses => Err(invalid("unexpected TN3270E request from server")),
        _ => Err(invalid("unnegotiated TN3270E data type or response")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_wraps_after_the_largest_15_bit_number() {
        assert_eq!(next_sequence(0), 1);
        assert_eq!(next_sequence(0x7ffe), 0x7fff);
        assert_eq!(next_sequence(0x7fff), 0);
    }
}
