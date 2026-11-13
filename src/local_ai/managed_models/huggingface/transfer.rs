//! Resumable, verified transfers into the private managed model registry.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kortty_vault::Secret;
use serde::{Deserialize, Serialize};
use ureq::{Agent, http::Response};

use crate::local_ai::managed_models::{
    MANIFEST, MAX_MODELS, Model, REGISTRY_LOCK, Registry, WEIGHTS, check_cancelled, discard_stage,
    import_name, lock, model_ids, prepare_staging, split,
};
use crate::local_ai::runtime_package::{Pending, work_with_agent};
use crate::private_files::{Directory, EntryIdentity, FileStamp, require_single_link};

use super::{Artifact, Hub, Plan, lowercase_hex, status_message};

mod component;
#[cfg(test)]
mod tests_multipart;
use component::{Checkpoint, Component, Outcome, Verified};

const FOLDER: &str = "huggingface-downloads";
const TRANSFER: &str = "paused";
const LOCK: &str = ".download.lock";
const RECEIPT: &str = "receipt.json";
const MAX_RECEIPT: u64 = 256 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const RESERVE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct Progress {
    pub(crate) received: u64,
    pub(crate) total: u64,
    pub(crate) resumable: bool,
}

pub(crate) struct Pause {
    requested: Arc<AtomicBool>,
    agent: Agent,
}

impl Pause {
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.agent.clone().shutdown();
    }
}

pub(crate) struct Download {
    pub(crate) pending: Pending<Downloaded>,
    pub(crate) progress: async_channel::Receiver<Progress>,
    pub(crate) pause: Pause,
}

#[derive(Clone, Debug)]
pub(crate) enum Downloaded {
    Installed(Model, bool),
    Paused { plan: Plan, interrupted: bool },
}

pub(crate) fn download(
    profile: PathBuf,
    plan: Plan,
    token: Option<Secret>,
) -> Result<Download, String> {
    plan.validate()?;
    let hub = Hub::new(token, DOWNLOAD_TIMEOUT)?;
    let pause = Pause {
        requested: Arc::new(AtomicBool::new(false)),
        agent: hub.agent.clone(),
    };
    let paused = Arc::clone(&pause.requested);
    let (updates, progress) = async_channel::bounded(2);
    let pending = work_with_agent(Some(hub.agent.clone()), move |cancelled| {
        run(&profile, &plan, &hub, &paused, cancelled, |update| {
            let _ = updates.try_send(update);
        })
        .map_err(|error| {
            if cancelled.load(Ordering::Acquire) {
                "Hugging Face download canceled.".into()
            } else {
                format!("Could not install the Hugging Face model ({error}).")
            }
        })
    })?;
    Ok(Download {
        pending,
        progress,
        pause,
    })
}

pub(crate) fn paused(profile: PathBuf) -> Result<Pending<Option<Plan>>, String> {
    work_with_agent(None, move |cancelled| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        read_paused(&profile).map_err(|error| error.to_string())
    })
}

pub(crate) fn discard_paused(profile: PathBuf) -> Result<Pending<bool>, String> {
    work_with_agent(None, move |cancelled| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        discard(&profile).map_err(|error| error.to_string())
    })
}

fn read_paused(profile: &Path) -> io::Result<Option<Plan>> {
    let Some(root) = existing_root(profile)? else {
        return Ok(None);
    };
    let _lock = lock(&root, LOCK, true)?;
    let Some(stage) = Stage::existing(&root)? else {
        return Ok(None);
    };
    let receipt = read_receipt(stage.directory())?;
    if receipt.is_none() && has_components(stage.directory())? {
        return Err(io::Error::other(
            "Discard the incomplete download before starting again",
        ));
    }
    Ok(receipt.map(|receipt| receipt.plan))
}

fn discard(profile: &Path) -> io::Result<bool> {
    let Some(root) = existing_root(profile)? else {
        return Ok(false);
    };
    let _lock = lock(&root, LOCK, false)?;
    let Some(mut stage) = Stage::existing(&root)? else {
        return Ok(false);
    };
    stage.cleanup()?;
    Ok(true)
}

fn existing_root(profile: &Path) -> io::Result<Option<Directory>> {
    match Directory::open(profile, false).and_then(|root| root.private_child(FOLDER, false)) {
        Ok(root) => Ok(Some(root)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn run(
    profile: &Path,
    plan: &Plan,
    hub: &Hub,
    paused: &AtomicBool,
    cancelled: &AtomicBool,
    progress: impl FnMut(Progress),
) -> io::Result<Downloaded> {
    check_cancelled(cancelled)?;
    hub.require_downloads()?;
    let root = Directory::open(profile, false)?.private_child(FOLDER, true)?;
    let _lock = lock(&root, LOCK, false)?;
    let mut stage = match Stage::existing(&root)? {
        Some(stage) => stage,
        None => Stage::new(&root, root.private_child(TRANSFER, true)?, false)?,
    };
    let result = transfer(
        profile,
        plan,
        hub,
        &mut stage,
        Stop {
            paused,
            cancelled,
            deadline: Instant::now() + DOWNLOAD_TIMEOUT,
        },
        progress,
    );
    if result.is_err() && cancelled.load(Ordering::Acquire) {
        stage.keep = false;
    }
    result
}

#[derive(Clone, Copy)]
struct Stop<'a> {
    paused: &'a AtomicBool,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

impl Stop<'_> {
    fn active(self) -> io::Result<()> {
        check_cancelled(self.cancelled)?;
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The model transfer exceeded its time limit",
            ));
        }
        Ok(())
    }

    fn pausing(self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
}

fn transfer(
    profile: &Path,
    plan: &Plan,
    hub: &Hub,
    stage: &mut Stage<'_>,
    stop: Stop<'_>,
    mut progress: impl FnMut(Progress),
) -> io::Result<Downloaded> {
    let prior = read_receipt(stage.directory())?;
    if prior.as_ref().is_some_and(|receipt| &receipt.plan != plan) {
        return Err(io::Error::other(
            "Finish or discard the paused model download before selecting another artifact",
        ));
    }
    validate_components(stage.directory(), plan, prior.as_ref())?;
    let saved_bytes = prior.as_ref().map_or(0, |receipt| {
        plan.files()
            .take(receipt.index)
            .map(Artifact::size)
            .sum::<u64>()
            + receipt.received
    });
    if saved_bytes < plan.total()
        && free_bytes(profile)? < plan.total() - saved_bytes + RESERVE_BYTES
    {
        return Err(io::Error::other(
            "There is insufficient free space for the reviewed model",
        ));
    }

    let mut verified = Vec::with_capacity(plan.count());
    let mut tensors = split::TensorTotals::default();
    let mut completed_bytes = 0;
    for (index, artifact) in plan.files().enumerate() {
        stop.active()?;
        hub.require_downloads()?;
        if stop.pausing() && stage.keep {
            return Ok(Downloaded::Paused {
                plan: plan.clone(),
                interrupted: false,
            });
        }
        let name = if plan.count() == 1 {
            WEIGHTS.to_owned()
        } else {
            split::private_name(index, plan.count())
        };
        let component = Component {
            directory: stage.directory(),
            name: &name,
            plan,
            artifact,
            hub,
            stop,
            ordinal: index,
            completed_bytes,
        };
        let prior_component = prior.as_ref().filter(|receipt| receipt.index == index);
        let verify_only = prior.as_ref().is_some_and(|receipt| index < receipt.index)
            || prior_component.is_some_and(|receipt| receipt.received == artifact.size());
        let component = if verify_only {
            component
                .verify()
                .map(Outcome::Complete)
                .map_err(component::Failure::from)
        } else {
            component.fetch(
                prior_component.map(Receipt::checkpoint).as_ref(),
                &mut progress,
            )
        };
        match component {
            Ok(Outcome::Complete(component)) => {
                if let Some(inspection) = &component.split
                    && let Err(error) = tensors.include(inspection)
                {
                    stage.keep = false;
                    return Err(error);
                }
                verified.push((name, component));
                completed_bytes += artifact.size();
                if !verify_only {
                    stage.checkpoint(Receipt {
                        plan: plan.clone(),
                        index,
                        received: artifact.size(),
                        etag: None,
                        prefix_digest: artifact.digest().to_owned(),
                    })?;
                }
            }
            Ok(Outcome::Paused {
                checkpoint,
                interrupted,
            }) => {
                if let Some(checkpoint) = checkpoint {
                    stage.checkpoint(Receipt {
                        plan: plan.clone(),
                        index,
                        received: checkpoint.received,
                        etag: checkpoint.etag,
                        prefix_digest: checkpoint.prefix_digest,
                    })?;
                }
                return Ok(Downloaded::Paused {
                    plan: plan.clone(),
                    interrupted,
                });
            }
            Err(failure) => {
                if failure.discard {
                    stage.keep = false;
                }
                return Err(failure.error);
            }
        }
    }
    if plan.count() > 1
        && let Err(error) = tensors.finish()
    {
        stage.keep = false;
        return Err(error);
    }
    if stop.pausing() {
        return Ok(Downloaded::Paused {
            plan: plan.clone(),
            interrupted: false,
        });
    }
    stop.active()?;
    let (model, sync_failed) = register(
        profile,
        stage.directory(),
        plan,
        &verified,
        stop.cancelled,
        hub,
    )?;
    stage.keep = false;
    Ok(Downloaded::Installed(model, sync_failed))
}

fn validate_response(
    response: &Response<impl Sized>,
    received: u64,
    total: u64,
    prior: Option<&str>,
) -> io::Result<Option<String>> {
    let headers = response.headers();
    let status = response.status().as_u16();
    let etag = headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| strong_etag(value))
        .map(str::to_owned);
    if received == 0 {
        if status != 200 {
            return Err(io::Error::other(status_message(status)));
        }
    } else {
        let expected = format!("bytes {received}-{}/{total}", total - 1);
        if status != 206
            || etag.as_deref() != prior
            || prior.is_none()
            || headers
                .get("content-range")
                .and_then(|value| value.to_str().ok())
                != Some(expected.as_str())
        {
            return Err(io::Error::other(
                "The server did not confirm the paused version and byte range. Discard it to restart",
            ));
        }
    }
    if headers.get("content-length").is_some_and(|value| {
        value
            .to_str()
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            != Some(total - received)
    }) {
        return Err(io::Error::other(
            "The Hub response length differs from the reviewed artifact",
        ));
    }
    if headers.get("content-encoding").is_some_and(|value| {
        value
            .to_str()
            .ok()
            .is_none_or(|value| !value.eq_ignore_ascii_case("identity"))
    }) {
        return Err(io::Error::other(
            "The Hub returned an unexpected content encoding",
        ));
    }
    Ok(etag)
}

fn strong_etag(value: &str) -> bool {
    let Some(opaque) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return false;
    };
    !opaque.is_empty()
        && value.len() <= 256
        && opaque
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b'"')
}

fn register(
    profile: &Path,
    source: &Directory,
    plan: &Plan,
    components: &[(String, Verified)],
    cancelled: &AtomicBool,
    hub: &Hub,
) -> io::Result<(Model, bool)> {
    check_cancelled(cancelled)?;
    hub.require_downloads()?;
    let name = split::components(plan.file.path()).map_or_else(
        || plan.file.path().to_owned(),
        |(stem, _, _)| format!("{stem}.gguf"),
    );
    let mut model = Model::new(
        import_name(Path::new(&name))?,
        plan.total(),
        components[0].1.context,
        plan.purpose,
    )?;
    if plan.count() > 1 {
        model.shard_bytes = plan.files().map(Artifact::size).collect();
    }
    let root = Registry::new(profile.to_path_buf()).root(true)?;
    let _lock = lock(&root, REGISTRY_LOCK, false)?;
    if model_ids(&root)?.len() >= MAX_MODELS {
        return Err(io::Error::other("Managed model registry is full"));
    }
    let staging = prepare_staging(&root)?;
    let target = staging.create_private_subdirectory(OsStr::new(&model.id))?;
    let mut moved = 0;
    let prepared = (|| {
        target.publish_copy(
            MANIFEST,
            &serde_json::to_vec(&model).map_err(io::Error::other)?,
        )?;
        for (name, verified) in components {
            let file = source.open_private_file(name)?;
            require_single_link(&file)?;
            verified.stamp.require_unchanged(&file)?;
            drop(file);
            check_cancelled(cancelled)?;
            source.move_entry(OsStr::new(name), &target)?;
            moved += 1;
            let file = target.open_private_file(name)?;
            require_single_link(&file)?;
            #[cfg(unix)]
            verified.stamp.stamp_after_move(&file)?;
            #[cfg(windows)]
            verified.stamp.require_unchanged(&file)?;
        }
        target.sync()?;
        check_cancelled(cancelled)
    })();
    drop(target);
    if let Err(error) = prepared.and_then(|()| {
        hub.require_downloads()?;
        staging.move_subdirectory(&model.id, &root)
    }) {
        if let Ok(target) = staging.private_child(&model.id, false) {
            for (name, _) in &components[..moved] {
                let _ = target.move_entry(OsStr::new(name), source);
            }
            drop(target);
            let _ = discard_stage(&staging, &model.id);
        }
        return Err(error);
    }
    let root_failed = root.sync().is_err();
    let staging_failed = staging.sync().is_err();
    Ok((model, root_failed || staging_failed))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    plan: Plan,
    #[serde(default, skip_serializing_if = "is_first_component")]
    index: usize,
    received: u64,
    etag: Option<String>,
    prefix_digest: String,
}

impl Receipt {
    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            received: self.received,
            etag: self.etag.clone(),
            prefix_digest: self.prefix_digest.clone(),
        }
    }
}

fn is_first_component(index: &usize) -> bool {
    *index == 0
}

fn has_components(directory: &Directory) -> io::Result<bool> {
    Ok(directory
        .names(split::MAX_SHARDS + 4)?
        .iter()
        .any(|name| name != RECEIPT))
}

fn validate_components(
    directory: &Directory,
    plan: &Plan,
    receipt: Option<&Receipt>,
) -> io::Result<()> {
    for name in directory.names(split::MAX_SHARDS + 4)? {
        let approved = receipt.is_some_and(|receipt| {
            name.to_str().is_some_and(|name| {
                name == RECEIPT
                    || if plan.count() == 1 {
                        name == WEIGHTS
                    } else {
                        matches!(split::components(name), Some(("model", ordinal, count))
                        if count == plan.count() && (1..=receipt.index + 1).contains(&ordinal))
                    }
            })
        });
        if !approved {
            return Err(io::Error::other(
                "Discard the incomplete download before starting again",
            ));
        }
    }
    Ok(())
}

fn read_receipt(directory: &Directory) -> io::Result<Option<Receipt>> {
    let file = match directory.open_private_file(RECEIPT) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(io::Error::other(
            "The paused download receipt exceeds its size limit",
        ));
    }
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::other("The paused download receipt is invalid"))?;
    receipt.plan.validate().map_err(io::Error::other)?;
    let file = receipt
        .plan
        .files()
        .nth(receipt.index)
        .ok_or_else(|| io::Error::other("The paused download component is invalid"))?;
    if receipt.received == 0
        || receipt.received > file.size()
        || !lowercase_hex(&receipt.prefix_digest, 64)
        || (receipt.received == file.size() && receipt.prefix_digest != file.digest())
        || (receipt.received < file.size()
            && receipt
                .etag
                .as_deref()
                .is_none_or(|etag| !strong_etag(etag)))
    {
        return Err(io::Error::other("The paused download receipt is invalid"));
    }
    Ok(Some(receipt))
}

fn save_receipt(directory: &Directory, receipt: Receipt) -> io::Result<()> {
    let bytes = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(io::Error::other(
            "The paused download receipt exceeds its size limit",
        ));
    }
    directory.replace_file(RECEIPT, &bytes)?;
    directory.sync()
}

struct Stage<'a> {
    root: &'a Directory,
    directory: Option<Directory>,
    identity: EntryIdentity,
    keep: bool,
}

impl<'a> Stage<'a> {
    fn new(root: &'a Directory, directory: Directory, keep: bool) -> io::Result<Self> {
        Ok(Self {
            root,
            identity: directory.identity()?,
            directory: Some(directory),
            keep,
        })
    }

    fn existing(root: &'a Directory) -> io::Result<Option<Self>> {
        match root.private_child(TRANSFER, false) {
            Ok(directory) => Self::new(root, directory, true).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn directory(&self) -> &Directory {
        self.directory
            .as_ref()
            .expect("active model download stage")
    }

    fn checkpoint(&mut self, receipt: Receipt) -> io::Result<()> {
        self.keep = false;
        save_receipt(self.directory(), receipt)?;
        self.keep = true;
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let Some(directory) = self.directory.take() else {
            return Ok(());
        };
        if directory.identity()? != self.identity
            || !directory.same_as(&self.root.private_deletion_child(OsStr::new(TRANSFER))?)?
        {
            return Err(io::Error::other("The model download directory changed"));
        }
        for name in directory.names(split::MAX_SHARDS + 4)? {
            let name = name
                .to_str()
                .filter(|name| {
                    matches!(*name, WEIGHTS | RECEIPT)
                        || split::is_private_name(name)
                        || name
                            .strip_prefix(".stage-")
                            .is_some_and(crate::local_ai::managed_models::valid_id)
                })
                .ok_or_else(|| io::Error::other("Unexpected model download entry"))?;
            let file = directory.open_private_file(name)?;
            require_single_link(&file)?;
            let stamp = FileStamp::read(&file)?;
            drop(file);
            directory.remove_unchanged(name, &stamp)?;
        }
        drop(directory);
        self.root
            .remove_deletion_entry(OsStr::new(TRANSFER), self.identity, true)?;
        self.root.sync()
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.cleanup();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing into a String");
    }
    output
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> io::Result<u64> {
    let space = rustix::fs::statvfs(path)?;
    Ok(space.f_bavail.saturating_mul(space.f_frsize))
}

#[cfg(windows)]
fn free_bytes(path: &Path) -> io::Result<u64> {
    kortty_files::available_space(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::managed_models::Purpose;
    use pgp::crypto::hash::HashAlgorithm;
    use std::io::{BufRead as _, Write as _};
    use url::Url;

    #[test]
    fn resume_requires_a_strong_matching_etag_and_the_exact_tail() {
        for (status, etag, range, valid) in [
            (206, "\"fixture\"", "bytes 10-19/20", true),
            (200, "\"fixture\"", "bytes 10-19/20", false),
            (206, "W/\"fixture\"", "bytes 10-19/20", false),
            (206, "\"other\"", "bytes 10-19/20", false),
            (206, "\"fixture\"", "bytes 9-19/20", false),
            (206, "\"fixture\"", "bytes 10-18/20", false),
        ] {
            let response = Response::builder()
                .status(status)
                .header("etag", etag)
                .header("content-range", range)
                .header("content-length", "10")
                .body(())
                .unwrap();
            assert_eq!(
                validate_response(&response, 10, 20, Some("\"fixture\"")).is_ok(),
                valid
            );
        }
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&6_u64.to_le_bytes());
        bytes.extend_from_slice(b"weight");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.resize(bytes.len().next_multiple_of(32), 0);
        bytes.extend_from_slice(&1_f32.to_le_bytes());
        bytes
    }

    #[test]
    fn ai_model_download_policy_rolls_back_a_prepared_model_when_publication_is_denied() {
        use std::sync::atomic::AtomicUsize;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = Directory::open(&root, false)
            .unwrap()
            .private_child("model-fixture", true)
            .unwrap();
        let bytes = fixture();
        source.publish_copy(WEIGHTS, &bytes).unwrap();
        let file = source.open_private_file(WEIGHTS).unwrap();
        let stamp = FileStamp::read(&file).unwrap();
        drop(file);
        let components = [(
            WEIGHTS.to_owned(),
            Verified {
                stamp,
                context: 1024,
                split: None,
            },
        )];
        let plan = Plan {
            repository: super::super::Repository::parse("fixture/model").unwrap(),
            revision: "a".repeat(40),
            file: super::super::Artifact {
                path: "fixture.gguf".into(),
                size: bytes.len() as u64,
                digest: hex(&HashAlgorithm::Sha256.digest(&bytes).unwrap()),
            },
            purpose: Purpose::Chat,
            additional: Vec::new(),
        };
        let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
        let checks = AtomicUsize::new(0);
        hub.authorize_downloads = Box::new(move || {
            if checks.fetch_add(1, Ordering::Relaxed) == 0 {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        });
        let error = register(
            &root,
            &source,
            &plan,
            &components,
            &AtomicBool::new(false),
            &hub,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let mut retained = Vec::new();
        source
            .open_private_file(WEIGHTS)
            .unwrap()
            .read_to_end(&mut retained)
            .unwrap();
        assert_eq!(retained, bytes);
        assert!(Registry::new(root).list().unwrap().is_empty());
    }

    #[test]
    fn interrupted_http_download_resumes_and_registers_only_the_verified_gguf() {
        use crate::local_ai::test_support::{accept, listener};
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (server, _) = listener();
        let origin = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
        let bytes = fixture();
        let model_bytes = bytes.clone();
        let size = bytes.len() as u64;
        let split = 25_usize;
        let plan = Plan {
            repository: super::super::Repository::parse("fixture/model").unwrap(),
            revision: "a".repeat(40),
            file: super::super::Artifact {
                path: "fixture.gguf".into(),
                size,
                digest: hex(&HashAlgorithm::Sha256.digest(&bytes).unwrap()),
            },
            purpose: Purpose::Chat,
            additional: Vec::new(),
        };
        let wire_path = format!(
            "GET /fixture/model/resolve/{}/fixture.gguf HTTP/1.1",
            plan.revision
        );
        let worker = std::thread::spawn(move || {
            let read = |socket: &mut std::io::BufReader<std::net::TcpStream>| {
                let mut line = String::new();
                socket.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), wire_path);
                let mut headers = Vec::new();
                loop {
                    line.clear();
                    assert!(socket.read_line(&mut line).unwrap() != 0);
                    if line == "\r\n" {
                        break;
                    }
                    headers.push(line.to_ascii_lowercase());
                }
                headers
            };
            let mut first = accept(&server);
            let headers = read(&mut first);
            assert!(!headers.iter().any(|line| line.starts_with("range:")));
            write!(first.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n").unwrap();
            first.get_mut().write_all(&model_bytes[..split]).unwrap();
            drop(first);
            let mut second = accept(&server);
            let headers = read(&mut second);
            assert!(
                headers
                    .iter()
                    .any(|line| line.trim_end() == format!("range: bytes={split}-"))
            );
            assert!(
                headers
                    .iter()
                    .any(|line| line.trim_end() == "if-range: \"fixture\"")
            );
            write!(second.get_mut(), "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {split}-{}/{size}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n", size - split as u64, size - 1).unwrap();
            second.get_mut().write_all(&model_bytes[split..]).unwrap();
        });
        let make_hub = || {
            let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
            hub.origin = origin.clone();
            hub
        };
        let flag = AtomicBool::new(false);
        let first = run(&root, &plan, &make_hub(), &flag, &flag, |_| {}).unwrap();
        assert!(matches!(
            first,
            Downloaded::Paused {
                interrupted: true,
                ..
            }
        ));
        assert_eq!(read_paused(&root).unwrap(), Some(plan.clone()));
        assert!(Registry::new(root.clone()).list().unwrap().is_empty());
        let final_result = run(&root, &plan, &make_hub(), &flag, &flag, |_| {}).unwrap();
        let Downloaded::Installed(model, _) = final_result else {
            panic!("expected the verified model registration");
        };
        assert_eq!(
            std::fs::read(Registry::new(root.clone()).open(&model.id).unwrap().path).unwrap(),
            bytes
        );
        assert!(read_paused(&root).unwrap().is_none());
        worker.join().unwrap();
    }
}
