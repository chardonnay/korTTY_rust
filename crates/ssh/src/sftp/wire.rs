use std::io::{Read, Write};

use super::{Attributes, Error, RemoteStatus, StatusCode};

pub(super) const INIT: u8 = 1;
pub(super) const VERSION: u8 = 2;
pub(super) const OPEN: u8 = 3;
pub(super) const CLOSE: u8 = 4;
pub(super) const READ: u8 = 5;
pub(super) const WRITE: u8 = 6;
pub(super) const LSTAT: u8 = 7;
pub(super) const FSTAT: u8 = 8;
pub(super) const SETSTAT: u8 = 9;
pub(super) const FSETSTAT: u8 = 10;
pub(super) const OPENDIR: u8 = 11;
pub(super) const READDIR: u8 = 12;
pub(super) const REMOVE: u8 = 13;
pub(super) const MKDIR: u8 = 14;
pub(super) const RMDIR: u8 = 15;
pub(super) const REALPATH: u8 = 16;
pub(super) const STAT: u8 = 17;
pub(super) const RENAME: u8 = 18;
pub(super) const STATUS: u8 = 101;
pub(super) const HANDLE: u8 = 102;
pub(super) const DATA: u8 = 103;
pub(super) const NAME: u8 = 104;
pub(super) const ATTRS: u8 = 105;
pub(super) const EXTENDED: u8 = 200;

const ATTR_SIZE: u32 = 0x0000_0001;
pub(super) const ATTR_UID_GID: u32 = 0x0000_0002;
pub(super) const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_TIMES: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;
const ATTR_MASK: u32 = ATTR_SIZE | ATTR_UID_GID | ATTR_PERMISSIONS | ATTR_TIMES | ATTR_EXTENDED;

pub(super) const MAX_PACKET: usize = 256 * 1024;

pub(super) fn send(writer: &mut impl Write, packet: &[u8]) -> Result<(), Error> {
    if packet.is_empty() || packet.len() > MAX_PACKET {
        return Err(Error::Limit("outgoing SFTP packet length"));
    }
    writer.write_all(&(packet.len() as u32).to_be_bytes())?;
    writer.write_all(packet)?;
    writer.flush()?;
    Ok(())
}

pub(super) fn receive(reader: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_PACKET {
        return Err(Error::Protocol("invalid incoming SFTP packet length"));
    }
    let mut packet = vec![0; length];
    reader.read_exact(&mut packet)?;
    Ok(packet)
}

pub(super) fn string(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    let length = u32::try_from(bytes.len()).map_err(|_| Error::Limit("SFTP string length"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

pub(super) struct Decoder<'a>(&'a [u8]);

impl<'a> Decoder<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let (field, remaining) = self
            .0
            .split_at_checked(length)
            .ok_or(Error::Protocol("truncated SFTP response field"))?;
        self.0 = remaining;
        Ok(field)
    }

    pub(super) fn length(&self) -> usize {
        self.0.len()
    }

    pub(super) fn finish(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Protocol("trailing bytes in SFTP response"))
        }
    }

    pub(super) fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self
            .0
            .first_chunk::<4>()
            .ok_or(Error::Protocol("truncated SFTP response integer"))?;
        self.0 = &self.0[4..];
        Ok(u32::from_be_bytes(*bytes))
    }

    pub(super) fn u64(&mut self) -> Result<u64, Error> {
        let bytes = self
            .0
            .first_chunk::<8>()
            .ok_or(Error::Protocol("truncated SFTP response integer"))?;
        self.0 = &self.0[8..];
        Ok(u64::from_be_bytes(*bytes))
    }

    pub(super) fn string(&mut self) -> Result<&'a [u8], Error> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    pub(super) fn status(&mut self) -> Result<RemoteStatus, Error> {
        let code = StatusCode::from(self.u32()?);
        let message = self.string()?;
        self.string()?;
        let message = String::from_utf8_lossy(message)
            .chars()
            .take(256)
            .map(|character| match character {
                '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}' => '�',
                character if character.is_control() => '�',
                character => character,
            })
            .collect();
        Ok(RemoteStatus { code, message })
    }

    pub(super) fn attributes(&mut self) -> Result<Attributes, Error> {
        let flags = self.u32()?;
        if flags & !ATTR_MASK != 0 {
            return Err(Error::Protocol("unknown SFTP v3 attribute flags"));
        }
        let mut attrs = Attributes::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UID_GID != 0 {
            attrs.uid = Some(self.u32()?);
            attrs.gid = Some(self.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_TIMES != 0 {
            attrs.accessed = Some(self.u32()?);
            attrs.modified = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let count = self.u32()? as usize;
            if count > self.length() / 8 {
                return Err(Error::Protocol("invalid SFTP extended attribute count"));
            }
            for _ in 0..count {
                self.string()?;
                self.string()?;
            }
        }
        Ok(attrs)
    }
}
