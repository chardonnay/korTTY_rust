use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::ssh_dialog::normalize_host;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Version {
    Four,
    Five,
}

pub(super) struct Request {
    pub version: Version,
    pub host: String,
    pub port: u16,
}

impl Version {
    pub fn reply(self, stream: &mut impl Write, connected: bool) -> io::Result<()> {
        self.send_reply(stream, if connected { 0 } else { 1 })
    }

    fn send_reply(self, stream: &mut impl Write, result: u8) -> io::Result<()> {
        // SSH's open confirmation does not disclose the remote socket's local address.
        match self {
            Self::Four => {
                stream.write_all(&[0, if result == 0 { 90 } else { 91 }, 0, 0, 0, 0, 0, 0])
            }
            Self::Five => stream.write_all(&[5, result, 0, 1, 0, 0, 0, 0, 0, 0]),
        }
    }

    fn reject<T>(self, stream: &mut impl Write, code: u8, message: &str) -> io::Result<T> {
        self.send_reply(stream, code)?;
        Err(invalid(message))
    }
}

pub(super) fn handshake(stream: &mut (impl Read + Write)) -> io::Result<Request> {
    match read::<1>(stream)?[0] {
        4 => request_v4(stream),
        5 => request_v5(stream),
        _ => Err(invalid("unsupported SOCKS version")),
    }
}

fn request_v4(stream: &mut (impl Read + Write)) -> io::Result<Request> {
    let version = Version::Four;
    let [command, hi, lo, a, b, c, d] = read(stream)?;
    let port = u16::from_be_bytes([hi, lo]);
    if command != 1 || port == 0 {
        return version.reject(stream, 7, "invalid SOCKS4 CONNECT request");
    }
    let _user = read_nul(stream)?;
    let host = if [a, b, c] == [0, 0, 0] && d != 0 {
        let domain = read_nul(stream)?;
        match domain_name(&domain) {
            Ok(host) => host,
            Err(_) => return version.reject(stream, 8, "invalid SOCKS4a destination"),
        }
    } else {
        Ipv4Addr::new(a, b, c, d).to_string()
    };
    Ok(Request {
        version,
        host,
        port,
    })
}

fn request_v5(stream: &mut (impl Read + Write)) -> io::Result<Request> {
    let version = Version::Five;
    let count = usize::from(read::<1>(stream)?[0]);
    let mut methods = [0; 255];
    stream.read_exact(&mut methods[..count])?;
    if !methods[..count].contains(&0) {
        stream.write_all(&[5, 255])?;
        return Err(invalid(
            "SOCKS5 client did not offer the no-authentication method",
        ));
    }
    stream.write_all(&[5, 0])?;
    let [protocol, command, reserved, address_kind] = read(stream)?;
    if protocol != 5 || reserved != 0 {
        return version.reject(stream, 1, "invalid SOCKS5 request header");
    }
    if command != 1 {
        return version.reject(stream, 7, "SOCKS5 request is not CONNECT");
    }
    let host = match address_kind {
        1 => Ipv4Addr::from(read::<4>(stream)?).to_string(),
        4 => Ipv6Addr::from(read::<16>(stream)?).to_string(),
        3 => {
            let len = usize::from(read::<1>(stream)?[0]);
            let mut domain = [0; 255];
            stream.read_exact(&mut domain[..len])?;
            match domain_name(&domain[..len]) {
                Ok(host) => host,
                Err(_) => return version.reject(stream, 8, "invalid SOCKS5 destination"),
            }
        }
        _ => return version.reject(stream, 8, "unsupported SOCKS5 address type"),
    };
    let port = u16::from_be_bytes(read(stream)?);
    if port == 0 {
        return version.reject(stream, 1, "invalid SOCKS5 destination port");
    }
    Ok(Request {
        version,
        host,
        port,
    })
}

fn domain_name(bytes: &[u8]) -> io::Result<String> {
    let input = std::str::from_utf8(bytes).map_err(|_| invalid("invalid SOCKS domain"))?;
    if input.trim() != input {
        return Err(invalid("invalid SOCKS domain"));
    }
    normalize_host(input).map_err(|_| invalid("invalid SOCKS domain"))
}

fn read_nul(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut value = Vec::new();
    for _ in 0..=255 {
        let [byte] = read(stream)?;
        if byte == 0 {
            return Ok(value);
        }
        value.push(byte);
    }
    Err(invalid("SOCKS4 field exceeds 255 bytes"))
}

fn read<const N: usize>(stream: &mut impl Read) -> io::Result<[u8; N]> {
    let mut field = [0; N];
    stream.read_exact(&mut field)?;
    Ok(field)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
