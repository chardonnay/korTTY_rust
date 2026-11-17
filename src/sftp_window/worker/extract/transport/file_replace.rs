//! Checked upload to a protected regular file over the pinned helper channel.

use std::io::{self, Read, Seek};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use kortty_server_archive::privileged_file_replace::{
    ARGUMENT, Decision, MAX_BYTES, Payload, READY, Reply, Request, valid_recovery,
};
use kortty_server_archive::rsync_tree::{read_frame, write_frame};
use kortty_ssh::{ExecOpener, SensitiveLine};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::private_files::require_single_link;
use crate::sftp_window::LocalSource;
use crate::sftp_window::privileged_file::{copy_digest, valid_path};

use super::{DECISION_TIMEOUT, FixedHelper, Permit, invalid_output};

pub(crate) struct Source {
    file: LocalSource,
    pub(crate) payload: Payload,
}

impl Source {
    pub(crate) fn open(path: &Path, limit: u64, allowed: &impl Fn() -> bool) -> io::Result<Self> {
        super::running(&|| !allowed())?;
        let file = LocalSource::open(path)?;
        require_single_link(file.file())?;
        let bytes = file.size();
        if bytes > limit.min(MAX_BYTES) {
            return Err(io::Error::other(
                "The protected upload source exceeds its transfer limit",
            ));
        }
        let sha256 = copy_digest(&mut file.file(), &mut io::sink(), bytes, allowed)
            .map_err(|_| io::Error::other("Could not read the checked local upload source"))?;
        file.verify()?;
        require_single_link(file.file())?;
        Ok(Self {
            file,
            payload: Payload { bytes, sha256 },
        })
    }

    fn require_current(&self) -> io::Result<()> {
        require_single_link(self.file.file()).and_then(|()| self.file.verify())
    }

    fn proof(&self, receiver: [u8; 32]) -> [u8; 32] {
        let mut hash = Sha256::new_with_prefix(b"kortty-protected-upload-approval-v1\0");
        hash.update(receiver);
        hash.update(self.payload.bytes.to_be_bytes());
        hash.update(self.payload.sha256);
        hash.finalize().into()
    }

    fn send(&self, output: &mut FixedHelper, permitted: Permit) -> io::Result<()> {
        self.require_current()?;
        let mut input = self.file.file();
        input.rewind()?;
        let mut bytes = Zeroizing::new([0u8; 32 * 1024]);
        let mut remaining = self.payload.bytes;
        while remaining != 0 {
            super::running(&|| !permitted())?;
            let count = remaining.min(bytes.len() as u64) as usize;
            input.read_exact(&mut bytes[..count])?;
            output.write(&bytes[..count], permitted.clone())?;
            remaining -= count as u64;
        }
        self.require_current()
    }
}

pub(crate) struct Review {
    pub(crate) proof: [u8; 32],
    pub(crate) bytes: u64,
}

pub(crate) fn review(
    opener: &ExecOpener,
    path: &str,
    source: &Source,
    recurring: bool,
    password: Option<SensitiveLine>,
    permitted: Permit,
) -> io::Result<Review> {
    let mut channel = Channel::start(
        opener,
        Request {
            path: path.to_owned(),
            review_only: true,
            recurring,
        },
        password,
        permitted.clone(),
    )?;
    let stopped = || !permitted();
    let (fingerprint, bytes) = channel.receiver(&stopped)?;
    channel.finish(&stopped)?;
    source.require_current()?;
    Ok(Review {
        proof: if recurring {
            fingerprint
        } else {
            source.proof(fingerprint)
        },
        bytes,
    })
}

pub(crate) struct Upload<'a> {
    pub(crate) path: &'a str,
    pub(crate) source: &'a Source,
    pub(crate) proof: [u8; 32],
    pub(crate) recurring: bool,
    pub(crate) transfer_allowed: Permit,
    pub(crate) publication_allowed: Permit,
}

pub(crate) struct Publication {
    pub(crate) recovery: String,
    pub(crate) successor: Option<[u8; 32]>,
}

pub(crate) enum Failure {
    BeforePublication,
    Changed,
    Refused,
    Uncertain(Option<String>),
}

pub(crate) fn upload(
    opener: &ExecOpener,
    password: Option<SensitiveLine>,
    upload: Upload<'_>,
    claim: impl FnOnce() -> io::Result<()>,
) -> Result<Publication, Failure> {
    use Failure::{BeforePublication, Changed, Refused, Uncertain};

    let Upload {
        path,
        source,
        proof,
        recurring,
        transfer_allowed,
        publication_allowed,
    } = upload;
    let mut channel = Channel::start(
        opener,
        Request {
            path: path.to_owned(),
            review_only: false,
            recurring,
        },
        password,
        publication_allowed.clone(),
    )
    .map_err(|_| BeforePublication)?;
    let stopped = || !transfer_allowed();
    let (receiver, _) = channel.receiver(&stopped).map_err(|_| Refused)?;
    let current = if recurring {
        receiver
    } else {
        source.proof(receiver)
    };
    if current != proof || !publication_allowed() {
        return Err(Changed);
    }
    channel
        .send(&source.payload, transfer_allowed.clone())
        .map_err(|_| BeforePublication)?;
    if !matches!(channel.read(&stopped), Ok(Reply::Receive)) {
        return Err(Refused);
    }
    source
        .send(&mut channel.0, transfer_allowed.clone())
        .map_err(|_| BeforePublication)?;
    if !matches!(channel.read(&stopped), Ok(Reply::Verified { payload }) if payload == source.payload)
    {
        return Err(Refused);
    }
    if !publication_allowed() || source.require_current().is_err() {
        return Err(Changed);
    }
    claim().map_err(|_| Changed)?;
    channel
        .send(&Decision::Quarantine, publication_allowed.clone())
        .map_err(|_| Uncertain(None))?;

    // Read the outcome even when the job loses permission after submitting a decision.
    let deadline = Instant::now() + DECISION_TIMEOUT;
    let waiting = || Instant::now() >= deadline;
    let recovery = match channel.read(&waiting).map_err(|_| Uncertain(None))? {
        Reply::Quarantined { recovery } if valid_recovery(path, &recovery) => recovery,
        reply => return Err(channel.refusal(path, reply, None, &waiting)),
    };
    let authorized = publication_allowed() && source.require_current().is_ok();
    let deadline = Instant::now() + DECISION_TIMEOUT;
    let ending = move || Instant::now() >= deadline;
    let (decision, permitted) = if authorized {
        (Decision::Publish, publication_allowed)
    } else {
        (Decision::Abort, Arc::new(move || !ending()) as Permit)
    };
    channel
        .send(&decision, permitted)
        .map_err(|_| Uncertain(Some(recovery.clone())))?;
    match channel
        .read(&ending)
        .map_err(|_| Uncertain(Some(recovery.clone())))?
    {
        Reply::Published {
            payload,
            recovery: retained,
            successor,
        } if authorized
            && retained == recovery
            && payload == source.payload
            && if recurring {
                successor.is_some_and(|current| current != [0; 32] && current != receiver)
            } else {
                successor.is_none()
            } =>
        {
            channel
                .finish(&ending)
                .map_err(|_| Uncertain(Some(recovery.clone())))?;
            Ok(Publication {
                recovery,
                successor,
            })
        }
        reply => Err(channel.refusal(path, reply, Some(recovery), &ending)),
    }
}

struct Channel(FixedHelper);

impl Channel {
    fn start(
        opener: &ExecOpener,
        request: Request,
        password: Option<SensitiveLine>,
        permitted: Permit,
    ) -> io::Result<Self> {
        if !valid_path(&request.path) || request.path.len() > 2048 {
            return Err(invalid_output());
        }
        let mut channel = Self(FixedHelper::open(
            opener,
            ARGUMENT,
            READY,
            password,
            permitted.clone(),
        )?);
        channel.send(&request, permitted)?;
        Ok(channel)
    }

    fn send(&mut self, message: &impl serde::Serialize, allowed: Permit) -> io::Result<()> {
        let mut frame = Vec::new();
        write_frame(&mut frame, message)?;
        self.0.write(&frame, allowed)
    }

    fn read(&mut self, stopped: &impl Fn() -> bool) -> io::Result<Reply> {
        read_frame(self.0.reader(stopped))
    }

    fn receiver(&mut self, stopped: &impl Fn() -> bool) -> io::Result<([u8; 32], u64)> {
        match self.read(stopped)? {
            Reply::Reviewed { fingerprint, bytes }
                if fingerprint != [0; 32] && bytes <= MAX_BYTES =>
            {
                Ok((fingerprint, bytes))
            }
            _ => Err(invalid_output()),
        }
    }

    fn finish(&mut self, stopped: &impl Fn() -> bool) -> io::Result<()> {
        self.0.finish(stopped)
    }

    fn refusal(
        &mut self,
        path: &str,
        reply: Reply,
        recovery: Option<String>,
        stopped: &impl Fn() -> bool,
    ) -> Failure {
        match reply {
            Reply::Aborted
            | Reply::Failed {
                outcome_unknown: false,
                recovery: None,
            } if self.finish(stopped).is_ok() => Failure::Refused,
            Reply::Failed {
                recovery: Some(reported),
                ..
            } if valid_recovery(path, &reported) => Failure::Uncertain(recovery.or(Some(reported))),
            _ => Failure::Uncertain(recovery),
        }
    }
}
