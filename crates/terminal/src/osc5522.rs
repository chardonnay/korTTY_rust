//! Bounded OSC 5522 protocol state. Clipboard access belongs to the host.

use std::collections::VecDeque;
use std::fmt::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const MAX_PACKET_BYTES: usize = crate::MAX_OSC_BYTES - b"5522;".len();
const MAX_METADATA_BYTES: usize = 8192;
const MAX_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
const MAX_MIME_TYPES: usize = 32;
const MAX_PASTE_PASSWORDS: usize = 16;
const MAX_ID_LENGTH: usize = 256;
const DATA_CHUNK_BYTES: usize = 4096;
const TARGETS: &str = ".";
const PASTE_EVENT_NAME: &str = "Paste event";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardLocation {
    Clipboard,
    Primary,
}

/// Local identifier for a pending request. The remote protocol id is separate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClipboardRequestId(u64);

#[derive(Default)]
pub struct ClipboardCredentials {
    pub name: Option<String>,
    pub password: Option<String>,
}

impl fmt::Debug for ClipboardCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardCredentials")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct ClipboardRequest {
    pub id: ClipboardRequestId,
    pub kind: ClipboardRequestKind,
}

pub enum ClipboardRequestKind {
    List {
        location: ClipboardLocation,
        user_initiated: bool,
    },
    Read {
        location: ClipboardLocation,
        mime_types: Vec<String>,
        credentials: ClipboardCredentials,
        granted_by_paste: bool,
    },
    Write {
        location: ClipboardLocation,
        data: Vec<(String, Vec<u8>)>,
        aliases: Vec<(String, Vec<String>)>,
        credentials: ClipboardCredentials,
    },
}

impl fmt::Debug for ClipboardRequestKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List {
                location,
                user_initiated,
            } => formatter
                .debug_struct("List")
                .field("location", location)
                .field("user_initiated", user_initiated)
                .finish(),
            Self::Read {
                location,
                mime_types,
                granted_by_paste,
                ..
            } => formatter
                .debug_struct("Read")
                .field("location", location)
                .field("mime_types", mime_types)
                .field("granted_by_paste", granted_by_paste)
                .finish_non_exhaustive(),
            Self::Write { location, data, .. } => formatter
                .debug_struct("Write")
                .field("location", location)
                .field("mime_count", &data.len())
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    PermissionDenied,
    Busy,
    Unsupported,
    Io,
    Invalid,
    TooLarge,
}

impl ClipboardError {
    const fn status(self) -> &'static str {
        match self {
            Self::PermissionDenied => "EPERM",
            Self::Busy => "EBUSY",
            Self::Unsupported => "ENOSYS",
            Self::Io => "EIO",
            Self::Invalid => "EINVAL",
            Self::TooLarge => "EFBIG",
        }
    }

    const fn read_status(self) -> &'static str {
        match self {
            Self::Invalid | Self::TooLarge => Self::Io.status(),
            _ => self.status(),
        }
    }
}

pub enum ClipboardResult {
    Types(Vec<String>),
    Data(Vec<(String, Vec<u8>)>),
    Written,
    Error(ClipboardError),
}

#[derive(Default)]
pub(crate) struct ClipboardOutput {
    pub(crate) responses: Vec<u8>,
    pub(crate) request: Option<ClipboardRequest>,
}

#[derive(Default)]
pub(crate) struct ClipboardProtocol {
    enabled: bool,
    generation: u64,
    write_limit: Option<usize>,
    paste_events: bool,
    next_id: u64,
    pending_read: Option<PendingRead>,
    pending_write: Option<PendingWrite>,
    write_transfer: Option<WriteTransfer>,
    paste_passwords: VecDeque<PasteGrant>,
}

impl ClipboardProtocol {
    pub(crate) fn set_write_limit(&mut self, bytes: usize) {
        self.write_limit = Some(bytes.min(MAX_TRANSFER_BYTES));
        self.write_transfer = None;
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.reset();
        }
    }

    /// Revokes pending requests and all paste grants. Keeps the host's policy.
    pub(crate) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.paste_events = false;
        self.pending_read = None;
        self.pending_write = None;
        self.write_transfer = None;
        self.paste_passwords.clear();
    }

    pub(crate) fn framing_generation(&self) -> Option<u64> {
        self.enabled.then_some(self.generation)
    }

    pub(crate) fn mode(&self) -> u8 {
        if !self.enabled {
            0
        } else if self.paste_events {
            1
        } else {
            2
        }
    }

    pub(crate) fn set_paste_events(&mut self, enabled: bool) {
        self.paste_events = self.enabled && enabled;
        if !self.paste_events {
            self.revoke_paste_grants();
        }
    }

    pub(crate) fn revoke_paste_grants(&mut self) {
        self.paste_passwords.clear();
        if self
            .pending_read
            .as_ref()
            .is_some_and(|read| read.paste_password.is_some())
        {
            self.pending_read = None;
        }
    }

    /// Call only for a real user paste, using a fresh cryptographically random nonce.
    pub(crate) fn begin_paste(
        &mut self,
        location: ClipboardLocation,
        nonce: [u8; 16],
    ) -> Option<ClipboardRequest> {
        if !self.paste_events || self.pending_read.is_some() {
            return None;
        }
        let mut password = String::with_capacity(nonce.len() * 2);
        for byte in nonce {
            let _ = write!(password, "{byte:02x}");
        }
        if self
            .paste_passwords
            .iter()
            .any(|grant| grant.password == password)
        {
            return None;
        }
        let id = self.next_request_id();
        self.pending_read = Some(PendingRead {
            id,
            wire_id: String::new(),
            location,
            mime_types: Vec::new(),
            paste_password: Some(password),
        });
        Some(ClipboardRequest {
            id,
            kind: ClipboardRequestKind::List {
                location,
                user_initiated: true,
            },
        })
    }

    /// Handles the body after `5522;` in a complete OSC frame.
    pub(crate) fn handle(&mut self, body: &[u8]) -> ClipboardOutput {
        if !self.enabled {
            return ClipboardOutput::default();
        }
        let separator = body.iter().position(|&byte| byte == b';');
        let (metadata, payload) = separator.map_or((body, &[][..]), |index| {
            (&body[..index], &body[index + 1..])
        });
        let meta = Metadata::parse(metadata);
        if body.len() > MAX_PACKET_BYTES || metadata.len() > MAX_METADATA_BYTES {
            return ClipboardOutput::respond(self.reject(meta.kind));
        }
        match meta.kind {
            Some(b"read") => self.read(meta, payload),
            Some(b"write") => self.begin_write(meta, payload),
            Some(b"wdata") => self.write_data(meta, payload),
            Some(b"walias") => self.write_alias(meta, payload),
            _ => ClipboardOutput::default(),
        }
    }

    /// A lost write frame invalidates the transaction. Reads have no syntax-error reply.
    pub(crate) fn reject_packet(&mut self, body: &[u8]) -> Vec<u8> {
        let metadata = body.split(|&byte| byte == b';').next().unwrap_or_default();
        self.reject(Metadata::parse(metadata).kind)
    }

    fn reject(&mut self, kind: Option<&[u8]>) -> Vec<u8> {
        // The frame may have ended mid-field, so an ambiguous write prefix fails closed.
        if let Some(kind) = kind
            && ![b"write".as_slice(), b"wdata", b"walias"]
                .iter()
                .any(|name| name.starts_with(kind))
        {
            return Vec::new();
        }
        self.write_transfer.take().map_or_else(Vec::new, |write| {
            write_packet(&write.wire_id, ClipboardError::Invalid.status())
        })
    }

    pub(crate) fn is_pending(&self, id: ClipboardRequestId) -> bool {
        self.pending_read
            .as_ref()
            .is_some_and(|request| request.id == id)
            || self
                .pending_write
                .as_ref()
                .is_some_and(|request| request.id == id)
    }

    /// Completes a host request once. Late completions after reset are discarded.
    pub(crate) fn complete(&mut self, id: ClipboardRequestId, result: ClipboardResult) -> Vec<u8> {
        if self
            .pending_read
            .as_ref()
            .is_some_and(|pending| pending.id == id)
            && let Some(pending) = self.pending_read.take()
        {
            return self.complete_read(pending, result);
        }
        if self
            .pending_write
            .as_ref()
            .is_some_and(|pending| pending.id == id)
            && let Some(pending) = self.pending_write.take()
        {
            let status = match result {
                ClipboardResult::Written => "DONE",
                ClipboardResult::Error(error) => error.status(),
                _ => ClipboardError::Io.status(),
            };
            return write_packet(&pending.wire_id, status);
        }
        Vec::new()
    }

    fn next_request_id(&mut self) -> ClipboardRequestId {
        self.next_id = self.next_id.wrapping_add(1);
        ClipboardRequestId(self.next_id)
    }

    fn read(&mut self, metadata: Metadata<'_>, payload: &[u8]) -> ClipboardOutput {
        let Some(list) = decode_text(Some(payload)) else {
            return ClipboardOutput::default();
        };
        let Some(mime_types) = requested_types(&list) else {
            return ClipboardOutput::default();
        };
        let wire_id = metadata.wire_id();
        if self.pending_read.is_some() {
            return ClipboardOutput::respond(read_packet(&wire_id, ClipboardError::Busy.status()));
        }
        let location = metadata.location();
        let id = self.next_request_id();
        let is_listing = mime_types == [TARGETS];
        let kind = if is_listing {
            ClipboardRequestKind::List {
                location,
                user_initiated: false,
            }
        } else {
            let granted_by_paste = self.consume_grant(metadata.password, location, &mime_types);
            ClipboardRequestKind::Read {
                location,
                mime_types: mime_types.clone(),
                credentials: metadata.credentials(),
                granted_by_paste,
            }
        };
        self.pending_read = Some(PendingRead {
            id,
            wire_id,
            location,
            mime_types: if is_listing { Vec::new() } else { mime_types },
            paste_password: None,
        });
        ClipboardOutput::request(ClipboardRequest { id, kind })
    }

    fn consume_grant(
        &mut self,
        encoded_password: Option<&[u8]>,
        location: ClipboardLocation,
        mime_types: &[String],
    ) -> bool {
        let Some(password) = decode_text(encoded_password) else {
            return false;
        };
        let Some(index) = self
            .paste_passwords
            .iter()
            .position(|grant| grant.password == password)
        else {
            return false;
        };
        self.paste_passwords.remove(index).is_some_and(|grant| {
            grant.location == location
                && mime_types
                    .iter()
                    .all(|mime| grant.mime_types.contains(mime))
        })
    }

    fn complete_read(&mut self, pending: PendingRead, result: ClipboardResult) -> Vec<u8> {
        match result {
            ClipboardResult::Types(types) if pending.mime_types.is_empty() => {
                let types = advertised_types(types);
                let mut responses = Vec::new();
                pending.open(&mut responses);
                if !types.is_empty() {
                    let list = types.join(" ") + "\n";
                    pending.data(&mut responses, TARGETS, list.as_bytes());
                    if let Some(password) = &pending.paste_password {
                        if self.paste_passwords.len() == MAX_PASTE_PASSWORDS {
                            self.paste_passwords.pop_front();
                        }
                        self.paste_passwords.push_back(PasteGrant {
                            password: password.clone(),
                            location: pending.location,
                            mime_types: types,
                        });
                    }
                }
                pending.done(&mut responses);
                responses
            }
            ClipboardResult::Data(data) if !pending.mime_types.is_empty() => {
                pending.send_requested_data(&data)
            }
            ClipboardResult::Error(error) => pending.error(error),
            _ => pending.error(ClipboardError::Io),
        }
    }

    fn begin_write(&mut self, metadata: Metadata<'_>, payload: &[u8]) -> ClipboardOutput {
        self.write_transfer = None;
        let wire_id = metadata.wire_id();
        let error = if self.pending_write.is_some() {
            Some(ClipboardError::Busy)
        } else if !payload.is_empty() {
            Some(ClipboardError::Invalid)
        } else {
            None
        };
        if let Some(error) = error {
            return ClipboardOutput::respond(write_packet(&wire_id, error.status()));
        }
        self.write_transfer = Some(WriteTransfer {
            location: metadata.location(),
            wire_id,
            credentials: metadata.credentials(),
            data: Vec::new(),
            aliases: Vec::new(),
            size: 0,
            limit: self.write_limit.unwrap_or(MAX_TRANSFER_BYTES),
        });
        ClipboardOutput::default()
    }

    fn write_data(&mut self, metadata: Metadata<'_>, payload: &[u8]) -> ClipboardOutput {
        let Some(mut transfer) = self.write_transfer.take() else {
            return ClipboardOutput::default();
        };
        if metadata.mime.is_none() {
            return self.finish_write(transfer, payload);
        }
        match transfer.append_data(metadata.mime, payload) {
            Ok(()) => {
                self.write_transfer = Some(transfer);
                ClipboardOutput::default()
            }
            Err(error) => ClipboardOutput::respond(write_packet(&transfer.wire_id, error.status())),
        }
    }

    fn write_alias(&mut self, metadata: Metadata<'_>, payload: &[u8]) -> ClipboardOutput {
        let Some(mut transfer) = self.write_transfer.take() else {
            return ClipboardOutput::default();
        };
        match transfer.append_aliases(metadata.mime, payload) {
            Ok(()) => {
                self.write_transfer = Some(transfer);
                ClipboardOutput::default()
            }
            Err(error) => ClipboardOutput::respond(write_packet(&transfer.wire_id, error.status())),
        }
    }

    fn finish_write(&mut self, mut transfer: WriteTransfer, payload: &[u8]) -> ClipboardOutput {
        if !payload.is_empty() || transfer.data.iter().any(|(_, data)| data.tail_len != 0) {
            return ClipboardOutput::respond(write_packet(
                &transfer.wire_id,
                ClipboardError::Invalid.status(),
            ));
        }
        transfer
            .aliases
            .retain(|(target, _)| transfer.data.iter().any(|(mime, _)| mime == target));
        let id = self.next_request_id();
        self.pending_write = Some(PendingWrite {
            id,
            wire_id: transfer.wire_id,
        });
        ClipboardOutput::request(ClipboardRequest {
            id,
            kind: ClipboardRequestKind::Write {
                location: transfer.location,
                data: transfer
                    .data
                    .into_iter()
                    .map(|(mime, data)| (mime, data.bytes))
                    .collect(),
                aliases: transfer.aliases,
                credentials: transfer.credentials,
            },
        })
    }
}

impl ClipboardOutput {
    fn request(request: ClipboardRequest) -> Self {
        Self {
            request: Some(request),
            ..Self::default()
        }
    }

    fn respond(responses: Vec<u8>) -> Self {
        Self {
            responses,
            ..Self::default()
        }
    }
}

struct PendingRead {
    id: ClipboardRequestId,
    wire_id: String,
    location: ClipboardLocation,
    mime_types: Vec<String>,
    paste_password: Option<String>,
}

impl PendingRead {
    fn error(&self, error: ClipboardError) -> Vec<u8> {
        let mut packet = Packet::new(
            "read",
            error.read_status(),
            &self.wire_id,
            ClipboardLocation::Clipboard,
        );
        if let Some(password) = &self.paste_password {
            packet.encoded("pw", password);
        }
        let mut response = Vec::new();
        packet.emit(&mut response);
        response
    }

    fn open(&self, output: &mut Vec<u8>) {
        let mut packet = Packet::new("read", "OK", &self.wire_id, self.location);
        if let Some(password) = &self.paste_password {
            packet.encoded("pw", password);
            packet.encoded("name", PASTE_EVENT_NAME);
        }
        packet.emit(output);
    }

    fn data(&self, output: &mut Vec<u8>, mime: &str, data: &[u8]) {
        for chunk in data.chunks(DATA_CHUNK_BYTES) {
            let mut packet =
                Packet::new("read", "DATA", &self.wire_id, ClipboardLocation::Clipboard);
            packet.encoded("mime", mime);
            if let Some(password) = &self.paste_password {
                packet.encoded("pw", password);
            }
            packet.0.push(';');
            STANDARD.encode_string(chunk, &mut packet.0);
            packet.emit(output);
        }
    }

    fn done(&self, output: &mut Vec<u8>) {
        let mut packet = Packet::new("read", "DONE", &self.wire_id, ClipboardLocation::Clipboard);
        if let Some(password) = &self.paste_password {
            packet.encoded("pw", password);
        }
        packet.emit(output);
    }

    fn send_requested_data(&self, data: &[(String, Vec<u8>)]) -> Vec<u8> {
        let requested: Vec<_> = self
            .mime_types
            .iter()
            .filter_map(|requested| data.iter().find(|(mime, _)| mime == requested))
            .collect();
        let total = requested
            .iter()
            .try_fold(0usize, |size, (_, bytes)| size.checked_add(bytes.len()));
        if total.is_none_or(|size| size > MAX_TRANSFER_BYTES) {
            return read_packet(&self.wire_id, ClipboardError::Io.status());
        }
        let mut responses = Vec::new();
        self.open(&mut responses);
        for (mime, bytes) in requested {
            self.data(&mut responses, mime, bytes);
        }
        self.done(&mut responses);
        responses
    }
}

struct PendingWrite {
    id: ClipboardRequestId,
    wire_id: String,
}

struct PasteGrant {
    password: String,
    location: ClipboardLocation,
    mime_types: Vec<String>,
}

#[derive(Default)]
struct Metadata<'a> {
    kind: Option<&'a [u8]>,
    location: Option<&'a [u8]>,
    id: Option<&'a [u8]>,
    mime: Option<&'a [u8]>,
    name: Option<&'a [u8]>,
    password: Option<&'a [u8]>,
}

impl<'a> Metadata<'a> {
    fn parse(input: &'a [u8]) -> Self {
        let mut meta = Self::default();
        for field in input.split(|&byte| byte == b':') {
            let Some(separator) = field.iter().position(|&byte| byte == b'=') else {
                continue;
            };
            let target = match &field[..separator] {
                b"type" => &mut meta.kind,
                b"loc" => &mut meta.location,
                b"id" => &mut meta.id,
                b"mime" => &mut meta.mime,
                b"name" => &mut meta.name,
                b"pw" => &mut meta.password,
                _ => continue,
            };
            target.get_or_insert(&field[separator + 1..]);
        }
        meta
    }

    fn location(&self) -> ClipboardLocation {
        if self.location == Some(b"primary") {
            ClipboardLocation::Primary
        } else {
            ClipboardLocation::Clipboard
        }
    }

    fn wire_id(&self) -> String {
        self.id
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|byte| byte.is_ascii_alphanumeric() || b"-_+.".contains(byte))
            .take(MAX_ID_LENGTH)
            .map(char::from)
            .collect()
    }

    fn credentials(&self) -> ClipboardCredentials {
        let name = decode_text(self.name);
        let password = if name.as_ref().is_some_and(|name| !name.is_empty()) {
            decode_text(self.password).filter(|password| !password.is_empty())
        } else {
            None
        };
        ClipboardCredentials { name, password }
    }
}

fn decode_text(input: Option<&[u8]>) -> Option<String> {
    String::from_utf8(STANDARD.decode(input?).ok()?).ok()
}

fn requested_types(list: &str) -> Option<Vec<String>> {
    let mut types = Vec::new();
    for mime in list.split_ascii_whitespace() {
        if !types.iter().any(|known| known == mime) {
            if types.len() == MAX_MIME_TYPES {
                return None;
            }
            types.push(mime.to_owned());
        }
    }
    (!types.is_empty() && (types == [TARGETS] || types.iter().all(|mime| valid_mime(mime))))
        .then_some(types)
}

fn advertised_types(types: Vec<String>) -> Vec<String> {
    let mut valid = Vec::new();
    for mime in types {
        if valid.len() == MAX_MIME_TYPES {
            break;
        }
        if valid_mime(&mime)
            && !mime.bytes().any(|byte| byte.is_ascii_whitespace())
            && !valid.contains(&mime)
        {
            valid.push(mime);
        }
    }
    valid
}

fn valid_mime(value: &str) -> bool {
    let name = match value.split_once(';') {
        Some((name, parameters))
            if parameters.len() <= 255
                && parameters.bytes().all(|byte| (b' '..=b'~').contains(&byte)) =>
        {
            name
        }
        Some(_) => return false,
        None => value,
    };
    let Some((kind, subtype)) = name.split_once('/') else {
        return false;
    };
    let valid_component = |part: &str| {
        (1..=127).contains(&part.len())
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
    };
    kind.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && valid_component(kind)
        && valid_component(subtype)
}

struct WriteTransfer {
    location: ClipboardLocation,
    wire_id: String,
    credentials: ClipboardCredentials,
    data: Vec<(String, MimeData)>,
    aliases: Vec<(String, Vec<String>)>,
    size: usize,
    limit: usize,
}

impl WriteTransfer {
    fn mime_entries(&self) -> usize {
        self.data.len()
            + self
                .aliases
                .iter()
                .map(|(_, names)| names.len())
                .sum::<usize>()
    }

    fn append_data(
        &mut self,
        encoded_mime: Option<&[u8]>,
        payload: &[u8],
    ) -> Result<(), ClipboardError> {
        let mime = decode_text(encoded_mime)
            .filter(|mime| valid_mime(mime))
            .ok_or(ClipboardError::Invalid)?;
        let index = match self.data.iter().position(|(name, _)| name == &mime) {
            Some(index) => index,
            None if self.mime_entries() == MAX_MIME_TYPES => return Err(ClipboardError::Invalid),
            None => {
                self.data.push((mime, MimeData::default()));
                self.data.len() - 1
            }
        };
        self.data[index]
            .1
            .append(payload, &mut self.size, self.limit)
    }

    fn append_aliases(
        &mut self,
        encoded_target: Option<&[u8]>,
        payload: &[u8],
    ) -> Result<(), ClipboardError> {
        let target = decode_text(encoded_target)
            .filter(|mime| valid_mime(mime))
            .ok_or(ClipboardError::Invalid)?;
        let list = decode_text(Some(payload)).ok_or(ClipboardError::Invalid)?;
        let mut aliases = list
            .split_ascii_whitespace()
            .filter(|mime| valid_mime(mime))
            .peekable();
        if aliases.peek().is_none() {
            return Ok(());
        }
        let index = self
            .aliases
            .iter()
            .position(|(mime, _)| mime == &target)
            .unwrap_or_else(|| {
                self.aliases.push((target, Vec::new()));
                self.aliases.len() - 1
            });
        for alias in aliases {
            if self.aliases[index].1.iter().any(|mime| mime == alias) {
                continue;
            }
            if self.mime_entries() == MAX_MIME_TYPES {
                return Err(ClipboardError::Invalid);
            }
            self.aliases[index].1.push(alias.to_owned());
        }
        Ok(())
    }
}

#[derive(Default)]
struct MimeData {
    bytes: Vec<u8>,
    tail: [u8; 4],
    tail_len: usize,
    ended: bool,
}

impl MimeData {
    fn append(
        &mut self,
        mut payload: &[u8],
        total: &mut usize,
        limit: usize,
    ) -> Result<(), ClipboardError> {
        if self.ended && !payload.is_empty() {
            return Err(ClipboardError::Invalid);
        }
        if self.tail_len != 0 {
            let take = (4 - self.tail_len).min(payload.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&payload[..take]);
            self.tail_len += take;
            payload = &payload[take..];
            if self.tail_len < 4 {
                return Ok(());
            }
            let quartet = self.tail;
            self.decode(&quartet, total, limit)?;
        }
        let (groups, tail) = payload.split_at(payload.len() / 4 * 4);
        for group in groups.chunks(4096) {
            self.decode(group, total, limit)?;
        }
        if self.ended && !tail.is_empty() {
            return Err(ClipboardError::Invalid);
        }
        self.tail[..tail.len()].copy_from_slice(tail);
        self.tail_len = tail.len();
        Ok(())
    }

    fn decode(
        &mut self,
        input: &[u8],
        total: &mut usize,
        limit: usize,
    ) -> Result<(), ClipboardError> {
        if self.ended {
            return Err(ClipboardError::Invalid);
        }
        let mut decoded = [0; 3072];
        let length = STANDARD
            .decode_slice(input, &mut decoded)
            .map_err(|_| ClipboardError::Invalid)?;
        if *total + length > limit {
            return Err(ClipboardError::TooLarge);
        }
        self.bytes
            .try_reserve(length)
            .map_err(|_| ClipboardError::Io)?;
        self.bytes.extend_from_slice(&decoded[..length]);
        *total += length;
        self.ended = input.last() == Some(&b'=');
        Ok(())
    }
}

struct Packet(String);

impl Packet {
    fn new(kind: &str, status: &str, wire_id: &str, location: ClipboardLocation) -> Self {
        let mut packet = format!("\x1b]5522;type={kind}:status={status}");
        if location == ClipboardLocation::Primary {
            packet.push_str(":loc=primary");
        }
        if !wire_id.is_empty() {
            packet.push_str(":id=");
            packet.push_str(wire_id);
        }
        Self(packet)
    }

    fn encoded(&mut self, key: &str, value: &str) {
        self.0.push(':');
        self.0.push_str(key);
        self.0.push('=');
        STANDARD.encode_string(value, &mut self.0);
    }

    fn emit(self, output: &mut Vec<u8>) {
        output.extend_from_slice(self.0.as_bytes());
        output.extend_from_slice(b"\x1b\\");
    }
}

fn read_packet(wire_id: &str, status: &str) -> Vec<u8> {
    status_packet("read", wire_id, status)
}

fn write_packet(wire_id: &str, status: &str) -> Vec<u8> {
    status_packet("write", wire_id, status)
}

fn status_packet(kind: &str, wire_id: &str, status: &str) -> Vec<u8> {
    let mut output = Vec::new();
    Packet::new(kind, status, wire_id, ClipboardLocation::Clipboard).emit(&mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ClipboardProtocol {
        let mut protocol = ClipboardProtocol::default();
        protocol.set_enabled(true);
        protocol
    }

    fn command(protocol: &mut ClipboardProtocol, meta: &str, payload: &str) -> ClipboardOutput {
        protocol.handle(format!("{meta};{payload}").as_bytes())
    }

    fn read(protocol: &mut ClipboardProtocol, meta: &str, mimes: &str) -> ClipboardOutput {
        command(protocol, meta, &STANDARD.encode(mimes))
    }

    fn write_data(protocol: &mut ClipboardProtocol, mime: &str, data: &str) -> ClipboardOutput {
        command(
            protocol,
            &format!("type=wdata:mime={}", STANDARD.encode(mime)),
            data,
        )
    }

    fn packets(bytes: &[u8]) -> Vec<&str> {
        if bytes.is_empty() {
            return Vec::new();
        }
        std::str::from_utf8(bytes)
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap()
            .split("\x1b\\")
            .map(|packet| packet.strip_prefix("\x1b]5522;").unwrap())
            .collect()
    }

    #[test]
    fn disabled_protocol_cannot_create_requests_or_paste_grants() {
        let mut protocol = ClipboardProtocol::default();
        protocol.set_paste_events(true);
        assert_eq!(protocol.mode(), 0);
        assert!(
            protocol
                .begin_paste(ClipboardLocation::Clipboard, [1; 16])
                .is_none()
        );
        let output = read(&mut protocol, "type=read:id=r1", "text/plain");
        assert!(output.request.is_none() && output.responses.is_empty());
        protocol.handle(b"type=write");
        assert!(protocol.write_transfer.is_none());

        protocol.set_enabled(true);
        assert_eq!(protocol.mode(), 2);
        protocol.set_paste_events(true);
        assert_eq!(protocol.mode(), 1);
        protocol.reset();
        assert_eq!(protocol.mode(), 2);
    }

    #[test]
    fn split_write_and_aliases_are_completed_only_by_the_host() {
        let mut protocol = enabled();
        let begin = format!(
            "type=write:loc=primary:id=abc:name={}:pw={}",
            STANDARD.encode("Application"),
            STANDARD.encode("secret")
        );
        assert!(protocol.handle(begin.as_bytes()).request.is_none());
        let encoded = STANDARD.encode("Grüße");
        for group in encoded.as_bytes().chunks(3) {
            let output = write_data(
                &mut protocol,
                "text/plain",
                std::str::from_utf8(group).unwrap(),
            );
            assert!(output.request.is_none() && output.responses.is_empty());
        }
        let output = command(
            &mut protocol,
            &format!("type=walias:mime={}", STANDARD.encode("text/plain")),
            &STANDARD.encode("UTF8_STRING text/plain;charset=utf-8 text/plain;charset=utf-8"),
        );
        assert!(output.request.is_none() && output.responses.is_empty());

        let output = protocol.handle(b"type=wdata");
        assert!(output.responses.is_empty());
        let request = output.request.unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("secret") && !debug.contains("Grüße"));
        let ClipboardRequestKind::Write {
            location,
            data,
            aliases,
            credentials,
        } = request.kind
        else {
            panic!("expected a clipboard write");
        };
        assert_eq!(location, ClipboardLocation::Primary);
        assert_eq!(data, [("text/plain".into(), "Grüße".as_bytes().to_vec())]);
        assert_eq!(
            aliases,
            [("text/plain".into(), vec!["text/plain;charset=utf-8".into()])]
        );
        assert_eq!(credentials.name.as_deref(), Some("Application"));
        assert_eq!(credentials.password.as_deref(), Some("secret"));
        assert_eq!(
            packets(&protocol.complete(request.id, ClipboardResult::Written)),
            ["type=write:status=DONE:id=abc"]
        );
        assert!(
            protocol
                .complete(request.id, ClipboardResult::Written)
                .is_empty()
        );
    }

    #[test]
    fn invalid_or_missing_write_packets_cannot_commit_a_prefix() {
        let mut protocol = enabled();
        for bad_chunk in ["!!!!", "Q Q==", "AQ==AAAA", "AQ==A", "AA"] {
            protocol.handle(b"type=write:id=abc");
            let output = write_data(&mut protocol, "text/plain", bad_chunk);
            let responses = if output.responses.is_empty() {
                protocol.handle(b"type=wdata").responses
            } else {
                output.responses
            };
            assert_eq!(packets(&responses), ["type=write:status=EINVAL:id=abc"]);
            assert!(protocol.write_transfer.is_none());
            assert!(protocol.handle(b"type=wdata").request.is_none());
        }

        protocol.handle(b"type=write:id=missing");
        let output = protocol.handle(b"type=wdata;AAAA");
        assert_eq!(
            packets(&output.responses),
            ["type=write:status=EINVAL:id=missing"]
        );

        protocol.handle(b"type=write:id=aborted");
        write_data(&mut protocol, "text/plain", "YQ==");
        assert_eq!(
            packets(&protocol.reject_packet(b"type=wdata:mime=")),
            ["type=write:status=EINVAL:id=aborted"]
        );
        assert!(protocol.handle(b"type=wdata").request.is_none());

        protocol.handle(b"type=write:id=recovered");
        write_data(&mut protocol, "text/plain", "YQ==");
        let id = protocol.handle(b"type=wdata").request.unwrap().id;
        assert_eq!(
            packets(
                &protocol.complete(id, ClipboardResult::Error(ClipboardError::PermissionDenied))
            ),
            ["type=write:status=EPERM:id=recovered"]
        );
    }

    #[test]
    fn byte_and_mime_limits_apply_to_the_complete_write() {
        let mut protocol = enabled();
        protocol.handle(b"type=write:id=bytes");
        protocol.write_transfer.as_mut().unwrap().size = MAX_TRANSFER_BYTES - 2;
        let output = write_data(&mut protocol, "text/plain", "AAAA");
        assert_eq!(
            packets(&output.responses),
            ["type=write:status=EFBIG:id=bytes"]
        );

        protocol.handle(b"type=write:id=mimes");
        for index in 0..MAX_MIME_TYPES {
            let output = write_data(&mut protocol, &format!("application/x-test-{index}"), "");
            assert!(output.responses.is_empty());
        }
        let output = write_data(&mut protocol, "text/plain", "AAAA");
        assert_eq!(
            packets(&output.responses),
            ["type=write:status=EINVAL:id=mimes"]
        );

        protocol.handle(b"type=write:id=aliases");
        let names: Vec<_> = (0..MAX_MIME_TYPES)
            .map(|index| format!("application/x-alias-{index}"))
            .collect();
        let output = command(
            &mut protocol,
            &format!("type=walias:mime={}", STANDARD.encode("text/plain")),
            &STANDARD.encode(names.join(" ")),
        );
        assert!(output.responses.is_empty());
        let output = write_data(&mut protocol, "text/plain", "AAAA");
        assert_eq!(
            packets(&output.responses),
            ["type=write:status=EINVAL:id=aliases"]
        );
        assert!(protocol.write_transfer.is_none());

        protocol.handle(b"type=write:id=combined");
        let output = command(
            &mut protocol,
            &format!("type=walias:mime={}", STANDARD.encode("text/plain")),
            &STANDARD.encode(names[..MAX_MIME_TYPES - 1].join(" ")),
        );
        assert!(output.responses.is_empty());
        assert!(
            write_data(&mut protocol, "text/plain", "AAAA")
                .responses
                .is_empty()
        );
        let request = protocol.handle(b"type=wdata").request.unwrap();
        let ClipboardRequestKind::Write { data, aliases, .. } = request.kind else {
            panic!("expected clipboard write");
        };
        assert_eq!(data.len(), 1);
        assert_eq!(aliases[0].1, names[..MAX_MIME_TYPES - 1]);
    }

    #[test]
    fn accepts_a_full_64_mib_binary_representation_with_protocol_chunk_sizes() {
        const CHUNK: &[u8] = &[0xa5; 4095];
        let mut protocol = enabled();
        protocol.handle(b"type=write:id=binary");
        let mime = STANDARD.encode("application/octet-stream");
        let header = format!("type=wdata:mime={mime};");
        let packet = format!("{header}{}", STANDARD.encode(CHUNK));
        for _ in 0..MAX_TRANSFER_BYTES / CHUNK.len() {
            let output = protocol.handle(packet.as_bytes());
            assert!(output.responses.is_empty() && output.request.is_none());
        }
        let tail = STANDARD.encode(&CHUNK[..MAX_TRANSFER_BYTES % CHUNK.len()]);
        let output = protocol.handle(format!("{header}{tail}").as_bytes());
        assert!(output.responses.is_empty() && output.request.is_none());

        let request = protocol.handle(b"type=wdata").request.unwrap();
        let ClipboardRequestKind::Write { data, .. } = request.kind else {
            panic!("expected clipboard write");
        };
        assert_eq!(data[0].0, "application/octet-stream");
        assert_eq!(data[0].1.len(), MAX_TRANSFER_BYTES);
        assert!(data[0].1.iter().all(|&byte| byte == 0xa5));
        assert_eq!(
            packets(&protocol.complete(request.id, ClipboardResult::Written)),
            ["type=write:status=DONE:id=binary"]
        );
    }

    #[test]
    fn read_sanitizes_ids_deduplicates_types_and_chunks_only_requested_data() {
        let mut protocol = enabled();
        let output = read(
            &mut protocol,
            &format!("type=read:id=r!1:pw={}", STANDARD.encode("no name")),
            "text/plain text/plain image/png",
        );
        let request = output.request.unwrap();
        let ClipboardRequestKind::Read {
            location,
            mime_types,
            credentials,
            granted_by_paste,
        } = request.kind
        else {
            panic!("expected a clipboard read");
        };
        assert_eq!(location, ClipboardLocation::Clipboard);
        assert_eq!(mime_types, ["text/plain", "image/png"]);
        assert!(credentials.password.is_none() && !granted_by_paste);
        assert_eq!(
            packets(&read(&mut protocol, "type=read:id=r2", "text/plain").responses),
            ["type=read:status=EBUSY:id=r2"]
        );
        let text = vec![b'a'; 2 * DATA_CHUNK_BYTES + 7];
        let responses = protocol.complete(
            request.id,
            ClipboardResult::Data(vec![
                ("application/octet-stream".into(), b"hidden".to_vec()),
                ("text/plain".into(), text.clone()),
            ]),
        );
        let packets = packets(&responses);
        assert_eq!(packets.len(), 5);
        assert_eq!(packets[0], "type=read:status=OK:id=r1");
        assert_eq!(packets[4], "type=read:status=DONE:id=r1");
        let prefix = format!(
            "type=read:status=DATA:id=r1:mime={};",
            STANDARD.encode("text/plain")
        );
        for (packet, expected) in packets[1..4].iter().zip(text.chunks(DATA_CHUNK_BYTES)) {
            assert_eq!(
                STANDARD
                    .decode(packet.strip_prefix(&prefix).unwrap())
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn listing_primary_validates_mimes_and_read_denials_send_no_data() {
        let mut protocol = enabled();
        let list = read(&mut protocol, "type=read:loc=primary", ".")
            .request
            .unwrap();
        assert!(matches!(
            list.kind,
            ClipboardRequestKind::List {
                location: ClipboardLocation::Primary,
                user_initiated: false
            }
        ));
        let responses = protocol.complete(
            list.id,
            ClipboardResult::Types(vec!["text/plain".into(), "image/png".into(), "bad".into()]),
        );
        assert_eq!(
            packets(&responses),
            [
                "type=read:status=OK:loc=primary",
                &format!(
                    "type=read:status=DATA:mime={};{}",
                    STANDARD.encode("."),
                    STANDARD.encode("text/plain image/png\n")
                ),
                "type=read:status=DONE",
            ]
        );
        let read = read(&mut protocol, "type=read:id=deny", "text/plain")
            .request
            .unwrap();
        assert_eq!(
            packets(&protocol.complete(
                read.id,
                ClipboardResult::Error(ClipboardError::PermissionDenied)
            )),
            ["type=read:status=EPERM:id=deny"]
        );
    }

    #[test]
    fn malformed_reads_and_stale_host_completions_are_ignored() {
        let mut protocol = enabled();
        for payload in ["YQ", "YQ==\n", "/w==", "!!!!", ""] {
            let output = command(&mut protocol, "type=read:id=one", payload);
            assert!(output.responses.is_empty() && output.request.is_none());
        }
        for names in ["invalid-mime", ". text/plain", "application//nested"] {
            let output = read(&mut protocol, "type=read:id=one", names);
            assert!(output.responses.is_empty() && output.request.is_none());
        }
        let old = read(&mut protocol, "type=read:id=old", ".")
            .request
            .unwrap()
            .id;
        protocol.reset();
        let new = read(&mut protocol, "type=read:id=new", ".")
            .request
            .unwrap()
            .id;
        assert_ne!(old, new);
        assert!(
            protocol
                .complete(old, ClipboardResult::Types(vec!["text/plain".into()]))
                .is_empty()
        );
        protocol.set_enabled(false);
        assert!(
            protocol
                .complete(new, ClipboardResult::Types(vec!["text/plain".into()]))
                .is_empty()
        );
    }

    fn paste_password(
        protocol: &mut ClipboardProtocol,
        location: ClipboardLocation,
        nonce: [u8; 16],
    ) -> String {
        let request = protocol.begin_paste(location, nonce).unwrap();
        assert!(matches!(
            request.kind,
            ClipboardRequestKind::List { location: requested, user_initiated: true } if requested == location
        ));
        let responses = protocol.complete(
            request.id,
            ClipboardResult::Types(vec!["text/plain".into()]),
        );
        let packets = packets(&responses);
        assert_eq!(packets.len(), 3);
        let (_, fields) = packets[0].split_once(":pw=").unwrap();
        let (password, name) = fields.split_once(":name=").unwrap();
        assert_eq!(STANDARD.decode(name).unwrap(), b"Paste event");
        assert!(packets[1].contains(&format!(":pw={password};")));
        assert!(packets[2].ends_with(&format!(":pw={password}")));
        password.to_owned()
    }

    fn use_password(
        protocol: &mut ClipboardProtocol,
        password: &str,
        location: &str,
        mime: &str,
    ) -> bool {
        let request = read(
            protocol,
            &format!("type=read:loc={location}:pw={password}"),
            mime,
        )
        .request
        .unwrap();
        let ClipboardRequestKind::Read {
            granted_by_paste, ..
        } = request.kind
        else {
            panic!("expected a clipboard read");
        };
        protocol.complete(request.id, ClipboardResult::Data(Vec::new()));
        granted_by_paste
    }

    #[test]
    fn a_failed_paste_listing_identifies_the_event_without_granting_access() {
        let mut protocol = enabled();
        protocol.set_paste_events(true);
        let request = protocol
            .begin_paste(ClipboardLocation::Clipboard, [0xba; 16])
            .unwrap();
        let response = protocol.complete(
            request.id,
            ClipboardResult::Error(ClipboardError::PermissionDenied),
        );
        let password = STANDARD.encode("ba".repeat(16));
        assert_eq!(
            packets(&response),
            [format!("type=read:status=EPERM:pw={password}")]
        );
        assert!(!use_password(
            &mut protocol,
            &password,
            "clipboard",
            "text/plain"
        ));
    }

    #[test]
    fn paste_passwords_are_single_use_scoped_and_revocable() {
        let mut protocol = enabled();
        protocol.set_paste_events(true);

        let password = paste_password(&mut protocol, ClipboardLocation::Primary, [1; 16]);
        assert!(use_password(
            &mut protocol,
            &password,
            "primary",
            "text/plain"
        ));
        assert!(!use_password(
            &mut protocol,
            &password,
            "primary",
            "text/plain"
        ));

        let password = paste_password(&mut protocol, ClipboardLocation::Primary, [2; 16]);
        assert!(!use_password(
            &mut protocol,
            &password,
            "clipboard",
            "text/plain"
        ));
        assert!(!use_password(
            &mut protocol,
            &password,
            "primary",
            "text/plain"
        ));

        let password = paste_password(&mut protocol, ClipboardLocation::Primary, [3; 16]);
        assert!(!use_password(
            &mut protocol,
            &password,
            "primary",
            "image/png"
        ));

        let password = paste_password(&mut protocol, ClipboardLocation::Clipboard, [4; 16]);
        protocol.set_paste_events(false);
        assert!(!use_password(
            &mut protocol,
            &password,
            "clipboard",
            "text/plain"
        ));
    }

    #[test]
    fn busy_write_and_read_lanes_are_bounded() {
        let mut protocol = enabled();
        protocol.handle(b"type=write:id=w1");
        let write = protocol.handle(b"type=wdata").request.unwrap();
        let output = protocol.handle(b"type=write:id=w2");
        assert_eq!(
            packets(&output.responses),
            ["type=write:status=EBUSY:id=w2"]
        );
        assert_eq!(
            packets(&protocol.complete(
                write.id,
                ClipboardResult::Error(ClipboardError::Unsupported)
            )),
            ["type=write:status=ENOSYS:id=w1"]
        );
        protocol.set_paste_events(true);
        let paste = protocol
            .begin_paste(ClipboardLocation::Clipboard, [1; 16])
            .unwrap();
        assert!(
            protocol
                .begin_paste(ClipboardLocation::Primary, [2; 16])
                .is_none()
        );
        assert_eq!(
            packets(&read(&mut protocol, "type=read:id=r1", ".").responses),
            ["type=read:status=EBUSY:id=r1"]
        );
        protocol.set_paste_events(false);
        assert!(
            protocol
                .complete(paste.id, ClipboardResult::Types(vec!["text/plain".into()]))
                .is_empty()
        );
    }
}
