use std::io::Cursor;

use super::*;

// Test double that delivers one byte per read, independently of the write stream.
struct Wire<'a> {
    requests: Cursor<&'a [u8]>,
    replies: Vec<u8>,
}

impl<'a> Wire<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            requests: Cursor::new(input),
            replies: Vec::new(),
        }
    }
}

impl Read for Wire<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(1);
        self.requests.read(&mut output[..count])
    }
}

impl Write for Wire<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.replies.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn fragmented_socks5_keeps_absolute_dns_and_leaves_payload_unread() {
    let mut wire = Wire::new(b"\x05\x02\x02\x00\x05\x01\x00\x03\x0cExample.com.\x01\xbbpayload");
    let request = handshake(&mut wire).unwrap();
    assert_eq!(request.version, Version::Five);
    assert_eq!(request.host, "example.com.");
    assert_eq!(request.port, 443);
    assert_eq!(wire.replies, [5, 0]);
    request.version.reply(&mut wire, true).unwrap();
    assert_eq!(&wire.replies[2..], &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    let mut payload = Vec::new();
    wire.read_to_end(&mut payload).unwrap();
    assert_eq!(payload, b"payload");
}

#[test]
fn socks4_and_4a_decode_ipv4_and_server_side_dns() {
    for (input, host) in [
        (
            &b"\x04\x01\x00\x50\x7f\x00\x00\x01user\x00"[..],
            "127.0.0.1",
        ),
        (
            &b"\x04\x01\x00\x50\x00\x00\x00\x01user\x00server.example\x00"[..],
            "server.example",
        ),
    ] {
        let mut wire = Wire::new(input);
        let request = handshake(&mut wire).unwrap();
        assert_eq!(request.version, Version::Four);
        assert_eq!(request.host, host);
        assert_eq!(request.port, 80);
        request.version.reply(&mut wire, true).unwrap();
        assert_eq!(wire.replies, [0, 90, 0, 0, 0, 0, 0, 0]);
    }
}

#[test]
fn invalid_requests_fail_without_opening_a_channel() {
    for (input, reply) in [
        (&b"\x05\x01\x02"[..], &b"\x05\xff"[..]),
        (
            &b"\x05\x01\x00\x05\x03\x00\x01"[..],
            &b"\x05\x00\x05\x07\x00\x01\x00\x00\x00\x00\x00\x00"[..],
        ),
        (
            &b"\x05\x01\x00\x05\x01\x00\x03\x00"[..],
            &b"\x05\x00\x05\x08\x00\x01\x00\x00\x00\x00\x00\x00"[..],
        ),
        (
            &b"\x04\x02\x00\x50\x7f\x00\x00\x01"[..],
            &b"\x00\x5b\x00\x00\x00\x00\x00\x00"[..],
        ),
    ] {
        let mut wire = Wire::new(input);
        assert!(handshake(&mut wire).is_err());
        assert_eq!(wire.replies, reply);
    }
    let mut bytes = b"\x04\x01\x00\x50\x7f\x00\x00\x01".to_vec();
    bytes.extend([b'x'; 256]);
    bytes.push(0);
    assert!(handshake(&mut Wire::new(&bytes)).is_err());
}
