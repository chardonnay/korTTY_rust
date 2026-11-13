//! Bounded review and publication of an existing server file.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::manifest::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES};
use crate::protocol::{read_frame, read_message, write_frame};

pub const ARGUMENT: &str = "--replace-draft-v1";
pub const COMMAND: &str = "exec /usr/local/bin/kortty-server-archive --replace-draft-v1";
pub const MAX_DRAFT_BYTES: usize = 96 * 1024;
pub const MAX_ORIGINAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FRAME: usize = 32 * 1024;
const TOKEN_LENGTH: usize = 96;
const NONCE_LENGTH: usize = 32;
const STAGE_PREFIX: &str = ".kortty-draft-";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Original {
    pub bytes: u64,
    pub digest: [u8; 32],
    pub uid: u32,
    pub gid: u32,
    /// The nine access permission bits.
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub destination: String,
    pub original: Original,
    pub draft_bytes: usize,
    pub draft_digest: [u8; 32],
    pub backup: bool,
}

impl Spec {
    pub fn validate(&self) -> io::Result<()> {
        if self.original.bytes > MAX_ORIGINAL_BYTES
            || self.original.uid == u32::MAX
            || self.original.gid == u32::MAX
            || self.original.mode & !0o777 != 0
            || self.original.mode & 0o022 != 0
            || !(1..=MAX_DRAFT_BYTES).contains(&self.draft_bytes)
        {
            return Err(invalid("Invalid reviewed draft or original file"));
        }
        let (parent, _) = self.parts()?;
        self.backup_path()?;
        if parent.trim_end_matches('/').len()
            + STAGE_PREFIX.len()
            + NONCE_LENGTH
            + "/original".len()
            + 1
            > MAX_PATH_BYTES
        {
            return Err(invalid("The server recovery path exceeds its length limit"));
        }
        Ok(())
    }

    pub fn parts(&self) -> io::Result<(&str, &str)> {
        let (parent, name) = path_parts(&self.destination)?;
        if self.destination.split('/').any(|part| {
            part.strip_prefix(STAGE_PREFIX)
                .is_some_and(valid_stage_nonce)
        }) {
            return Err(invalid(
                "A private recovery path cannot be a draft destination",
            ));
        }
        Ok((parent, name))
    }

    pub fn backup_path(&self) -> io::Result<Option<String>> {
        if !self.backup {
            return Ok(None);
        }
        let backup = format!("{}.bak", self.destination);
        path_parts(&backup)?;
        Ok(Some(backup))
    }

    pub fn stage_name(&self, token: &str) -> io::Result<String> {
        if !valid_token(token) {
            return Err(invalid("Invalid server file review token"));
        }
        Ok(format!("{STAGE_PREFIX}{}", &token[..NONCE_LENGTH]))
    }

    pub fn recovery_path(&self, token: &str) -> io::Result<String> {
        let (parent, _) = self.parts()?;
        let stage = self.stage_name(token)?;
        Ok(format!("{}/{stage}/original", parent.trim_end_matches('/')))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub spec: Spec,
    pub token: String,
    pub backup: Option<String>,
}

impl Review {
    pub fn validate_for(&self, spec: &Spec) -> io::Result<()> {
        spec.validate()?;
        if &self.spec != spec || !valid_token(&self.token) || self.backup != spec.backup_path()? {
            return Err(invalid("The server file review did not match the request"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Review { spec: Spec },
    Apply { spec: Spec, token: String },
}

impl Request {
    fn payload_size(&self) -> io::Result<usize> {
        match self {
            Self::Review { spec } => {
                spec.validate()?;
                Ok(0)
            }
            Self::Apply { spec, token } => {
                spec.validate()?;
                if !valid_token(token) {
                    return Err(invalid("Invalid server file review token"));
                }
                Ok(spec.draft_bytes)
            }
        }
    }

    fn require_payload(&self, draft: &[u8]) -> io::Result<()> {
        if draft.len() != self.payload_size()? {
            return Err(invalid("The draft size differs from its review"));
        }
        if let Self::Apply { spec, .. } = self
            && (<[u8; 32]>::from(Sha256::digest(draft)) != spec.draft_digest
                || std::str::from_utf8(draft).is_err()
                || draft.contains(&0))
        {
            return Err(invalid(
                "The draft bytes differ from their review or are invalid text",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Reviewed {
        review: Review,
    },
    Applied {
        destination: String,
        draft_digest: [u8; 32],
        backup: Option<String>,
        recovery: Option<String>,
    },
    Failed {
        message: String,
        outcome_unknown: bool,
        recovery: Option<String>,
        backup: Option<String>,
    },
    Unsupported,
}

/// Read a single request and its exact raw payload, followed by EOF.
pub fn read_request(mut input: impl Read) -> io::Result<(Request, Vec<u8>)> {
    let request: Request = read_message(&mut input, MAX_FRAME)?;
    let mut draft = vec![0; request.payload_size()?];
    input.read_exact(&mut draft)?;
    if io::copy(&mut input.take(1), &mut io::sink())? != 0 {
        return Err(invalid("Unexpected bytes after the reviewed server draft"));
    }
    request.require_payload(&draft)?;
    Ok((request, draft))
}

/// Write a request and its raw payload. The caller must close the input channel.
pub fn write_request(mut output: impl Write, request: &Request, draft: &[u8]) -> io::Result<()> {
    request.require_payload(draft)?;
    write_frame(&mut output, request, MAX_FRAME)?;
    output.write_all(draft)?;
    output.flush()
}

pub fn read_reply(input: impl Read) -> io::Result<Reply> {
    read_frame(input, MAX_FRAME)
}

pub fn write_reply(output: impl Write, reply: &Reply) -> io::Result<()> {
    write_frame(output, reply, MAX_FRAME)
}

fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_LENGTH && lowercase_hex(token)
}

fn valid_stage_nonce(nonce: &str) -> bool {
    nonce.len() == NONCE_LENGTH && lowercase_hex(nonce)
}

fn lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn path_parts(path: &str) -> io::Result<(&str, &str)> {
    if path.len() <= MAX_PATH_BYTES
        && let Some(components) = path.strip_prefix('/')
        && components.split('/').all(|part| {
            !part.is_empty()
                && part.len() <= MAX_COMPONENT_BYTES
                && !matches!(part, "." | "..")
                && !part.contains('\\')
                && !part.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '\u{061c}' | '\u{200b}'..='\u{200f}'
                            | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
                })
        })
        && let Some((parent, name)) = path.rsplit_once('/')
    {
        return Ok((if parent.is_empty() { "/" } else { parent }, name));
    }
    Err(invalid("Choose a canonical absolute server file path"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
