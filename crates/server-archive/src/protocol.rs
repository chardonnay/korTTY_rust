//! Framed messages for the ordinary and privileged archive helper modes.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::manifest::Manifest;

pub const COMMAND: &str = "exec /usr/local/bin/kortty-server-archive --stdio-v1";
pub const PRIVILEGED_ARGUMENT: &str = "--privileged-create-v1";
pub const PRIVILEGED_PATH: &str = "/usr/libexec/kortty/kortty-server-archive";
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateFormat {
    Zip,
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
    if io::copy(&mut input.take(1), &mut io::sink())? != 0 {
        return Err(invalid("Unexpected data after the archive helper frame"));
    }
    Ok(value)
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
    let mut payload = vec![0; size];
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
