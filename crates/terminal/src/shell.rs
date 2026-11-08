//! Passive shell data. The terminal does not act on a path or trust its host.

use std::net::Ipv6Addr;

use crate::is_bidi_control;

const MAX_SHELL_PAYLOAD: usize = 4096;

/// Screen coordinates when the shell event arrived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellPosition {
    /// Zero-based row of the visible screen.
    pub row: u16,
    /// Zero-based grid column.
    pub column: u16,
    /// Number of retained history rows at the time of the event.
    pub scrollback_lines: usize,
    /// Whether the alternate screen was active.
    pub alternate_screen: bool,
}

/// An announcement from program output. Text is untrusted even after validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellEvent {
    pub kind: ShellEventKind,
    pub position: ShellPosition,
}

/// Shell prompts, command transitions and working directory reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellEventKind {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandFinished {
        exit_status: Option<i32>,
    },
    /// A passive OSC 7 location. An empty host means the URI supplied no host.
    WorkingDirectory {
        host: String,
        path: String,
    },
}

impl ShellEventKind {
    pub(crate) fn parse_mark(payload: &str) -> Option<Self> {
        if payload.len() > MAX_SHELL_PAYLOAD {
            return None;
        }
        let mut parameters = payload.split(';');
        let kind = match parameters.next()? {
            "A" => Self::PromptStart,
            "B" => Self::CommandStart,
            "C" => Self::OutputStart,
            "D" => Self::CommandFinished {
                exit_status: parameters.find_map(|value| {
                    let digits = value.strip_prefix('-').unwrap_or(value);
                    if (1..=10).contains(&digits.len())
                        && digits.bytes().all(|byte| byte.is_ascii_digit())
                    {
                        value.parse().ok()
                    } else {
                        None
                    }
                }),
            },
            _ => return None,
        };
        Some(kind)
    }

    pub(crate) fn parse_directory(payload: &str) -> Option<Self> {
        if payload.len() > MAX_SHELL_PAYLOAD {
            return None;
        }
        let (scheme, authority_and_path) = payload.split_once("://")?;
        if !scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        let path_start = authority_and_path.find('/')?;
        let (host, path) = authority_and_path.split_at(path_start);
        if !valid_host(host) || path.contains(['?', '#']) {
            return None;
        }
        let path = decode_path(path)?;
        Some(Self::WorkingDirectory {
            host: host.to_ascii_lowercase(),
            path,
        })
    }
}

fn valid_host(host: &str) -> bool {
    if let Some(ip) = host.strip_prefix('[').and_then(|ip| ip.strip_suffix(']')) {
        return ip.parse::<Ipv6Addr>().is_ok();
    }
    host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn decode_path(path: &str) -> Option<String> {
    let mut source = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(byte) = source.next() {
        let byte = if byte == b'%' {
            let high = char::from(source.next()?).to_digit(16)? as u8;
            let low = char::from(source.next()?).to_digit(16)? as u8;
            (high << 4) | low
        } else {
            byte
        };
        decoded.push(byte);
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded
        .chars()
        .any(|ch| ch.is_control() || is_bidi_control(ch)))
    .then_some(decoded)
}
