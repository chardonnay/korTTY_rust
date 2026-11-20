//! One checkpointed component of an owned Hub transfer.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use pgp::crypto::hash::HashAlgorithm;

use crate::local_ai::managed_models::{check_cancelled, gguf};
use crate::private_files::{Directory, FileStamp, require_single_link};

use super::super::{Artifact, Hub, Range, Repository};
use super::{Progress, Stop, hex, validate_response};

pub(in crate::local_ai::managed_models::huggingface) struct Component<'a> {
    pub(in crate::local_ai::managed_models::huggingface) directory: &'a Directory,
    pub(in crate::local_ai::managed_models::huggingface) name: &'a str,
    pub(in crate::local_ai::managed_models::huggingface) repository: &'a Repository,
    pub(in crate::local_ai::managed_models::huggingface) revision: &'a str,
    pub(in crate::local_ai::managed_models::huggingface) total: u64,
    pub(in crate::local_ai::managed_models::huggingface) artifact: &'a Artifact,
    pub(in crate::local_ai::managed_models::huggingface) hub: &'a Hub,
    pub(in crate::local_ai::managed_models::huggingface) stop: Stop<'a>,
    pub(in crate::local_ai::managed_models::huggingface) completed_bytes: u64,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::local_ai::managed_models::huggingface) struct Checkpoint {
    pub(in crate::local_ai::managed_models::huggingface) received: u64,
    pub(in crate::local_ai::managed_models::huggingface) etag: Option<String>,
    pub(in crate::local_ai::managed_models::huggingface) prefix_digest: String,
}

pub(in crate::local_ai::managed_models::huggingface) struct Verified {
    pub(in crate::local_ai::managed_models::huggingface) stamp: FileStamp,
    pub(in crate::local_ai::managed_models::huggingface) context: u32,
    pub(in crate::local_ai::managed_models::huggingface) split: Option<gguf::Inspection>,
}

pub(in crate::local_ai::managed_models::huggingface) enum Outcome {
    Complete,
    Paused {
        checkpoint: Option<Checkpoint>,
        interrupted: bool,
    },
}

pub(in crate::local_ai::managed_models::huggingface) struct Failure {
    pub(in crate::local_ai::managed_models::huggingface) error: io::Error,
    pub(in crate::local_ai::managed_models::huggingface) discard: bool,
}

impl Failure {
    fn discard(error: io::Error) -> Self {
        Self {
            error,
            discard: true,
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            discard: false,
        }
    }
}

impl Component<'_> {
    pub(in crate::local_ai::managed_models::huggingface) fn verified_file(
        &self,
    ) -> io::Result<(File, FileStamp)> {
        let mut file = self.directory.open_private_file(self.name)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let size = self.artifact.size();
        let mut buffer = vec![0; 128 * 1024];
        let digest = hash_prefix(&mut file, size, &mut buffer, self.stop)?;
        stamp.require_unchanged(&file)?;
        if file.metadata()?.len() != size || hex(&digest) != self.artifact.digest() {
            return Err(io::Error::other(
                "A staged model component does not match its reviewed SHA-256",
            ));
        }
        Ok((file, stamp))
    }

    pub(in crate::local_ai::managed_models::huggingface) fn verify_gguf(
        &self,
        ordinal: usize,
        count: usize,
    ) -> io::Result<Verified> {
        let (mut file, stamp) = self.verified_file()?;
        let size = self.artifact.size();
        file.rewind()?;
        let (context, split) = if count == 1 {
            (gguf::context(&mut file, size, self.stop.cancelled)?, None)
        } else {
            let inspection =
                gguf::inspect_split(&mut file, size, ordinal, count, self.stop.cancelled)?;
            (inspection.context, Some(inspection))
        };
        stamp.require_unchanged(&file)?;
        Ok(Verified {
            stamp,
            context,
            split,
        })
    }

    pub(in crate::local_ai::managed_models::huggingface) fn fetch(
        &self,
        prior: Option<&Checkpoint>,
        mut progress: impl FnMut(Progress),
    ) -> Result<Outcome, Failure> {
        let size = self.artifact.size();
        let mut received = prior.map_or(0, |receipt| receipt.received);
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        let mut buffer = vec![0; 128 * 1024];
        if let Some(prior) = prior {
            let mut file = self.directory.open_private_file(self.name)?;
            require_single_link(&file)?;
            if file.metadata()?.len() != received {
                return Err(io::Error::other(
                    "The paused component length changed. Discard it and start again",
                )
                .into());
            }
            let stamp = FileStamp::read(&file)?;
            read_prefix(&mut file, received, &mut buffer, self.stop, |bytes| {
                hash.update(bytes)
            })?;
            stamp.require_unchanged(&file)?;
            if hex(&hash.box_clone().finalize()) != prior.prefix_digest {
                return Err(io::Error::other(
                    "The paused component checksum changed. Discard it and start again",
                )
                .into());
            }
        } else if self.directory.has_entry(self.name)? {
            return Err(io::Error::other(
                "Discard the incomplete model download before starting again",
            )
            .into());
        }
        if self.stop.pausing() && (prior.is_some() || self.completed_bytes != 0) {
            return Ok(Outcome::Paused {
                checkpoint: None,
                interrupted: false,
            });
        }
        let range = prior
            .and_then(|checkpoint| checkpoint.etag.as_deref())
            .map(|etag| Range {
                start: received,
                etag,
            });
        let mut response = match self.hub.artifact(
            self.repository,
            self.revision,
            self.artifact,
            range,
            self.stop.deadline,
            self.stop.cancelled,
        ) {
            Ok(response) => response,
            Err(_) if self.stop.pausing() && (prior.is_some() || self.completed_bytes != 0) => {
                return Ok(Outcome::Paused {
                    checkpoint: None,
                    interrupted: false,
                });
            }
            Err(error) => return Err(io::Error::other(error).into()),
        };
        let validator =
            validate_response(&response, received, size, range.map(|range| range.etag))?;
        let mut part = self
            .directory
            .open_lock(self.name)
            .map_err(Failure::discard)?;
        let fail = Failure::discard;
        require_single_link(&part).map_err(fail)?;
        if part.metadata().map_err(fail)?.len() != received {
            return Err(fail(io::Error::other(
                "The model component changed before writing",
            )));
        }
        part.seek(SeekFrom::Start(received)).map_err(fail)?;
        let mut input = response.body_mut().as_reader();
        let resumable = validator.is_some() || self.completed_bytes != 0;
        let mut emitted = std::time::Instant::now();
        let mut next_authorization = emitted + std::time::Duration::from_secs(1);
        let mut policy_interrupted = false;
        let current = |received, resumable| Progress {
            received: self.completed_bytes + received,
            total: self.total,
            resumable,
        };
        progress(current(received, resumable));
        let transfer = (|| -> io::Result<bool> {
            while received < size {
                self.stop.active()?;
                if self.stop.pausing() {
                    return Ok(false);
                }
                let limit = (size - received).min(buffer.len() as u64) as usize;
                let count = input.read(&mut buffer[..limit])?;
                if count == 0 {
                    return Ok(false);
                }
                if std::time::Instant::now() >= next_authorization {
                    if let Err(error) = self.hub.require_downloads() {
                        policy_interrupted = true;
                        return Err(error);
                    }
                    next_authorization =
                        std::time::Instant::now() + std::time::Duration::from_secs(1);
                }
                part.write_all(&buffer[..count])?;
                hash.update(&buffer[..count]);
                received += count as u64;
                if received == size || emitted.elapsed() >= std::time::Duration::from_millis(100) {
                    progress(current(received, resumable && received < size));
                    emitted = std::time::Instant::now();
                }
            }
            if input.read(&mut buffer[..1])? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The Hub sent more bytes than the reviewed component",
                ));
            }
            Ok(true)
        })();
        check_cancelled(self.stop.cancelled)?;
        if !matches!(transfer, Ok(true)) {
            let recoverable = policy_interrupted
                || transfer.as_ref().err().is_none_or(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::Other
                    )
                });
            let interrupted = !self.stop.pausing();
            if recoverable && received < size {
                if received != 0
                    && validator.is_some()
                    && part.metadata().map_err(fail)?.len() == received
                {
                    part.sync_all().map_err(fail)?;
                    return Ok(Outcome::Paused {
                        checkpoint: Some(Checkpoint {
                            received,
                            etag: validator,
                            prefix_digest: hex(&hash.finalize()),
                        }),
                        interrupted,
                    });
                }
                if prior.is_none() && self.completed_bytes != 0 {
                    drop(part);
                    let file = self.directory.open_private_file(self.name).map_err(fail)?;
                    let stamp = FileStamp::read(&file).map_err(fail)?;
                    drop(file);
                    self.directory
                        .remove_unchanged(self.name, &stamp)
                        .map_err(fail)?;
                    return Ok(Outcome::Paused {
                        checkpoint: None,
                        interrupted,
                    });
                }
            }
            return Err(fail(transfer.err().unwrap_or_else(|| {
                io::Error::other("The transfer has no usable resume validator")
            })));
        }
        if hex(&hash.finalize()) != self.artifact.digest() {
            return Err(fail(io::Error::other(
                "The downloaded component does not match its reviewed SHA-256",
            )));
        }
        part.sync_all().map_err(fail)?;
        // Windows commits the last write time when the write handle closes.
        drop(part);
        Ok(Outcome::Complete)
    }
}

fn hash_prefix(
    file: &mut File,
    remaining: u64,
    buffer: &mut [u8],
    stop: Stop<'_>,
) -> io::Result<Box<[u8]>> {
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    read_prefix(file, remaining, buffer, stop, |bytes| hash.update(bytes))?;
    Ok(hash.finalize())
}

fn read_prefix(
    file: &mut File,
    mut remaining: u64,
    buffer: &mut [u8],
    stop: Stop<'_>,
    mut hash: impl FnMut(&[u8]),
) -> io::Result<()> {
    while remaining != 0 {
        stop.active()?;
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(io::Error::other(
                "The staged model component is shorter than expected",
            ));
        }
        hash(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(())
}
