use std::{io, str::FromStr};

use crate::invalid;

const IS: u8 = 0;
const SEND: u8 = 1;
const VAR: u8 = 0;
const VALUE: u8 = 1;
const ESC: u8 = 2;
const USERVAR: u8 = 3;
const MAX_ENVIRONMENT_BYTES: usize = 1024;

// IBM i pairs the USB keyboard with character set 697 and code page 37.
// The latter is also the code page of the local 5250 screen codec.
const DISPLAY_ENVIRONMENT: [(&[u8], &[u8]); 3] = [
    (b"KBDTYPE", b"USB"),
    (b"CODEPAGE", b"37"),
    (b"CHARSET", b"697"),
];

/// Validated name of a TN5250 virtual display device (RFC 4777 DEVNAME).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceName5250(String);

impl FromStr for DeviceName5250 {
    type Err = io::Error;

    /// Accepts one to ten ASCII characters and normalizes letters to uppercase.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] for names outside the IBM i
    /// DEVNAME character and length rules.
    fn from_str(name: &str) -> io::Result<Self> {
        let valid_first = |byte: u8| byte.is_ascii_alphabetic() || b"#$@".contains(&byte);
        let valid = name.len() <= 10
            && name.as_bytes().split_first().is_some_and(|(&first, rest)| {
                valid_first(first)
                    && rest
                        .iter()
                        .copied()
                        .all(|byte| valid_first(byte) || byte.is_ascii_digit() || byte == b'_')
            });
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "5250 device name must use 1–10 letters, digits, #, $, @ or _, starting with a letter, #, $ or @",
            ));
        }
        Ok(Self(name.to_ascii_uppercase()))
    }
}

pub(crate) fn reply(data: &[u8], device: Option<&DeviceName5250>) -> io::Result<Vec<u8>> {
    let Some((&SEND, mut names)) = data.split_first() else {
        return Err(invalid("invalid 5250 environment request"));
    };
    let mut reply = vec![IS];
    // RFC 4777 §3 shows iSeries asking for its display USERVARs with a
    // bare VAR selector. Treat that form like an unqualified SEND.
    if matches!(names, [] | [VAR]) {
        append_display_environment(&mut reply, device);
        return Ok(reply);
    }
    while let Some((&kind, rest)) = names.split_first() {
        if !matches!(kind, VAR | USERVAR) {
            return Err(invalid("invalid 5250 environment variable type"));
        }
        let mut end = 0;
        while end < rest.len() {
            match rest[end] {
                VAR | USERVAR => break,
                VALUE => return Err(invalid("value in a 5250 environment request")),
                ESC if rest.get(end + 1).is_some_and(|&byte| byte <= USERVAR) => end += 2,
                ESC => return Err(invalid("invalid 5250 environment escape")),
                _ => end += 1,
            }
        }
        let name = &rest[..end];
        if name.is_empty() {
            if kind == USERVAR {
                append_display_environment(&mut reply, device);
            }
        } else {
            let value = match (kind, name) {
                (USERVAR, b"DEVNAME") => device.map(|device| device.0.as_bytes()),
                (USERVAR, _) => DISPLAY_ENVIRONMENT
                    .iter()
                    .find_map(|&(variable, value)| (variable == name).then_some(value)),
                _ => None,
            };
            append_variable(&mut reply, kind, name, value);
        }
        if reply.len() - 1 > MAX_ENVIRONMENT_BYTES {
            return Err(invalid("5250 environment reply exceeds the wire limit"));
        }
        names = &rest[end..];
    }
    Ok(reply)
}

fn append_display_environment(reply: &mut Vec<u8>, device: Option<&DeviceName5250>) {
    for (name, value) in DISPLAY_ENVIRONMENT {
        append_variable(reply, USERVAR, name, Some(value));
    }
    if let Some(device) = device {
        append_variable(reply, USERVAR, b"DEVNAME", Some(device.0.as_bytes()));
    }
}

fn append_variable(reply: &mut Vec<u8>, kind: u8, name: &[u8], value: Option<&[u8]>) {
    // Names received from the host already carry RFC 1572 escaping. Values
    // are limited to the fixed display settings and validated ASCII DEVNAME.
    reply.push(kind);
    reply.extend_from_slice(name);
    if let Some(value) = value {
        reply.push(VALUE);
        reply.extend_from_slice(value);
    }
}
