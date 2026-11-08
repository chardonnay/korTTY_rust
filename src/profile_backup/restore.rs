//! Replay a validated restore before opening any live profile stores.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::{Snapshot, archive, data};
use crate::private_files::{Directory, nonce};
use crate::ssh_key_catalog::{Catalog, KeySource};

mod merge;
#[cfg(test)]
mod tests;

const LEASE: &str = ".profile.lock";
const PENDING: &str = ".pending-backup-restore";
const PENDING_MAGIC: &[u8] = b"kortty restore\x01";
const PENDING_MAGIC_WITH_MODE: &[u8] = b"kortty restore\x02";
const KEEP_EXISTING: u8 = 1;
const NONCE_LEN: usize = 32;
const PINS: &str = "ssh/known_hosts";
const JOBS: &str = "jobs.json";
const RECORDINGS: &str = "job-recordings";
const JOB_HISTORY: &str = "job-history";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestoreMode {
    Replace,
    KeepExisting,
}

pub(crate) struct ProfileLease {
    lock: File,
    root: PathBuf,
}

impl ProfileLease {
    pub(crate) fn acquire(root: &Path, protect_pins: impl Fn() -> bool) -> io::Result<Self> {
        let (root, directory) = open_root(root)?;
        let lease = Self {
            lock: directory.open_lock(LEASE)?,
            root,
        };
        lease.lock.try_lock_shared().map_err(lock_error)?;
        if missing(directory.open_private_file(PENDING))?.is_some() {
            lease.lock.unlock()?;
            lease.lock.try_lock().map_err(lock_error)?;
            if let Some(file) = missing(directory.open_private_file(PENDING))? {
                apply_pending(&lease.root, &directory, file, &protect_pins).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("The backup restore is still pending. {error}"),
                    )
                })?;
            }
            lease.lock.unlock()?;
            lease.lock.try_lock_shared().map_err(lock_error)?;
        }
        Ok(lease)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ProfileLease {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

#[cfg(test)]
fn queue(root: &Path, snapshot: Snapshot, protect_pins: bool) -> io::Result<bool> {
    queue_with_mode(root, snapshot, RestoreMode::Replace, protect_pins)
}

pub(super) fn queue_with_mode(
    root: &Path,
    mut snapshot: Snapshot,
    mode: RestoreMode,
    protect_pins: bool,
) -> io::Result<bool> {
    let (_, directory) = open_root(root)?;
    match mode {
        RestoreMode::Replace => check_pins(&directory, &snapshot, protect_pins)?,
        RestoreMode::KeepExisting => {
            if protect_pins {
                snapshot.remove(PINS);
            }
        }
    }
    let payload = archive::encode_snapshot(&snapshot)?;
    drop(snapshot);
    let id = nonce()?;
    let mut pending = Zeroizing::new(Vec::with_capacity(
        PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN + payload.len(),
    ));
    match mode {
        RestoreMode::Replace => pending.extend_from_slice(PENDING_MAGIC),
        RestoreMode::KeepExisting => {
            pending.extend_from_slice(PENDING_MAGIC_WITH_MODE);
            pending.push(KEEP_EXISTING);
        }
    }
    pending.extend_from_slice(id.as_bytes());
    pending.extend_from_slice(&payload);
    directory.publish_copy(PENDING, &pending)?;
    Ok(directory.sync().is_ok())
}

fn open_root(root: &Path) -> io::Result<(PathBuf, Directory)> {
    if !root.is_absolute() {
        return Err(invalid("The profile path must be absolute."));
    }
    // Resolve an existing profile alias or its nearest existing ancestor once.
    // Creating the remaining components through handles still refuses redirects.
    for ancestor in root.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(parent) => {
                let relative = root.strip_prefix(ancestor).map_err(io::Error::other)?;
                let create = !relative.as_os_str().is_empty();
                let path = if create {
                    parent.join(relative)
                } else {
                    parent
                };
                let directory = Directory::open(&path, create)?;
                return Ok((path, directory));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "The profile has no existing parent.",
    ))
}

fn apply_pending(
    root: &Path,
    directory: &Directory,
    file: File,
    protect_pins: &impl Fn() -> bool,
) -> io::Result<()> {
    let pending = read_bounded(
        file,
        PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN + archive::MAX_PAYLOAD_BYTES,
    )?;
    let (mode, body) = if let Some(body) = pending.strip_prefix(PENDING_MAGIC) {
        (RestoreMode::Replace, body)
    } else if let Some(body) = pending
        .strip_prefix(PENDING_MAGIC_WITH_MODE)
        .and_then(|body| body.strip_prefix(&[KEEP_EXISTING]))
    {
        (RestoreMode::KeepExisting, body)
    } else {
        return Err(invalid("Invalid pending backup restore mode."));
    };
    let (id, payload) = body
        .split_at_checked(NONCE_LEN)
        .ok_or_else(|| invalid("Invalid pending backup restore."))?;
    let id = std::str::from_utf8(id)
        .ok()
        .filter(|id| {
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| invalid("Invalid pending backup restore identifier."))?
        .to_owned();
    let snapshot = archive::decode_snapshot(payload)?;
    drop(pending);
    data::validate(&snapshot)?;
    match mode {
        RestoreMode::Replace => apply_replace(root, directory, &id, &snapshot, protect_pins)?,
        RestoreMode::KeepExisting => merge::apply(directory, &snapshot, protect_pins)?,
    }
    directory.remove_copy(PENDING)?;
    // A repeated marker after a power loss safely replays the same snapshot.
    let _ = directory.sync();
    Ok(())
}

fn apply_replace(
    root: &Path,
    directory: &Directory,
    id: &str,
    snapshot: &Snapshot,
    protect_pins: &impl Fn() -> bool,
) -> io::Result<()> {
    check_pins(directory, snapshot, protect_pins())?;

    let removals: BTreeSet<String> = data::STATIC_PATHS
        .iter()
        .map(|path| (*path).to_owned())
        .chain(data::existing_project_paths(root)?)
        .chain(old_key_paths(root))
        .filter(|path| !snapshot.contains_key(path))
        .collect();
    let paths = snapshot.keys().chain(removals.iter());
    for path in paths {
        with_target(directory, path, false, |parent, name| {
            missing(parent.open_file(name)).map(|_| ())
        })
        .map_err(|error| path_error(path, error))?;
    }
    preserve_job_history(
        directory,
        id,
        snapshot.get(JOBS).map(|data| data.as_slice()),
    )?;

    for path in removals {
        if path == PINS && protect_pins() {
            check_pins(directory, snapshot, true)?;
            continue;
        }
        with_target(directory, &path, false, |parent, name| {
            if missing(parent.remove_file(name))?.is_some() {
                parent.sync()?;
            }
            Ok(())
        })
        .map_err(|error| path_error(&path, error))?;
    }
    for (path, data) in snapshot {
        if path == PINS && protect_pins() {
            check_pins(directory, snapshot, true)?;
            continue;
        }
        with_target(directory, path, true, |parent, name| {
            parent.replace_file(name, data)?;
            parent.sync()
        })
        .map_err(|error| path_error(path, error))?;
    }
    Ok(())
}

fn with_target<T>(
    directory: &Directory,
    path: &str,
    create: bool,
    operation: impl FnOnce(&Directory, &str) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let Some((parent, name)) = path.split_once('/') else {
        return operation(directory, path).map(Some);
    };
    let result = if parent == "ssh-keys" || cfg!(unix) {
        directory.private_child(parent, create)
    } else {
        // Older Windows profiles use inherited ACLs on these directories. New
        // children and all restored files use the private file creation path.
        directory.subdirectory(parent, create)
    };
    if create {
        operation(&result?, name).map(Some)
    } else {
        missing(result)?
            .map(|directory| operation(&directory, name))
            .transpose()
    }
}

fn old_key_paths(root: &Path) -> Vec<String> {
    let Ok(catalog) = Catalog::open(root.to_owned()) else {
        // A corrupt old catalog is replaceable. Unknown files are left alone.
        return Vec::new();
    };
    catalog
        .records()
        .iter()
        .filter_map(|key| match &key.source {
            KeySource::Managed(name) => {
                Some([format!("ssh-keys/{name}"), format!("ssh-keys/{name}.pub")])
            }
            KeySource::External(_) => None,
        })
        .flatten()
        .collect()
}

fn check_pins(directory: &Directory, snapshot: &Snapshot, protected: bool) -> io::Result<()> {
    if !protected {
        return Ok(());
    }
    let current = with_target(directory, PINS, false, |parent, name| {
        missing(parent.open_file(name))?
            .map(|file| read_bounded(file, archive::MAX_PAYLOAD_BYTES))
            .transpose()
    })?
    .flatten();
    if current.as_deref() != snapshot.get(PINS).map(|bytes| &**bytes) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Managed policy blocks replacing trusted SSH host keys from this backup.",
        ));
    }
    Ok(())
}

fn preserve_job_history(
    directory: &Directory,
    transaction: &str,
    desired_jobs: Option<&[u8]>,
) -> io::Result<()> {
    let jobs = missing(directory.open_file(JOBS))?
        .map(|file| read_bounded(file, kortty_jobs::MAX_STORE_BYTES as usize))
        .transpose()?;
    let recordings = missing(directory.subdirectory(RECORDINGS, false))?.is_some();
    if !recordings
        && jobs
            .as_ref()
            .is_none_or(|bytes| Some(bytes.as_slice()) == desired_jobs)
    {
        return Ok(());
    }
    let history = directory.private_child(JOB_HISTORY, true)?;
    let previous = history.private_child(transaction, true)?;
    if let Some(jobs) = jobs
        && missing(previous.open_private_file(JOBS))?.is_none()
    {
        previous.publish_copy(JOBS, &jobs)?;
        previous.sync()?;
    }
    if recordings {
        directory.move_subdirectory(RECORDINGS, &previous)?;
        previous.sync()?;
        directory.sync()?;
    }
    Ok(())
}

fn read_bounded(file: File, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let len = file.metadata()?.len();
    if len > limit as u64 {
        return Err(invalid("Profile restore file exceeds its size limit."));
    }
    let mut data = Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1).read_to_end(&mut data)?;
    if data.len() as u64 != len {
        return Err(invalid("Profile restore file changed while reading."));
    }
    Ok(data)
}

fn missing<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}

fn lock_error(error: TryLockError) -> io::Error {
    match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Close the other kortty instances before applying the pending backup, then start kortty again.",
        ),
        TryLockError::Error(error) => error,
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn path_error(path: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("Could not restore {path} ({error})."))
}
