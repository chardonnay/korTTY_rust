//! Checkpointed transfers of one reviewed MLX package.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use kortty_vault::Secret;
use serde::{Deserialize, Serialize};

use crate::local_ai::managed_models::{check_cancelled, lock, valid_id};
use crate::local_ai::runtime_package::{Pending, work_with_agent};
use crate::local_ai::{mlx_models, mlx_runtime};
use crate::private_files::{Directory, EntryIdentity, FileStamp, require_single_link};

use super::{Hub, PackageReview, lowercase_hex, transfer};
use transfer::component::{Checkpoint, Component, Outcome};
use transfer::{Pause, Progress, Stop};

pub(super) const STAGING: &str = "huggingface-mlx-downloads";
pub(super) const LOCK: &str = ".download.lock";
const TRANSFER: &str = "paused";

pub(crate) struct Download {
    pub(crate) pending: Pending<Downloaded>,
    pub(crate) progress: async_channel::Receiver<Progress>,
    pub(crate) pause: Pause,
}

#[derive(Clone, Debug)]
pub(crate) enum Downloaded {
    Installed {
        model: mlx_models::Model,
        cleanup_warning: bool,
        sync_warning: bool,
    },
    Paused {
        review: Arc<PackageReview>,
        interrupted: bool,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    review: Arc<PackageReview>,
    index: usize,
    component: Checkpoint,
}

impl Receipt {
    fn read(stage: &Directory) -> io::Result<Option<Self>> {
        let Some(receipt): Option<Self> = transfer::read_saved(stage)? else {
            return Ok(None);
        };
        receipt.review.validate()?;
        let file = receipt
            .review
            .files
            .get(receipt.index)
            .ok_or_else(|| invalid("The paused MLX asset index is invalid."))?;
        let component = &receipt.component;
        if component.received == 0
            || component.received > file.size()
            || !lowercase_hex(&component.prefix_digest, 64)
            || (component.received == file.size() && component.prefix_digest != file.digest())
            || (component.received < file.size()
                && component
                    .etag
                    .as_deref()
                    .is_none_or(|etag| !transfer::strong_etag(etag)))
        {
            return Err(invalid("The paused MLX asset checkpoint is invalid."));
        }
        Ok(Some(receipt))
    }

    fn bytes(&self) -> u64 {
        self.review.files[..self.index]
            .iter()
            .map(super::Artifact::size)
            .sum::<u64>()
            + self.component.received
    }
}

pub(crate) fn download(
    profile: PathBuf,
    review: Arc<PackageReview>,
    resume: bool,
    credentials: Option<(Secret, Weak<()>)>,
) -> Result<Download, String> {
    if !mlx_runtime::supported() {
        return Err("MLX requires Apple Silicon and macOS 14 or later.".into());
    }
    review.validate().map_err(|error| error.to_string())?;
    let (token, revision) = credentials.unzip();
    let mut hub = Hub::new(token, transfer::DOWNLOAD_TIMEOUT)?;
    hub.bind_credential_revision(revision);
    let (pause, paused) = Pause::new(hub.agent.clone());
    let (updates, progress) = async_channel::bounded(2);
    let pending = work_with_agent(Some(hub.agent.clone()), move |cancelled| {
        install(
            &profile,
            &review,
            &hub,
            resume,
            &paused,
            cancelled,
            |update| {
                let _ = updates.try_send(update);
            },
        )
        .map_err(|error| format!("Could not install the Hugging Face MLX model ({error})."))
    })?;
    Ok(Download {
        pending,
        progress,
        pause,
    })
}

pub(crate) fn paused(profile: PathBuf) -> Result<Pending<Option<Arc<PackageReview>>>, String> {
    work_with_agent(None, move |cancelled| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        read_paused(&profile).map_err(|error| error.to_string())
    })
}

pub(super) fn read_paused(profile: &Path) -> io::Result<Option<Arc<PackageReview>>> {
    with_saved_stage(profile, |_, stage| {
        Receipt::read(stage)?
            .map(|receipt| receipt.review)
            .ok_or_else(|| {
                invalid(
                    "The interrupted MLX transfer has no checkpoint. Discard it to start again.",
                )
            })
    })
}

pub(crate) fn discard_paused(profile: PathBuf) -> Result<Pending<bool>, String> {
    work_with_agent(None, move |cancelled| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        discard_saved(&profile).map_err(|error| error.to_string())
    })
}

pub(super) fn discard_saved(profile: &Path) -> io::Result<bool> {
    with_saved_stage(profile, |root, stage| {
        cleanup(root, stage, stage.identity()?)
    })
    .map(|removed| removed.is_some())
}

fn with_saved_stage<T>(
    profile: &Path,
    action: impl FnOnce(&Directory, &Directory) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let root = match Directory::open(profile, false)?.private_child(STAGING, false) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let _lock = lock(&root, LOCK, false)?;
    match root.private_child(TRANSFER, false) {
        Ok(stage) => action(&root, &stage).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn install(
    profile: &Path,
    review: &Arc<PackageReview>,
    hub: &Hub,
    resume: bool,
    paused: &AtomicBool,
    cancelled: &AtomicBool,
    progress: impl FnMut(Progress),
) -> io::Result<Downloaded> {
    review.validate()?;
    check_cancelled(cancelled)?;
    hub.require_downloads()?;
    let root = Directory::open(profile, false)?.private_child(STAGING, true)?;
    let _lock = lock(&root, LOCK, false)?;
    for entry in root.names(130)? {
        if entry == LOCK || entry == TRANSFER {
            continue;
        }
        let id = entry
            .to_str()
            .filter(|name| valid_id(name))
            .ok_or_else(|| invalid("Unexpected MLX download stage entry."))?;
        mlx_models::discard_stage(&root, id)?;
    }
    let exists = root.has_entry(TRANSFER)?;
    if exists != resume {
        return Err(invalid(if exists {
            "Resume or discard the existing MLX download before starting another."
        } else {
            "The saved MLX download is no longer available."
        }));
    }
    let stage = if exists {
        root.private_child(TRANSFER, false)?
    } else {
        root.create_private_subdirectory(std::ffi::OsStr::new(TRANSFER))?
    };
    let identity = stage.identity()?;
    let mut keep = exists;
    let stop = Stop {
        paused,
        cancelled,
        deadline: Instant::now() + transfer::DOWNLOAD_TIMEOUT,
    };
    let result = transfer(profile, review, hub, &stage, stop, &mut keep, progress);
    if keep && !cancelled.load(Ordering::Acquire) {
        return result;
    }
    let cleanup = cleanup(&root, &stage, identity);
    match (result, cleanup) {
        (
            Ok(Downloaded::Installed {
                model,
                sync_warning,
                ..
            }),
            cleanup,
        ) => Ok(Downloaded::Installed {
            model,
            sync_warning,
            cleanup_warning: cleanup.is_err(),
        }),
        (Ok(paused), Ok(())) => Ok(paused),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(io::Error::other(format!(
            "{error}. Could not clean up the MLX download ({cleanup})"
        ))),
    }
}

fn transfer(
    profile: &Path,
    review: &Arc<PackageReview>,
    hub: &Hub,
    stage: &Directory,
    stop: Stop<'_>,
    keep: &mut bool,
    mut progress: impl FnMut(Progress),
) -> io::Result<Downloaded> {
    let prior = Receipt::read(stage)?;
    if *keep && prior.is_none()
        || prior
            .as_ref()
            .is_some_and(|record| record.review != *review)
    {
        return Err(invalid(
            "The paused MLX package differs from the reviewed package.",
        ));
    }
    let accounted = prior
        .as_ref()
        .map_or(&[][..], |record| &review.files[..=record.index]);
    for name in stage.names(132)? {
        if name != transfer::RECEIPT && !accounted.iter().any(|file| name == file.path()) {
            return Err(invalid(
                "The MLX download has unaccounted files. Discard it to start again.",
            ));
        }
    }
    let saved_bytes = prior.as_ref().map_or(0, Receipt::bytes);
    if transfer::free_bytes(profile)?
        < review
            .bytes
            .saturating_mul(2)
            .saturating_sub(saved_bytes)
            .saturating_add(transfer::RESERVE_BYTES)
    {
        return Err(invalid(
            "There is insufficient space to stage and register the MLX model.",
        ));
    }
    let paused = |interrupted| Downloaded::Paused {
        review: Arc::clone(review),
        interrupted,
    };
    progress(Progress {
        received: saved_bytes,
        total: review.bytes,
        resumable: *keep,
    });
    let mut completed_bytes = 0;
    for (index, file) in review.files.iter().enumerate() {
        stop.active()?;
        if stop.pausing() && *keep {
            return Ok(paused(false));
        }
        hub.require_downloads()?;
        let component = Component {
            directory: stage,
            name: file.path(),
            repository: &review.repository,
            revision: &review.revision,
            total: review.bytes,
            artifact: file,
            hub,
            stop,
            completed_bytes,
        };
        let checkpoint = prior
            .as_ref()
            .filter(|record| record.index == index)
            .map(|record| &record.component);
        let verify_only = prior.as_ref().is_some_and(|record| index < record.index)
            || checkpoint.is_some_and(|checkpoint| checkpoint.received == file.size());
        if verify_only {
            component.verified_file()?;
        } else {
            let outcome = component
                .fetch(checkpoint, &mut progress)
                .map_err(|failure| {
                    if failure.discard {
                        *keep = false;
                    }
                    failure.error
                })?;
            let (component, interrupted) = match outcome {
                Outcome::Complete => (
                    Some(Checkpoint {
                        received: file.size(),
                        etag: None,
                        prefix_digest: file.digest().into(),
                    }),
                    None,
                ),
                Outcome::Paused {
                    checkpoint,
                    interrupted,
                } => (checkpoint, Some(interrupted)),
            };
            if let Some(component) = component {
                if let Err(error) = transfer::save_receipt(
                    stage,
                    Receipt {
                        review: Arc::clone(review),
                        index,
                        component,
                    },
                ) {
                    *keep = false;
                    return Err(error);
                }
                *keep = true;
            }
            if let Some(interrupted) = interrupted {
                return Ok(paused(interrupted));
            }
        }
        completed_bytes += file.size();
    }
    if stop.pausing() {
        return Ok(paused(false));
    }
    progress(Progress {
        received: review.bytes,
        total: review.bytes,
        resumable: false,
    });
    let result = register(profile, stage, review, hub, stop);
    if stop.pausing() && result.is_err() && !stop.cancelled.load(Ordering::Acquire) {
        return Ok(paused(false));
    }
    let (model, sync_warning) = result?;
    *keep = false;
    Ok(Downloaded::Installed {
        model,
        sync_warning,
        cleanup_warning: false,
    })
}

fn register(
    profile: &Path,
    stage: &Directory,
    review: &PackageReview,
    hub: &Hub,
    stop: Stop<'_>,
) -> io::Result<(mlx_models::Model, bool)> {
    let source = profile.join(STAGING).join(TRANSFER);
    if !Directory::open(&source, false)?.same_as(stage)? {
        return Err(invalid("The private MLX download stage changed."));
    }
    let mut native = mlx_models::Review::prepare_hub_stage(&source, stop.cancelled)?;
    if native.asset_names().count() != review.files.len()
        || review.files.iter().any(|file| {
            native
                .asset_identity(file.path())
                .is_none_or(|(bytes, digest)| {
                    bytes != file.size() || transfer::hex(digest) != file.digest()
                })
        })
    {
        return Err(invalid(
            "The native MLX review differs from the Hugging Face package.",
        ));
    }
    native.set_hub_metadata(review.repository.id(), review.published_at.as_deref())?;
    mlx_models::Registry::new(profile).import_authorized(&native, stop.cancelled, || {
        if stop.pausing() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Paused MLX registration.",
            ));
        }
        hub.require_downloads()
    })
}

fn cleanup(root: &Directory, stage: &Directory, identity: EntryIdentity) -> io::Result<()> {
    if let Ok(file) = stage.open_private_file(transfer::RECEIPT) {
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        drop(file);
        stage.remove_unchanged(transfer::RECEIPT, &stamp)?;
    }
    mlx_models::discard_stage_checked(root, TRANSFER, Some(identity))
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}
