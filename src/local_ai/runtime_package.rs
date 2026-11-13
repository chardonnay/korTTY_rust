//! Installation of packages named by the signed llama.cpp runtime index.

pub(super) mod extract;
pub(super) mod install;
pub(super) mod loopback;
pub(crate) mod online;
mod pool;
mod process;
#[cfg(test)]
pub(crate) use install::review_removal as inspect_removal;
pub(crate) use install::{
    RemovalPlan, active_package, active_package_backend, lock_active_package,
};
pub(super) use pool::acquire_mlx;
pub(super) use pool::{Lease, acquire as acquire_model};
pub(crate) use pool::{ModelStatus, model_status, shutdown, stop_model, with_stopped_model};
pub(crate) use pool::{mlx_model_status, with_mlx_maintenance, with_stopped_mlx_model};
pub(in crate::local_ai) use pool::{trust_changed, with_mlx_maintenance_io};

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::runtime_index::{
    API_CONTRACT_VERSION, Host, MAX_INDEX_BYTES, MAX_SIGNATURE_BYTES, PackageBackend, VerifiedIndex,
};

const DIRECTORY: &str = "llama-runtime";
const INDEX_STATE: &str = "accepted-index.json";
const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
// This is the pinned Java release whose runtime contract the native port targets.
const COMPATIBILITY_VERSION: &str = "2.26.0";

static IMPORTING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum BackendUse {
    RequireMatch,
    AllowCpuFallback,
}

pub(crate) struct Import {
    pub(crate) index: PathBuf,
    pub(crate) signature: PathBuf,
    pub(crate) archive: PathBuf,
    pub(crate) expected_backend: Option<PackageBackend>,
}

pub(crate) struct Cancel(Arc<AtomicBool>, Option<ureq::Agent>);

impl Cancel {
    pub(crate) fn request(&self) {
        self.0.store(true, Ordering::Release);
        if let Some(agent) = &self.1 {
            agent.clone().shutdown();
        }
    }

    #[cfg(test)]
    pub(crate) fn test_token() -> (Self, Arc<AtomicBool>) {
        let requested = Arc::new(AtomicBool::new(false));
        (Self(Arc::clone(&requested), None), requested)
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.request();
    }
}

struct Worker;

impl Drop for Worker {
    fn drop(&mut self) {
        IMPORTING.store(false, Ordering::Release);
    }
}

pub(crate) struct Pending<T> {
    pub(crate) reply: async_channel::Receiver<Result<T, String>>,
    pub(crate) cancel: Cancel,
}

pub(in crate::local_ai) type Authorization = dyn Fn() -> io::Result<File> + Send + Sync;

pub(crate) fn start(profile: PathBuf, paths: Import) -> Result<Pending<String>, String> {
    work(move |cancelled| import(&profile, &paths, cancelled))
}

pub(crate) fn review_removal(profile: PathBuf) -> Result<Pending<Option<RemovalPlan>>, String> {
    work(move |cancelled| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        let plan = install::review_removal(&profile);
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        plan.map_err(|error| format!("Could not review runtime removal ({error})"))
    })
}

pub(crate) fn remove(
    profile: PathBuf,
    plan: RemovalPlan,
) -> Result<Pending<Option<String>>, String> {
    work(move |cancelled| {
        install::uninstall(&profile, &plan, cancelled)
            .map_err(|error| format!("Could not remove the installed runtime ({error})"))
    })
}

pub(crate) fn test_model(profile: PathBuf, model: String) -> Result<Pending<String>, String> {
    work(move |cancelled| process::test_model(&profile, &model, None, cancelled))
}

pub(crate) fn test_model_with_runtime(
    profile: PathBuf,
    model: String,
    runtime: online::Choice,
) -> Result<Pending<String>, String> {
    work(move |cancelled| process::test_model(&profile, &model, Some(runtime), cancelled))
}

pub(crate) fn start_model(
    profile: &Path,
    model: &str,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + process::STARTUP_TIMEOUT;
    let _lease = pool::acquire(
        profile,
        model,
        None,
        None,
        BackendUse::RequireMatch,
        deadline,
        cancelled,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn work<T: Send + 'static>(
    run: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    work_with_agent(None, run)
}

pub(super) fn work_with_agent<T: Send + 'static>(
    agent: Option<ureq::Agent>,
    run: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    IMPORTING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "A managed local model operation is already running")?;
    let worker = Worker;
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let (send, reply) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-managed-model".into())
        .spawn(move || {
            let result = run(&token);
            drop(worker);
            let _ = send.send_blocking(result);
        })
        .map_err(|error| format!("Could not start the managed local model operation ({error})"))?;
    Ok(Pending {
        reply,
        cancel: Cancel(cancelled, agent),
    })
}

fn import(profile: &Path, paths: &Import, cancelled: &AtomicBool) -> Result<String, String> {
    check_cancelled(cancelled).map_err(|error| error.to_string())?;
    let index_bytes = read_input(&paths.index, MAX_INDEX_BYTES as u64)
        .map_err(|error| format!("Could not read the signed runtime index ({error})"))?;
    let signature = read_input(&paths.signature, MAX_SIGNATURE_BYTES as u64)
        .map_err(|error| format!("Could not read the runtime index signature ({error})"))?;
    let signature =
        std::str::from_utf8(&signature).map_err(|_| "The signature must contain Base64 text")?;
    let index = VerifiedIndex::verify(&index_bytes, signature)?;
    let host = Host::current()?;
    let archive = open_input(&paths.archive)
        .map_err(|error| format!("Could not open the runtime ZIP archive ({error})"))?;
    let receipt = install::Receipt {
        index: &index,
        bytes: &index_bytes,
        signature,
        automatic: None,
        online: false,
        pinned: false,
    };
    install::activate(
        profile,
        receipt,
        host,
        archive,
        paths.expected_backend,
        cancelled,
        process::version,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted && cancelled.load(Ordering::Acquire) {
            "Runtime package import cancelled.".into()
        } else {
            format!("Could not import the runtime package ({error})")
        }
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptedIndex {
    generated_at: String,
    sha256: String,
    revoked_ids: BTreeSet<String>,
}

impl AcceptedIndex {
    fn read(directory: &Directory) -> io::Result<Option<Self>> {
        let file = match directory.open_private_file(INDEX_STATE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let bytes = read_bounded(file, MAX_STATE_BYTES)?;
        let state: Self = serde_json::from_slice(&bytes)?;
        if state.sha256.len() != 64
            || !state.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || state.revoked_ids.iter().any(|id| id.is_empty())
        {
            return Err(io::Error::other("Invalid accepted runtime index state"));
        }
        Ok(Some(state))
    }

    fn accept(
        directory: &Directory,
        index: &VerifiedIndex,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        let digest = index.sha256().to_string();
        let generated_at = index.generated_at();
        let mut revoked_ids = match Self::read(directory)? {
            Some(state) => {
                let previous: jiff::Timestamp =
                    state.generated_at.parse().map_err(io::Error::other)?;
                if generated_at < previous {
                    return Err(io::Error::other("Signed runtime index replay rejected"));
                }
                if generated_at == previous {
                    if digest != state.sha256 {
                        return Err(io::Error::other("Signed runtime index timestamp collision"));
                    }
                    return Ok(state);
                }
                state.revoked_ids
            }
            None => BTreeSet::new(),
        };
        revoked_ids.extend(index.revoked_ids().iter().cloned());
        let state = Self {
            generated_at: generated_at.to_string(),
            sha256: digest,
            revoked_ids,
        };
        let bytes = serde_json::to_vec(&state)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::other(
                "Accepted runtime index state exceeds its size limit",
            ));
        }
        directory.replace_with(
            INDEX_STATE,
            |file| file.write_all(&bytes),
            || check_cancelled(cancelled),
        )?;
        directory.sync()?;
        Ok(state)
    }
}

pub(super) fn copy_archive(
    input: &mut File,
    output: &mut impl Write,
    size: u64,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = vec![0; 64 * 1024];
    let mut remaining = size;
    while remaining != 0 {
        check_cancelled(cancelled)?;
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(io::Error::other(
                "Runtime archive became shorter while reading",
            ));
        }
        let bytes = &buffer[..count];
        hash.update(bytes);
        output.write_all(bytes)?;
        remaining -= count as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::other("Runtime archive grew while reading"));
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn open_input(path: &Path) -> io::Result<File> {
    let (directory, name) = open_input_parent(path)?;
    directory.open_file(name)
}

pub(super) fn open_input_parent(path: &Path) -> io::Result<(Directory, &str)> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::other("Enter an absolute file path"));
    }
    #[cfg(windows)]
    if !matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
    ) {
        return Err(io::Error::other("Enter a path on a local Windows drive"));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.chars().any(char::is_control))
        .ok_or_else(|| io::Error::other("Enter an ordinary file name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("The file has no parent directory"))?;
    Ok((Directory::open(parent, false)?, name))
}

fn read_input(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    read_bounded(open_input(path)?, limit)
}

pub(super) fn read_bounded(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("File exceeds its size limit"));
    }
    stamp.require_unchanged(&file)?;
    Ok(bytes)
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if pool::is_closing() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Managed models are shutting down",
        ))
    } else if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Managed runtime operation cancelled",
        ))
    } else {
        Ok(())
    }
}
