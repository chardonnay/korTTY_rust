//! Framed messages for the ordinary and privileged archive helper modes.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use crate::manifest::Manifest;

pub const COMMAND: &str = "exec /usr/local/bin/kortty-server-archive --stdio-v1";
pub const PRIVILEGED_ARGUMENT: &str = "--privileged-create-v1";
pub const PRIVILEGED_PASSWORD_ARGUMENT: &str = "--privileged-create-v2";
pub const PRIVILEGED_PASSWORD_READY: &[u8] = b"KORTTY-PRIVILEGED-ARCHIVE-V2\n";
pub const PRIVILEGED_PASSWORD_COMMIT: &[u8] = b"KORTTY-PRIVILEGED-ARCHIVE-COMMIT\n";
pub const PRIVILEGED_PATH: &str = "/usr/libexec/kortty/kortty-server-archive";
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const PASSWORD_READY: &[u8] = b"KORTTY-ARCHIVE-PASSWORD-V1\n";
pub const PASSWORD_COMMIT: &[u8] = b"KORTTY-ARCHIVE-COMMIT\n";
const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Review {
        source: String,
    },
    Apply {
        source: String,
        destination: String,
        token: String,
    },
    ReviewCreation {
        spec: CreateSpec,
    },
    ApplyCreation {
        spec: CreateSpec,
        token: String,
    },
}

impl Request {
    pub fn publishes(&self) -> bool {
        matches!(self, Self::Apply { .. } | Self::ApplyCreation { .. })
    }

    pub fn needs_password(&self) -> bool {
        matches!(self, Self::ApplyCreation { spec, .. } if spec.format == CreateFormat::ZipPassword)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateFormat {
    Zip,
    ZipPassword,
    Tar,
    TarBzip2,
    SevenZip,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSpec {
    pub sources: Vec<String>,
    pub destination: String,
    pub format: CreateFormat,
    pub level: u8,
    pub excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputMetadata>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub report_payload: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedPayload {
    pub bytes: u64,
    pub sha256: [u8; 32],
}

impl CreatedPayload {
    pub fn validate(self) -> io::Result<()> {
        if !(1..=crate::MAX_ARCHIVE_BYTES).contains(&self.bytes) {
            return Err(invalid("Invalid published server archive size"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u16>,
}

impl OutputMetadata {
    pub fn validate(self) -> io::Result<()> {
        if self.uid == Some(u32::MAX)
            || self.gid == Some(u32::MAX)
            || self.mode.is_some_and(|mode| mode > 0o777)
            || (self.uid.is_none() && self.gid.is_none() && self.mode.is_none())
        {
            return Err(invalid(
                "Choose valid archive ownership IDs and ordinary permission bits",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateReview {
    pub spec: CreateSpec,
    pub token: String,
    pub manifest: Manifest,
    pub excluded: usize,
}

impl CreateReview {
    pub fn validate_for(&self, spec: &CreateSpec) -> io::Result<()> {
        use crate::manifest::{MAX_MEMBERS, MemberKind};

        if let Some(output) = spec.output {
            output.validate()?;
        }
        if &self.spec != spec
            || self.token.len() != 64
            || !self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.manifest.members.is_empty()
            || self.excluded > MAX_MEMBERS
        {
            return Err(invalid(
                "The server archive review did not match the request",
            ));
        }
        self.manifest.validate()?;
        for member in &self.manifest.members {
            let root = member.path.split('/').next();
            if !matches!(member.kind, MemberKind::File | MemberKind::Directory)
                || !spec
                    .sources
                    .iter()
                    .any(|source| source.rsplit('/').next() == root)
            {
                return Err(invalid(
                    "The server archive review included an unexpected member",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub source: String,
    pub destination: String,
    pub token: String,
    pub manifest: Manifest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Review {
        review: Review,
    },
    CreationReview {
        review: CreateReview,
    },
    Applied {
        destination: String,
    },
    CreationApplied {
        destination: String,
        payload: CreatedPayload,
    },
    Failed {
        message: String,
        outcome_unknown: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivilegedRequest {
    Review { spec: CreateSpec },
    Commit { token: String },
    Abort,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivilegedReply {
    Review {
        review: CreateReview,
    },
    Applied {
        destination: String,
    },
    /// Followed by exactly `payload.bytes` bytes on stdout.
    AppliedCopy {
        destination: String,
        payload: CreatedPayload,
    },
    Aborted,
    Failed {
        message: String,
        outcome_unknown: bool,
    },
}

pub fn read_request(input: impl Read) -> io::Result<Request> {
    read_frame(input, MAX_REQUEST_BYTES)
}

pub fn write_request(output: impl Write, request: &Request) -> io::Result<()> {
    write_frame(output, request, MAX_REQUEST_BYTES)
}

/// Encodes the separately transmitted password for one protected creation.
pub fn creation_password_body(password: &str) -> io::Result<Zeroizing<Vec<u8>>> {
    validate_password(password.as_bytes())?;
    let mut body = Zeroizing::new(Vec::with_capacity(4 + password.len()));
    body.extend_from_slice(&(password.len() as u32).to_be_bytes());
    body.extend_from_slice(password.as_bytes());
    Ok(body)
}

/// Reads a public frame and, for password ZIP, acknowledges the commit protocol
/// before reading the secret body and the explicit commit marker.
pub fn read_stdio_request(
    mut input: impl Read,
    mut output: impl Write,
) -> io::Result<(Request, Option<Zeroizing<Vec<u8>>>)> {
    let request: Request = read_message(&mut input, MAX_REQUEST_BYTES)?;
    let password = if request.needs_password() {
        output.write_all(PASSWORD_READY)?;
        output.flush()?;
        Some(read_committed_password(input)?)
    } else {
        require_eof(input)?;
        None
    };
    Ok((request, password))
}

/// Reads the bounded secret followed by its explicit commit marker and EOF.
pub fn read_committed_password(mut input: impl Read) -> io::Result<Zeroizing<Vec<u8>>> {
    let password = read_creation_password(&mut input)?;
    let mut commit = Zeroizing::new([0; PASSWORD_COMMIT.len()]);
    input.read_exact(commit.as_mut())?;
    if commit.as_slice() != PASSWORD_COMMIT {
        return Err(invalid("Missing password ZIP commit marker"));
    }
    require_eof(input)?;
    Ok(password)
}

/// Requires the V2 archive commitment independently of the SSH channel closing.
pub fn require_privileged_password_commit(mut input: impl Read) -> io::Result<()> {
    let mut commit = [0; PRIVILEGED_PASSWORD_COMMIT.len()];
    input.read_exact(&mut commit)?;
    if commit.as_slice() != PRIVILEGED_PASSWORD_COMMIT {
        return Err(invalid("Missing privileged archive commit marker"));
    }
    require_eof(input)
}

fn read_creation_password(input: &mut impl Read) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_PASSWORD_BYTES).contains(&length) {
        return Err(invalid("Invalid archive password"));
    }
    let mut password = Zeroizing::new(vec![0; length]);
    input.read_exact(&mut password)?;
    validate_password(&password)?;
    Ok(password)
}

fn validate_password(password: &[u8]) -> io::Result<()> {
    if password.len() > MAX_PASSWORD_BYTES
        || !std::str::from_utf8(password).is_ok_and(|value| !value.trim().is_empty())
    {
        return Err(invalid("Invalid archive password"));
    }
    Ok(())
}

pub fn read_reply(input: impl Read) -> io::Result<Reply> {
    read_frame(input, MAX_RESPONSE_BYTES)
}

pub fn write_reply(output: impl Write, reply: &Reply) -> io::Result<()> {
    write_frame(output, reply, MAX_RESPONSE_BYTES)
}

pub fn read_privileged_request(input: impl Read) -> io::Result<PrivilegedRequest> {
    read_message(input, MAX_REQUEST_BYTES)
}

pub fn write_privileged_request(output: impl Write, request: &PrivilegedRequest) -> io::Result<()> {
    write_frame(output, request, MAX_REQUEST_BYTES)
}

pub fn read_privileged_reply(input: impl Read) -> io::Result<PrivilegedReply> {
    read_message(input, MAX_RESPONSE_BYTES)
}

pub fn write_privileged_reply(output: impl Write, reply: &PrivilegedReply) -> io::Result<()> {
    write_frame(output, reply, MAX_RESPONSE_BYTES)
}

pub(crate) fn read_frame<T: DeserializeOwned>(mut input: impl Read, max: usize) -> io::Result<T> {
    let value = read_message(&mut input, max)?;
    require_eof(input)?;
    Ok(value)
}

pub fn require_eof(mut input: impl Read) -> io::Result<()> {
    let mut excess = Zeroizing::new([0; 1]);
    loop {
        match input.read(excess.as_mut()) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(invalid("Unexpected data after the archive helper frame")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn read_message<T: DeserializeOwned>(mut input: impl Read, max: usize) -> io::Result<T> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > max {
        return Err(invalid(
            "Archive helper frame length exceeds the protocol limit",
        ));
    }
    let mut payload = Zeroizing::new(vec![0; size]);
    input.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|_| invalid("Invalid archive helper message"))
}

pub(crate) fn write_frame(
    mut output: impl Write,
    value: &impl Serialize,
    max: usize,
) -> io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    if payload.len() > max {
        return Err(invalid(
            "Archive helper frame length exceeds the protocol limit",
        ));
    }
    output.write_all(&(payload.len() as u32).to_be_bytes())?;
    output.write_all(&payload)?;
    output.flush()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_require_bounded_lengths_one_message_and_exact_fields() {
        let request = Request::Review {
            source: "/tmp/ä.zip".into(),
        };
        let mut wire = Vec::new();
        write_request(&mut wire, &request).unwrap();
        assert!(
            matches!(read_request(wire.as_slice()).unwrap(), Request::Review { source } if source == "/tmp/ä.zip")
        );
        assert!(read_request(&wire[..wire.len() - 1]).is_err());
        wire.push(b'\n');
        assert!(read_request(wire.as_slice()).is_err());
        assert!(read_request(&(MAX_REQUEST_BYTES as u32 + 1).to_be_bytes()[..]).is_err());
        let mut invalid = Vec::new();
        let extra =
            serde_json::json!({"operation":"review","source":"/a.zip","command":"unexpected"});
        write_frame(&mut invalid, &extra, MAX_REQUEST_BYTES).unwrap();
        assert!(read_request(invalid.as_slice()).is_err());
    }

    #[test]
    fn privileged_frames_keep_one_stream_open_for_the_review_and_commit() {
        let spec = CreateSpec {
            sources: vec!["/srv/kortty/input".into()],
            destination: "/srv/kortty/output/input.tar".into(),
            format: CreateFormat::Tar,
            level: 0,
            excludes: vec![],
            output: None,
            report_payload: false,
        };
        let mut frames = Vec::new();
        write_privileged_request(
            &mut frames,
            &PrivilegedRequest::Review { spec: spec.clone() },
        )
        .unwrap();
        write_privileged_request(
            &mut frames,
            &PrivilegedRequest::Commit {
                token: "a".repeat(64),
            },
        )
        .unwrap();
        let mut input = frames.as_slice();
        assert!(
            matches!(read_privileged_request(&mut input).unwrap(), PrivilegedRequest::Review { spec: actual } if actual == spec)
        );
        assert!(
            matches!(read_privileged_request(&mut input).unwrap(), PrivilegedRequest::Commit { token } if token == "a".repeat(64))
        );
        assert!(input.is_empty());
        assert!(
            read_privileged_request(&(MAX_REQUEST_BYTES as u32 + 1).to_be_bytes()[..]).is_err()
        );
        let forged =
            serde_json::json!({"operation":"commit","token":"secret","destination":"/other"});
        frames.clear();
        write_frame(&mut frames, &forged, MAX_REQUEST_BYTES).unwrap();
        assert!(read_privileged_request(frames.as_slice()).is_err());
    }
}
